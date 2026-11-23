//! Link previews made by the sender, as Signal makes them: while a link is typed the device reads
//! the page's title, description and picture, and on sending attaches them to the message as
//! `com.beeper.linkpreviews` (MSC4095), the picture uploaded like any attachment. Readers fetch
//! nothing: the site sees the sender once and never learns who reads the message.

use std::io::Cursor;
use std::time::Duration;

use anyhow::{Result, bail};
use image::AnimationDecoder as _;
use image::codecs::gif::{GifDecoder, GifEncoder, Repeat};
use matrix_sdk::Room;
use matrix_sdk::reqwest::{self, Url, header};
use matrix_sdk::ruma::events::room::MediaSource;
use matrix_sdk::ruma::html::{Html, NodeData, NodeRef};
use serde_json::{Value, json};

use super::rich::attr;

const TIMEOUT: Duration = Duration::from_secs(10);
/// Bytes of a page read for its head; the tags a preview needs come before the body.
const MAX_PAGE: usize = 512 * 1024;
/// Elements of a page looked at: past the head there is nothing more to find.
const MAX_NODES: usize = 20_000;
/// Largest picture taken from a site.
pub(crate) const MAX_PICTURE: usize = 8 * 1024 * 1024;
/// Longest side of a still picture sent with a preview.
const PICTURE_PX: u32 = 800;
/// Decoded bytes of all of a GIF's frames, which a reader holds while it plays. A larger one goes
/// out scaled down; one from elsewhere shows as a still.
pub(crate) const MAX_MOVING: u64 = 16 * 1024 * 1024;
/// Frames of a moving picture kept; a longer one stops at its last.
const MAX_FRAMES: usize = 150;
/// Longest title or description kept from a page.
const TEXT_MAX: usize = 300;
const AGENT: &str = "Mozilla/5.0 (compatible; Unisend link preview)";
/// The page's color, which the card's bar takes, as only Unisend writes and reads it.
pub(crate) const COLOR_KEY: &str = "unisend:theme_color";

/// What a page says about itself in its head.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Meta {
    pub title: String,
    pub description: String,
    pub site: String,
    /// The picture's address, made absolute.
    pub image: Option<String>,
    /// `#rrggbb`, from the page's `theme-color`.
    pub color: Option<String>,
    /// A video page: the card's picture gets a play mark.
    pub video: bool,
}

pub(crate) struct Picture {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub width: u32,
    pub height: u32,
}

/// A link's preview, ready to attach. `url` is the link as typed.
pub(crate) struct Preview {
    pub url: String,
    pub meta: Meta,
    pub picture: Option<Picture>,
}

/// Reads the page at `url`, or the picture it is, and the page's picture.
pub(super) async fn fetch(url: &str) -> Result<Preview> {
    let page = Url::parse(url)?;
    if !allowed(&page) {
        bail!("not an address on the web");
    }
    // Redirects are followed only to where a link itself could go.
    let http = super::http_builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| if attempt.previous().len() < 5 && allowed(attempt.url()) { attempt.follow() } else { attempt.stop() }))
        .build()?;
    let http = &http;
    let response = get(http, page).await?;
    let base = response.url().clone();
    let kind = response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default().to_ascii_lowercase();
    let host = base.host_str().unwrap_or_default().trim_start_matches("www.").to_owned();
    // A link straight to a picture is shown as that picture.
    if kind.starts_with("image/") {
        let bytes = read(response, MAX_PICTURE, false).await?;
        let picture = tokio::task::spawn_blocking(move || picture(&bytes)).await??;
        return Ok(Preview { url: url.to_owned(), meta: Meta { site: host, ..Meta::default() }, picture: Some(picture) });
    }
    if !kind.starts_with("text/html") && !kind.starts_with("application/xhtml") {
        bail!("not a page");
    }
    let head = read(response, MAX_PAGE, true).await?;
    let mut meta = parse(&String::from_utf8_lossy(&head), &base);
    if meta.site.is_empty() {
        meta.site = host;
    }
    let picture = match meta.image.as_deref().and_then(|src| Url::parse(src).ok()).filter(allowed) {
        Some(src) => match fetch_picture(http, src).await {
            Ok(picture) => Some(picture),
            Err(e) => {
                log::debug!("link preview picture: {e:#}");
                None
            }
        },
        None => None,
    };
    if meta.title.is_empty() && meta.description.is_empty() && picture.is_none() {
        bail!("nothing to preview");
    }
    Ok(Preview { url: url.to_owned(), meta, picture })
}

/// Only what a site can be: https and a domain name, so a link never has the device reach into
/// the network it is on (a router, a printer) or send a page over plain http. Debug builds also
/// take this machine, for tests.
pub(crate) fn allowed(url: &Url) -> bool {
    if cfg!(debug_assertions) && matches!(url.host_str(), Some("127.0.0.1" | "localhost")) {
        return matches!(url.scheme(), "http" | "https");
    }
    url.scheme() == "https" && url.domain().is_some_and(|d| d != "localhost" && d.contains('.'))
}

async fn fetch_picture(http: &reqwest::Client, src: Url) -> Result<Picture> {
    let response = get(http, src).await?;
    let bytes = read(response, MAX_PICTURE, false).await?;
    tokio::task::spawn_blocking(move || picture(&bytes)).await?
}

async fn get(http: &reqwest::Client, url: Url) -> Result<reqwest::Response> {
    Ok(http
        .get(url)
        .header(header::USER_AGENT, AGENT)
        .header(header::ACCEPT, "text/html,application/xhtml+xml,image/*;q=0.8")
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| e.without_url())?
        .error_for_status()
        .map_err(|e| e.without_url())?)
}

/// The body up to `max` bytes; a page stops at the end of its head, a picture past `max` fails.
async fn read(mut response: reqwest::Response, max: usize, page: bool) -> Result<Vec<u8>> {
    if !page && response.content_length().is_some_and(|n| n > max as u64) {
        bail!("the file is too large");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.without_url())? {
        let from = body.len().saturating_sub(6);
        body.extend_from_slice(&chunk);
        if body.len() > max {
            if !page {
                bail!("the file is too large");
            }
            body.truncate(max);
            break;
        }
        if page && body[from..].windows(6).any(|w| w.eq_ignore_ascii_case(b"</head")) {
            break;
        }
    }
    Ok(body)
}

/// The preview's tags of a page: Open Graph first, then Twitter's, then plain HTML.
pub(crate) fn parse(html: &str, base: &Url) -> Meta {
    let doc = Html::parse(html);
    let mut tags: Vec<(String, String)> = Vec::new();
    let mut title = String::new();
    let mut stack: Vec<NodeRef> = doc.children().collect::<Vec<_>>().into_iter().rev().collect();
    let mut seen = 0;
    while let Some(node) = stack.pop() {
        seen += 1;
        if seen > MAX_NODES {
            break;
        }
        let NodeData::Element(el) = node.data() else { continue };
        match &*el.name.local {
            "meta" => {
                let key = attr(&node, "property").or_else(|| attr(&node, "name"));
                if let (Some(key), Some(value)) = (key, attr(&node, "content")) {
                    tags.push((key.trim().to_ascii_lowercase(), value));
                }
            }
            "title" if title.is_empty() => {
                title = node.children().filter_map(|c| if let NodeData::Text(t) = c.data() { Some(t.borrow().to_string()) } else { None }).collect();
            }
            "script" | "style" | "noscript" | "svg" => continue,
            _ => {}
        }
        stack.extend(node.children().collect::<Vec<_>>().into_iter().rev());
    }
    let tag = |keys: &[&str]| keys.iter().find_map(|k| tags.iter().find(|(key, v)| key == k && !v.trim().is_empty()).map(|(_, v)| v.clone()));
    let image = tag(&["og:image:secure_url", "og:image", "og:image:url", "twitter:image", "twitter:image:src"])
        .and_then(|src| base.join(src.trim()).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .map(String::from);
    let video = tag(&["og:type"]).is_some_and(|t| t.trim().starts_with("video"))
        || tag(&["og:video", "og:video:url", "og:video:secure_url"]).is_some()
        || tag(&["twitter:card"]).is_some_and(|c| c.trim() == "player");
    Meta {
        title: clean(&tag(&["og:title", "twitter:title"]).unwrap_or(title)),
        description: clean(&tag(&["og:description", "twitter:description", "description"]).unwrap_or_default()),
        site: clean(&tag(&["og:site_name", "application-name"]).unwrap_or_default()),
        image,
        color: tag(&["theme-color"]).and_then(|c| color(&c)),
        video,
    }
}

/// One line of at most `TEXT_MAX` characters.
fn clean(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(TEXT_MAX).collect()
}

/// `#rgb` or `#rrggbb` as `#rrggbb`; anything else is not taken.
pub(crate) fn color(text: &str) -> Option<String> {
    let hex = text.trim().strip_prefix('#')?;
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => Some(format!("#{}", hex.to_ascii_lowercase())),
        3 => Some(format!("#{}", hex.chars().flat_map(|c| [c, c]).collect::<String>().to_ascii_lowercase())),
        _ => None,
    }
}

/// The picture as it goes out: a moving GIF kept moving within `MAX_MOVING`, anything else a
/// still of at most `PICTURE_PX`, which also leaves the site's metadata behind.
fn picture(bytes: &[u8]) -> Result<Picture> {
    if let Some((width, height, frames)) = gif_frames(bytes).filter(|&(_, _, frames)| frames > 1) {
        let decoded = u64::from(width) * u64::from(height) * 4 * frames.min(MAX_FRAMES) as u64;
        if decoded <= MAX_MOVING && frames <= MAX_FRAMES {
            return Ok(Picture { bytes: bytes.to_vec(), mime: "image/gif", width, height });
        }
        let scale = (MAX_MOVING as f64 / decoded as f64).sqrt().min(1.);
        let (w, h) = (((f64::from(width) * scale) as u32).max(1), ((f64::from(height) * scale) as u32).max(1));
        let mut out = Vec::new();
        {
            let mut encoder = GifEncoder::new_with_speed(&mut out, 30);
            encoder.set_repeat(Repeat::Infinite)?;
            for frame in GifDecoder::new(Cursor::new(bytes))?.into_frames().take(MAX_FRAMES) {
                let frame = frame?;
                let delay = frame.delay();
                let small = image::imageops::resize(frame.buffer(), w, h, image::imageops::FilterType::Triangle);
                encoder.encode_frame(image::Frame::from_parts(small, 0, 0, delay))?;
            }
        }
        return Ok(Picture { bytes: out, mime: "image/gif", width: w, height: h });
    }
    let image = image::load_from_memory(bytes)?;
    let image = if image.width().max(image.height()) > PICTURE_PX { image.thumbnail(PICTURE_PX, PICTURE_PX) } else { image };
    let mut out = Cursor::new(Vec::new());
    let mime = if image.color().has_alpha() {
        image.write_to(&mut out, image::ImageFormat::Png)?;
        "image/png"
    } else {
        image.to_rgb8().write_to(&mut out, image::ImageFormat::Jpeg)?;
        "image/jpeg"
    };
    Ok(Picture { bytes: out.into_inner(), mime, width: image.width(), height: image.height() })
}

/// A GIF's size and number of frames, read from its blocks without decoding any; `None` when it
/// is not a GIF or is cut short.
pub(crate) fn gif_frames(bytes: &[u8]) -> Option<(u32, u32, usize)> {
    if !bytes.starts_with(b"GIF8") || bytes.len() < 13 {
        return None;
    }
    let size = |at: usize| u32::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]));
    let (width, height) = (size(6), size(8));
    let table = |packed: u8| if packed & 0x80 != 0 { 3 << ((packed & 7) + 1) } else { 0 };
    let mut at = 13 + table(bytes[10]);
    // Data sub-blocks: a length byte, that many bytes, until a zero length.
    let skip_blocks = |mut at: usize| -> Option<usize> {
        loop {
            let len = usize::from(*bytes.get(at)?);
            at += 1 + len;
            if len == 0 {
                return Some(at);
            }
        }
    };
    let mut frames = 0;
    loop {
        match *bytes.get(at)? {
            0x21 => at = skip_blocks(at + 2)?,
            0x2C => {
                let packed = *bytes.get(at + 9)?;
                at = skip_blocks(at + 10 + table(packed) + 1)?;
                frames += 1;
            }
            0x3B => return Some((width, height, frames)),
            _ => return None,
        }
    }
}

/// The message's `com.beeper.linkpreviews` entry. The picture is uploaded here, encrypted in an
/// encrypted chat; when that fails the preview goes without it.
pub(super) async fn entry(room: &Room, preview: &Preview) -> Value {
    let mut entry = json!({ "matched_url": preview.url });
    let meta = &preview.meta;
    for (key, value) in [("og:title", &meta.title), ("og:description", &meta.description), ("og:site_name", &meta.site)] {
        if !value.is_empty() {
            entry[key] = value.as_str().into();
        }
    }
    if meta.video {
        entry["og:type"] = "video".into();
    }
    if let Some(color) = &meta.color {
        entry[COLOR_KEY] = color.as_str().into();
    }
    if let Some(picture) = &preview.picture {
        // Unknown: encrypted, so the picture never sits in the clear where the chat is not.
        let encrypted = room.latest_encryption_state().await.map(|s| s.is_encrypted()).unwrap_or(true);
        let mime = picture.mime.parse().unwrap_or(mime_guess::mime::IMAGE_JPEG);
        match super::gifs::upload(&room.client(), encrypted, &mime, picture.bytes.clone()).await {
            Ok(source) => {
                match source {
                    MediaSource::Plain(uri) => entry["og:image"] = uri.to_string().into(),
                    MediaSource::Encrypted(file) => {
                        entry["og:image"] = file.url.to_string().into();
                        entry["beeper:image:encryption"] = serde_json::to_value(&*file).unwrap_or_default();
                    }
                }
                entry["og:image:type"] = picture.mime.into();
                entry["og:image:width"] = picture.width.into();
                entry["og:image:height"] = picture.height.into();
                entry["matrix:image:size"] = picture.bytes.len().into();
            }
            Err(e) => log::warn!("link preview picture not sent: {e:#}"),
        }
    }
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://www.example.org/a/page").unwrap()
    }

    #[test]
    fn a_page_says_what_it_is_in_open_graph_then_twitter_then_html() {
        let html = r##"<!doctype html><html><head><title> Plain   title </title>
            <meta name="description" content="plain description">
            <meta property="og:title" content="OG &amp; title">
            <meta name="twitter:title" content="Twitter title">
            <meta property="og:image" content="/img/cover.jpg">
            <meta name="theme-color" content="#A0F">
            <meta property="og:type" content="video.other">
            </head><body><p>text</p></body></html>"##;
        let meta = parse(html, &base());
        assert_eq!(meta.title, "OG & title");
        assert_eq!(meta.description, "plain description");
        assert_eq!(meta.image.as_deref(), Some("https://www.example.org/img/cover.jpg"));
        assert_eq!(meta.color.as_deref(), Some("#aa00ff"));
        assert!(meta.video);
        assert_eq!(meta.site, "");
        let bare = parse("<title>Only\n a title</title>", &base());
        assert_eq!((bare.title.as_str(), bare.image, bare.video), ("Only a title", None, false));
        let script = parse(r#"<head><meta property="og:image" content="javascript:alert(1)"></head>"#, &base());
        assert_eq!(script.image, None);
    }

    #[test]
    fn only_sites_on_the_web_are_read() {
        let ok = |u: &str| allowed(&Url::parse(u).unwrap());
        assert!(ok("https://www.example.org/a"));
        assert!(!ok("http://example.org/a"), "plain http");
        assert!(!ok("https://192.168.1.1/"));
        assert!(!ok("https://[::1]/"));
        assert!(!ok("https://router/"));
        assert!(!ok("ftp://example.org/"));
        assert_eq!(ok("http://127.0.0.1:8000/"), cfg!(debug_assertions));
    }

    #[test]
    fn colors_are_hex_only() {
        assert_eq!(color("#1E90FF").as_deref(), Some("#1e90ff"));
        assert_eq!(color(" #fff ").as_deref(), Some("#ffffff"));
        assert_eq!(color("red"), None);
        assert_eq!(color("#12345"), None);
        assert_eq!(color("#ggg"), None);
    }

    fn gif(frames: usize, side: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = GifEncoder::new_with_speed(&mut out, 30);
            encoder.set_repeat(Repeat::Infinite).unwrap();
            for n in 0..frames {
                let buffer = image::RgbaImage::from_pixel(side, side, image::Rgba([(n * 40) as u8, 0, 0, 255]));
                encoder.encode_frame(image::Frame::new(buffer)).unwrap();
            }
        }
        out
    }

    #[test]
    fn a_gif_is_counted_without_decoding_and_kept_moving_within_the_limit() {
        let small = gif(3, 16);
        assert_eq!(gif_frames(&small), Some((16, 16, 3)));
        assert_eq!(gif_frames(&small[..small.len() - 1]), None, "cut short");
        assert_eq!(gif_frames(b"\x89PNG"), None);
        let kept = picture(&small).unwrap();
        assert_eq!((kept.mime, kept.bytes.len()), ("image/gif", small.len()));
        // 12 frames of 600 x 600 decode to 17.3 MB: sent smaller, still moving.
        let big = picture(&gif(12, 600)).unwrap();
        let (w, h, frames) = gif_frames(&big.bytes).unwrap();
        assert_eq!(frames, 12);
        assert!(u64::from(w) * u64::from(h) * 4 * 12 <= MAX_MOVING, "{w}x{h}");
        assert_eq!((big.width, big.height), (w, h));
        // One frame is a still.
        assert_eq!(picture(&gif(1, 16)).unwrap().mime, "image/png");
    }
}

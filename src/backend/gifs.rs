//! GIFs from KLIPY (docs.klipy.com/gifs-api): trending and search for the picker, previews for its
//! grid, and sending one as the bridges expect a GIF. The app key comes from the build environment;
//! without one the picker is hidden. Requests go straight from the app to KLIPY and media is loaded
//! from the URLs KLIPY returns, unchanged, as its integration requirements ask.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use futures_util::StreamExt as _;
use matrix_sdk::{
    Client, Room, reqwest,
    room::reply::{EnforceThread, Reply},
    ruma::{
        EventId, TransactionId, UInt,
        events::room::{
            ImageInfo, MediaSource, ThumbnailInfo,
            message::{AddMentions, ImageMessageEventContent, MessageType, RoomMessageEventContent, VideoInfo, VideoMessageEventContent},
        },
    },
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use super::{Ev, safe_name, write_private};

const KEY: Option<&str> = option_env!("KLIPY_API_KEY");
const API: &str = "https://api.klipy.com/api/v1";
/// GIFs asked for at a time (KLIPY allows 8 to 50).
const PER_PAGE: u32 = 24;
/// Pages of one search or of trending the grid scrolls through: at most 96 GIFs.
pub(crate) const MAX_PAGES: u32 = 4;
/// Previews downloading at once.
const PREVIEW_FETCHES: usize = 4;
const TIMEOUT: Duration = Duration::from_secs(15);
/// Largest answer read: a page of 24 is about 65 KB with only gif, jpg and mp4 asked for.
const MAX_JSON: usize = 1024 * 1024;
/// Largest preview or still read; KLIPY's small JPEGs are around 10 KB.
const MAX_STILL: usize = 512 * 1024;
/// Largest GIF or video sent.
const MAX_MEDIA: usize = 16 * 1024 * 1024;
/// A Meta chat gets the medium GIF up to this size, else the small one.
const MAX_META_GIF: u64 = 4 * 1024 * 1024;

/// Searches and preview downloads in flight; a new search stops the previous ones.
static TASKS: Mutex<Vec<AbortHandle>> = Mutex::new(Vec::new());

pub(crate) fn enabled() -> bool {
    KEY.is_some_and(|k| !k.is_empty())
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Rendition {
    pub url: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Gif {
    pub slug: String,
    pub title: String,
    /// Small JPEG for the picker's grid.
    pub preview: Rendition,
    /// Medium JPEG, sent as the thumbnail.
    pub still: Rendition,
    pub mp4: Rendition,
    pub gif: Rendition,
    pub small_gif: Option<Rendition>,
}

/// One page of trending (empty query) or of a search.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub gifs: Vec<Gif>,
    pub more: bool,
}

#[derive(Deserialize)]
struct Answer {
    result: bool,
    data: Option<Page>,
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    data: Vec<Value>,
    #[serde(default)]
    has_next: bool,
}

#[derive(Deserialize)]
struct Item {
    slug: String,
    #[serde(default)]
    title: String,
    #[serde(rename = "type", default)]
    kind: String,
    file: Sizes,
}

#[derive(Deserialize)]
struct Sizes {
    hd: Option<Formats>,
    md: Option<Formats>,
    sm: Option<Formats>,
    xs: Option<Formats>,
}

#[derive(Clone, Deserialize)]
struct Formats {
    gif: Option<Rendition>,
    jpg: Option<Rendition>,
    mp4: Option<Rendition>,
}

impl Sizes {
    /// The first size in `order` that has the format.
    fn pick(&self, order: [&Option<Formats>; 4], format: fn(&Formats) -> &Option<Rendition>) -> Option<Rendition> {
        order.into_iter().flatten().find_map(|f| format(f).clone())
    }

    fn gif(self, slug: String, title: String) -> Option<Gif> {
        let small_first = [&self.sm, &self.md, &self.xs, &self.hd];
        let medium_first = [&self.md, &self.hd, &self.sm, &self.xs];
        Some(Gif {
            preview: self.pick(small_first, |f| &f.jpg)?,
            still: self.pick(medium_first, |f| &f.jpg)?,
            mp4: self.pick(medium_first, |f| &f.mp4)?,
            gif: self.pick(medium_first, |f| &f.gif)?,
            small_gif: self.sm.as_ref().and_then(|f| f.gif.clone()),
            slug,
            title,
        })
    }
}

/// Reads a trending or search answer. Items that are not a GIF with every rendition the app uses
/// (an ad slot, a malformed entry) are left out; the rest keep KLIPY's order.
pub(crate) fn parse(json: &[u8]) -> Result<Found> {
    let answer: Answer = serde_json::from_slice(json)?;
    let page = match answer {
        Answer { result: true, data: Some(page) } => page,
        _ => bail!("KLIPY answered without results"),
    };
    let gifs = page
        .data
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Item>(v).ok())
        .filter(|item| item.kind.is_empty() || item.kind == "gif")
        .filter_map(|item| item.file.gif(item.slug, item.title))
        .collect();
    Ok(Found { gifs, more: page.has_next })
}

/// Meta's bridge uploads a file as it is and has no GIF flag, so Messenger and Instagram get a GIF
/// file; the other bridges turn an MP4 marked `fi.mau.gif` into their network's own GIF.
pub(crate) fn wants_gif_file(network: Option<&str>) -> bool {
    network.is_some_and(|n| {
        let n = n.to_ascii_lowercase();
        ["messenger", "instagram", "facebook"].iter().any(|k| n.contains(k))
    })
}

impl Gif {
    fn file_for_meta(&self) -> &Rendition {
        match &self.small_gif {
            Some(small) if self.gif.size > MAX_META_GIF => small,
            _ => &self.gif,
        }
    }
}

/// A random ID made once per install, so KLIPY can tell its users apart without knowing who they
/// are. Never derived from the account.
fn customer_id(dir: &Path) -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let path = dir.join("klipy-id");
        if let Ok(id) = std::fs::read_to_string(&path)
            && !id.trim().is_empty()
        {
            return id.trim().to_owned();
        }
        let id = TransactionId::new().to_string();
        if let Err(e) = write_private(&path, id.as_bytes()) {
            log::warn!("klipy id: {e:#}");
        }
        id
    })
}

fn key() -> Result<&'static str> {
    KEY.filter(|k| !k.is_empty()).ok_or_else(|| anyhow!("no KLIPY key in this build"))
}

/// The body, refused past `max` bytes. Errors never carry the URL: it holds the app key.
async fn get(request: reqwest::RequestBuilder, max: usize) -> Result<Vec<u8>> {
    let mut response = request.timeout(TIMEOUT).send().await.map_err(|e| e.without_url())?.error_for_status().map_err(|e| e.without_url())?;
    if response.content_length().is_some_and(|n| n > max as u64) {
        bail!("the file is too large");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.without_url())? {
        if body.len() + chunk.len() > max {
            bail!("the file is too large");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn page(http: &reqwest::Client, dir: &Path, query: &str, page: u32) -> Result<Found> {
    let key = key()?;
    let query = query.trim();
    let endpoint = if query.is_empty() { "trending" } else { "search" };
    let mut url = reqwest::Url::parse(&format!("{API}/{key}/gifs/{endpoint}"))?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("page", &page.to_string());
        pairs.append_pair("per_page", &PER_PAGE.to_string());
        pairs.append_pair("customer_id", customer_id(dir));
        pairs.append_pair("format_filter", "gif,jpg,mp4");
        if !query.is_empty() {
            pairs.append_pair("q", query);
        }
    }
    let request = http.get(url);
    let mut found = parse(&get(request, MAX_JSON).await?)?;
    found.more &= page < MAX_PAGES;
    Ok(found)
}

/// Loads a page and then its previews, as `Ev::Gifs` and one `Ev::GifPreview` each. Page 1 is a new
/// search and stops whatever the previous one still had in flight.
pub(super) fn search(http: reqwest::Client, dir: PathBuf, ev: mpsc::Sender<Ev>, query: String, number: u32) {
    if number == 0 || number > MAX_PAGES {
        return;
    }
    let mut tasks = TASKS.lock().unwrap_or_else(|e| e.into_inner());
    if number == 1 {
        tasks.drain(..).for_each(|t| t.abort());
    }
    let task = tokio::spawn(async move {
        let found = match page(&http, &dir, &query, number).await {
            Ok(found) => found,
            Err(e) => {
                log::warn!("gifs: {e:#}");
                let _ = ev.send(Ev::Gifs { query, page: number, found: None }).await;
                return;
            }
        };
        let urls: Vec<String> = found.gifs.iter().map(|g| g.preview.url.clone()).collect();
        if ev.send(Ev::Gifs { query, page: number, found: Some(found) }).await.is_err() {
            return;
        }
        futures_util::stream::iter(urls)
            .for_each_concurrent(PREVIEW_FETCHES, |url| {
                let (http, ev) = (http.clone(), ev.clone());
                async move {
                    match get(http.get(&url), MAX_STILL).await {
                        Ok(bytes) => {
                            let _ = ev.send(Ev::GifPreview(url, bytes)).await;
                        }
                        Err(e) => log::debug!("gif preview: {e:#}"),
                    }
                }
            })
            .await;
    });
    tasks.retain(|t| !t.is_finished());
    tasks.push(task.abort_handle());
}

/// The picker closed: nothing it asked for is needed any more.
pub(super) fn stop() {
    TASKS.lock().unwrap_or_else(|e| e.into_inner()).drain(..).for_each(|t| t.abort());
}

async fn upload(client: &Client, encrypted: bool, mime: &mime_guess::Mime, data: Vec<u8>) -> Result<MediaSource> {
    Ok(if encrypted {
        MediaSource::Encrypted(Box::new(client.upload_encrypted_file(&mut Cursor::new(data)).await?))
    } else {
        MediaSource::Plain(client.media().upload(mime, data, None).await?.content_uri)
    })
}

/// The message for an uploaded GIF: an MP4 marked as a GIF, or for Meta the GIF file itself.
fn message(gif: &Gif, as_file: bool, sent: &Rendition, source: MediaSource, thumb: MediaSource, thumb_size: usize) -> Result<Value> {
    let mut thumbnail = ThumbnailInfo::new();
    thumbnail.width = Some(gif.still.width.into());
    thumbnail.height = Some(gif.still.height.into());
    thumbnail.mimetype = Some("image/jpeg".to_owned());
    thumbnail.size = UInt::new(thumb_size as u64);
    let (width, height, size) = (Some(sent.width.into()), Some(sent.height.into()), UInt::new(sent.size));
    let name = safe_name(&gif.slug);
    let msgtype = if as_file {
        let mut info = ImageInfo::new();
        (info.width, info.height, info.size) = (width, height, size);
        info.mimetype = Some("image/gif".to_owned());
        info.thumbnail_info = Some(Box::new(thumbnail));
        info.thumbnail_source = Some(thumb);
        MessageType::Image(ImageMessageEventContent::new(format!("{name}.gif"), source).info(Box::new(info)))
    } else {
        let mut info = VideoInfo::new();
        (info.width, info.height, info.size) = (width, height, size);
        info.mimetype = Some("video/mp4".to_owned());
        info.thumbnail_info = Some(Box::new(thumbnail));
        info.thumbnail_source = Some(thumb);
        MessageType::Video(VideoMessageEventContent::new(format!("{name}.mp4"), source).info(Box::new(info)))
    };
    let mut content = serde_json::to_value(RoomMessageEventContent::new(msgtype))?;
    mark(&mut content, as_file);
    Ok(content)
}

/// The flags mautrix bridges read and write for a GIF (mautrix-go event/message.go `MauGIF`;
/// mautrix-whatsapp pkg/msgconv/wa-media.go sets the rest on GIFs it brings in).
fn mark(content: &mut Value, as_file: bool) {
    let Some(info) = content.get_mut("info").and_then(Value::as_object_mut) else { return };
    let flags: &[&str] = if as_file {
        &["is_animated"]
    } else {
        &["fi.mau.gif", "fi.mau.loop", "fi.mau.autoplay", "fi.mau.hide_controls", "fi.mau.no_audio"]
    };
    for flag in flags {
        info.insert((*flag).to_owned(), Value::Bool(true));
    }
}

/// Downloads the GIF from KLIPY, sends it to the room and tells KLIPY it was shared.
pub(super) async fn send(http: &reqwest::Client, dir: &Path, room: &Room, gif: &Gif, query: &str, network: Option<&str>, reply_to: Option<String>) -> Result<()> {
    let as_file = wants_gif_file(network);
    let sent = if as_file { gif.file_for_meta() } else { &gif.mp4 };
    let (data, still) = tokio::try_join!(get(http.get(&sent.url), MAX_MEDIA), get(http.get(&gif.still.url), MAX_STILL))?;
    let encrypted = room.latest_encryption_state().await?.is_encrypted();
    let client = room.client();
    let mime: mime_guess::Mime = if as_file { "image/gif" } else { "video/mp4" }.parse()?;
    let thumb_size = still.len();
    let mut sized = sent.clone();
    sized.size = data.len() as u64;
    let (source, thumb) = tokio::try_join!(upload(&client, encrypted, &mime, data), upload(&client, encrypted, &mime_guess::mime::IMAGE_JPEG, still))?;
    let mut content = message(gif, as_file, &sized, source, thumb, thumb_size)?;
    if let Some(id) = reply_to {
        let reply = Reply { event_id: EventId::parse(id)?, enforce_thread: EnforceThread::MaybeThreaded, add_mentions: AddMentions::Yes };
        let plain: RoomMessageEventContent = serde_json::from_value(content)?;
        content = serde_json::to_value(room.make_reply_event(plain.into(), reply).await?)?;
        mark(&mut content, as_file);
    }
    room.send_raw("m.room.message", content).await?;
    shared(http, dir, &gif.slug, query).await;
    Ok(())
}

/// KLIPY's share trigger: `q` is the search that found it, empty from trending.
async fn shared(http: &reqwest::Client, dir: &Path, slug: &str, query: &str) {
    let done = async {
        let mut url = reqwest::Url::parse(&format!("{API}/{}/gifs/share/", key()?))?;
        url.path_segments_mut().map_err(|()| anyhow!("bad share URL"))?.pop_if_empty().push(slug);
        let body = serde_json::json!({ "customer_id": customer_id(dir), "q": query.trim() });
        let request = http.post(url).header("content-type", "application/json").body(serde_json::to_vec(&body)?);
        get(request, MAX_JSON).await
    };
    if let Err(e) = done.await {
        log::debug!("klipy share: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trending as KLIPY answered it (May 2026, two items), with the blur previews shortened.
    const TRENDING: &[u8] = include_bytes!("klipy-trending.json");

    #[test]
    fn a_recorded_page_is_read_in_order() {
        let found = parse(TRENDING).unwrap();
        assert!(found.more);
        let slugs: Vec<&str> = found.gifs.iter().map(|g| g.slug.as_str()).collect();
        assert_eq!(slugs, ["dog-doggy-45", "good-morning-17328"]);
        let dog = &found.gifs[0];
        assert_eq!(dog.title, "Dog Doggy");
        assert!(dog.preview.url.ends_with(".jpg") && dog.preview.width == 165 && dog.preview.height == 262);
        assert!(dog.mp4.url.ends_with(".mp4") && (dog.mp4.width, dog.mp4.height, dog.mp4.size) == (250, 396, 36362));
        assert!(dog.gif.url.ends_with(".gif") && dog.gif.size == 584380);
        assert!(dog.still.url.ends_with(".jpg") && dog.still.width == 250);
    }

    #[test]
    fn ads_and_broken_items_are_skipped() {
        let mut json: Value = serde_json::from_slice(TRENDING).unwrap();
        let items = json["data"]["data"].as_array_mut().unwrap();
        items.insert(1, serde_json::json!({ "type": "ad", "content": "<div></div>", "width": 320, "height": 50 }));
        items.insert(0, serde_json::json!({ "slug": "no-files", "type": "gif", "file": {} }));
        let found = parse(&serde_json::to_vec(&json).unwrap()).unwrap();
        assert_eq!(found.gifs.len(), 2);
        assert!(parse(br#"{"result":false,"errors":{"message":["The provided API key is invalid."]}}"#).is_err());
    }

    #[test]
    fn only_meta_chats_get_the_gif_file() {
        assert!(wants_gif_file(Some("Facebook Messenger")));
        assert!(wants_gif_file(Some("Instagram")));
        assert!(!wants_gif_file(Some("WhatsApp")));
        assert!(!wants_gif_file(Some("Signal")));
        assert!(!wants_gif_file(None));
        let mut dog = parse(TRENDING).unwrap().gifs.remove(0);
        assert_eq!(dog.file_for_meta(), &dog.gif);
        dog.gif.size = MAX_META_GIF + 1;
        assert_eq!(Some(dog.file_for_meta()), dog.small_gif.as_ref());
    }

    #[test]
    fn a_gif_is_sent_as_a_marked_mp4() {
        let dog = parse(TRENDING).unwrap().gifs.remove(0);
        let mxc = |s: &str| MediaSource::Plain(format!("mxc://example.org/{s}").into());
        let video = message(&dog, false, &dog.mp4, mxc("v"), mxc("t"), 9).unwrap();
        assert_eq!(video["msgtype"], "m.video");
        assert_eq!(video["body"], "dog-doggy-45.mp4");
        assert_eq!(video["url"], "mxc://example.org/v");
        let info = &video["info"];
        assert_eq!((info["mimetype"].as_str(), info["w"].as_u64(), info["h"].as_u64()), (Some("video/mp4"), Some(250), Some(396)));
        assert_eq!(info["thumbnail_url"], "mxc://example.org/t");
        for flag in ["fi.mau.gif", "fi.mau.loop", "fi.mau.autoplay", "fi.mau.hide_controls", "fi.mau.no_audio"] {
            assert_eq!(info[flag], true, "{flag}");
        }
        let image = message(&dog, true, &dog.gif, mxc("g"), mxc("t"), 9).unwrap();
        assert_eq!((image["msgtype"].as_str(), image["info"]["mimetype"].as_str()), (Some("m.image"), Some("image/gif")));
        assert!(image["info"].get("fi.mau.gif").is_none());
    }
}

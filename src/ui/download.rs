//! Media auto-download, after Signal's: which media loads by itself, chosen separately for mobile
//! data, Wi-Fi and roaming. Photos are the picture in the bubble; videos their poster; audio and
//! documents are fetched into the media cache before a tap, up to `PREFETCH_MAX`, so they open at
//! once and offline. What may not load waits for a tap, its place showing a download mark and the
//! size. Stickers and link previews always load: they are small.
//!
//! Android reads the network from `dev.gpui.beeper.Net` (`ConnectivityManager`: roaming, else
//! metered or not) at most every `NET_EVERY`, one JNI call; a desktop counts as Wi-Fi.

use gpui_kit::component::switch::Switch;

use super::panels::{Page, card, footnote, link, row, section};
use super::*;
use crate::i18n::t;

pub(crate) const PHOTOS: u8 = 1;
pub(crate) const AUDIO: u8 = 2;
pub(crate) const VIDEOS: u8 = 4;
pub(crate) const FILES: u8 = 8;

/// The largest file fetched before a tap: the media cache's limit per file, past which the SDK
/// would not keep it anyway.
const PREFETCH_MAX: u64 = 20 * 1024 * 1024;
/// Messages remembered as tapped to load or as fetched ahead; forgotten all at once past this.
const MAX_HELD: usize = 512;
#[cfg(target_os = "android")]
const NET_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// What loads by itself on each kind of network, as `PHOTOS | AUDIO | VIDEOS | FILES` bits.
/// Signal's defaults (from general knowledge): photos and audio on mobile data, everything on
/// Wi-Fi, nothing while roaming.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Download {
    pub mobile: u8,
    pub wifi: u8,
    pub roaming: u8,
}

impl Default for Download {
    fn default() -> Self {
        Self { mobile: PHOTOS | AUDIO, wifi: PHOTOS | AUDIO | VIDEOS | FILES, roaming: 0 }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Net {
    Mobile,
    Wifi,
    Roaming,
}

impl Download {
    fn of(&mut self, net: Net) -> &mut u8 {
        match net {
            Net::Mobile => &mut self.mobile,
            Net::Wifi => &mut self.wifi,
            Net::Roaming => &mut self.roaming,
        }
    }

    /// Whether media of this kind loads by itself on `net`.
    fn allows(mut self, net: Net, kind: MediaKind) -> bool {
        let class = class(kind);
        class == 0 || *self.of(net) & class != 0
    }
}

/// The setting a kind of media follows; 0 for what always loads.
fn class(kind: MediaKind) -> u8 {
    match kind {
        MediaKind::Image => PHOTOS,
        MediaKind::Video => VIDEOS,
        MediaKind::Audio | MediaKind::Voice => AUDIO,
        MediaKind::File => FILES,
        MediaKind::Sticker => 0,
    }
}

const KINDS: [(u8, &str); 4] = [(PHOTOS, n_!("Photos")), (AUDIO, n_!("Audio")), (VIDEOS, n_!("Videos")), (FILES, n_!("Documents"))];

/// The kinds set in `bits`, by name.
fn summary(bits: u8) -> String {
    let names: Vec<&str> = KINDS.iter().filter(|(bit, _)| bits & bit != 0).map(|&(_, name)| crate::i18n::tr(name)).collect();
    if names.is_empty() { t!("Nothing").to_owned() } else { names.join(", ") }
}

/// Media the user asked for past the setting, and media fetched ahead, by message.
#[derive(Default)]
pub(super) struct Held {
    tapped: HashSet<String>,
    fetched: HashSet<String>,
}

fn remember(set: &mut HashSet<String>, id: &str) {
    if set.len() >= MAX_HELD {
        set.clear();
    }
    set.insert(id.to_owned());
}

/// The network in use now.
pub(super) fn network() -> Net {
    #[cfg(target_os = "android")]
    {
        use std::time::Instant;
        static LAST: std::sync::Mutex<Option<(Instant, Net)>> = std::sync::Mutex::new(None);
        let mut last = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((at, net)) = *last
            && at.elapsed() < NET_EVERY
        {
            return net;
        }
        let net = android::kind().map_or(Net::Wifi, |kind| match kind {
            1 => Net::Mobile,
            2 => Net::Roaming,
            _ => Net::Wifi,
        });
        *last = Some((Instant::now(), net));
        net
    }
    #[cfg(not(target_os = "android"))]
    Net::Wifi
}

#[cfg(target_os = "android")]
mod android {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::JValue;

    pub(super) fn kind() -> Option<i32> {
        let kind = helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, "dev.gpui.beeper.Net")?;
            env.call_static_method(&class, jni::jni_str!("kind"), jni::jni_sig!("(Landroid/content/Context;)I"), &[JValue::Object(&activity)])
                .and_then(|v| v.i())
                .map_err(|e| crate::java_error(env, e))
        });
        kind.map_err(|e| log::warn!("network kind: {e}")).ok()
    }
}

impl BeeperApp {
    /// Whether `media` of message `id` loads without a tap.
    pub(super) fn may_download(&self, id: &str, media: &Media) -> bool {
        self.settings.media.download.allows(network(), media.kind) || self.storage.held.tapped.contains(id)
    }

    /// A tap on media that did not load by itself: it loads now, and stays loading by itself.
    /// Returns whether the tap was taken for that.
    pub(super) fn download_anyway(&mut self, id: &str, media: &Media, cx: &mut Context<Self>) -> bool {
        if self.may_download(id, media) {
            return false;
        }
        remember(&mut self.storage.held.tapped, id);
        cx.notify();
        // A video plays once it is downloaded; a photo shows in the bubble first.
        media.kind == MediaKind::Image
    }

    /// Fetches audio and documents shown in the chat into the media cache, where the setting
    /// allows it. One request per message, sharing the backend's limit on downloads at once.
    pub(super) fn prefetch(&mut self, ix: usize) {
        let m = &self.msgs[ix];
        let Some(media) = m.media.as_ref() else { return };
        let ahead = matches!(media.kind, MediaKind::Audio | MediaKind::Voice | MediaKind::File) && !media.once;
        if !ahead || media.size.is_none_or(|size| size > PREFETCH_MAX) || self.storage.held.fetched.contains(&m.id) || !self.may_download(&m.id, media) {
            return;
        }
        if self.cmd.capacity() >= self.cmd.max_capacity() / 2 && self.cmd.try_send(Cmd::Prefetch(media.clone())).is_ok() {
            let id = m.id.clone();
            remember(&mut self.storage.held.fetched, &id);
        }
    }

    fn set_download(&mut self, net: Net, bit: u8, on: bool, cx: &mut Context<Self>) {
        let bits = self.settings.media.download.of(net);
        *bits = if on { *bits | bit } else { *bits & !bit };
        self.settings.save(&self.dir);
        cx.notify();
    }

    /// The four switches of one network.
    fn download_switches(&mut self, net: Net, cx: &mut Context<Self>) -> Div {
        let bits = *self.settings.media.download.of(net);
        card(cx).children(KINDS.iter().enumerate().map(|(ix, &(bit, name))| {
            let weak = cx.entity().downgrade();
            let switch = Switch::new(("download", net as usize * 4 + ix)).checked(bits & bit != 0).on_click(move |on, _, cx| {
                let _ = weak.update(cx, |this, cx| this.set_download(net, bit, *on, cx));
            });
            row(crate::i18n::tr(name), "", cx).child(switch)
        }))
    }

    /// The Media auto-download section of Data and storage: a page per network on a phone, the
    /// Wi-Fi switches in place on a desktop.
    pub(super) fn render_download_section(&mut self, cx: &mut Context<Self>) -> Div {
        let body = if gpui_kit::is_mobile() {
            let d = self.settings.media.download;
            let links = [(Page::DownloadMobile, d.mobile), (Page::DownloadWifi, d.wifi), (Page::DownloadRoaming, d.roaming)].map(|(page, bits)| {
                link(page.id(), page.title(), summary(bits).into(), None, Some(IconName::ChevronRight), cx).on_click(cx.listener(move |this, _, _, cx| this.open_page(page, cx)))
            });
            card(cx).children(links)
        } else {
            self.download_switches(Net::Wifi, cx)
        };
        section(t!("Media auto-download"), cx).child(body).child(footnote(t!("Media that does not download by itself shows its size; a tap downloads it."), cx))
    }

    pub(super) fn render_download(&mut self, net: Net, cx: &mut Context<Self>) -> AnyElement {
        self.download_switches(net, cx).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use super::{Download, MediaKind, Net};

    #[test]
    fn each_network_follows_its_own_choice() {
        let d = Download::default();
        assert!(d.allows(Net::Mobile, MediaKind::Image) && d.allows(Net::Mobile, MediaKind::Voice));
        assert!(!d.allows(Net::Mobile, MediaKind::Video) && !d.allows(Net::Mobile, MediaKind::File));
        assert!(d.allows(Net::Wifi, MediaKind::File) && d.allows(Net::Wifi, MediaKind::Video));
        assert!(!d.allows(Net::Roaming, MediaKind::Image));
        assert!(d.allows(Net::Roaming, MediaKind::Sticker), "stickers always load");
    }

    #[test]
    fn settings_from_before_keep_the_defaults() {
        let parsed: crate::ui::storage::MediaSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.download, Download::default());
    }
}

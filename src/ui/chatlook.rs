//! A chat's own colour and wallpaper, as Signal's "Chat color & wallpaper" in a chat's settings:
//! either may be left to follow the one chosen for all chats in Settings, Appearance. Kept on this
//! device in `settings.json`, as Beeper Desktop keeps them per chat in its local prefs
//! (`chat_appearance.perChat`), never in account data. A chat's photo is its own file under
//! `wallpapers/`, decoded only while that chat is open.

use super::appearance::{Wallpaper, swatch};
use super::disappear::{plain_row, timer_row};
use super::*;
use crate::ChatColour;
use gpui_kit::component::Theme;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Chats with a look of their own; past this the chats no longer in the list let theirs go, then
/// the first by ID.
const MAX_LOOKS: usize = 256;
/// Where the chats' photos are kept, in the data directory.
const PHOTOS: &str = "wallpapers";

/// What one chat changes; `None` follows the choice for all chats.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ChatLook {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<ChatColour>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallpaper: Option<Wallpaper>,
    /// The file under `wallpapers/` of a `Wallpaper::Photo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photo: Option<String>,
}

impl ChatLook {
    fn is_empty(&self) -> bool {
        self.colour.is_none() && self.wallpaper.is_none()
    }
}

/// Which page of a chat's own settings its sheet shows.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum ChatPage {
    Look,
    Notify,
}

/// A fresh name for a chat's photo: hex of the time in nanoseconds, so two never meet.
pub(super) fn new_photo_name() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    format!("{nanos:016x}.jpg")
}

/// The path of a chat's photo, for a name this module made; any other name is refused, so a
/// changed `settings.json` cannot point outside `wallpapers/`.
pub(super) fn photo_path(dir: &Path, name: &str) -> Option<PathBuf> {
    let stem = name.strip_suffix(".jpg")?;
    (stem.len() == 16 && stem.bytes().all(|b| b.is_ascii_hexdigit())).then(|| dir.join(PHOTOS).join(name))
}

/// Makes room for one more look: first the chats `listed` says are gone, then the first by ID.
fn make_room(looks: &mut BTreeMap<OwnedRoomId, ChatLook>, keep: &OwnedRoomId, listed: impl Fn(&OwnedRoomId) -> bool) -> Vec<ChatLook> {
    let mut dropped = Vec::new();
    if looks.len() < MAX_LOOKS || looks.contains_key(keep) {
        return dropped;
    }
    let gone: Vec<OwnedRoomId> = looks.keys().filter(|id| !listed(id)).cloned().collect();
    for id in gone {
        dropped.extend(looks.remove(&id));
    }
    while looks.len() >= MAX_LOOKS {
        let Some((_, look)) = looks.pop_first() else { break };
        dropped.push(look);
    }
    dropped
}

impl BeeperApp {
    /// Once per frame: the own bubbles take the open chat's colour, and its photo is read. Settings
    /// and the other panels keep the colour for all chats, so their preview shows that one.
    pub(super) fn follow_chat_look(&mut self, cx: &mut Context<Self>) {
        let look = self.open.as_ref().filter(|_| self.panel.is_none()).and_then(|id| self.settings.chat_looks.get(id));
        let colour = look.and_then(|l| l.colour).unwrap_or(self.settings.chat_colour);
        if cx.try_global::<ChatColour>().copied().unwrap_or_default() != colour {
            cx.set_global(colour);
            crate::apply_palette(cx);
        }
        let want = self.open.as_ref().filter(|id| self.settings.chat_looks.get(*id).is_some_and(|l| l.wallpaper == Some(Wallpaper::Photo)));
        if self.look.chat_photo.as_ref().map(|(id, _)| id) == want {
            return;
        }
        let loaded = want.and_then(|id| {
            let path = photo_path(&self.dir, self.settings.chat_looks.get(id)?.photo.as_deref()?)?;
            // One read of a file of a few hundred KB, when a chat with a photo of its own opens.
            let bytes = std::fs::read(path).ok()?;
            Some((id.clone(), Arc::new(Image::from_bytes(ImageFormat::Jpeg, bytes))))
        });
        if let Some((_, old)) = std::mem::replace(&mut self.look.chat_photo, loaded) {
            self.released.push(old, cx);
        }
    }

    /// Changes a chat's look, keeps it within `MAX_LOOKS`, and removes the photos no look uses.
    fn change_look(&mut self, room: &OwnedRoomId, change: impl FnOnce(&mut ChatLook), cx: &mut Context<Self>) {
        let rooms = &self.rooms;
        let mut dropped = make_room(&mut self.settings.chat_looks, room, |id| rooms.iter().any(|r| &r.id == id));
        let look = self.settings.chat_looks.entry(room.clone()).or_default();
        let before = look.clone();
        change(look);
        if look.wallpaper != Some(Wallpaper::Photo) {
            look.photo = None;
        }
        if before.photo.is_some() && before.photo != look.photo {
            dropped.push(before);
        }
        if look.is_empty() {
            self.settings.chat_looks.remove(room);
        }
        for name in dropped.iter().filter_map(|l| l.photo.as_deref()) {
            if let Some(path) = photo_path(&self.dir, name) {
                let _ = std::fs::remove_file(path);
            }
        }
        // The open chat's photo is read again on the next frame.
        if let Some((_, old)) = self.look.chat_photo.take() {
            self.released.push(old, cx);
        }
        self.settings.save(&self.dir);
        cx.notify();
    }

    pub(super) fn set_chat_photo(&mut self, room: OwnedRoomId, dest: &Path, image: Arc<Image>, cx: &mut Context<Self>) {
        let name = dest.file_name().and_then(|n| n.to_str()).map(str::to_owned);
        self.change_look(&room, |look| (look.wallpaper, look.photo) = (Some(Wallpaper::Photo), name), cx);
        self.look.chat_photo = Some((room, image));
    }

    /// The chat's entry to its colour and wallpaper, on its info screen.
    pub(super) fn look_entry(&self, room: &OwnedRoomId, cx: &mut Context<Self>) -> Stateful<Div> {
        let room = room.clone();
        timer_row("look-entry", Some(IconName::Palette), t!("Chat colour and wallpaper"), cx).on_click(cx.listener(move |this, _, _, cx| {
            this.open_chat_page(room.clone(), ChatPage::Look, cx);
        }))
    }

    /// Opens the chat's sheet on one of its own settings pages.
    pub(super) fn open_chat_page(&mut self, room: OwnedRoomId, page: ChatPage, cx: &mut Context<Self>) {
        self.chat_sheet = Some(room);
        (self.disappear.picking, self.mute_picking, self.folders.picking) = (false, false, false);
        self.info.member = None;
        self.look.chat_page = Some(page);
        cx.notify();
    }

    /// The sheet of the chat's own settings page, while one is open.
    pub(super) fn chat_page_sheet(&self, room: &RoomSummary, live: bool, window: &mut Window, cx: &mut Context<Self>) -> Option<(usize, String, Vec<Stateful<Div>>)> {
        match self.look.chat_page? {
            ChatPage::Look => Some((6, t!("Chat colour and wallpaper").to_owned(), self.look_rows(&room.id, live, window, cx))),
            ChatPage::Notify => Some((7, t!("Notifications").to_owned(), self.chat_notify_rows(room, live, cx))),
        }
    }

    fn look_rows(&self, room: &OwnedRoomId, live: bool, window: &mut Window, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let p = Palette::of(cx);
        let look = self.settings.chat_looks.get(room).cloned().unwrap_or_default();
        let label = |id: &'static str, text: &'static str| h_flex().id(id).px(px(GUTTER + 4.)).pt(px(8.)).text_size(fs(13.)).text_color(p.faint).child(text);
        let colours = h_flex().id("look-colours").flex_wrap().px(px(GUTTER)).children(ChatColour::ALL.map(|colour| {
            let fill = div().bg(colour.fill().map_or(p.raised, |hex| rgb(hex).into()));
            let name = t!("Chat colour: {colour}", colour = colour.label());
            let room = room.clone();
            swatch(&name, look.colour.unwrap_or(self.settings.chat_colour) == colour, fill, cx).when(live, |el| {
                el.on_click(cx.listener(move |this, _, _, cx| this.change_look(&room, |l| l.colour = Some(colour), cx)))
            })
        }));
        let mut papers = h_flex().id("look-papers").flex_wrap().px(px(GUTTER));
        for paper in Wallpaper::PRESETS {
            let fill = div().bg(paper.fill(&p).unwrap_or_else(|| p.ground.into()));
            let room = room.clone();
            papers = papers.child(swatch(paper.label(), look.wallpaper.unwrap_or(self.settings.wallpaper) == paper, fill, cx).when(live, |el| {
                el.on_click(cx.listener(move |this, _, _, cx| this.change_look(&room, |l| l.wallpaper = Some(paper), cx)))
            }));
        }
        if let Some((_, photo)) = self.look.chat_photo.as_ref().filter(|(id, _)| id == room) {
            let fill = div().child(img(photo.clone()).size_full().object_fit(ObjectFit::Cover));
            papers = papers.child(swatch(t!("Photo"), look.wallpaper == Some(Wallpaper::Photo), fill, cx));
        }
        let mut rows = vec![label("look-colour-label", t!("Your messages")), colours, label("look-paper-label", t!("Wallpaper")), papers];
        if !Theme::global(cx).is_dark() {
            rows.push(label("look-dark-note", t!("Wallpapers show with the dark theme.")));
        }
        let choose = sheet_row("look-photo", IconName::Image, t!("Choose photo"), p.text, window, cx);
        let room_for_photo = room.clone();
        rows.push(choose.when(live, |el| el.on_click(cx.listener(move |this, _, window, cx| this.choose_photo(Some(room_for_photo.clone()), window, cx)))));
        if !look.is_empty() {
            let room = room.clone();
            let reset = plain_row("look-reset", t!("Same as all chats"), cx);
            rows.push(reset.when(live, |el| el.on_click(cx.listener(move |this, _, _, cx| this.change_look(&room, |l| *l = ChatLook::default(), cx)))));
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::{BTreeMap, ChatColour, ChatLook, MAX_LOOKS, OwnedRoomId, PHOTOS, Path, make_room, new_photo_name, photo_path};

    #[test]
    fn photo_names_stay_inside_their_folder() {
        let dir = Path::new("/data");
        let name = new_photo_name();
        assert_eq!(photo_path(dir, &name), Some(dir.join(PHOTOS).join(&name)));
        for bad in ["../settings.json", "0123456789abcdef.png", "../../0123456789.jpg", "0123456789abcdeg.jpg", ""] {
            assert_eq!(photo_path(dir, bad), None, "{bad}");
        }
    }

    #[test]
    fn looks_stay_bounded_and_let_gone_chats_go_first() {
        let id = |n: usize| OwnedRoomId::try_from(format!("!r{n:04}:x")).unwrap();
        let look = || ChatLook { colour: Some(ChatColour::Teal), ..Default::default() };
        let mut looks: BTreeMap<_, _> = (0..MAX_LOOKS).map(|n| (id(n), look())).collect();
        assert!(make_room(&mut looks, &id(3), |_| true).is_empty());
        assert_eq!(make_room(&mut looks, &id(9999), |r| r != &id(7) && r != &id(8)).len(), 2);
        assert!(!looks.contains_key(&id(7)) && looks.contains_key(&id(0)));
        looks.insert(id(9998), look());
        looks.insert(id(9999), look());
        assert_eq!(make_room(&mut looks, &id(10_000), |_| true).len(), 1);
        assert_eq!(looks.len(), MAX_LOOKS - 1);
        assert!(!looks.contains_key(&id(0)));
    }

    #[test]
    fn an_empty_look_is_not_kept_and_old_files_have_none() {
        assert!(ChatLook::default().is_empty());
        let s: crate::Settings = serde_json::from_str("{}").unwrap();
        assert!(s.chat_looks.is_empty());
    }
}

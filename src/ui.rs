use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use chrono::{Datelike as _, Local, TimeZone as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _,
    avatar::Avatar,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};
use tokio::sync::mpsc;

mod panels;

use std::path::PathBuf;

use panels::Panel;

use crate::Keyboard;
use crate::bridges::{Flow, Network, Step};
use crate::backend::{ChatAction, Cmd, DELETED, Ev, Item, Media, MediaKind, Msg, RoomSummary, UTD};

const NARROW: f32 = 720.;
// Chat list metrics, measured from Beeper Android 4.64 at 3x density.
const ROOM_ROW_H: f32 = 80.;
const AVATAR: f32 = 46.;
const GUTTER: f32 = 24.;
const BAR_H: f32 = 48.;
/// Edits, reactions and deletions kept until the message they target is paged in.
const MAX_PENDING: usize = 512;
/// Inline thumbnails kept decoded; each is at most 2.6 MB of pixels.
const MAX_THUMBS: usize = 60;
const QUICK_REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "😮", "😢", "🙏"];

/// What the composer's next send does.
#[derive(Clone, PartialEq)]
enum Compose {
    New,
    Reply(String),
    Edit(String),
}

/// Chat list sections, as in Beeper.
#[derive(Clone, Copy, PartialEq)]
enum View {
    Inbox,
    Archive,
    LowPriority,
}

const VIEWS: [(View, &str, &str, IconName); 3] = [
    (View::Inbox, "Inbox", "You've hit inbox zero!", IconName::Inbox),
    (View::Archive, "Archive", "Your Archive is empty", IconName::Archive),
    (View::LowPriority, "Low Priority", "No messages in Low Priority", IconName::ChevronsDown),
];

#[derive(PartialEq)]
enum Screen {
    Starting,
    Email,
    Code,
    Main,
}

pub struct BeeperApp {
    cmd: mpsc::Sender<Cmd>,
    screen: Screen,
    busy: bool,
    error: Option<SharedString>,
    email: Entity<InputState>,
    code: Entity<InputState>,
    recovery: Entity<InputState>,
    composer: Entity<InputState>,
    need_recovery: bool,
    /// Holds keyboard focus when no input does, so Escape (Android's back) still reaches the app.
    focus: FocusHandle,
    dir: PathBuf,
    settings: crate::Settings,
    user: Option<OwnedUserId>,
    /// Settings page shown in place of the chat.
    panel: Option<Panel>,
    /// `None` until loaded.
    networks: Option<Vec<Network>>,
    /// Ways to sign in to the chosen bridge, when it offers more than one.
    flows: Option<(String, Vec<Flow>)>,
    step: Option<Step>,
    /// Inputs for the current step's fields, by field ID, with their keyboard subscriptions.
    fields: Vec<(String, Entity<InputState>)>,
    field_subs: Vec<Subscription>,
    cookie_paste: Entity<InputState>,
    qr: Option<Arc<Image>>,
    /// A request for the settings pages is in flight.
    working: bool,
    search: Entity<InputState>,
    /// The search field takes the place of the list title.
    searching: bool,
    view: View,
    only_unread: bool,
    /// Pinned first, then newest first.
    rooms: Vec<RoomSummary>,
    /// Indices into `rooms` that pass the current view, filter and search.
    shown: Vec<usize>,
    open: Option<OwnedRoomId>,
    msgs: Vec<Msg>,
    pending: Vec<Item>,
    /// Inline thumbnails by event ID, oldest first in `thumb_order`.
    thumbs: HashMap<String, Arc<Image>>,
    thumb_order: VecDeque<String>,
    /// Messages whose thumbnail was requested.
    asked: HashSet<String>,
    /// Image shown full-screen; the picture arrives after the overlay opens.
    viewer: Option<(String, Option<Arc<Image>>)>,
    /// Attachments being downloaded to open.
    fetching: HashSet<String>,
    uploads: usize,
    /// Message whose actions are showing.
    picked: Option<String>,
    compose: Compose,
    more: bool,
    loading: bool,
    timeline: ListState,
    rooms_scroll: UniformListScrollHandle,
    /// Room avatars; `None` while loading or when the room has none. One entry per room at most.
    avatars: HashMap<OwnedRoomId, Option<Arc<Image>>>,
    /// Rooms whose missing or undecryptable preview was already re-read this session.
    peeked: HashSet<OwnedRoomId>,
    _subs: Vec<Subscription>,
    _events: Task<()>,
}

impl BeeperApp {
    pub fn new(
        cmd: mpsc::Sender<Cmd>,
        mut events: mpsc::Receiver<Ev>,
        dir: PathBuf,
        settings: crate::Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let cookie_paste = cx.new(|cx| InputState::new(window, cx).placeholder("Paste cookies or a cURL command"));
        let email = cx.new(|cx| InputState::new(window, cx).placeholder("you@example.com"));
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("123456"));
        let recovery = cx.new(|cx| InputState::new(window, cx).placeholder("Recovery code"));
        let composer = cx.new(|cx| InputState::new(window, cx).placeholder("Message"));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search chats"));

        let mut subs = Vec::new();
        for (input, on_enter, keyboard) in [
            (&email, Self::send_code as fn(&mut Self, &mut Window, &mut Context<Self>), Keyboard::Email),
            (&code, Self::submit_code, Keyboard::Key),
            (&recovery, Self::recover, Keyboard::Key),
            (&composer, Self::send_message, Keyboard::Text),
        ] {
            subs.push(cx.subscribe_in(input, window, move |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { shift: false, .. } => on_enter(this, window, cx),
                InputEvent::Focus => crate::show_keyboard(keyboard),
                InputEvent::Blur => crate::hide_keyboard(),
                _ => {}
            }));
        }

        subs.push(cx.subscribe_in(&cookie_paste, window, |_, _, ev: &InputEvent, _, _| keyboard_follows(ev, Keyboard::Key)));
        subs.push(cx.subscribe_in(&search, window, |this, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::Change => this.refilter(cx),
            InputEvent::Focus => crate::show_keyboard(Keyboard::Text),
            InputEvent::Blur => crate::hide_keyboard(),
            InputEvent::PressEnter { .. } => {}
        }));

        subs.push(cx.observe_window_appearance(window, |_, window, cx| crate::sync_theme(Some(window), cx)));

        let timeline = ListState::new(0, ListAlignment::Bottom, px(800.));
        let weak = cx.entity().downgrade();
        timeline.set_scroll_handler(move |ev, _, cx| {
            if ev.visible_range.start == 0 {
                let _ = weak.update(cx, |this, cx| this.load_older(cx));
            }
        });

        let events = cx.spawn(async move |this, cx| {
            while let Some(ev) = events.recv().await {
                if this.update(cx, |this, cx| this.on_event(ev, cx)).is_err() {
                    break;
                }
            }
        });

        Self {
            cmd,
            screen: Screen::Starting,
            busy: false,
            error: None,
            email,
            code,
            recovery,
            composer,
            need_recovery: false,
            focus: cx.focus_handle(),
            dir,
            settings,
            user: None,
            panel: None,
            networks: None,
            flows: None,
            step: None,
            fields: Vec::new(),
            field_subs: Vec::new(),
            cookie_paste,
            qr: None,
            working: false,
            search,
            searching: false,
            view: View::Inbox,
            only_unread: false,
            rooms: Vec::new(),
            shown: Vec::new(),
            open: None,
            msgs: Vec::new(),
            pending: Vec::new(),
            thumbs: HashMap::new(),
            thumb_order: VecDeque::new(),
            asked: HashSet::new(),
            viewer: None,
            fetching: HashSet::new(),
            uploads: 0,
            picked: None,
            compose: Compose::New,
            more: false,
            loading: false,
            timeline,
            rooms_scroll: UniformListScrollHandle::new(),
            avatars: HashMap::new(),
            peeked: HashSet::new(),
            _subs: subs,
            _events: events,
        }
    }

    fn send(&mut self, cmd: Cmd, cx: &mut Context<Self>) -> bool {
        let ok = self.cmd.try_send(cmd).is_ok();
        if !ok {
            self.error = Some("Still working on the previous request, try again".into());
        }
        cx.notify();
        ok
    }

    fn on_event(&mut self, ev: Ev, cx: &mut Context<Self>) {
        match ev {
            Ev::NeedLogin => {
                (self.panel, self.networks, self.user) = (None, None, None);
                self.rooms.clear();
                self.refilter(cx);
                self.close_room(cx);
                self.screen = Screen::Email;
                self.busy = false;
            }
            Ev::CodeSent => {
                self.screen = Screen::Code;
                self.busy = false;
                self.error = None;
            }
            ev @ (Ev::Networks(_) | Ev::Flows(..) | Ev::LoginStep(_) | Ev::PanelFailed) => self.on_panel_event(ev, cx),
            Ev::Ready(user) => {
                self.user = Some(user);
                self.send(Cmd::Incognito(self.settings.incognito), cx);
                self.screen = Screen::Main;
                self.busy = false;
                self.error = None;
            }
            Ev::NeedRecovery(need) => {
                if self.need_recovery && !need {
                    // Keys just arrived: re-read what was shown as undecryptable.
                    self.peeked.clear();
                    if let Some(id) = self.open.take() {
                        self.open_room(id, cx);
                    }
                }
                self.need_recovery = need;
                self.busy = false;
            }
            Ev::Avatar(id, bytes) => {
                if let Some(format) = image_format(&bytes) {
                    self.avatars.insert(id, Some(Arc::new(Image::from_bytes(format, bytes))));
                }
            }
            Ev::Thumb(id, bytes) => {
                // Late answers for a chat that was closed meanwhile are dropped.
                if let (true, Some(format)) = (self.asked.contains(&id), image_format(&bytes)) {
                    self.thumbs.insert(id.clone(), Arc::new(Image::from_bytes(format, bytes)));
                    self.thumb_order.push_back(id);
                    while self.thumb_order.len() > MAX_THUMBS {
                        let Some(old) = self.thumb_order.pop_front() else { break };
                        // Forgetting the request lets it load again when scrolled back to.
                        self.asked.remove(&old);
                        if let Some(image) = self.thumbs.remove(&old) {
                            image.remove_asset(cx);
                        }
                    }
                }
            }
            Ev::Viewed(id, bytes) => {
                if let (Some((open, slot)), Some(format)) = (&mut self.viewer, image_format(&bytes))
                    && *open == id
                {
                    *slot = Some(Arc::new(Image::from_bytes(format, bytes)));
                }
            }
            Ev::Fetched(id, path) => {
                self.fetching.remove(&id);
                if let Some(path) = path {
                    let mime = self.msgs.iter().find(|m| m.id == id).and_then(|m| m.media.as_ref()?.mime.clone());
                    crate::open_file(cx, &path, mime.as_deref());
                }
            }
            Ev::Attached => self.uploads = self.uploads.saturating_sub(1),
            Ev::Rooms(changed) => {
                let mut by_id: HashMap<_, _> = self.rooms.drain(..).map(|r| (r.id.clone(), r)).collect();
                by_id.extend(changed.into_iter().map(|r| (r.id.clone(), r)));
                self.rooms = by_id.into_values().collect();
                self.rooms.sort_unstable_by_key(|r| (!r.pinned, std::cmp::Reverse(r.ts)));
                self.refilter(cx);
            }
            Ev::RoomGone(id) => {
                self.rooms.retain(|r| r.id != id);
                self.refilter(cx);
                if self.open.as_ref() == Some(&id) {
                    self.close_room(cx);
                }
            }
            Ev::History { room, items, reset, more } => {
                if self.open.as_ref() != Some(&room) {
                    return;
                }
                self.loading = false;
                self.more = more;
                let (mut page, mut relations) = (Vec::new(), Vec::new());
                for item in items {
                    match item {
                        Item::Msg(m) => page.push(m),
                        other => relations.push(other),
                    }
                }
                if reset {
                    self.timeline.reset(page.len());
                    self.msgs = page;
                    self.pending.clear();
                } else {
                    self.timeline.splice(0..0, page.len());
                    page.append(&mut self.msgs);
                    self.msgs = page;
                    // The page may hold the messages that earlier relations were waiting for.
                    relations.splice(0..0, self.pending.drain(..));
                }
                self.relate(relations);
            }
            Ev::Live { room, items } => {
                if self.open.as_ref() != Some(&room) || self.loading && self.msgs.is_empty() {
                    return;
                }
                let mut relations = Vec::new();
                for item in items {
                    match item {
                        Item::Msg(m) => {
                            // A just-fetched history page can already contain events from this sync.
                            if self.msgs.iter().rev().take(64).any(|x| x.id == m.id) {
                                continue;
                            }
                            let at = self.msgs.len();
                            self.msgs.push(m);
                            self.timeline.splice(at..at, 1);
                        }
                        other => relations.push(other),
                    }
                }
                self.relate(relations);
            }
            Ev::Error(e) => {
                self.error = Some(e.into());
                self.busy = false;
                self.loading = false;
            }
        }
        cx.notify();
    }

    fn send_code(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let email = self.email.read(cx).value().trim().to_string();
        if email.is_empty() || self.busy {
            return;
        }
        self.error = None;
        self.busy = self.send(Cmd::SendCode(email), cx);
    }

    fn submit_code(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let code = self.code.read(cx).value().trim().to_string();
        if code.is_empty() || self.busy {
            return;
        }
        self.error = None;
        self.busy = self.send(Cmd::SubmitCode(code), cx);
    }

    fn recover(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let key = self.recovery.read(cx).value().trim().to_string();
        if key.is_empty() || self.busy {
            return;
        }
        self.error = None;
        self.busy = self.send(Cmd::Recover(key), cx);
    }

    fn paste_recovery(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match crate::paste(cx) {
            Some(text) => self.recovery.update(cx, |r, cx| r.set_value(text.trim(), window, cx)),
            None => self.error = Some("The clipboard has no text".into()),
        }
        cx.notify();
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().trim().to_lowercase();
        let passes = |r: &RoomSummary| {
            // A search looks through every section.
            if !query.is_empty() {
                return r.name.to_lowercase().contains(&query);
            }
            let in_view = match self.view {
                View::Inbox => !r.archived && !r.low_priority,
                View::Archive => r.archived,
                View::LowPriority => r.low_priority && !r.archived,
            };
            in_view && (!self.only_unread || r.is_unread())
        };
        self.shown = self.rooms.iter().enumerate().filter(|(_, r)| passes(r)).map(|(ix, _)| ix).collect();
        cx.notify();
    }

    /// Menu of what can be done to a chat; shared by the row's context menu and the chat header.
    fn chat_menu(
        room: &RoomSummary,
        this: WeakEntity<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let unread = room.is_unread();
        let entries = [
            if unread {
                ("Mark as Read", IconName::MailOpen, ChatAction::Unread(false))
            } else {
                ("Mark as Unread", IconName::Mail, ChatAction::Unread(true))
            },
            if room.pinned {
                ("Unpin", IconName::PinOff, ChatAction::Pin(false))
            } else {
                ("Pin", IconName::Pin, ChatAction::Pin(true))
            },
            if room.muted {
                ("Unmute", IconName::Bell, ChatAction::Mute(false))
            } else {
                ("Mute", IconName::BellOff, ChatAction::Mute(true))
            },
            if room.archived {
                ("Unarchive", IconName::ArchiveRestore, ChatAction::Archive(false))
            } else {
                ("Archive", IconName::Archive, ChatAction::Archive(true))
            },
            if room.low_priority {
                ("Move to Inbox", IconName::Inbox, ChatAction::LowPriority(false))
            } else {
                ("Move to Low Priority", IconName::ChevronsDown, ChatAction::LowPriority(true))
            },
        ];
        let id = room.id.clone();
        move |menu, _, _| {
            entries.iter().fold(menu, |menu, &(label, icon, action)| {
                let (this, id) = (this.clone(), id.clone());
                menu.item(PopupMenuItem::new(label).icon(icon).on_click(move |_, _, cx| {
                    let _ = this.update(cx, |this, cx| this.send(Cmd::Chat(id.clone(), action), cx));
                }))
            })
        }
    }

    /// Asks the backend for avatars and previews of rows that just became visible.
    /// Uses at most half the command channel so user actions always fit.
    fn fetch_rows(&mut self, range: std::ops::Range<usize>) {
        if self.cmd.capacity() < self.cmd.max_capacity() / 2 {
            return;
        }
        let rows: Vec<&RoomSummary> = self.shown[range].iter().map(|&ix| &self.rooms[ix]).collect();
        let avatars: Vec<_> = rows.iter().filter(|r| !self.avatars.contains_key(&r.id)).map(|r| r.id.clone()).collect();
        let previews: Vec<_> = rows
            .iter()
            .filter(|r| !self.peeked.contains(&r.id))
            .filter(|r| r.preview.as_deref().is_none_or(|p| !self.need_recovery && p.ends_with(UTD)))
            .map(|r| r.id.clone())
            .collect();
        if !avatars.is_empty() && self.cmd.try_send(Cmd::Avatars(avatars.clone())).is_ok() {
            self.avatars.extend(avatars.into_iter().map(|id| (id, None)));
        }
        if !previews.is_empty() && self.cmd.try_send(Cmd::Previews(previews.clone())).is_ok() {
            self.peeked.extend(previews);
        }
    }

    /// Applies edits, reactions and deletions to loaded messages; the rest wait in `pending`.
    fn relate(&mut self, items: Vec<Item>) {
        for item in items {
            match apply(&mut self.msgs, &item) {
                Some(ix) => self.timeline.splice(ix..ix + 1, 1),
                None if self.pending.len() < MAX_PENDING => self.pending.push(item),
                None => {}
            }
        }
    }

    fn send_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let body = self.composer.read(cx).value().trim().to_string();
        let Some(room) = self.open.clone() else { return };
        if body.is_empty() {
            return;
        }
        let (reply_to, edit) = match self.compose.clone() {
            Compose::New => (None, None),
            Compose::Reply(id) => (Some(id), None),
            Compose::Edit(id) => (None, Some(id)),
        };
        if self.send(Cmd::Send { room, body, reply_to, edit }, cx) {
            self.compose = Compose::New;
            self.composer.update(cx, |c, cx| c.set_value("", window, cx));
        }
    }

    fn start_compose(&mut self, compose: Compose, window: &mut Window, cx: &mut Context<Self>) {
        let text = match &compose {
            Compose::Edit(id) => self.msgs.iter().find(|m| &m.id == id).map(|m| m.body.clone()).unwrap_or_default(),
            _ => String::new(),
        };
        if matches!(compose, Compose::Edit(_)) || matches!(self.compose, Compose::Edit(_)) {
            self.composer.update(cx, |c, cx| c.set_value(text, window, cx));
        }
        self.compose = compose;
        self.picked = None;
        self.composer.update(cx, |c, cx| c.focus(window, cx));
        cx.notify();
    }

    /// Escape, which is also Android's back gesture: undoes the innermost thing on screen.
    /// Returns whether there was anything to undo.
    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let narrow = window.viewport_size().width < px(NARROW);
        if self.viewer.is_some() {
            self.close_viewer(cx);
        } else if self.picked.take().is_some() {
            cx.notify();
        } else if self.compose != Compose::New {
            self.start_compose(Compose::New, window, cx);
        } else if self.panel.is_some() {
            self.back(cx);
        } else if self.searching {
            self.set_searching(false, window, cx);
        } else if narrow && self.open.is_some() {
            self.close_room(cx);
            cx.notify();
        } else {
            return false;
        }
        true
    }

    fn close_viewer(&mut self, cx: &mut Context<Self>) {
        if let Some((_, Some(image))) = self.viewer.take() {
            image.remove_asset(cx);
        }
        cx.notify();
    }

    /// Requests the thumbnail of a message that is being drawn. Like `fetch_rows`, leaves half
    /// the command channel to user actions.
    fn want_thumb(&mut self, ix: usize) {
        let m = &self.msgs[ix];
        let Some(media) = &m.media else { return };
        let pictured = matches!(media.kind, MediaKind::Image | MediaKind::Sticker | MediaKind::Video);
        if !pictured || self.asked.contains(&m.id) || self.cmd.capacity() < self.cmd.max_capacity() / 2 {
            return;
        }
        if self.cmd.try_send(Cmd::Thumbs(vec![(m.id.clone(), media.clone())])).is_ok() {
            self.asked.insert(m.id.clone());
        }
    }

    /// A tap on an attachment: pictures open full-screen, everything else in the system's app.
    fn open_media(&mut self, id: String, media: Media, cx: &mut Context<Self>) {
        if matches!(media.kind, MediaKind::Image | MediaKind::Sticker) {
            if self.send(Cmd::View(id.clone(), media), cx) {
                self.viewer = Some((id, None));
            }
        } else if !self.fetching.contains(&id) && self.send(Cmd::Fetch(id.clone(), media), cx) {
            self.fetching.insert(id);
        }
    }

    fn attach(&mut self, cx: &mut Context<Self>) {
        let picked = crate::pick_file(cx);
        cx.spawn(async move |this, cx| {
            let Some(path) = picked.await else { return };
            let _ = this.update(cx, |this, cx| {
                let Some(room) = this.open.clone() else { return };
                let reply_to = match std::mem::replace(&mut this.compose, Compose::New) {
                    Compose::Reply(id) => Some(id),
                    _ => None,
                };
                if this.send(Cmd::Attach { room, path, reply_to }, cx) {
                    this.uploads += 1;
                }
            });
        })
        .detach();
    }

    fn render_media(&self, m: &Msg, media: &Media, cx: &mut Context<Self>) -> AnyElement {
        let (id, owned) = (m.id.clone(), media.clone());
        let open = cx.listener(move |this, _: &ClickEvent, _, cx| {
            // The bubble's own tap selects the message.
            cx.stop_propagation();
            this.open_media(id.clone(), owned.clone(), cx);
        });
        let raised = cx.theme().list_active;
        let pictured = matches!(media.kind, MediaKind::Image | MediaKind::Sticker | MediaKind::Video);
        if pictured && (media.kind != MediaKind::Video || media.dims.is_some()) {
            // The picture's own shape, within what a bubble may take.
            let (max_w, max_h) = if media.kind == MediaKind::Sticker { (140., 140.) } else { (260., 320.) };
            let (w, h) = media.dims.map_or((max_w, max_w * 0.75), |(w, h)| (w.max(1) as f32, h.max(1) as f32));
            let scale = (max_w / w).min(max_h / h).min(1.);
            let thumb = self.thumbs.get(&m.id).cloned();
            let video = media.kind == MediaKind::Video;
            return div()
                .id(SharedString::from(format!("{}-media", m.id)))
                .relative()
                .w(px((w * scale).max(80.)))
                .h(px((h * scale).max(60.)))
                .rounded(px(14.))
                .overflow_hidden()
                .bg(raised)
                .cursor_pointer()
                .on_click(open)
                .map(|el| match thumb {
                    Some(image) => el.child(img(image).size_full().rounded(px(14.)).object_fit(ObjectFit::Cover)),
                    None => el.child(
                        div().size_full().flex().items_center().justify_center().opacity(0.5).child(
                            Icon::new(if video { IconName::Video } else { IconName::Image }).size_6(),
                        ),
                    ),
                })
                .when(video, |el| {
                    el.child(
                        div().absolute().inset_0().flex().items_center().justify_center().child(
                            div()
                                .size(px(44.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .bg(gpui_kit::black().opacity(0.55))
                                .text_color(gpui_kit::white())
                                .child(Icon::new(IconName::Play).size_5()),
                        ),
                    )
                    .children(media.duration.map(|d| {
                        div()
                            .absolute()
                            .left_2()
                            .bottom_2()
                            .px_1p5()
                            .rounded_full()
                            .bg(gpui_kit::black().opacity(0.55))
                            .text_color(gpui_kit::white())
                            .text_size(px(11.))
                            .child(length(d))
                    }))
                })
                .into_any_element();
        }
        let icon = match media.kind {
            MediaKind::Voice => IconName::Mic,
            MediaKind::Audio => IconName::Music,
            MediaKind::Video => IconName::Video,
            _ => IconName::File,
        };
        let detail = if self.fetching.contains(&m.id) {
            "Downloading…".to_owned()
        } else {
            match (media.duration, media.size) {
                (Some(d), _) => length(d),
                (None, Some(size)) => file_size(size),
                (None, None) => String::new(),
            }
        };
        h_flex()
            .id(SharedString::from(format!("{}-media", m.id)))
            .gap_2()
            .max_w(px(260.))
            .cursor_pointer()
            .on_click(open)
            .child(
                div()
                    .size(px(40.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(raised)
                    .child(Icon::new(icon).size_5()),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .child(div().truncate().text_size(px(15.)).child(match media.kind {
                        MediaKind::Voice => "Voice Note".to_owned(),
                        _ => media.name.clone(),
                    }))
                    .child(div().text_size(px(12.)).opacity(0.7).child(detail)),
            )
            .into_any_element()
    }

    /// Adds the reaction, or removes it when it is already ours.
    fn toggle_reaction(&mut self, target: &str, key: &str, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone() else { return };
        let mine = self.msgs.iter().find(|m| m.id == target).and_then(|m| m.reactions.iter().find(|r| r.own && r.key == key));
        let cmd = match mine {
            Some(r) => Cmd::Delete { room, target: r.id.clone() },
            None => Cmd::React { room, target: target.to_owned(), key: key.to_owned() },
        };
        self.picked = None;
        self.send(cmd, cx);
    }

    fn open_room(&mut self, id: OwnedRoomId, cx: &mut Context<Self>) {
        if self.open.as_ref() == Some(&id) {
            return;
        }
        self.close_room(cx);
        self.panel = None;
        self.open = Some(id.clone());
        self.loading = self.send(Cmd::Open(id), cx);
    }

    fn close_room(&mut self, cx: &mut Context<Self>) {
        self.open = None;
        self.msgs.clear();
        self.pending.clear();
        self.asked.clear();
        self.thumb_order.clear();
        for (_, image) in self.thumbs.drain() {
            image.remove_asset(cx);
        }
        self.close_viewer(cx);
        self.fetching.clear();
        self.picked = None;
        self.compose = Compose::New;
        self.timeline.reset(0);
        self.more = false;
        self.loading = false;
    }

    fn load_older(&mut self, cx: &mut Context<Self>) {
        if self.more && !self.loading && self.open.is_some() {
            self.loading = self.send(Cmd::LoadOlder, cx);
        }
    }

    fn render_login(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let on_code = self.screen == Screen::Code;
        let (title, hint, input, label) = if on_code {
            ("Check your email", "Enter the code Beeper sent you.", &self.code, "Sign in")
        } else {
            ("Sign in to Beeper", "We will email you a sign-in code.", &self.email, "Send code")
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_6()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(360.))
                    .gap_3()
                    .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child(hint))
                    .child(Input::new(input))
                    .child(
                        Button::new("login-submit")
                            .primary()
                            .label(label)
                            .loading(self.busy)
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if on_code { this.submit_code(window, cx) } else { this.send_code(window, cx) }
                            })),
                    )
                    .when(on_code, |el| {
                        el.child(Button::new("login-back").ghost().label("Use a different email").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.screen = Screen::Email;
                                this.error = None;
                                cx.notify();
                            }),
                        ))
                    })
                    .children(self.error.clone().map(|e| div().text_sm().text_color(cx.theme().danger).child(e))),
            )
    }

    fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        self.view = view;
        self.refilter(cx);
    }

    fn set_searching(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.searching = on;
        self.search.update(cx, |s, cx| {
            if on {
                s.focus(window, cx);
            } else {
                s.set_value("", window, cx);
            }
        });
        self.refilter(cx);
    }

    fn render_rooms(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let list = uniform_list(
            "rooms",
            self.shown.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                this.fetch_rows(range.clone());
                range
                    .map(|ix| {
                        let room = &this.rooms[this.shown[ix]];
                        let selected = this.open.as_ref() == Some(&room.id);
                        let id = room.id.clone();
                        let unread = room.is_unread();
                        let dim = cx.theme().muted_foreground;
                        let mark = |icon: IconName| Icon::new(icon).size_3p5().flex_none().text_color(dim);
                        // Beeper shows a bare "Message" when it has nothing readable to preview.
                        let preview = room.preview.as_deref().filter(|p| !p.ends_with(UTD));
                        h_flex()
                            .id(SharedString::from(room.id.to_string()))
                            .w_full()
                            .h(px(ROOM_ROW_H))
                            .pl(px(GUTTER))
                            .gap(px(10.))
                            .when(unread, |el| el.bg(cx.theme().list_hover))
                            .when(selected, |el| el.bg(cx.theme().list_active))
                            .hover(|el| el.bg(cx.theme().list_hover))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.open_room(id.clone(), cx)))
                            .child(avatar(room, this.avatars.get(&room.id)))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .pt(px(9.))
                                    .pr(px(GUTTER))
                                    .border_b_1()
                                    .border_color(cx.theme().border)
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_size(px(17.))
                                                    .line_height(px(22.))
                                                    .font_weight(FontWeight::BOLD)
                                                    .child(room.name.clone()),
                                            )
                                            .when(room.pinned, |el| el.child(mark(IconName::Pin)))
                                            .when(room.muted, |el| el.child(mark(IconName::BellOff)))
                                            .child(
                                                div().flex_none().text_size(px(14.)).text_color(dim).child(short_time(room.ts)),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .items_start()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .line_clamp(2)
                                                    .text_size(px(15.))
                                                    .line_height(px(20.))
                                                    .text_color(dim)
                                                    .when(preview.is_none(), |el| el.italic())
                                                    .child(preview.unwrap_or("Message").to_owned()),
                                            )
                                            .when(unread, |el| el.child(count_pill(room.unread, room.muted, cx))),
                                    ),
                            )
                            .context_menu(Self::chat_menu(room, weak.clone()))
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.rooms_scroll)
        .size_full();

        let view_ix = VIEWS.iter().position(|v| v.0 == self.view).unwrap_or(0);
        let has_query = !self.search.read(cx).value().trim().is_empty();
        let empty = if self.rooms.is_empty() {
            "Syncing chats..."
        } else if has_query {
            "No results found"
        } else if self.only_unread {
            "No unread chats"
        } else {
            VIEWS[view_ix].2
        };
        let title = if self.searching {
            h_flex()
                .w_full()
                .gap_2()
                .child(div().flex_1().min_w_0().child(Input::new(&self.search).prefix(Icon::new(IconName::Search).size_4())))
                .child(Button::new("close-search").ghost().icon(IconName::X).tooltip("Close search").on_click(cx.listener(
                    |this, _, window, cx| this.set_searching(false, window, cx),
                )))
                .into_any_element()
        } else {
            let weak = cx.entity().downgrade();
            Button::new("view")
                .ghost()
                .large()
                .child(div().text_size(px(20.)).font_weight(FontWeight::MEDIUM).child(VIEWS[view_ix].1))
                .dropdown_caret(true)
                .dropdown_menu(move |menu, _, _| {
                    let sections = VIEWS.iter().fold(menu, |menu, &(view, label, _, icon)| {
                        let weak = weak.clone();
                        menu.item(PopupMenuItem::new(label).icon(icon).on_click(move |_, _, cx| {
                            let _ = weak.update(cx, |this, cx| this.set_view(view, cx));
                        }))
                    });
                    let weak = weak.clone();
                    sections.separator().item(PopupMenuItem::new("Settings").icon(IconName::Settings).on_click(move |_, _, cx| {
                        let _ = weak.update(cx, |this, cx| this.open_panel(Panel::Settings, cx));
                    }))
                })
                .into_any_element()
        };
        // Floating bar, as in Beeper: filter, sections, search.
        let bar = cx.theme().title_bar;
        let chip = cx.theme().list_active;
        let round = |id: &'static str, icon: IconName, on: bool| {
            div()
                .id(id)
                .size(px(BAR_H))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(if on { chip } else { bar })
                .shadow_lg()
                .cursor_pointer()
                // Floats over the list: without this a tap also opens the chat underneath.
                .occlude()
                .child(Icon::new(icon).size_5())
        };
        let sections = h_flex().h(px(BAR_H)).p(px(4.)).gap(px(4.)).rounded_full().bg(bar).shadow_lg().occlude().children(
            VIEWS.iter().enumerate().map(|(ix, &(view, label, _, icon))| {
                div()
                    .id(label)
                    .w(px(60.))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .when(ix == view_ix, |el| el.bg(chip))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.set_view(view, cx)))
                    .child(Icon::new(icon).size_5())
            }),
        );
        v_flex()
            .size_full()
            .child(h_flex().flex_none().h(px(56.)).px(px(GUTTER - 12.)).child(title))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(if self.shown.is_empty() {
                        div()
                            .px(px(GUTTER))
                            .py_4()
                            .text_color(cx.theme().muted_foreground)
                            .child(empty)
                            .into_any_element()
                    } else {
                        list.into_any_element()
                    })
                    .child(
                        h_flex()
                            .absolute()
                            .bottom(px(16.))
                            .left_0()
                            .right_0()
                            .justify_center()
                            .gap(px(8.))
                            .child(round("only-unread", IconName::ListFilter, self.only_unread).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.only_unread = !this.only_unread;
                                    this.refilter(cx);
                                },
                            )))
                            .child(sections)
                            .child(round("search", IconName::Search, self.searching).on_click(cx.listener(
                                |this, _, window, cx| this.set_searching(!this.searching, window, cx),
                            ))),
                    ),
            )
    }

    fn render_chat(&mut self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let room = self.open.as_ref().and_then(|id| self.rooms.iter().find(|r| &r.id == id));
        let name = room.map(|r| r.name.clone()).unwrap_or_default();
        let network = room.and_then(|r| r.network.clone());
        let header_avatar = room.map(|r| avatar(r, self.avatars.get(&r.id)).with_size(px(36.)));
        let menu = room.map(|r| Self::chat_menu(r, cx.entity().downgrade()));
        let dm = room.is_some_and(|r| r.dm);
        let timeline = list(
            self.timeline.clone(),
            cx.processor(move |this, ix: usize, _, cx| {
                this.want_thumb(ix);
                let m = &this.msgs[ix];
                let attachment = m.media.as_ref().filter(|_| !m.deleted).map(|media| this.render_media(m, media, cx));
                let prev = ix.checked_sub(1).map(|p| &this.msgs[p]);
                let new_day = prev.is_none_or(|p| day(p.ts) != day(m.ts));
                let first = new_day || prev.is_none_or(|p| p.from != m.from);
                let faded = m.deleted || m.body == UTD;
                let picked = this.picked.as_deref() == Some(m.id.as_str());
                let quote = m.reply_to.as_ref().map(|id| match this.msgs.iter().rfind(|x| &x.id == id) {
                    Some(q) => (q.sender.clone(), q.summary()),
                    None => (String::new(), "Message unavailable".to_owned()),
                });
                let id = m.id.clone();
                let bubble = v_flex()
                    .id(SharedString::from(m.id.clone()))
                    .max_w(relative(0.8))
                    .px(px(14.))
                    .py_2()
                    .rounded(px(20.))
                    .cursor_pointer()
                    .when(picked, |el| el.border_2().border_color(cx.theme().ring))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.picked = (this.picked.as_ref() != Some(&id)).then(|| id.clone());
                        cx.notify();
                    }))
                    .when(!m.own, |el| {
                        el.bg(cx.theme().secondary).when(first && !dm, |el| {
                            el.child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(m.sender.clone()),
                            )
                        })
                    })
                    .when(m.own, |el| el.bg(cx.theme().primary).text_color(cx.theme().primary_foreground))
                    .children(quote.map(|(who, text)| {
                        v_flex()
                            .my_1()
                            .pl_2()
                            .border_l_2()
                            .border_color(cx.theme().ring)
                            .text_xs()
                            .opacity(0.8)
                            .when(!who.is_empty(), |el| el.child(div().font_weight(FontWeight::SEMIBOLD).child(who)))
                            .child(div().line_clamp(2).child(text))
                    }))
                    .children(attachment.map(|a| div().py_1().child(a)))
                    .when(!m.body.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(px(16.))
                                .line_height(px(22.))
                                .when(faded, |el| el.italic().opacity(0.7))
                                .child(m.body.clone()),
                        )
                    })
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_1()
                            .text_size(px(11.))
                            .opacity(0.6)
                            .when(m.edited && !m.deleted, |el| el.child("Edited"))
                            .child(clock(m.ts)),
                    );
                // One chip per emoji, in order of first use.
                let mut chips: Vec<(&str, usize, bool)> = Vec::new();
                for r in &m.reactions {
                    match chips.iter_mut().find(|c| c.0 == r.key) {
                        Some(c) => (c.1, c.2) = (c.1 + 1, c.2 || r.own),
                        None => chips.push((&r.key, 1, r.own)),
                    }
                }
                let reactions = (!chips.is_empty()).then(|| {
                    h_flex().flex_wrap().gap_1().pt_0p5().children(chips.into_iter().enumerate().map(|(n, (key, count, mine))| {
                        let (id, key_owned) = (m.id.clone(), key.to_owned());
                        h_flex()
                            .id(SharedString::from(format!("{}-reaction-{n}", m.id)))
                            .gap_1()
                            .px_1p5()
                            .rounded_full()
                            .border_1()
                            .border_color(if mine { cx.theme().ring } else { cx.theme().border })
                            .bg(cx.theme().secondary)
                            .py_0p5()
                            .text_size(px(13.))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_reaction(&id, &key_owned, cx)))
                            .child(key.to_owned())
                            .when(count > 1, |el| el.child(count.to_string()))
                    }))
                });
                v_flex()
                    .w_full()
                    .px_3()
                    .pt(if first { px(10.) } else { px(3.) })
                    .when(new_day, |el| {
                        el.child(
                            div().w_full().py_3().flex().justify_center().child(
                                div()
                                    .px_3()
                                    .py_1()
                                    .rounded_full()
                                    .bg(cx.theme().secondary)
                                    .text_size(px(12.))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(day_label(m.ts)),
                            ),
                        )
                    })
                    .child(
                        v_flex()
                            .w_full()
                            .map(|el| if m.own { el.items_end() } else { el.items_start() })
                            .child(bubble)
                            .children(reactions),
                    )
                    .into_any_element()
            }),
        )
        .size_full();

        v_flex()
            .size_full()
            .child(
                header(cx)
                    .gap_2()
                    .when(narrow, |el| {
                        el.child(
                            Button::new("back").ghost().icon(IconName::ArrowLeft).tooltip("Back").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.close_room(cx);
                                    cx.notify();
                                },
                            )),
                        )
                    })
                    .children(header_avatar)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(px(17.)).font_weight(FontWeight::BOLD).truncate().child(name))
                            .children(network.map(|n| {
                                div().text_size(px(13.)).text_color(cx.theme().muted_foreground).child(n)
                            })),
                    )
                    .children(menu.map(|menu| {
                        Button::new("chat-menu")
                            .ghost()
                            .icon(IconName::Ellipsis)
                            .tooltip("Chat actions")
                            .dropdown_menu_with_anchor(Anchor::TopRight, menu)
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .pb_2()
                    .child(timeline)
                    .when(self.loading && self.msgs.is_empty(), |el| {
                        el.child(div().p_4().text_sm().text_color(cx.theme().muted_foreground).child("Loading messages..."))
                    }),
            )
            .children(self.render_message_actions(cx))
            .children(self.render_compose_strip(cx))
            .when(self.uploads > 0, |el| {
                el.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_size(px(12.))
                        .text_color(cx.theme().muted_foreground)
                        .child("Sending attachment…"),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        Button::new("attach")
                            .ghost()
                            .icon(IconName::Paperclip)
                            .tooltip("Attach a file")
                            .on_click(cx.listener(|this, _, _, cx| this.attach(cx))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px_2()
                            .rounded(px(22.))
                            .bg(cx.theme().secondary)
                            .child(Input::new(&self.composer).appearance(false).large()),
                    )
                    .child(
                        div()
                            .id("send")
                            .size(px(44.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                            .cursor_pointer()
                            .hover(|el| el.bg(cx.theme().primary_hover))
                            .on_click(cx.listener(|this, _, window, cx| this.send_message(window, cx)))
                            .child(Icon::new(IconName::SendHorizontal).size_5()),
                    ),
            )
    }

    /// Actions for the picked message. A bar rather than a context menu: touch has no right-click.
    fn render_message_actions(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let m = self.msgs.iter().find(|m| Some(&m.id) == self.picked.as_ref())?;
        if m.deleted {
            return None;
        }
        let (id, body, own) = (m.id.clone(), m.body.clone(), m.own);
        let action = |name: &'static str, icon: IconName, tip: &'static str| Button::new(name).ghost().icon(icon).tooltip(tip);
        Some(
            h_flex()
                .flex_wrap()
                .gap_1()
                .px_2()
                .py_1()
                .border_t_1()
                .border_color(cx.theme().border)
                .children(QUICK_REACTIONS.iter().enumerate().map(|(n, key)| {
                    let id = id.clone();
                    Button::new(("quick-reaction", n))
                        .ghost()
                        .label(*key)
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_reaction(&id, key, cx)))
                }))
                .child(div().flex_1())
                .child(action("reply", IconName::Reply, "Reply").on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, window, cx| this.start_compose(Compose::Reply(id.clone()), window, cx)
                })))
                .child(action("copy", IconName::Copy, "Copy").on_click(cx.listener(move |this, _, _, cx| {
                    crate::copy(cx, body.clone());
                    this.picked = None;
                    cx.notify();
                })))
                .when(own, |el| {
                    el.child(action("edit", IconName::Pencil, "Edit").on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| this.start_compose(Compose::Edit(id.clone()), window, cx)
                    })))
                    .child(action("delete", IconName::Trash, "Delete").on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(room) = this.open.clone() {
                            this.send(Cmd::Delete { room, target: id.clone() }, cx);
                        }
                        this.picked = None;
                    })))
                }),
        )
    }

    /// What the composer is replying to or editing, with a way out.
    fn render_compose_strip(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let (title, id) = match &self.compose {
            Compose::New => return None,
            Compose::Reply(id) => ("Replying to:", id),
            Compose::Edit(id) => ("Edit message", id),
        };
        let quoted = self.msgs.iter().find(|m| &m.id == id).map(|m| format!("{}: {}", m.sender, m.summary())).unwrap_or_default();
        Some(
            h_flex()
                .gap_2()
                .px_3()
                .py_1()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().truncate().text_color(cx.theme().muted_foreground).child(quoted)),
                )
                .child(Button::new("cancel-compose").ghost().icon(IconName::Close).tooltip("Cancel").on_click(cx.listener(
                    |this, _, window, cx| this.start_compose(Compose::New, window, cx),
                ))),
        )
    }

    fn render_banners(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .when(self.need_recovery, |el| {
                el.child(
                    v_flex()
                        .gap_2()
                        .p_3()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().secondary)
                        .child(div().text_sm().child(
                            "Encrypted messages are locked on this device. Copy your Beeper recovery code \
                             from a device where you are signed in, then paste it here.",
                        ))
                        .child(div().w_full().child(Input::new(&self.recovery)))
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .child(
                                    Button::new("paste-recovery")
                                        .label("Paste")
                                        .on_click(cx.listener(|this, _, window, cx| this.paste_recovery(window, cx))),
                                )
                                .child(
                                    Button::new("recover")
                                        .primary()
                                        .label("Unlock")
                                        .loading(self.busy)
                                        .disabled(self.busy)
                                        .on_click(cx.listener(|this, _, window, cx| this.recover(window, cx))),
                                ),
                        ),
                )
            })
            .children(self.error.clone().map(|e| {
                h_flex()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().flex_1().text_sm().text_color(cx.theme().danger).child(e))
                    .child(Button::new("dismiss-error").ghost().label("Dismiss").on_click(cx.listener(|this, _, _, cx| {
                        this.error = None;
                        cx.notify();
                    })))
            }))
    }
}

fn header(cx: &App) -> Div {
    h_flex().h(px(56.)).flex_none().px_3().items_center().border_b_1().border_color(cx.theme().border)
}

fn avatar(room: &RoomSummary, image: Option<&Option<Arc<Image>>>) -> Avatar {
    Avatar::new().name(room.name.clone()).with_size(px(AVATAR)).when_some(image.cloned().flatten(), |a, img| a.src(img))
}

/// Unread count; a bare dot for a chat marked unread by hand, greyed when muted.
fn count_pill(n: u64, muted: bool, cx: &App) -> Div {
    div()
        .flex_none()
        .min_w(px(20.))
        .h(px(20.))
        .px_1p5()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(if muted { cx.theme().muted_foreground } else { cx.theme().primary })
        .text_color(cx.theme().primary_foreground)
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .children((n > 0).then(|| if n > 99 { "99+".to_string() } else { n.to_string() }))
}

fn local(ts_ms: u64) -> Option<chrono::DateTime<Local>> {
    Local.timestamp_millis_opt(i64::try_from(ts_ms).ok()?).single()
}

/// "1:05" or "1:02:03".
fn length(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn file_size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.0} KB", bytes as f64 / 1024.),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.),
    }
}

fn day(ts_ms: u64) -> Option<chrono::NaiveDate> {
    local(ts_ms).map(|t| t.date_naive())
}

/// Separator between days: Today, Yesterday, the weekday within a week, else the date.
fn day_label(ts_ms: u64) -> String {
    let Some(t) = local(ts_ms) else { return String::new() };
    let today = Local::now().date_naive();
    match (today - t.date_naive()).num_days() {
        0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        2..=6 => t.format("%A").to_string(),
        _ if t.year() == today.year() => t.format("%-d %B").to_string(),
        _ => t.format("%-d %B %Y").to_string(),
    }
}

fn clock(ts_ms: u64) -> String {
    local(ts_ms).map(|t| t.format("%H:%M").to_string()).unwrap_or_default()
}

/// Room list time: clock today, "Yesterday", weekday this week, date otherwise.
fn short_time(ts_ms: u64) -> String {
    let Some(t) = local(ts_ms) else { return String::new() };
    let today = Local::now().date_naive();
    let days = (today - t.date_naive()).num_days();
    let format = match days {
        0 => "%H:%M",
        1 => return "Yesterday".to_owned(),
        2..=6 => "%a",
        _ if t.year() == today.year() => "%-d %b",
        _ => "%-d.%m.%Y",
    };
    t.format(format).to_string()
}

/// Thumbnails come without a content type, so go by magic bytes.
fn image_format(bytes: &[u8]) -> Option<ImageFormat> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some(ImageFormat::Png),
        [0xFF, 0xD8, ..] => Some(ImageFormat::Jpeg),
        [b'G', b'I', b'F', b'8', ..] => Some(ImageFormat::Gif),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(ImageFormat::Webp),
        _ => None,
    }
}

/// Raises the on-screen keyboard while an input has focus. Android shows none by itself for a
/// GPUI input, so every input needs this.
fn keyboard_follows(event: &InputEvent, kind: Keyboard) {
    match event {
        InputEvent::Focus => crate::show_keyboard(kind),
        InputEvent::Blur => crate::hide_keyboard(),
        _ => {}
    }
}

/// Applies an edit, reaction or deletion to its loaded target and returns the row to redraw;
/// `None` when the target is not loaded.
fn apply(msgs: &mut [Msg], item: &Item) -> Option<usize> {
    let find = |msgs: &[Msg], id: &str| msgs.iter().rposition(|m| m.id == id);
    match item {
        Item::Edit { target, body } => {
            let ix = find(msgs, target)?;
            (msgs[ix].body, msgs[ix].edited) = (body.clone(), true);
            Some(ix)
        }
        Item::Reaction { target, reaction } => {
            let ix = find(msgs, target)?;
            if !msgs[ix].reactions.iter().any(|r| r.id == reaction.id) {
                msgs[ix].reactions.push(reaction.clone());
            }
            Some(ix)
        }
        Item::Redact { target } => {
            if let Some(ix) = find(msgs, target) {
                let m = &mut msgs[ix];
                (m.body, m.deleted, m.reply_to) = (DELETED.to_owned(), true, None);
                m.reactions.clear();
                return Some(ix);
            }
            let ix = msgs.iter().rposition(|m| m.reactions.iter().any(|r| &r.id == target))?;
            msgs[ix].reactions.retain(|r| &r.id != target);
            Some(ix)
        }
        Item::Msg(_) => None,
    }
}

impl Render for BeeperApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (top, bottom) = crate::insets();
        if window.focused(cx).is_none() {
            window.focus(&self.focus, cx);
        }
        let root = v_flex()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && this.go_back(window, cx) {
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .pt(px(top))
            .pb(px(bottom))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground);
        match self.screen {
            Screen::Starting => root
                .items_center()
                .justify_center()
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Connecting..."))
                .into_any_element(),
            Screen::Email | Screen::Code => root.child(self.render_login(cx)).into_any_element(),
            Screen::Main => {
                let narrow = window.viewport_size().width < px(NARROW);
                let panel = self.panel.map(|panel| self.render_panel(panel, window, cx));
                let body = if narrow {
                    if let Some(panel) = panel {
                        panel
                    } else if self.open.is_some() {
                        self.render_chat(true, cx).into_any_element()
                    } else {
                        self.render_rooms(cx).into_any_element()
                    }
                } else {
                    h_flex()
                        .size_full()
                        .child(
                            div()
                                .w(px(360.))
                                .flex_none()
                                .h_full()
                                .border_r_1()
                                .border_color(cx.theme().border)
                                .child(self.render_rooms(cx)),
                        )
                        .child(div().flex_1().min_w_0().h_full().child(if let Some(panel) = panel {
                            panel
                        } else if self.open.is_some() {
                            self.render_chat(false, cx).into_any_element()
                        } else {
                            v_flex()
                                .size_full()
                                .items_center()
                                .justify_center()
                                .gap_3()
                                .text_color(cx.theme().muted_foreground)
                                .child(Icon::new(IconName::MessageCircle).size_10())
                                .child("Select a chat")
                                .into_any_element()
                        }))
                        .into_any_element()
                };
                let viewer = self.viewer.as_ref().map(|(id, full)| {
                    // The thumbnail stands in until the full picture arrives.
                    let shown = full.clone().or_else(|| self.thumbs.get(id).cloned());
                    div()
                        .id("viewer")
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(gpui_kit::black().opacity(0.94))
                        .text_color(gpui_kit::white())
                        .occlude()
                        .on_click(cx.listener(|this, _, _, cx| this.close_viewer(cx)))
                        .map(|el| match shown {
                            Some(image) => el.child(img(image).size_full().object_fit(ObjectFit::Contain)),
                            None => el.child("Loading…"),
                        })
                        .child(div().absolute().top(px(top + 12.)).right_3().child(Icon::new(IconName::X).size_6()))
                });
                root.relative()
                    .child(self.render_banners(cx))
                    .child(div().flex_1().min_h_0().child(body))
                    .children(viewer)
                    .into_any_element()
            }
        }
    }
}

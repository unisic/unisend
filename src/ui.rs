use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use chrono::{Datelike as _, Local, TimeZone as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};
use tokio::sync::mpsc;

mod cards;
mod panels;

use std::path::PathBuf;

use panels::Panel;

use crate::{Keyboard, Palette};
use crate::bridges::{Flow, Network, Step};
use crate::backend::{ChatAction, Cmd, Delivery, Ev, Kind, Media, MediaKind, Msg, RoomSummary, UTD};

const NARROW: f32 = 720.;
// Metrics; `docs/design.md` gives the reasons.
const ROOM_ROW_H: f32 = 72.;
const AVATAR: f32 = 48.;
/// From the screen edge to content.
const GUTTER: f32 = 16.;
/// Between neighbouring things.
const GAP: f32 = 14.;
const BAR_H: f32 = 56.;
const NAV_H: f32 = 64.;
/// Smallest thing a finger is asked to hit.
const TAP: f32 = 44.;
const RADIUS: f32 = 20.;
/// Width of the navigation and of the chat list beside an open chat on a wide window.
const NAV_W: f32 = 210.;
const LIST_W: f32 = 384.;
/// Widest a message and a sheet grow on a wide window.
const BUBBLE_W: f32 = 560.;
/// Widest a link's preview grows inside a message, and the tallest its picture is shown.
const LINK_W: f32 = 300.;
const LINK_PICTURE_H: f32 = 240.;
/// On a phone, the share of the row a message may take.
const BUBBLE_SHARE: f32 = 0.8;
const SHEET_W: f32 = 480.;
/// Sender's picture beside a run of messages in a group.
const SENDER: f32 = 28.;
/// The panel of a message's actions, and how far from the finger it opens.
const MENU_W: f32 = 6. * TAP + 16.;
const MENU_GAP: f32 = 28.;
/// How far a message is swiped sideways to reply to it.
const SWIPE_REPLY: f32 = 56.;
/// How far a message's reactions reach up over its bubble.
const REACTION_LAP: f32 = 10.;
/// Joins counted into one line; a longer burst says so of the last hundred.
const MAX_JOINED: usize = 100;
/// The reaction a double tap gives.
const LIKE: &str = QUICK_REACTIONS[1];
/// A pause this long starts a new run of messages, with its own time.
const RUN_PAUSE_MS: u64 = 10 * 60 * 1000;
/// How often the others are told again that the user is still writing.
const TYPING_EVERY: std::time::Duration = std::time::Duration::from_secs(4);
/// How long a message sent through a bridge may go without the bridge's word before it is taken
/// as sent: some bridges never give one.
const BRIDGE_WAIT_MS: u64 = 2 * 60 * 1000;
/// Inline thumbnails kept decoded; each is at most 2.6 MB of pixels.
const MAX_THUMBS: usize = 60;
/// Sender pictures kept for the open chat; people past that many keep their initials.
const MAX_FACES: usize = 64;
const QUICK_REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "😮", "😢", "🙏"];

/// What the composer's next send does.
#[derive(Clone, PartialEq)]
enum Compose {
    New,
    Reply(String),
    Edit(String),
}

/// Which chats the list holds.
#[derive(Clone, Copy, PartialEq)]
enum View {
    Inbox,
    Archive,
    LowPriority,
}

/// Where the navigation points. Derived from `panel` and `view`, not stored.
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Chats,
    Archive,
    Networks,
    Settings,
}

const TABS: [(Tab, &str, IconName); 4] = [
    (Tab::Chats, "Chats", IconName::MessageCircle),
    (Tab::Archive, "Archive", IconName::Archive),
    (Tab::Networks, "Networks", IconName::Waypoints),
    (Tab::Settings, "Settings", IconName::Settings),
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
    /// The connection is lost and being retried.
    offline: bool,
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
    /// Rows of the open conversation, mirrored from the backend's timeline.
    msgs: Vec<Msg>,
    /// Who is writing in the open conversation.
    typing: Vec<String>,
    /// When the others were last told that the user is writing.
    typing_sent: Option<std::time::Instant>,
    /// Row of the newest message, when it is the user's own.
    tail: Option<usize>,
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
    /// Where it was picked: its actions open at the finger.
    picked_at: Point<Pixels>,
    /// A horizontal pan in progress over a message, with how far it has travelled.
    swipe: Option<(String, f32)>,
    /// The chat whose actions are showing.
    chat_sheet: Option<OwnedRoomId>,
    compose: Compose,
    more: bool,
    loading: bool,
    timeline: ListState,
    rooms_scroll: UniformListScrollHandle,
    /// Room avatars; `None` while loading or when the room has none. One entry per room at most.
    avatars: HashMap<OwnedRoomId, Option<Arc<Image>>>,
    /// Pictures of the people writing in the open chat; `None` while loading or when they have
    /// none. At most `MAX_FACES`, 37 kB of pixels each.
    faces: HashMap<OwnedUserId, Option<Arc<Image>>>,
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
                other => keyboard_follows(other, keyboard, cx),
            }));
        }

        subs.push(cx.subscribe_in(&cookie_paste, window, |_, _, ev: &InputEvent, _, cx| keyboard_follows(ev, Keyboard::Key, cx)));
        subs.push(cx.subscribe_in(&composer, window, |this, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                this.typed(cx);
            }
        }));
        subs.push(cx.subscribe_in(&search, window, |this, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::Change => this.refilter(cx),
            other => keyboard_follows(other, Keyboard::Text, cx),
        }));

        subs.push(cx.observe_window_appearance(window, |_, window, cx| crate::sync_theme(Some(window), cx)));
        subs.push(cx.observe_window_activation(window, |this, window, cx| {
            let active = window.is_window_active();
            crate::FOREGROUND.store(active, std::sync::atomic::Ordering::Relaxed);
            if active {
                this.open_from_notification(cx);
            }
        }));

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
            offline: false,
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
            typing: Vec::new(),
            typing_sent: None,
            tail: None,
            thumbs: HashMap::new(),
            thumb_order: VecDeque::new(),
            asked: HashSet::new(),
            viewer: None,
            fetching: HashSet::new(),
            uploads: 0,
            picked: None,
            picked_at: Point::default(),
            swipe: None,
            chat_sheet: None,
            faces: HashMap::new(),
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
                crate::background(true);
                // The app may have been started by a tap on a notification.
                self.open_from_notification(cx);
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
            Ev::Face(user, bytes) => {
                // Asked for by the chat that is open now, not by one closed meanwhile.
                if let (Some(slot), Some(format)) = (self.faces.get_mut(&user), image_format(&bytes)) {
                    *slot = Some(Arc::new(Image::from_bytes(format, bytes)));
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
            Ev::Timeline { room, diffs } => {
                if self.open.as_ref() != Some(&room) {
                    return;
                }
                for diff in diffs {
                    match crate::backend::apply(&mut self.msgs, diff) {
                        Some((old, new)) => self.timeline.splice(old, new),
                        None => self.timeline.reset(self.msgs.len()),
                    }
                }
            }
            Ev::Paged { room, more } => {
                if self.open.as_ref() != Some(&room) {
                    return;
                }
                (self.loading, self.more) = (false, more);
            }
            Ev::Typing { room, names } => {
                if self.open.as_ref() != Some(&room) {
                    return;
                }
                self.typing = names;
            }
            Ev::Online(online) => self.offline = !online,
            Ev::Error(e) => {
                self.error = Some(crate::backend::plain(&e).into());
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

    /// Tells the others that the user is writing, every few seconds while they are, and when
    /// they stop.
    fn typed(&mut self, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone() else { return };
        let writing = !self.composer.read(cx).value().is_empty();
        let fresh = self.typing_sent.is_some_and(|at| at.elapsed() < TYPING_EVERY);
        if writing != self.typing_sent.is_some() || writing && !fresh {
            self.typing_sent = writing.then(std::time::Instant::now);
            // Not worth an error when the channel is busy: the next keystroke tries again.
            let _ = self.cmd.try_send(Cmd::Typing(room, writing));
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
        } else if self.picked.take().is_some() || self.chat_sheet.take().is_some() {
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
        let pictured = m.media.as_ref().filter(|media| matches!(media.kind, MediaKind::Image | MediaKind::Sticker | MediaKind::Video));
        let linked = m.link.as_ref().and_then(|link| link.image.as_ref());
        let wanted: Vec<(String, Media)> =
            pictured.map(|media| (m.id.clone(), media.clone())).into_iter().chain(linked.map(|image| (link_key(&m.id), image.clone()))).collect();
        for (key, media) in wanted {
            if self.asked.contains(&key) || self.cmd.capacity() < self.cmd.max_capacity() / 2 {
                continue;
            }
            if self.cmd.try_send(Cmd::Thumbs(vec![(key.clone(), media)])).is_ok() {
                self.asked.insert(key);
            }
        }
    }

    /// Asks once for the picture of whoever wrote this message.
    fn want_face(&mut self, ix: usize) {
        let from = &self.msgs[ix].from;
        if self.faces.contains_key(from) || self.faces.len() >= MAX_FACES || self.cmd.capacity() < self.cmd.max_capacity() / 2 {
            return;
        }
        let Some(room) = self.open.clone() else { return };
        if self.cmd.try_send(Cmd::Face(room, from.clone())).is_ok() {
            self.faces.insert(from.clone(), None);
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

    /// `room` is the width a bubble has for its content.
    fn render_media(&self, m: &Msg, media: &Media, room: f32, cx: &mut Context<Self>) -> AnyElement {
        let (id, owned) = (m.id.clone(), media.clone());
        let open = cx.listener(move |this, _: &ClickEvent, _, cx| {
            // The bubble's own tap selects the message.
            cx.stop_propagation();
            this.open_media(id.clone(), owned.clone(), cx);
        });
        // Lighter than either kind of bubble it may sit in.
        let raised = cx.theme().foreground.opacity(0.1);
        let pictured = matches!(media.kind, MediaKind::Image | MediaKind::Sticker | MediaKind::Video);
        if pictured && (media.kind != MediaKind::Video || media.dims.is_some()) {
            // The picture's own shape, within what a bubble may take.
            let (max_w, max_h) = if media.kind == MediaKind::Sticker { (140., 140.) } else { (room.min(260.), 320.) };
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
                    .when(!detail.is_empty(), |el| el.child(div().text_size(px(12.)).opacity(0.7).child(detail))),
            )
            .into_any_element()
    }

    /// Adds the reaction, or removes it when it is already ours.
    fn toggle_reaction(&mut self, target: &str, key: &str, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone() else { return };
        self.picked = None;
        self.send(Cmd::React { room, target: target.to_owned(), key: key.to_owned() }, cx);
    }

    /// Opens the chat a tapped notification was for.
    fn open_from_notification(&mut self, cx: &mut Context<Self>) {
        if self.screen != Screen::Main {
            return;
        }
        let Some(chat) = crate::pending_chat() else { return };
        match OwnedRoomId::try_from(chat) {
            Ok(id) => {
                self.panel = None;
                self.chat_sheet = None;
                self.open_room(id, cx);
                cx.notify();
            }
            Err(e) => log::warn!("notification chat: {e}"),
        }
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
        if let Some(room) = self.open.take() {
            if self.typing_sent.take().is_some() {
                let _ = self.cmd.try_send(Cmd::Typing(room, false));
            }
            self.send(Cmd::Close, cx);
        }
        self.msgs.clear();
        self.typing.clear();
        self.asked.clear();
        self.thumb_order.clear();
        for (_, image) in self.thumbs.drain() {
            image.remove_asset(cx);
        }
        self.close_viewer(cx);
        self.fetching.clear();
        self.swipe = None;
        for (_, image) in self.faces.drain() {
            if let Some(image) = image {
                image.remove_asset(cx);
            }
        }
        (self.picked, self.chat_sheet) = (None, None);
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
        let p = Palette::of(cx);
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p(px(GUTTER + 8.))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(360.))
                    .gap(px(GAP))
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.faint).child("Beeper"))
                    .child(screen_title(title))
                    .child(div().pb(px(8.)).text_size(px(14.)).line_height(px(20.)).text_color(p.muted).child(hint))
                    .child(Input::new(input).large())
                    .child(
                        Button::new("login-submit")
                            .primary()
                            .h(px(TAP))
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
                    .children(self.error.clone().map(|e| div().text_size(px(14.)).text_color(p.negative).child(e))),
            )
    }

    fn tab(&self) -> Tab {
        match (self.panel, self.view) {
            (Some(Panel::Settings), _) => Tab::Settings,
            (Some(_), _) => Tab::Networks,
            (None, View::Archive) => Tab::Archive,
            (None, _) => Tab::Chats,
        }
    }

    fn set_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        if tab == self.tab() {
            return;
        }
        if self.searching {
            self.set_searching(false, window, cx);
        }
        match tab {
            Tab::Chats | Tab::Archive => {
                self.panel = None;
                self.only_unread = false;
                self.set_view(if tab == Tab::Archive { View::Archive } else { View::Inbox }, cx);
            }
            Tab::Networks => self.open_panel(Panel::Networks, cx),
            Tab::Settings => self.open_panel(Panel::Settings, cx),
        }
        cx.notify();
    }

    fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        self.view = view;
        self.refilter(cx);
    }

    fn set_filter(&mut self, view: View, only_unread: bool, cx: &mut Context<Self>) {
        self.only_unread = only_unread;
        self.set_view(view, cx);
    }

    fn set_searching(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.searching = on;
        if on {
            self.search.update(cx, |input, cx| input.focus(window, cx));
        } else {
            self.search.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.refilter(cx);
    }

    /// The destinations: a bar under a phone's screen, a column beside a wide window's.
    fn render_nav(&mut self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let current = self.tab();
        let items = TABS.map(|(tab, label, icon)| {
            let ink = if tab == current { p.text } else { p.faint };
            div()
                .id(label)
                .flex()
                .items_center()
                .text_color(ink)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| this.set_tab(tab, window, cx)))
                .map(|el| {
                    if narrow {
                        el.flex_1().h_full().flex_col().justify_center().gap(px(4.)).text_size(px(12.))
                    } else {
                        el.h(px(TAP)).px(px(GAP)).gap(px(GAP)).rounded(px(RADIUS)).hover(|el| el.bg(p.surface))
                    }
                })
                .child(Icon::new(icon).size_5())
                .child(label)
        });
        if narrow {
            h_flex().flex_none().h(px(NAV_H)).border_t_1().border_color(p.rule).children(items)
        } else {
            v_flex()
                .flex_none()
                .w(px(NAV_W))
                .h_full()
                .px(px(8.))
                .gap(px(2.))
                .child(h_flex().h(px(BAR_H)).px(px(GAP)).text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child("Beeper"))
                .children(items)
        }
    }

    fn render_rooms(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
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
                        let mark = |icon: IconName| Icon::new(icon).size_3p5().flex_none().text_color(p.faint);
                        // Nothing readable to preview: an undecrypted message says no more than "Message".
                        let preview = room.preview.as_deref().filter(|p| !p.ends_with(UTD));
                        let meta = match (room.network.as_deref().map(short_network), short_time(room.ts)) {
                            (Some(network), time) if time.is_empty() => network.to_owned(),
                            (Some(network), time) => format!("{network} · {time}"),
                            (None, time) => time,
                        };
                        h_flex()
                            .id(SharedString::from(room.id.to_string()))
                            .w_full()
                            .h(px(ROOM_ROW_H))
                            .px(px(GUTTER))
                            .gap(px(GAP))
                            .hover(|el| el.bg(if selected { p.raised } else { p.surface }))
                            .when(selected, |el| el.bg(p.raised))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.open_room(id.clone(), cx)))
                            .child(avatar(&room.name, this.avatars.get(&room.id), AVATAR, cx))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(3.))
                                    .child(
                                        h_flex()
                                            .gap(px(8.))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_size(px(16.))
                                                    .line_height(px(22.))
                                                    .when(unread, |el| el.font_weight(FontWeight::SEMIBOLD))
                                                    .child(room.name.clone()),
                                            )
                                            .child(div().flex_none().text_size(px(12.)).text_color(p.faint).child(meta)),
                                    )
                                    .child(
                                        h_flex()
                                            .gap(px(8.))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_size(px(14.))
                                                    .line_height(px(20.))
                                                    .text_color(if unread { p.text } else { p.muted })
                                                    .child(preview.unwrap_or("Message").to_owned()),
                                            )
                                            .when(room.pinned, |el| el.child(mark(IconName::Pin)))
                                            .when(room.muted, |el| el.child(mark(IconName::BellOff)))
                                            .when(room.mentions > 0, |el| {
                                                el.child(Icon::new(IconName::AtSign).size_4().flex_none().text_color(p.accent_ink))
                                            })
                                            .when(unread, |el| el.child(count_pill(room.unread, room.muted, cx))),
                                    ),
                            )
                            // A long press on a touch screen, the right button of a mouse.
                            .on_aux_click(cx.listener({
                                let id = room.id.clone();
                                move |this, event: &ClickEvent, _, cx| {
                                    if event.is_secondary() {
                                        this.chat_sheet = Some(id.clone());
                                        cx.notify();
                                    }
                                }
                            }))
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.rooms_scroll)
        .size_full();

        let archive = self.view == View::Archive;
        let has_query = !self.search.read(cx).value().trim().is_empty();
        let empty = if self.rooms.is_empty() {
            "Syncing chats..."
        } else if has_query {
            "No chat matches"
        } else if self.only_unread {
            "Nothing unread"
        } else {
            match self.view {
                View::Inbox => "No chats here yet",
                View::Archive => "Nothing archived",
                View::LowPriority => "Nothing in low priority",
            }
        };
        let top = if self.searching {
            h_flex()
                .flex_1()
                .h(px(TAP))
                .pl(px(GAP))
                .pr(px(4.))
                .rounded_full()
                .bg(p.surface)
                .child(Icon::new(IconName::Search).size_4().flex_none().text_color(p.faint))
                .child(div().flex_1().min_w_0().child(Input::new(&self.search).appearance(false)))
                .child(round_button("close-search", IconName::X, cx).on_click(cx.listener(|this, _, window, cx| {
                    this.set_searching(false, window, cx);
                })))
        } else {
            h_flex()
                .flex_1()
                .child(div().flex_1().child(screen_title(if archive { "Archive" } else { "Chats" })))
                .child(round_button("search", IconName::Search, cx).on_click(cx.listener(|this, _, window, cx| {
                    this.set_searching(true, window, cx);
                })))
        };
        let waiting = self.rooms.iter().filter(|r| !r.archived && !r.low_priority && r.is_unread()).count();
        let filters = [
            ("All", View::Inbox, false, 0),
            ("Unread", View::Inbox, true, waiting),
            ("Low priority", View::LowPriority, false, 0),
        ]
        .map(|(label, view, only_unread, count)| {
            let on = self.view == view && self.only_unread == only_unread;
            // The pill is slimmer than what a finger needs; the target around it is not.
            h_flex().id(label).h(px(TAP)).cursor_pointer().on_click(cx.listener(move |this, _, _, cx| this.set_filter(view, only_unread, cx))).child(
                h_flex()
                    .h(px(36.))
                    .px(px(GAP))
                    .gap(px(6.))
                    .rounded_full()
                    .text_size(px(14.))
                    .text_color(if on { p.text } else { p.muted })
                    .when(on, |el| el.bg(p.raised))
                    .child(label)
                    .when(count > 0, |el| el.child(div().text_color(p.faint).child(count.to_string()))),
            )
        });
        v_flex()
            .size_full()
            .child(h_flex().flex_none().h(px(BAR_H)).pl(px(GUTTER)).pr(px(if self.searching { GUTTER } else { GUTTER - 10. })).child(top))
            .when(!archive && !self.searching, |el| {
                el.child(h_flex().flex_none().px(px(GUTTER)).pb(px(4.)).gap(px(4.)).children(filters))
            })
            .child(div().flex_1().min_h_0().child(if self.shown.is_empty() {
                div().px(px(GUTTER)).py(px(GAP)).text_size(px(14.)).text_color(p.faint).child(empty).into_any_element()
            } else {
                list.into_any_element()
            }))
    }

    fn render_chat(&mut self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let room = self.open.as_ref().and_then(|id| self.rooms.iter().find(|r| &r.id == id));
        let name = room.map(|r| r.name.clone()).unwrap_or_default();
        let network = room.and_then(|r| r.network.clone());
        let header_avatar = room.map(|r| avatar(&r.name, self.avatars.get(&r.id), 36., cx));
        let dm = room.is_some_and(|r| r.dm);
        // The newest message, when it is the user's: the one whose way to the others is told.
        self.tail = self.msgs.iter().rposition(|m| m.kind == Kind::Message).filter(|&ix| self.msgs[ix].own);
        let timeline = list(
            self.timeline.clone(),
            cx.processor(move |this, ix: usize, window, cx| {
                this.want_thumb(ix);
                let m = &this.msgs[ix];
                if m.kind != Kind::Message {
                    return this.render_line(ix, cx);
                }
                // What a bubble has inside: its share of the row, less its padding and a mention's bar.
                let row = f32::from(window.viewport_size().width) - 2. * GUTTER;
                let room = if narrow { row * BUBBLE_SHARE } else { BUBBLE_W } - 2. * GAP - 9.;
                let attachment = m.media.as_ref().filter(|_| !m.deleted).map(|media| this.render_media(m, media, room, cx));
                // Rows that draw nothing do not come between two messages.
                let drawn = |row: &&Msg| !matches!(row.kind, Kind::Hidden | Kind::Status { .. });
                let prev = this.msgs[..ix].iter().rev().find(drawn);
                let next = this.msgs[ix + 1..].iter().find(drawn);
                // A run: one person's messages with no long pause, and no other row, between them.
                let joined = |a: &Msg, b: &Msg| b.kind == a.kind && a.from == b.from && b.ts.saturating_sub(a.ts) < RUN_PAUSE_MS;
                let first = prev.is_none_or(|p| !joined(p, m));
                let last = next.is_none_or(|n| !joined(m, n));
                let faded = m.deleted || m.body == UTD;
                let picked = this.picked.as_deref() == Some(m.id.as_str());
                let quote = m.reply.as_ref().map(|quote| (quote.id.clone(), quote.sender.clone(), quote.text.clone()));
                let state = if m.own { this.delivery_line(ix, dm) } else { None };
                // Still only here, not yet with the server.
                let leaving = m.delivery == Delivery::Sending && !m.id.starts_with('$');
                let (id, held) = (m.id.clone(), m.id.clone());
                let swiped = this.swipe.as_ref().filter(|(on, _)| *on == m.id).map_or(0., |(_, far)| far.clamp(-SWIPE_REPLY - 16., SWIPE_REPLY + 16.));
                let ink = HighlightStyle { color: Some(p.accent_ink), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
                let underline = HighlightStyle { underline: Some(UnderlineStyle { thickness: px(1.), ..Default::default() }), ..Default::default() };
                let mut marks: Vec<_> = m.mentions.iter().map(|r| (r.clone(), ink)).collect();
                let apart = |u: &std::ops::Range<usize>| m.mentions.iter().all(|r| r.end <= u.start || u.end <= r.start);
                marks.extend(urls(&m.body).into_iter().filter(apart).map(|r| (r, underline)));
                marks.sort_by_key(|(r, _)| r.start);
                // The preview its sender attached to a link; a touch opens the link.
                let link_card = m.link.as_ref().filter(|_| !m.deleted).map(|link| {
                    let url = link.url.clone();
                    let width = room.min(LINK_W);
                    let picture = link.image.as_ref().map(|image| {
                        let (w, h) = image.dims.unwrap_or((1200, 630));
                        // The picture's own shape; one taller than the limit is cut to it, since a
                        // picture drawn to cover its box is not clipped by it.
                        let height = (width * h.max(1) as f32 / w.max(1) as f32).clamp(60., LINK_PICTURE_H);
                        div().w_full().h(px(height)).overflow_hidden().map(|el| match this.thumbs.get(&link_key(&m.id)) {
                            Some(thumb) => el.child(img(thumb.clone()).size_full().rounded_t(px(12.)).object_fit(ObjectFit::Cover)),
                            None => el,
                        })
                    });
                    let line = |size: f32, height: f32| div().text_size(px(size)).line_height(px(height));
                    v_flex()
                        .id(SharedString::from(format!("{}-link", m.id)))
                        .w(px(width))
                        .rounded(px(12.))
                        .bg(p.text.opacity(0.07))
                        .cursor_pointer()
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            crate::open_link(cx, &url);
                        }))
                        .children(picture)
                        .child(
                            v_flex()
                                .px(px(10.))
                                .py(px(8.))
                                .gap(px(2.))
                                .child(line(12., 16.).text_color(p.faint).truncate().child(link.site.clone()))
                                .when(!link.title.is_empty(), |el| {
                                    el.child(line(14., 19.).font_weight(FontWeight::SEMIBOLD).line_clamp(2).text_ellipsis().child(link.title.clone()))
                                })
                                .when(!link.description.is_empty(), |el| {
                                    el.child(line(13., 18.).text_color(p.muted).line_clamp(3).text_ellipsis().child(link.description.clone()))
                                }),
                        )
                });
                let poll_card = m.poll.as_ref().filter(|_| !m.deleted).map(|poll| cards::poll(m, poll, this.user.as_ref(), room, p, cx));
                let place_card = m.place.as_ref().filter(|_| !m.deleted).map(|place| cards::place(m, place, room, p, cx));
                let bubble = v_flex()
                    .id(SharedString::from(m.id.clone()))
                    // Without this a flex child is as wide as its longest line and leaves the screen.
                    .min_w_0()
                    .px(px(GAP))
                    .py(px(9.))
                    .gap(px(4.))
                    .rounded(px(RADIUS))
                    .bg(if m.own || picked { p.raised } else { p.surface })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| this.pick(&id, event.position(), cx)))
                    // A long press on a touch screen, the right button of a mouse.
                    .on_aux_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if event.is_secondary() {
                            this.pick(&held, event.position(), cx);
                        }
                    }))
                    .children(quote.map(|(target, who, text)| {
                        v_flex()
                            .id(SharedString::from(format!("{}-quote", m.id)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.reveal(&target, cx);
                            }))
                            .min_w_0()
                            .px(px(10.))
                            .py(px(6.))
                            .rounded(px(12.))
                            .bg(p.text.opacity(0.07))
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .when(!who.is_empty(), |el| el.child(div().font_weight(FontWeight::SEMIBOLD).child(who)))
                            .child(div().line_clamp(2).text_ellipsis().text_color(p.muted).child(text))
                    }))
                    .children(attachment)
                    .when(!m.body.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(px(16.))
                                .line_height(px(22.))
                                .when(faded, |el| el.italic().text_color(p.faint))
                                .child(StyledText::new(m.body.clone()).with_highlights(marks)),
                        )
                    })
                    .children(link_card)
                    .children(poll_card)
                    .children(place_card)
                    .when(m.edited && !m.deleted, |el| el.child(div().text_size(px(12.)).text_color(p.faint).child("Edited")));
                // Badges pinned over the bubble's lower edge, ringed in the ground's colour: standing
                // free under it they read as small messages of their own.
                let reactions = (!m.reactions.is_empty()).then(|| {
                    h_flex().flex_wrap().gap(px(2.)).mt(px(-REACTION_LAP)).px(px(8.)).children(m.reactions.iter().enumerate().map(|(n, reaction)| {
                        let (key, count, mine) = (reaction.key.as_str(), reaction.count, reaction.own);
                        let (id, key_owned) = (m.id.clone(), key.to_owned());
                        h_flex()
                            .id(SharedString::from(format!("{}-reaction-{n}", m.id)))
                            .h(px(28.))
                            .px(px(7.))
                            .gap(px(4.))
                            .rounded_full()
                            .border_2()
                            // The user's own reaction is the ringed one.
                            .border_color(if mine { p.text } else { p.ground })
                            .bg(p.lifted)
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_reaction(&id, &key_owned, cx)))
                            .child(key.to_owned())
                            .when(count > 1, |el| el.child(count.to_string()))
                    }))
                });
                // In a group, the sender's picture stands beside the last message of the run.
                let faces = !dm && !m.own;
                if faces && last {
                    this.want_face(ix);
                }
                let m = &this.msgs[ix];
                let side = if faces { px(SENDER + 8.) } else { px(0.) };
                v_flex()
                    .w_full()
                    .relative()
                    .px(px(GUTTER))
                    .pt(if first { px(GAP) } else { px(3.) })
                    .when(faces && first, |el| {
                        el.child(
                            div()
                                .pl(side + px(GAP))
                                .pb(px(4.))
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.muted)
                                .child(m.sender.clone()),
                        )
                    })
                    .on_scroll_wheel(cx.listener({
                        let id = m.id.clone();
                        move |this, event: &ScrollWheelEvent, window, cx| this.swipe(&id, event, window, cx)
                    }))
                    .when(swiped != 0., |el| {
                        // What letting go will do, shown where the message came from.
                        el.child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .map(|el| if swiped > 0. { el.left(px(GUTTER)) } else { el.right(px(GUTTER)) })
                                .flex()
                                .items_center()
                                .text_color(if swiped.abs() >= SWIPE_REPLY { p.text } else { p.faint })
                                .child(Icon::new(IconName::Reply).size_5()),
                        )
                    })
                    .child(
                        h_flex()
                            .w_full()
                            .relative()
                            .left(px(swiped))
                            .items_end()
                            .gap(px(8.))
                            .when(m.own, |el| el.flex_row_reverse())
                            .when(faces, |el| {
                                el.child(if last { avatar(&m.sender, this.faces.get(&m.from), SENDER, cx) } else { div().w(px(SENDER)).flex_none() })
                            })
                            .child(
                                h_flex()
                                    .max_w(if narrow { relative(BUBBLE_SHARE) } else { px(BUBBLE_W).into() })
                                    .min_w_0()
                                    .items_stretch()
                                    .gap(px(6.))
                                    // Addressed to the user: the one place a message carries colour.
                                    .when(m.mentioned, |el| el.child(div().w(px(3.)).flex_none().rounded_full().bg(p.accent)))
                                    .when(leaving, |el| el.opacity(0.6))
                                    .child(bubble),
                            ),
                    )
                    .child(
                        v_flex()
                            .w_full()
                            .pl(side)
                            .map(|el| if m.own { el.items_end() } else { el.items_start() })
                            .children(reactions)
                            .when(last || state.is_some(), |el| {
                                let failed = state.as_ref().is_some_and(|(_, failed)| *failed);
                                let text = match (state, last) {
                                    (Some((state, _)), true) => format!("{} \u{b7} {state}", clock(m.ts)),
                                    (Some((state, _)), false) => state,
                                    (None, _) => clock(m.ts),
                                };
                                el.child(div().px(px(GAP)).pt(px(4.)).text_size(px(12.)).text_color(if failed { p.negative } else { p.faint }).child(text))
                            }),
                    )
                    .into_any_element()
            }),
        )
        .size_full();

        v_flex()
            .size_full()
            .child(
                header()
                    .when(narrow, |el| {
                        el.child(round_button("back", IconName::ArrowLeft, cx).on_click(cx.listener(|this, _, _, cx| {
                            this.close_room(cx);
                            cx.notify();
                        })))
                    })
                    .when(!narrow, |el| el.pl(px(GUTTER)))
                    .children(header_avatar)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .pl(px(8.))
                            .child(div().text_size(px(16.)).line_height(px(20.)).font_weight(FontWeight::SEMIBOLD).truncate().child(name))
                            .children(network.map(|n| div().text_size(px(12.)).line_height(px(16.)).text_color(p.faint).truncate().child(n))),
                    )
                    .when(room.is_some(), |el| {
                        el.child(round_button("chat-menu", IconName::Ellipsis, cx).on_click(cx.listener(|this, _, _, cx| {
                            this.chat_sheet = this.open.clone();
                            cx.notify();
                        })))
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(timeline)
                    .when(self.loading && self.msgs.is_empty(), |el| {
                        el.child(div().p(px(GUTTER)).text_size(px(14.)).text_color(p.faint).child("Loading messages..."))
                    }),
            )
            .when(!self.typing.is_empty(), |el| {
                let names: Vec<&str> = self.typing.iter().map(String::as_str).collect();
                let (who, many) = people(&names);
                let line = format!("{who} {} typing...", if many { "are" } else { "is" });
                el.child(div().px(px(GUTTER + GAP)).pt(px(6.)).text_size(px(12.)).text_color(p.faint).child(line))
            })
            .children(self.render_compose_strip(cx))
            .when(self.uploads > 0, |el| {
                el.child(div().px(px(GUTTER)).pt(px(8.)).text_size(px(12.)).text_color(p.faint).child("Sending attachment..."))
            })
            .child(
                h_flex()
                    .items_end()
                    .gap(px(6.))
                    // The paperclip's own padding stands in for the rest of the left margin.
                    .pl(px(GUTTER - 10.))
                    .pr(px(GUTTER))
                    .py(px(8.))
                    .child(round_button("attach", IconName::Paperclip, cx).text_color(p.muted).on_click(cx.listener(|this, _, _, cx| this.attach(cx))))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h(px(TAP))
                            .px(px(6.))
                            .flex()
                            .items_center()
                            .rounded(px(TAP / 2.))
                            .bg(p.surface)
                            .child(Input::new(&self.composer).appearance(false).large()),
                    )
                    .child(
                        div()
                            .id("send")
                            .size(px(TAP))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(p.text)
                            .text_color(p.ground)
                            .cursor_pointer()
                            .hover(|el| el.bg(p.muted))
                            .on_click(cx.listener(|this, _, window, cx| this.send_message(window, cx)))
                            .child(Icon::new(IconName::ArrowUp).size_5()),
                    ),
            )
    }

    /// Rows that rise from the bottom over a dimmed screen: the app's menus. Touch has no
    /// right-click, and a popup's rows are too small for a finger.
    /// `title` says what the rows act on.
    fn sheet(&self, title: String, rows: Vec<Stateful<Div>>, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let (_, bottom) = crate::insets();
        div()
            .id("sheet")
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .justify_end()
            .items_center()
            .bg(gpui_kit::black().opacity(0.6))
            .occlude()
            .on_click(cx.listener(|this, _, _, cx| {
                (this.picked, this.chat_sheet) = (None, None);
                cx.notify();
            }))
            .child(
                v_flex()
                    .id("sheet-rows")
                    .w_full()
                    .max_w(px(SHEET_W))
                    .pt(px(8.))
                    .pb(px(bottom + 8.))
                    .rounded_t(px(RADIUS))
                    .bg(p.surface)
                    // A tap between the rows is not a tap outside the sheet.
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(div().h(px(36.)).px(px(GUTTER + 4.)).flex().items_center().truncate().text_size(px(14.)).text_color(p.faint).child(title))
                    .children(rows),
            )
            .into_any_element()
    }

    /// A row that is not a message: a day, a line about the chat, the mark of what is new.
    fn render_line(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = &self.msgs[ix];
        let (text, ink) = match &m.kind {
            Kind::Day => (day_label(m.ts), p.faint),
            Kind::Unread => ("New".to_owned(), p.accent_ink),
            Kind::Notice => (m.body.clone(), p.faint),
            // Joins that follow one another are told once, by the last of them.
            Kind::Joined if self.msgs.get(ix + 1).is_none_or(|next| next.kind != Kind::Joined) => {
                let mut names: Vec<&str> =
                    self.msgs[..=ix].iter().rev().take(MAX_JOINED).take_while(|m| m.kind == Kind::Joined).map(|m| m.sender.as_str()).collect();
                names.reverse();
                (format!("{} joined", people(&names).0), p.faint)
            }
            _ => return div().into_any_element(),
        };
        div().w_full().px(px(GUTTER)).pt(px(GAP)).flex().justify_center().text_size(px(12.)).text_color(ink).child(text).into_any_element()
    }

    /// What became of the user's message at `ix`, and whether that is a failure. Said of the
    /// newest message of the chat when it is the user's, and of any that failed.
    fn delivery_line(&self, ix: usize, dm: bool) -> Option<(String, bool)> {
        let m = &self.msgs[ix];
        // The bridge's word on it follows it; the latest counts, except as a step back from delivered.
        let mut state = m.delivery.clone();
        for row in &self.msgs[ix + 1..] {
            if let Kind::Status { target, delivery } = &row.kind
                && *target == m.id
                && !(state == Delivery::Delivered && *delivery == Delivery::Sent)
            {
                state = delivery.clone();
            }
        }
        if let Delivery::Failed(why) = &state {
            return Some((if why.is_empty() { "Not sent".to_owned() } else { format!("Not sent: {why}") }, true));
        }
        if self.tail != Some(ix) {
            return None;
        }
        // A read receipt on this row or under it: that person has read this far.
        let mut readers: Vec<&OwnedUserId> = Vec::new();
        for user in self.msgs[ix..].iter().flat_map(|row| &row.seen) {
            if !readers.contains(&user) {
                readers.push(user);
            }
        }
        if !readers.is_empty() {
            if dm {
                return Some(("Seen".to_owned(), false));
            }
            let named: Vec<&str> =
                readers.iter().filter_map(|user| self.msgs.iter().rev().find(|m| m.from == **user && !m.sender.is_empty())).map(|m| m.sender.as_str()).collect();
            let who = if named.len() == readers.len() { people(&named).0 } else { readers.len().to_string() };
            return Some((format!("Seen by {who}"), false));
        }
        let waited = (chrono::Utc::now().timestamp_millis() as u64).saturating_sub(m.ts) > BRIDGE_WAIT_MS;
        Some((
            match state {
                Delivery::Sending if m.id.starts_with('$') && waited => "Sent",
                Delivery::Sending => "Sending...",
                Delivery::Delivered => "Delivered",
                Delivery::Sent | Delivery::Failed(_) => "Sent",
            }
            .to_owned(),
            false,
        ))
    }

    /// Picks a message: its reactions and actions open where it was touched.
    fn pick(&mut self, id: &str, at: Point<Pixels>, cx: &mut Context<Self>) {
        (self.picked, self.picked_at) = (Some(id.to_owned()), at);
        cx.notify();
    }

    /// Scrolls to a message, when it is among the loaded ones.
    fn reveal(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(ix) = self.msgs.iter().position(|m| m.id == id) {
            self.timeline.scroll_to_reveal_item(ix);
            cx.notify();
        }
    }

    /// Follows a sideways pan over a message; one that travels far enough starts a reply.
    fn swipe(&mut self, id: &str, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(20.));
        let before = self.swipe.as_ref().map(|(_, far)| *far);
        let reply = swipe_step(&mut self.swipe, id, f32::from(delta.x), f32::from(delta.y), event.touch_phase);
        if let Some(target) = reply {
            self.start_compose(Compose::Reply(target), window, cx);
        } else if before != self.swipe.as_ref().map(|(_, far)| *far) {
            cx.notify();
        }
    }

    /// Reactions and actions for the picked message, in a panel at the finger. Whatever is tapped
    /// outside it closes it; the second tap of a double tap likes the message.
    fn render_message_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = Palette::of(cx);
        let m = self.msgs.iter().find(|m| Some(&m.id) == self.picked.as_ref()).filter(|m| !m.deleted)?;
        let (id, body, own) = (m.id.clone(), m.body.clone(), m.own);
        // A message that has not reached the server can be sent again or dropped, nothing else.
        let sent = id.starts_with('$');
        let failed = matches!(m.delivery, Delivery::Failed(_));
        let link = m.link.as_ref().map(|link| link.url.clone()).or_else(|| urls(&m.body).into_iter().next().map(|r| m.body[r].to_owned()));
        let reactions = h_flex().children(QUICK_REACTIONS.iter().enumerate().map(|(n, key)| {
            let id = id.clone();
            let mine = m.reactions.iter().any(|r| r.own && r.key == *key);
            div()
                .id(("quick-reaction", n))
                .size(px(TAP))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .when(mine, |el| el.bg(p.text.opacity(0.16)))
                .hover(|el| el.bg(p.text.opacity(0.1)))
                .text_size(px(24.))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_reaction(&id, key, cx)))
                .child(*key)
        }));
        let mut rows = Vec::new();
        if sent {
            rows.push(menu_row("reply", IconName::Reply, "Reply", p.text, cx).on_click(cx.listener({
                let id = id.clone();
                move |this, _, window, cx| this.start_compose(Compose::Reply(id.clone()), window, cx)
            })));
        } else if failed {
            rows.push(menu_row("retry", IconName::RotateCw, "Try again", p.text, cx).on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| {
                    if let Some(room) = this.open.clone() {
                        this.send(Cmd::Retry { room, target: id.clone() }, cx);
                    }
                    this.picked = None;
                    cx.notify();
                }
            })));
        }
        rows.push(menu_row("copy", IconName::Copy, "Copy text", p.text, cx).on_click(cx.listener(move |this, _, _, cx| {
            crate::copy(cx, body.clone());
            this.picked = None;
            cx.notify();
        })));
        if own {
            if sent {
                rows.push(menu_row("edit", IconName::Pencil, "Edit", p.text, cx).on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, window, cx| this.start_compose(Compose::Edit(id.clone()), window, cx)
                })));
            }
            rows.push(menu_row("delete", IconName::Trash, "Delete", p.negative, cx).on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| {
                    if let Some(room) = this.open.clone() {
                        this.send(Cmd::Delete { room, target: id.clone() }, cx);
                    }
                    this.picked = None;
                    cx.notify();
                }
            })));
        }
        if let Some(url) = link {
            let open = menu_row("open-link", IconName::ExternalLink, "Open link", p.text, cx).on_click(cx.listener(move |this, _, _, cx| {
                crate::open_link(cx, &url);
                this.picked = None;
                cx.notify();
            }));
            rows.insert(0, open);
        }
        // Under the finger when there is room, over it otherwise; never off the screen.
        let (top, bottom) = crate::insets();
        let view = window.viewport_size();
        let height = 16. + TAP * (usize::from(sent) + rows.len()) as f32;
        let floor = f32::from(view.height) - bottom - GUTTER;
        let at = self.picked_at;
        let left = (f32::from(at.x) - MENU_W / 2.).clamp(GUTTER, (f32::from(view.width) - MENU_W - GUTTER).max(GUTTER));
        let under = f32::from(at.y) + MENU_GAP;
        let y = if under + height <= floor { under } else { (f32::from(at.y) - MENU_GAP - height).max(top + GUTTER) };
        Some(
            div()
                .id("message-menu")
                .absolute()
                .inset_0()
                .bg(gpui_kit::black().opacity(0.35))
                .occlude()
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    if sent && event.click_count() >= 2 {
                        this.toggle_reaction(&id, LIKE, cx);
                    }
                    this.picked = None;
                    cx.notify();
                }))
                .child(
                    v_flex()
                        .id("message-menu-panel")
                        .absolute()
                        .left(px(left))
                        .top(px(y))
                        .w(px(MENU_W))
                        .p(px(8.))
                        .rounded(px(RADIUS))
                        .bg(p.raised)
                        // A tap between the rows is not a tap outside the panel.
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .when(sent, |el| el.child(reactions))
                        .children(rows),
                )
                .into_any_element(),
        )
    }

    /// Actions for the open chat.
    fn render_chat_sheet(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = Palette::of(cx);
        let room = self.chat_sheet.as_ref().and_then(|id| self.rooms.iter().find(|r| &r.id == id))?;
        let (id, title) = (room.id.clone(), room.name.clone());
        let rows = chat_actions(room)
            .into_iter()
            .map(|(label, icon, action)| {
                let id = id.clone();
                sheet_row(label, icon, label, p.text).on_click(cx.listener(move |this, _, _, cx| {
                    this.send(Cmd::Chat(id.clone(), action), cx);
                    this.chat_sheet = None;
                    cx.notify();
                }))
            })
            .collect();
        Some(self.sheet(title, rows, cx))
    }

    /// What the composer is replying to or editing, with a way out.
    fn render_compose_strip(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let p = Palette::of(cx);
        let (title, id) = match &self.compose {
            Compose::New => return None,
            Compose::Reply(id) => ("Replying to", id),
            Compose::Edit(id) => ("Editing", id),
        };
        let quoted = self.msgs.iter().find(|m| &m.id == id).map(|m| format!("{}: {}", m.sender, m.summary())).unwrap_or_default();
        Some(
            h_flex()
                .mx(px(GUTTER))
                .mt(px(8.))
                .pl(px(GAP))
                .rounded(px(RADIUS))
                .bg(p.surface)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .py(px(8.))
                        .text_size(px(13.))
                        .line_height(px(18.))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().truncate().text_color(p.muted).child(quoted)),
                )
                .child(round_button("cancel-compose", IconName::X, cx).on_click(cx.listener(|this, _, window, cx| {
                    this.start_compose(Compose::New, window, cx);
                }))),
        )
    }

    fn render_banners(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let card = || v_flex().mx(px(GUTTER)).mt(px(8.)).p(px(GAP)).gap(px(10.)).rounded(px(RADIUS)).text_size(px(14.)).line_height(px(20.));
        v_flex()
            // Being retried: a state, said quietly, that goes away by itself.
            .when(self.offline, |el| {
                el.child(div().h(px(28.)).flex().items_center().justify_center().text_size(px(12.)).text_color(p.faint).child("Connection lost. Reconnecting..."))
            })
            .when(self.need_recovery, |el| {
                el.child(
                    card()
                        .bg(p.warning_bg)
                        .child(div().text_color(p.warning).child(
                            "Encrypted messages are locked on this device. Copy your Beeper recovery code \
                             from a device where you are signed in, then paste it here.",
                        ))
                        .child(div().w_full().child(Input::new(&self.recovery)))
                        .child(
                            h_flex()
                                .gap(px(8.))
                                .justify_end()
                                .child(
                                    Button::new("paste-recovery")
                                        .h(px(TAP))
                                        .px(px(GUTTER))
                                        .label("Paste")
                                        .on_click(cx.listener(|this, _, window, cx| this.paste_recovery(window, cx))),
                                )
                                .child(
                                    Button::new("recover")
                                        .primary()
                                        .h(px(TAP))
                                        .px(px(GUTTER))
                                        .label("Unlock")
                                        .loading(self.busy)
                                        .disabled(self.busy)
                                        .on_click(cx.listener(|this, _, window, cx| this.recover(window, cx))),
                                ),
                        ),
                )
            })
            .children(self.error.clone().map(|e| {
                card().bg(p.surface).child(div().text_color(p.negative).child(e)).child(h_flex().justify_end().child(
                    h_flex()
                        .id("dismiss-error")
                        .h(px(TAP))
                        .px(px(GUTTER + 4.))
                        .rounded_full()
                        .bg(p.raised)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.error = None;
                            cx.notify();
                        }))
                        .child("Dismiss"),
                ))
            }))
    }
}

/// Bar across the top of a screen. It shares the ground: no line, no fill.
fn header() -> Div {
    h_flex().h(px(BAR_H)).flex_none().px(px(GUTTER - 10.)).gap(px(4.)).items_center()
}

fn screen_title(title: &'static str) -> Div {
    div().text_size(px(22.)).line_height(px(28.)).font_weight(FontWeight::SEMIBOLD).child(title)
}

/// An icon to tap, with a finger-sized target.
fn round_button(id: &'static str, icon: IconName, cx: &App) -> Stateful<Div> {
    let p = Palette::of(cx);
    div()
        .id(id)
        .size(px(TAP))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .hover(|el| el.bg(p.surface))
        .child(Icon::new(icon).size_5())
}

/// A chat's or a person's picture; until there is one, initials on a neutral disc.
fn avatar(name: &str, image: Option<&Option<Arc<Image>>>, size: f32, cx: &App) -> Div {
    let p = Palette::of(cx);
    let disc = div().size(px(size)).flex_none();
    match image.cloned().flatten() {
        Some(image) => disc.child(img(image).size_full().rounded_full().object_fit(ObjectFit::Cover)),
        None => {
            let initials: String =
                name.split_whitespace().filter_map(|w| w.chars().find(|c| c.is_alphanumeric())).take(2).flat_map(char::to_uppercase).collect();
            disc.flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(p.raised)
                .text_color(p.muted)
                .text_size(px(size * 0.36))
                .font_weight(FontWeight::SEMIBOLD)
                .child(initials)
        }
    }
}

/// What can be done with a chat, each as the opposite of its current state.
fn chat_actions(room: &RoomSummary) -> [(&'static str, IconName, ChatAction); 5] {
    [
        if room.is_unread() {
            ("Mark as read", IconName::MailOpen, ChatAction::Unread(false))
        } else {
            ("Mark as unread", IconName::Mail, ChatAction::Unread(true))
        },
        if room.pinned { ("Unpin", IconName::PinOff, ChatAction::Pin(false)) } else { ("Pin", IconName::Pin, ChatAction::Pin(true)) },
        if room.muted { ("Unmute", IconName::Bell, ChatAction::Mute(false)) } else { ("Mute", IconName::BellOff, ChatAction::Mute(true)) },
        if room.archived {
            ("Unarchive", IconName::ArchiveRestore, ChatAction::Archive(false))
        } else {
            ("Archive", IconName::Archive, ChatAction::Archive(true))
        },
        if room.low_priority {
            ("Move to chats", IconName::Inbox, ChatAction::LowPriority(false))
        } else {
            ("Move to low priority", IconName::ChevronsDown, ChatAction::LowPriority(true))
        },
    ]
}

/// Names as a phrase, and whether it stands for more than one person: "Ann", "Ann and Bob",
/// "Ann, Bob and 3 others".
fn people(names: &[&str]) -> (String, bool) {
    match names {
        [] => (String::new(), false),
        [one] => ((*one).to_owned(), *one == "You"),
        [one, two] => (format!("{one} and {two}"), true),
        [one, two, three] => (format!("{one}, {two} and {three}"), true),
        [one, two, rest @ ..] => (format!("{one}, {two} and {} others", rest.len()), true),
    }
}

/// Where a link preview's picture is kept among the thumbnails.
fn link_key(message: &str) -> String {
    format!("{message}#link")
}

/// Byte ranges of the web addresses written in a text.
fn urls(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("http").map(|at| from + at) {
        let rest = &text[at..];
        let scheme = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            0
        };
        let len = rest.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"')).unwrap_or(rest.len());
        // Punctuation after an address ends the sentence, not the address.
        let mut url = &rest[..len];
        loop {
            let cut = url.trim_end_matches(['.', ',', ';', ':', '!', '?', '\'']);
            let cut = if cut.ends_with(')') && !cut.contains('(') { &cut[..cut.len() - 1] } else { cut };
            if cut.len() == url.len() {
                break;
            }
            url = cut;
        }
        let inside_word = text[..at].ends_with(|c: char| c.is_alphanumeric());
        if scheme > 0 && !inside_word && url.len() > scheme {
            out.push(at..at + url.len());
        }
        from = at + len.max(4);
    }
    out
}

/// One action in a message's panel.
fn menu_row(id: &'static str, icon: IconName, label: &'static str, ink: Hsla, cx: &App) -> Stateful<Div> {
    let p = Palette::of(cx);
    h_flex()
        .id(id)
        .h(px(TAP))
        .px(px(GAP - 4.))
        .gap(px(GAP))
        .rounded(px(12.))
        .text_size(px(16.))
        .text_color(ink)
        .cursor_pointer()
        .hover(|el| el.bg(p.text.opacity(0.1)))
        .child(Icon::new(icon).size_5())
        .child(label)
}

/// Advances a sideways pan over a message by one event; gives the message to reply to when the
/// pan has just ended far enough. Only a pan that begins on a message counts, so a mouse wheel and
/// the glide after a fling (neither has a start) never reply.
fn swipe_step(swipe: &mut Option<(String, f32)>, id: &str, dx: f32, dy: f32, phase: TouchPhase) -> Option<String> {
    let sideways = if dx.abs() > dy.abs() { dx } else { 0. };
    match phase {
        TouchPhase::Started => *swipe = Some((id.to_owned(), sideways)),
        TouchPhase::Moved => {
            if let Some((_, far)) = swipe {
                *far += sideways;
            }
        }
        TouchPhase::Ended => return swipe.take().filter(|(_, far)| far.abs() >= SWIPE_REPLY).map(|(id, _)| id),
        TouchPhase::Cancelled => *swipe = None,
    }
    None
}

/// One action in a sheet.
fn sheet_row(id: &'static str, icon: IconName, label: &'static str, ink: Hsla) -> Stateful<Div> {
    h_flex()
        .id(id)
        .h(px(52.))
        .px(px(GUTTER + 4.))
        .gap(px(GAP))
        .text_size(px(16.))
        .text_color(ink)
        .cursor_pointer()
        .child(Icon::new(icon).size_5())
        .child(label)
}

/// A network's name as the chat list has room for it: "Facebook Messenger" is Messenger there,
/// "Google Messages (SMS)" is SMS.
fn short_network(name: &str) -> &str {
    let inner = name.split_once('(').and_then(|(_, rest)| rest.split_once(')')).map(|(inner, _)| inner);
    inner.unwrap_or_else(|| name.strip_prefix("Facebook ").unwrap_or(name))
}

/// Unread count; a bare dot for a chat marked unread by hand, grey when muted.
fn count_pill(n: u64, muted: bool, cx: &App) -> Div {
    let p = Palette::of(cx);
    div()
        .flex_none()
        .min_w(px(20.))
        .h(px(20.))
        .px(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(if muted { p.raised } else { p.accent })
        .text_color(if muted { p.text } else { p.on_accent })
        .text_size(px(12.))
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
    // No message yet: nothing to date.
    let Some(t) = local(ts_ms).filter(|_| ts_ms != 0) else { return String::new() };
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
fn keyboard_follows(event: &InputEvent, kind: Keyboard, cx: &mut Context<BeeperApp>) {
    match event {
        InputEvent::Focus => crate::show_keyboard(kind),
        InputEvent::Blur => crate::hide_keyboard(),
        _ => return,
    }
    // shortcut: timed redraws, because gpui-mobile changes the insets when the keyboard has moved
    // without asking for a frame, and the layout then keeps the keyboard's gap until the next
    // touch. Replace with its callback once it has one.
    if cfg!(target_os = "android") {
        cx.spawn(async move |this, cx| {
            for wait_ms in [150, 350, 700] {
                cx.background_executor().timer(std::time::Duration::from_millis(wait_ms)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
    }
}

impl Render for BeeperApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
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
            .font_family(crate::FACE)
            .bg(p.ground)
            .text_color(p.text);
        match self.screen {
            Screen::Starting => root
                .items_center()
                .justify_center()
                .child(div().text_size(px(14.)).text_color(p.faint).child("Connecting..."))
                .into_any_element(),
            Screen::Email | Screen::Code => root.child(self.render_login(cx)).into_any_element(),
            Screen::Main => {
                let narrow = window.viewport_size().width < px(NARROW);
                let panel = self.panel.map(|panel| self.render_panel(panel, narrow, window, cx));
                let body = if narrow {
                    // One thing at a time; the destinations stay in reach wherever they lead.
                    let deep = self.open.is_some() || self.searching || matches!(self.panel, Some(Panel::Add | Panel::Login));
                    let screen = if let Some(panel) = panel {
                        panel
                    } else if self.open.is_some() {
                        self.render_chat(true, cx).into_any_element()
                    } else {
                        self.render_rooms(cx).into_any_element()
                    };
                    v_flex()
                        .size_full()
                        .child(div().flex_1().min_h_0().child(screen))
                        .when(!deep, |el| el.child(self.render_nav(true, cx)))
                        .into_any_element()
                } else {
                    let main = if let Some(panel) = panel {
                        div().flex_1().min_w_0().h_full().child(panel).into_any_element()
                    } else {
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(div().w(px(LIST_W)).flex_none().h_full().child(self.render_rooms(cx)))
                            .child(div().flex_1().min_w_0().h_full().child(if self.open.is_some() {
                                self.render_chat(false, cx).into_any_element()
                            } else {
                                div()
                                    .size_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(14.))
                                    .text_color(p.faint)
                                    .child("Choose a chat")
                                    .into_any_element()
                            }))
                            .into_any_element()
                    };
                    h_flex().size_full().child(self.render_nav(false, cx)).child(main).into_any_element()
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
                        .bg(gpui_kit::black())
                        .text_color(gpui_kit::white())
                        .occlude()
                        .on_click(cx.listener(|this, _, _, cx| this.close_viewer(cx)))
                        .map(|el| match shown {
                            Some(image) => el.child(img(image).size_full().object_fit(ObjectFit::Contain)),
                            None => el.child("Loading..."),
                        })
                        .child(div().absolute().top(px(top + 12.)).right(px(GUTTER)).child(Icon::new(IconName::X).size_6()))
                });
                let sheet = self.render_message_menu(window, cx).or_else(|| self.render_chat_sheet(cx));
                root.relative()
                    .child(self.render_banners(cx))
                    .child(div().flex_1().min_h_0().child(body))
                    .children(sheet)
                    .children(viewer)
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use super::{SWIPE_REPLY, swipe_step, urls};
    use gpui_kit::TouchPhase::{Cancelled, Ended, Moved, Started};

    #[test]
    fn web_addresses_are_found_without_the_punctuation_around_them() {
        let found = |text: &'static str| urls(text).into_iter().map(|r| &text[r]).collect::<Vec<_>>();
        assert_eq!(found("see https://example.org/a?b=1, then (http://x.pl/ż)."), ["https://example.org/a?b=1", "http://x.pl/ż"]);
        assert_eq!(found("https://en.wikipedia.org/wiki/Queen_(band)"), ["https://en.wikipedia.org/wiki/Queen_(band)"]);
        assert_eq!(found("Zażółć https://a.pl! i xhttps://b.pl oraz https:// i http"), ["https://a.pl"]);
        assert!(found("no address, just httpd").is_empty());
    }

    #[test]
    fn only_a_sideways_pan_that_began_on_a_message_replies() {
        let run = |events: &[(f32, f32, gpui_kit::TouchPhase)]| {
            let mut swipe = None;
            events.iter().find_map(|&(dx, dy, phase)| swipe_step(&mut swipe, "$m", dx, dy, phase))
        };
        let far = SWIPE_REPLY / 2. + 1.;
        assert_eq!(run(&[(0., 0., Started), (far, 1., Moved), (far, 0., Moved), (0., 0., Ended)]).as_deref(), Some("$m"));
        assert_eq!(run(&[(0., 0., Started), (-far, 0., Moved), (-far, 2., Moved), (0., 0., Ended)]).as_deref(), Some("$m"), "either way");
        assert_eq!(run(&[(0., 0., Started), (far, 0., Moved), (0., 0., Ended)]), None, "too short");
        assert_eq!(run(&[(0., 0., Started), (2., 300., Moved), (1., 300., Moved), (0., 0., Ended)]), None, "scrolling the chat");
        assert_eq!(run(&[(far, 0., Moved), (far, 0., Moved), (0., 0., Ended)]), None, "a wheel has no start");
        assert_eq!(run(&[(0., 0., Started), (far, 0., Moved), (far, 0., Moved), (0., 0., Cancelled), (0., 0., Ended)]), None);
    }
}

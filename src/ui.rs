use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Datelike as _, Local, TimeZone as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    spinner::Spinner,
    v_flex,
};
use gpui_kit::base::{Presence, PresencePhase, Transition, spring, transition};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};
use tokio::sync::mpsc;

pub(crate) mod appearance;
mod camera;
mod cards;
mod emojipick;
mod contacts;
mod gallery;
mod heart;
mod location;
mod menu;
mod navigate;
pub(crate) mod motion;
mod panels;
mod pollnew;
mod rich;
mod signin;
mod video;
mod rooms;
mod viewer;

use std::path::PathBuf;

use menu::{MenuKind, QUICK_REACTIONS};
use appearance::fs;
use panels::Panel;

use crate::{Keyboard, Palette};
use crate::bridges::{Flow, Network, Step};
use crate::backend::{ChatAction, Cmd, Delivery, Diff, Ev, Kind, Media, MediaKind, Msg, Quote, QuoteState, RoomSummary, UTD};

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
/// The corners where two bubbles of one run meet.
const JOINED: f32 = 6.;
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
/// What the hover buttons beside a wide bubble take: three of `menu.rs`'s 28px buttons, their gaps and the row's.
const SIDE_ROOM: f32 = 3. * 28. + 2. * 4. + 8.;
const SHEET_W: f32 = 480.;
const SHEET_ROW_H: f32 = 52.;
/// How long a finger rests on a chat row before the row shows the press: Android's tap timeout.
const ROW_PRESS_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
/// Sender's picture beside a run of messages in a group.
const SENDER: f32 = 28.;
/// How far a message is swiped sideways to reply to it.
const SWIPE_REPLY: f32 = 56.;
/// Above the compose strip's tallest: its margin, padding and a quote with a picture.
const STRIP_H: f32 = 80.;
/// How far a message's reactions reach up over its bubble.
const REACTION_LAP: f32 = 10.;
/// Joins counted into one line; a longer burst says so of the last hundred.
const MAX_JOINED: usize = 100;
/// The reaction a double tap gives.
const LIKE: &str = QUICK_REACTIONS[0];
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
/// Messages that can be arriving at once; more in one go is a page of history, not news.
const MAX_ARRIVALS: usize = 3;
/// Pages of older messages loaded to find a quoted one before giving up.
const SEEK_PAGES: u8 = 5;

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
    /// Another Matrix homeserver, with a password.
    Matrix,
    Main,
}

pub struct BeeperApp {
    cmd: mpsc::Sender<Cmd>,
    screen: Screen,
    busy: bool,
    error: Option<SharedString>,
    email: Entity<InputState>,
    code: Entity<InputState>,
    matrix: signin::Form,
    recovery: Entity<InputState>,
    composer: Entity<InputState>,
    need_recovery: bool,
    /// The connection is lost and being retried.
    offline: bool,
    /// Holds keyboard focus when no input does, so Escape (Android's back) still reaches the app.
    focus: FocusHandle,
    dir: PathBuf,
    settings: crate::Settings,
    emoji: panels::EmojiChoice,
    look: appearance::Look,
    user: Option<OwnedUserId>,
    /// The account is Beeper's, so it has chat networks.
    beeper: bool,
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
    /// The message whose reactions and actions are showing.
    menu: Option<menu::MessageMenu>,
    /// A mouse button held on a message, on its way to being a long press.
    press: Option<menu::Press>,
    /// A horizontal pan in progress over a message, with how far it has travelled.
    swipe: Option<(String, f32)>,
    /// The message a finger let go of, springing back into place since then.
    swipe_back: Option<(String, Instant)>,
    /// Messages that just arrived at the end of the open chat, at most `MAX_ARRIVALS`.
    arrivals: VecDeque<(String, Instant)>,
    /// When the open chat was opened: what arrives in its first moments is history, not news.
    opened_at: Option<Instant>,
    /// The message a quote led to, tinted for a moment; the tint's clock starts when it is drawn.
    flash: Option<(String, Option<Instant>)>,
    /// A quoted message being looked for among older ones, with the pages loaded for it.
    seek: Option<(String, u8)>,
    /// The user's reaction just given: message, emoji, and when its badge was first drawn.
    fresh_reaction: Option<(String, String, Option<Instant>)>,
    /// Where and when a double tap last gave a heart.
    heart_pop: Option<(Point<Pixels>, Instant)>,
    /// What the compose strip showed last, drawn while it leaves.
    strip_last: Option<(Option<IconName>, SharedString, Quote)>,
    /// Bumped when the strip must go at once (Escape), so it does not play its exit.
    strip_epoch: usize,
    /// The chat whose actions are showing.
    chat_sheet: Option<OwnedRoomId>,
    /// The attach sheet's photos and videos.
    gallery: gallery::Gallery,
    location: location::Locate,
    poll_form: pollnew::Form,
    /// Attachments waiting for the one uploading now, so a batch keeps its order.
    attach_queue: VecDeque<Cmd>,
    attaching: bool,
    /// Received contact cards.
    contacts: contacts::State,
    /// The chat whose sheet is still falling after it closed.
    sheet_last: Option<OwnedRoomId>,
    /// The phone's last move between the chat list and a chat: when, and whether it went in.
    nav: Option<(std::time::Instant, bool)>,
    /// The picture still fading out after the viewer closed.
    viewer_last: Option<(String, Option<Arc<Image>>)>,
    zoom: viewer::Zoom,
    /// The reaction under the pointer in the picker.
    picker_hover: Option<usize>,
    emoji_pick: emojipick::Picker,
    emoji_open: Option<emojipick::Target>,
    /// A finger resting on a chat row, and since when; the row shows it after a tap's time.
    row_press: Option<(OwnedRoomId, std::time::Instant)>,
    compose: Compose,
    more: bool,
    loading: bool,
    timeline: ListState,
    drafts: navigate::Drafts,
    place: navigate::Place,
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
    /// Leaving chats and starting new ones.
    people: rooms::People,
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
        let emoji_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search emoji"));
        let emoji_pick = emojipick::Picker::new(emoji_search.clone(), &dir);

        let mut subs = Vec::new();
        let look = appearance::Look::new(&settings, &dir, cx);
        subs.push(cx.subscribe_in(&look.dim, window, |this, _, ev: &appearance::SliderEvent, _, cx| this.dim_changed(ev, cx)));
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

        let matrix = signin::Form::new(window, cx, &mut subs);
        subs.push(cx.subscribe_in(&cookie_paste, window, |_, _, ev: &InputEvent, _, cx| keyboard_follows(ev, Keyboard::Key, cx)));
        subs.push(cx.subscribe_in(&composer, window, |this, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) && this.composer_changed(cx) {
                this.typed(cx);
            }
        }));
        subs.push(cx.subscribe_in(&emoji_search, window, |_, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::Change => cx.notify(),
            other => keyboard_follows(other, Keyboard::Text, cx),
        }));
        subs.push(cx.subscribe_in(&search, window, |this, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::Change => this.refilter(cx),
            other => keyboard_follows(other, Keyboard::Text, cx),
        }));

        subs.push(cx.observe_window_appearance(window, |_, window, cx| crate::sync_theme(Some(window), cx)));
        subs.push(cx.observe_window_activation(window, |this, window, cx| {
            let active = window.is_window_active();
            crate::FOREGROUND.store(active, std::sync::atomic::Ordering::Relaxed);
            if !active {
                this.drafts.save();
            }
            if active {
                // The animation setting may have changed while the app was away.
                motion::follow_system(cx);
                this.open_from_notification(cx);
            }
        }));

        let timeline = ListState::new(0, ListAlignment::Bottom, px(800.));
        let weak = cx.entity().downgrade();
        timeline.set_scroll_handler(move |ev, _, cx| {
            if ev.visible_range.start == 0 {
                let _ = weak.update(cx, |this, cx| this.load_older(cx));
            }
            // Deferred: the list is borrowed while it calls this handler.
            let again = weak.clone();
            cx.defer(move |cx| {
                let _ = again.update(cx, |this, cx| this.scrolled(cx));
            });
        });

        let events = cx.spawn(async move |this, cx| {
            while let Some(ev) = events.recv().await {
                if this.update(cx, |this, cx| this.on_event(ev, cx)).is_err() {
                    break;
                }
            }
        });

        let drafts = navigate::Drafts::load(&dir);
        Self {
            cmd,
            screen: Screen::Starting,
            busy: false,
            error: None,
            email,
            code,
            matrix,
            recovery,
            composer,
            need_recovery: false,
            offline: false,
            focus: cx.focus_handle(),
            dir,
            emoji: Default::default(),
            look,
            settings,
            user: None,
            beeper: true,
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
            menu: None,
            press: None,
            swipe: None,
            swipe_back: None,
            arrivals: VecDeque::new(),
            opened_at: None,
            flash: None,
            seek: None,
            fresh_reaction: None,
            heart_pop: None,
            strip_last: None,
            strip_epoch: 0,
            chat_sheet: None,
            gallery: gallery::Gallery::default(),
            location: location::Locate::default(),
            poll_form: pollnew::Form::default(),
            attach_queue: VecDeque::new(),
            attaching: false,
            contacts: contacts::State::default(),
            sheet_last: None,
            nav: None,
            viewer_last: None,
            zoom: viewer::Zoom::default(),
            picker_hover: None,
            emoji_pick,
            emoji_open: None,
            row_press: None,
            faces: HashMap::new(),
            compose: Compose::New,
            more: false,
            loading: false,
            timeline,
            rooms_scroll: UniformListScrollHandle::new(),
            drafts,
            place: navigate::Place::default(),
            avatars: HashMap::new(),
            peeked: HashSet::new(),
            _subs: subs,
            _events: events,
            people: Default::default(),
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
            Ev::Ready { user, beeper } => {
                self.beeper = beeper;
                crate::background(true);
                // The app may have been started by a tap on a notification.
                self.open_from_notification(cx);
                self.user = Some(user);
                self.send(Cmd::Incognito(self.settings.incognito), cx);
                let l = &self.settings.lock;
                crate::applock::configure(l.lock, l.timeout_secs(), l.secure);
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
                    let media = self.msgs.iter().find(|m| m.id == id).and_then(|m| m.media.as_ref());
                    if !media.is_some_and(|media| video::play(&path, media)) {
                        let mime = media.and_then(|media| media.mime.clone());
                        crate::open_file(cx, &path, mime.as_deref());
                    }
                }
            }
            Ev::Attached => {
                self.uploads = self.uploads.saturating_sub(1);
                self.attaching = false;
                self.next_attach(cx);
            }
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
                // Rows added at the end, with what stood there before and the messages among them.
                let mut ends = Vec::new();
                for diff in diffs {
                    if let Diff::Set { index, value } = &diff
                        && let Some(old) = self.msgs.get(*index)
                        && let Some(arrival) = self.arrivals.iter_mut().find(|(id, _)| *id == old.id)
                    {
                        // The user's message reached the server: its transaction ID became the event's.
                        arrival.0 = value.id.clone();
                    }
                    let len_before = self.msgs.len();
                    match crate::backend::apply(&mut self.msgs, diff) {
                        Some((old, new)) => {
                            if old.start == len_before && old.is_empty() {
                                let added = &self.msgs[old.start..old.start + new];
                                let ids: Vec<String> = added.iter().filter(|m| m.kind == Kind::Message).map(|m| m.id.clone()).collect();
                                ends.push((len_before, new, ids));
                            }
                            self.timeline.splice(old, new);
                        }
                        None => self.timeline.reset(self.msgs.len()),
                    }
                }
                let appended = ends.iter().map(|(_, new, _)| new).sum();
                let open_for = self.opened_at.map_or(Duration::ZERO, |at| at.elapsed());
                let now = Instant::now();
                for (len_before, new, ids) in ends {
                    if arrived(len_before, appended, open_for) {
                        self.arrivals.extend(ids.into_iter().map(|id| (id, now)));
                    }
                    self.count_missed(len_before, new);
                }
                self.seek_unread();
                self.arrivals.retain(|(_, at)| now.duration_since(*at) < Duration::from_secs(1));
                while self.arrivals.len() > 2 * MAX_ARRIVALS {
                    self.arrivals.pop_front();
                }
            }
            Ev::Paged { room, more } => {
                if self.open.as_ref() != Some(&room) {
                    return;
                }
                (self.loading, self.more) = (false, more);
                self.place.seek_unread = false;
                if let Some((id, pages)) = self.seek.take() {
                    if self.msgs.iter().any(|m| m.id == id) {
                        self.reveal(&id, cx);
                    } else if more && pages + 1 < SEEK_PAGES {
                        self.seek = Some((id, pages + 1));
                        self.load_older(cx);
                    }
                }
            }
            Ev::Typing { room, names } => {
                if self.open.as_ref() != Some(&room) {
                    return;
                }
                self.typing = names;
            }
            Ev::Online(online) => self.offline = !online,
            Ev::Exported(id, path, what) => self.exported(id, path, what, cx),
            Ev::Error(e) => {
                self.error = Some(crate::backend::plain(&e).into());
                self.busy = false;
                self.loading = false;
            }
            Ev::Contact(id, cards) => self.contacts.keep(id, cards),
            Ev::People(answer) => self.on_people(answer, cx),
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
            self.jump_to_newest(cx);
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
        self.close_menu(cx);
        self.composer.update(cx, |c, cx| c.focus(window, cx));
        cx.notify();
    }

    /// Escape, which is also Android's back gesture: undoes the innermost thing on screen.
    /// Returns whether there was anything to undo.
    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let narrow = window.viewport_size().width < px(NARROW);
        if self.viewer.is_some() {
            self.close_viewer(cx);
        } else if self.close_confirm(cx) {
        } else if self.menu_open() {
            self.close_menu(cx);
        } else if self.location.open {
            self.close_location(cx);
        } else if self.emoji_open.is_some() {
            self.close_emoji(cx);
        } else if self.close_poll_form(cx) {
        } else if self.gallery.open {
            self.close_gallery(cx);
        } else if self.chat_sheet.take().is_some() {
            cx.notify();
        } else if self.compose != Compose::New {
            self.strip_epoch += 1;
            self.start_compose(Compose::New, window, cx);
        } else if self.panel.is_some() {
            self.back(cx);
        } else if self.searching {
            self.set_searching(false, window, cx);
        } else if narrow && self.open.is_some() {
            self.nav = Some((std::time::Instant::now(), false));
            self.close_room(cx);
            cx.notify();
        } else {
            return false;
        }
        true
    }

    /// The picture stays in `viewer_last` while it fades out; `render` frees it after.
    fn close_viewer(&mut self, cx: &mut Context<Self>) {
        if let Some(closed) = self.viewer.take()
            && let Some((_, Some(image))) = self.viewer_last.replace(closed)
        {
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
        let quoted = m.reply.as_ref().and_then(|q| q.media.as_ref()).filter(|media| matches!(media.kind, MediaKind::Image | MediaKind::Sticker | MediaKind::Video));
        let wanted: Vec<(String, Media)> = pictured
            .map(|media| (m.id.clone(), media.clone()))
            .into_iter()
            .chain(linked.map(|image| (link_key(&m.id), image.clone())))
            .chain(quoted.map(|media| (quote_key(&m.id), media.clone())))
            .collect();
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

    /// A tap on an attachment: pictures open full-screen, videos on Android in the app's player,
    /// everything else in the system's app.
    fn open_media(&mut self, id: String, media: Media, cx: &mut Context<Self>) {
        if matches!(media.kind, MediaKind::Image | MediaKind::Sticker) {
            if self.send(Cmd::View(id.clone(), media), cx) {
                self.viewer = Some((id, None));
                self.open_zoom();
            }
        } else if !self.fetching.contains(&id) && self.send(Cmd::Fetch(id.clone(), media), cx) {
            self.fetching.insert(id);
        }
    }

    fn attach(&mut self, cx: &mut Context<Self>) {
        if gallery::enabled() {
            self.open_gallery(cx);
        } else {
            self.attach_file(cx);
        }
    }

    /// The system's picker, for documents and for anything on desktop.
    fn attach_file(&mut self, cx: &mut Context<Self>) {
        let picked = crate::pick_file(cx);
        cx.spawn(async move |this, cx| {
            let Some(path) = picked.await else { return };
            let _ = this.update(cx, |this, cx| {
                let Some(room) = this.open.clone() else { return };
                let reply_to = match std::mem::replace(&mut this.compose, Compose::New) {
                    Compose::Reply(id) => Some(id),
                    _ => None,
                };
                this.uploads += 1;
                this.queue_attach(room, vec![path], reply_to, cx);
            });
        })
        .detach();
    }

    /// `room` is the width a bubble has for its content.
    /// `radius` is a picture's corner, concentric with the bubble around it.
    fn render_media(&self, m: &Msg, media: &Media, room: f32, radius: f32, cx: &mut Context<Self>) -> AnyElement {
        if let Some(card) = self.contact_card(m, media, room, cx) {
            return card;
        }
        let (id, owned) = (m.id.clone(), media.clone());
        let open = cx.listener(move |this, _: &ClickEvent, _, cx| {
            // Not a tap on the bubble as well.
            cx.stop_propagation();
            this.open_media(id.clone(), owned.clone(), cx);
        });
        // Lighter than either kind of bubble it may sit in.
        let raised = cx.theme().foreground.opacity(0.1);
        if drawn_as_picture(media) {
            // The picture's own shape, within what a bubble may take.
            let (w, h) = picture_size(media, room);
            let thumb = self.thumbs.get(&m.id).cloned();
            let video = media.kind == MediaKind::Video;
            return div()
                .id(SharedString::from(format!("{}-media", m.id)))
                .relative()
                .w(px(w))
                .h(px(h))
                .rounded(px(radius))
                .overflow_hidden()
                // A sticker stands free once it is there.
                .when(media.kind != MediaKind::Sticker || thumb.is_none(), |el| el.bg(raised))
                .cursor_pointer()
                .on_click(open)
                .map(|el| match thumb {
                    Some(image) if media.kind == MediaKind::Sticker => el.child(img(image).size_full().object_fit(ObjectFit::Contain)),
                    // The ring keeps a dark picture's edge from melting into the ground.
                    Some(image) => el.child(img(image).size_full().rounded(px(radius)).object_fit(ObjectFit::Cover)).child(
                        div().absolute().inset_0().rounded(px(radius)).border_1().border_color(Palette::of(cx).text.opacity(0.1)),
                    ),
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
                                .map(|el| {
                                    if self.fetching.contains(&m.id) {
                                        el.child(Spinner::new().color(gpui_kit::white()))
                                    } else {
                                        el.child(Icon::new(IconName::Play).size_5())
                                    }
                                }),
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
                            .text_size(fs(11.))
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
                    // Two lines, so a long file name keeps its extension in sight more often.
                    .child(div().line_clamp(2).text_ellipsis().text_size(fs(15.)).line_height(fs(20.)).child(match media.kind {
                        MediaKind::Voice => "Voice message".to_owned(),
                        _ => media.name.clone(),
                    }))
                    .when(!detail.is_empty(), |el| el.child(div().text_size(fs(12.)).font_features(tabular()).opacity(0.7).child(detail))),
            )
            .into_any_element()
    }

    /// Adds the reaction, or removes it when it is already ours.
    fn toggle_reaction(&mut self, target: &str, key: &str, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone() else { return };
        self.close_menu(cx);
        // The SDK takes a reaction back only by its exact key; the user's own may lack or carry U+FE0F.
        let own = self.msgs.iter().find(|m| m.id == target).and_then(|m| m.reactions.iter().find(|r| r.own && menu::same_emoji(&r.key, key)));
        let adding = own.is_none();
        let key = own.map_or(key, |r| r.key.as_str()).to_owned();
        if adding {
            self.fresh_reaction = Some((target.to_owned(), key.clone(), None));
        }
        self.send(Cmd::React { room, target: target.to_owned(), key }, cx);
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
                if self.open.as_ref() != Some(&id) {
                    self.nav = Some((std::time::Instant::now(), true));
                }
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
        self.opened_at = Some(Instant::now());
        self.show_draft(Some(&id));
        self.loading = self.send(Cmd::Open(id), cx);
    }

    fn close_room(&mut self, cx: &mut Context<Self>) {
        self.emoji_open = None;
        self.close_gallery(cx);
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
        (self.swipe, self.swipe_back, self.flash, self.seek, self.fresh_reaction, self.strip_last) = (None, None, None, None, None, None);
        self.arrivals.clear();
        for (_, image) in self.faces.drain() {
            if let Some(image) = image {
                image.remove_asset(cx);
            }
        }
        (self.menu, self.press, self.chat_sheet) = (None, None, None);
        self.compose = Compose::New;
        self.timeline.reset(0);
        self.drafts.save();
        self.show_draft(None);
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
                    .child(div().text_size(fs(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.faint).child("Murmur"))
                    .child(screen_title(title))
                    .child(div().pb(px(8.)).text_size(fs(14.)).line_height(fs(20.)).text_color(p.muted).child(hint))
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
                    .when(!on_code, |el| {
                        el.child(Button::new("login-matrix").ghost().label("Use another Matrix server").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.screen = Screen::Matrix;
                                this.error = None;
                                cx.notify();
                            }),
                        ))
                    })
                    .children(self.error.clone().map(|e| div().text_size(fs(14.)).text_color(p.negative).child(e))),
            )
    }

    fn tab(&self) -> Tab {
        match (self.panel, self.view) {
            (Some(Panel::Settings), _) => Tab::Settings,
            (Some(Panel::NewChat), _) => Tab::Chats,
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
                .active(|el| el.bg(p.surface))
                .on_click(cx.listener(move |this, _, window, cx| this.set_tab(tab, window, cx)))
                .map(|el| {
                    if narrow {
                        el.flex_1().h_full().flex_col().justify_center().gap(px(4.)).text_size(fs(12.))
                    } else {
                        el.h(px(TAP))
                            .px(px(GAP))
                            .gap(px(GAP))
                            .rounded(px(RADIUS))
                            .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.surface)))
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
                .child(h_flex().h(px(BAR_H)).px(px(GAP)).text_size(fs(16.)).font_weight(FontWeight::SEMIBOLD).child("Murmur"))
                .children(items)
        }
    }

    /// Where a finger lands on a chat row. A touch reaches the app as a click only when it lifts,
    /// so the row learns of the press from the touch itself; it shows it after `ROW_PRESS_DELAY`,
    /// so a finger that starts scrolling the list never lights a row up.
    fn row_touch(&self, room: OwnedRoomId, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let weak = cx.entity().downgrade();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let visible = window.content_mask().bounds;
                window.on_mouse_event(move |e: &TouchDragEvent, phase, _, cx| {
                    let at = e.start_position;
                    if phase.bubble() && e.phase == TouchPhase::Started && bounds.contains(&at) && visible.contains(&at) {
                        let _ = weak.update(cx, |this, cx| this.row_pressed(room.clone(), cx));
                    }
                });
            },
        )
        .absolute()
        .inset_0()
    }

    fn row_pressed(&mut self, room: OwnedRoomId, cx: &mut Context<Self>) {
        if self.chat_sheet.is_some() || self.menu_open() || self.viewer.is_some() {
            return;
        }
        let at = std::time::Instant::now();
        self.row_press = Some((room, at));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ROW_PRESS_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if this.row_press.as_ref().is_some_and(|(_, since)| *since == at) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Ends a chat row's press: the finger lifted (a tap), started scrolling, or let go of a long
    /// press.
    fn row_release(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let weak = cx.entity().downgrade();
        let release = move |cx: &mut App| {
            let _ = weak.update(cx, |this, cx| {
                if this.row_press.take().is_some() {
                    cx.notify();
                }
            });
        };
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let scroll = release.clone();
                window.on_mouse_event(move |_: &ScrollWheelEvent, phase, _, cx| {
                    if phase.bubble() {
                        scroll(cx);
                    }
                });
                let up = release.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase.bubble() {
                        up(cx);
                    }
                });
                window.on_mouse_event(move |e: &LongPressEvent, phase, _, cx| {
                    if phase.bubble() && matches!(e.phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                        release(cx);
                    }
                });
            },
        )
        .absolute()
        .size_0()
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
                        let held = this.row_press.as_ref().is_some_and(|(id, at)| id == &room.id && at.elapsed() >= ROW_PRESS_DELAY);
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
                            .relative()
                            .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(if selected { p.raised } else { p.surface })))
                            .when(selected || held, |el| el.bg(p.raised))
                            .active(|el| el.bg(p.raised))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.open.as_ref() != Some(&id) {
                                    this.nav = Some((std::time::Instant::now(), true));
                                }
                                this.open_room(id.clone(), cx)
                            }))
                            .child(this.row_touch(room.id.clone(), cx))
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
                                                    .text_size(fs(16.))
                                                    .line_height(fs(22.))
                                                    .when(unread, |el| el.font_weight(FontWeight::SEMIBOLD))
                                                    .child(room.name.clone()),
                                            )
                                            .child(div().flex_none().text_size(fs(12.)).font_features(tabular()).text_color(p.faint).child(meta)),
                                    )
                                    .child(
                                        h_flex()
                                            .gap(px(8.))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_size(fs(14.))
                                                    .line_height(fs(20.))
                                                    .text_color(if unread { p.text } else { p.muted })
                                                    .child(match this.drafts.get(&room.id) {
                                                        Some(draft) => h_flex()
                                                            .gap(px(4.))
                                                            .child(div().flex_none().text_color(p.faint).child("Draft:"))
                                                            .child(div().flex_1().min_w_0().truncate().child(crate::emoji::styled(draft.replace('\n', " "), Vec::new())))
                                                            .into_any_element(),
                                                        None => crate::emoji::styled(preview.unwrap_or("Message").to_owned(), Vec::new()).into_any_element(),
                                                    }),
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
            "Syncing chats…"
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
                .child(round_button("new-chat", IconName::Plus, cx).on_click(cx.listener(|this, _, window, cx| this.open_new_chat(window, cx))))
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
            h_flex().id(label).group(label).h(px(TAP)).cursor_pointer().on_click(cx.listener(move |this, _, _, cx| this.set_filter(view, only_unread, cx))).child(
                h_flex()
                    .id("pill")
                    .when(!on, |el| el.group_active(label, |el| el.bg(p.surface)))
                    .h(px(36.))
                    .px(px(GAP))
                    .gap(px(6.))
                    .rounded_full()
                    .text_size(fs(14.))
                    .text_color(if on { p.text } else { p.muted })
                    .when(on, |el| el.bg(p.raised))
                    .child(label)
                    .when(count > 0, |el| el.child(div().text_color(p.faint).font_features(tabular()).child(count.to_string()))),
            )
        });
        v_flex()
            .size_full()
            .child(h_flex().flex_none().h(px(BAR_H)).pl(px(GUTTER)).pr(px(if self.searching { GUTTER } else { GUTTER - 10. })).child(top))
            .when(!archive && !self.searching, |el| {
                el.child(h_flex().flex_none().px(px(GUTTER)).pb(px(4.)).gap(px(4.)).children(filters))
            })
            .child(self.row_release(cx))
            .child(div().flex_1().min_h_0().child(if self.shown.is_empty() {
                div().px(px(GUTTER)).py(px(GAP)).text_size(fs(14.)).text_color(p.faint).child(empty).into_any_element()
            } else {
                list.into_any_element()
            }))
    }

    fn render_chat(&mut self, narrow: bool, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        // The top of the messages is on screen: they do not fill it, or the user has scrolled up to
        // them. A list too short to scroll reports no scrolling, so this is not left to its handler.
        // Told by the offset, not by the oldest row: rows that draw nothing take no height, and the
        // list then puts its top at the first row that does.
        if self.timeline.scroll_px_offset_for_scrollbar().y >= px(0.) {
            self.load_older(cx);
        }
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
                this.want_card(ix);
                let flash = this.flash_level(ix, window, cx);
                let reaction_since = this.reaction_since(ix);
                let m = &this.msgs[ix];
                if m.kind != Kind::Message {
                    return this.render_line(ix, cx);
                }
                // What a bubble has inside: its share of the row, less its padding and a mention's bar.
                let row = f32::from(window.viewport_size().width) - 2. * GUTTER;
                // Wide, the pane beside the lists also holds a face and the hover buttons.
                let room = if narrow { row * BUBBLE_SHARE } else { BUBBLE_W.min(row - NAV_W - LIST_W - SENDER - 8. - SIDE_ROOM).max(TAP * 4.) } - 2. * GAP - 9.;
                // A picture sits in its bubble with a thin margin, concentric with it; alone, it is the bubble.
                let pictured = !m.deleted && m.media.as_ref().is_some_and(drawn_as_picture);
                // A few emoji on their own are drawn large and free, as the phones do.
                let jumbo = m.media.is_none() && m.reply.is_none() && m.link.is_none() && m.poll.is_none() && m.place.is_none() && !m.edited && emoji_only(&m.body);
                let bare = jumbo || (pictured && m.body.is_empty() && m.reply.is_none() && !m.edited && m.link.is_none());
                // A place alone fills its bubble, its card concentric with it.
                let place_only = !m.deleted && m.place.is_some() && m.body.is_empty() && m.reply.is_none() && !m.edited;
                let (pad_x, pad_y) = if bare {
                    (0., 0.)
                } else if pictured {
                    (4., 4.)
                } else if place_only {
                    (8., 8.)
                } else {
                    (GAP, 9.)
                };
                let text_x = if pictured { GAP - pad_x } else { 0. };
                // A caption keeps to its picture's width.
                let framed_w = m.media.as_ref().filter(|_| pictured && !bare).map(|media| picture_size(media, room).0 + 2. * pad_x);
                let attachment = m.media.as_ref().filter(|_| !m.deleted).map(|media| this.render_media(m, media, room, RADIUS - pad_x, cx));
                // Rows that draw nothing do not come between two messages.
                let drawn = |row: &&Msg| !matches!(row.kind, Kind::Hidden | Kind::Status { .. });
                let prev = this.msgs[..ix].iter().rev().find(drawn);
                let next = this.msgs[ix + 1..].iter().find(drawn);
                // A run: one person's messages with no long pause, and no other row, between them.
                let joined = |a: &Msg, b: &Msg| b.kind == a.kind && a.from == b.from && b.ts.saturating_sub(a.ts) < RUN_PAUSE_MS;
                let first = prev.is_none_or(|p| !joined(p, m));
                let last = next.is_none_or(|n| !joined(m, n));
                let faded = m.deleted || m.body == UTD;
                // Where a run's bubbles meet, their corners on the sender's side are tighter; a picture
                // keeps its round corners.
                let joins = !pictured;
                let top_r = if first || !joins { RADIUS } else { JOINED };
                let bottom_r = if last || !joins { RADIUS } else { JOINED };
                // A pointer's menu marks its message; the touch overlay lifts it instead.
                let picked = this.menu.as_ref().is_some_and(|menu| menu.id == m.id && menu.kind != MenuKind::Touch);
                let quote = m.reply.as_ref().map(|quote| {
                    let thumb = this.thumbs.get(&quote_key(&m.id)).cloned();
                    let target = quote.id.clone();
                    let inset = quote_inset(quote, None, inside_of(m, p), thumb).id(SharedString::from(format!("{}-quote", m.id))).mx(px(8. - pad_x)).mt(px(8. - pad_y));
                    match quote.state {
                        QuoteState::Missing => inset,
                        _ => inset.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.reveal(&target, cx);
                        })),
                    }
                });
                let state = if m.own { this.delivery_line(ix, dm) } else { None };
                // Still only here, not yet with the server.
                let leaving = m.delivery == Delivery::Sending && !m.id.starts_with('$');
                let (id, held) = (m.id.clone(), m.id.clone());
                // The finger's pan, followed 1:1 with resistance past the reply point; once it has let
                // go, a spring brings the message back.
                let dragged = this.swipe.as_ref().filter(|(on, _)| *on == m.id).map(|(_, far)| rubber(*far));
                let springing = this.swipe_back.as_ref().is_some_and(|(on, at)| *on == m.id && at.elapsed() < Duration::from_millis(600));
                let swiped = if dragged.is_some() || springing {
                    let release = motion::RELEASE.with_travel(dragged.is_none());
                    spring((SharedString::from(m.id.clone()), "swipe"), dragged.unwrap_or(0.), release, window, cx)
                } else {
                    0.
                };
                let armed = dragged.is_some_and(|far| far >= SWIPE_REPLY);
                let arrow_scale = if swiped > 0.5 {
                    let grow = Transition::new(Duration::from_millis(120)).easing(motion::EASE_OUT);
                    transition((SharedString::from(m.id.clone()), "armed"), if armed { 1.15 } else { 1. }, grow, window, cx)
                } else {
                    1.
                };
                let arriving = this.arrivals.iter().find(|(id, _)| *id == m.id).and_then(|&(_, at)| fresh(at, motion::ENTER, window, cx));
                // What is drawn inside the bubble: an own one may carry a colour of the user's choosing.
                let inside = inside_of(m, p);
                let ink = HighlightStyle { color: Some(inside.accent_ink), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
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
                    let line = |size: f32, height: f32| div().text_size(fs(size)).line_height(fs(height));
                    v_flex()
                        .id(SharedString::from(format!("{}-link", m.id)))
                        .w(px(width))
                        .rounded(px(12.))
                        .bg(inside.text.opacity(0.07))
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
                                .child(line(12., 16.).text_color(inside.faint).truncate().child(link.site.clone()))
                                .when(!link.title.is_empty(), |el| {
                                    el.child(line(14., 19.).font_weight(FontWeight::SEMIBOLD).line_clamp(2).text_ellipsis().child(link.title.clone()))
                                })
                                .when(!link.description.is_empty(), |el| {
                                    el.child(line(13., 18.).text_color(inside.muted).line_clamp(3).text_ellipsis().child(link.description.clone()))
                                }),
                        )
                });
                let poll_card = m.poll.as_ref().filter(|_| !m.deleted).map(|poll| cards::poll(m, poll, this.user.as_ref(), room, inside, window, cx));
                let place_card = m.place.as_ref().filter(|_| !m.deleted).map(|place| cards::place(m, place, room, inside, cx));
                let bubble = v_flex()
                    .id(SharedString::from(m.id.clone()))
                    // Without this a flex child is as wide as its longest line and leaves the screen.
                    .min_w_0()
                    .px(px(pad_x))
                    .pt(px(pad_y))
                    .pb(px(if pictured && !(m.body.is_empty() && !m.edited) { 9. } else { pad_y }))
                    .gap(px(4.))
                    .when_some(framed_w, |el, w| el.w(px(w)))
                    .map(|el| corners(el, m.own, top_r, bottom_r))
                    .when(!bare, |el| el.bg(if m.own { p.own } else if picked { p.raised } else { p.surface }))
                    .text_color(inside.text)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| this.bubble_click(&id, event, cx)))
                    // The right button of a mouse; a touch's long press is claimed before it gets here.
                    .on_aux_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if event.is_secondary() {
                            this.open_menu(held.clone(), MenuKind::List, event.position(), None, false, cx);
                        }
                    }))
                    // Where a quote led: the tint sits under the content and fades.
                    .when_some(flash, |el, level| {
                        el.relative().child(corners(div().absolute().inset_0(), m.own, top_r, bottom_r).bg(inside.text.opacity(0.16 * level)))
                    })
                    .children(quote)
                    .children(attachment)
                    .when(!m.body.is_empty(), |el| {
                        el.child(
                            div()
                                .px(px(text_x))
                                .map(|el| if jumbo { el.text_size(fs(40.)).line_height(fs(48.)) } else { el.text_size(fs(16.)).line_height(fs(22.)) })
                                .when(faded, |el| el.italic().text_color(inside.faint))
                                .child(match m.rich.as_ref().filter(|_| !faded) {
                                    Some(rich) => rich::body(m, rich, inside, ink),
                                    None => crate::emoji::styled(m.body.clone(), marks).into_any_element(),
                                }),
                        )
                    })
                    .children(link_card)
                    .children(poll_card)
                    .children(place_card)
                    .when(m.edited && !m.deleted, |el| el.child(div().px(px(text_x)).text_size(fs(12.)).text_color(inside.faint).child("Edited")));
                // Badges pinned over the bubble's lower edge, ringed in the ground's colour: standing
                // free under it they read as small messages of their own.
                let reactions = (!m.reactions.is_empty()).then(|| {
                    // Before the next message of the run, the badges keep a little room under them.
                    h_flex().flex_wrap().gap(px(2.)).mt(px(-REACTION_LAP)).px(px(8.)).when(!last, |el| el.pb(px(4.))).children(m.reactions.iter().enumerate().map(|(n, reaction)| {
                        let (key, count, mine) = (reaction.key.as_str(), reaction.count, reaction.own);
                        let (id, key_owned) = (m.id.clone(), key.to_owned());
                        // The one the user just gave rises into place; others appear as they are.
                        let rising = reaction_since.as_ref().filter(|(given, _)| mine && menu::same_emoji(given, key)).and_then(|(_, at)| fresh(*at, motion::ENTER, window, cx));
                        h_flex()
                            .id(SharedString::from(format!("{}-reaction-{n}", m.id)))
                            .when_some(rising, |el, v| el.relative().top(px(6. * (1. - v))).opacity(v))
                            .h(px(32.))
                            .px(px(7.))
                            .gap(px(4.))
                            .rounded_full()
                            .border_2()
                            // The user's own reaction is the ringed one.
                            .border_color(if mine { p.text } else { p.ground })
                            .bg(p.lifted)
                            .text_size(fs(13.))
                            .line_height(fs(18.))
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_reaction(&id, &key_owned, cx)))
                            .child(crate::emoji::styled(key.to_owned(), Vec::new()))
                            .when(count > 1, |el| el.child(div().font_features(tabular()).child(count.to_string())))
                    }))
                });
                // In a group, the sender's picture stands beside the last message of the run.
                let faces = !dm && !m.own;
                if faces && last {
                    this.want_face(ix);
                }
                let m = &this.msgs[ix];
                let side = if faces { px(SENDER + 8.) } else { px(0.) };
                // Arriving: the user's own message rises from the composer, another's settles in.
                let rise = if m.own { 12. } else { 8. };
                v_flex()
                    .group("message")
                    .w_full()
                    .relative()
                    .when_some(arriving, |el, v| el.top(px(rise * (1. - v))).opacity(v))
                    .px(px(GUTTER))
                    .pt(if first { px(GAP) } else { px(3.) })
                    .when(faces && first, |el| {
                        el.child(
                            div()
                                .pl(side + px(GAP))
                                .pb(px(4.))
                                .text_size(fs(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(p.muted)
                                .truncate()
                                .child(m.sender.clone()),
                        )
                    })
                    .on_scroll_wheel(cx.listener({
                        let id = m.id.clone();
                        move |this, event: &ScrollWheelEvent, window, cx| this.swipe(&id, event, window, cx)
                    }))
                    .when(swiped > 0.5, |el| {
                        // What letting go will do, shown where the message came from.
                        el.child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .left(px(GUTTER))
                                .flex()
                                .items_center()
                                .opacity((swiped / SWIPE_REPLY).min(1.))
                                .text_color(if armed { p.text } else { p.faint })
                                .child(Icon::new(IconName::Reply).size_5().transform(Transformation::scale(size(arrow_scale, arrow_scale)))),
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
                                    .child(this.lifted(bubble, &m.id, cx)),
                            )
                            .when(!narrow && !m.deleted, |el| el.child(this.side_buttons(m, cx))),
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
                                el.child(div().px(px(GAP)).pt(px(4.)).text_size(fs(12.)).font_features(tabular()).text_color(if failed { p.negative } else { this.on_wallpaper(p.faint, cx) }).child(text))
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
                            this.nav = Some((std::time::Instant::now(), false));
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
                            .child(div().text_size(fs(16.)).line_height(fs(20.)).font_weight(FontWeight::SEMIBOLD).truncate().child(name))
                            .children(network.map(|n| div().text_size(fs(12.)).line_height(fs(16.)).text_color(p.faint).truncate().child(n))),
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
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .children(self.wallpaper_layer(0., cx))
                    .child(timeline)
                    .child(self.press_tracker(cx))
                    .children(self.jump_button(cx))
                    .when(self.loading && self.msgs.is_empty(), |el| {
                        el.child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(fs(14.))
                                .text_color(self.on_wallpaper(p.faint, cx))
                                .child("Loading messages…")
                                // Said only when the wait is long enough to notice.
                                .with_animations(
                                    "loading",
                                    vec![Animation::new(Duration::from_millis(300)), Animation::new(motion::ENTER).with_easing(motion::ease_out)],
                                    |el, ix, t| el.opacity(if ix == 0 { 0. } else { t }),
                                ),
                        )
                    }),
            )
            .when(!self.typing.is_empty(), |el| {
                let names: Vec<&str> = self.typing.iter().map(String::as_str).collect();
                let (who, many) = people(&names);
                let line = format!("{who} {} typing…", if many { "are" } else { "is" });
                let faint = p.faint;
                // An activity indicator may loop; at 24 frames a second it costs little while it lasts.
                let pulse = Animation::new(Duration::from_millis(1200)).repeat().with_max_fps(24.);
                let dots = h_flex().gap(px(3.)).with_animation("typing-dots", pulse, move |el, t| {
                    el.children((0..3).map(|n| {
                        let phase = (t - n as f32 * 0.2).rem_euclid(1.);
                        let level = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
                        div().size(px(4.)).rounded_full().bg(faint).opacity(0.3 + 0.7 * level)
                    }))
                });
                el.child(
                    h_flex()
                        .px(px(GUTTER + GAP))
                        .pt(px(6.))
                        .gap(px(6.))
                        .text_size(fs(12.))
                        .text_color(faint)
                        .child(dots)
                        .child(line)
                        .with_animation(
                            SharedString::from(format!("typing-{}", names.join(","))),
                            Animation::new(Duration::from_millis(150)).with_easing(motion::ease_out),
                            |el, t| el.opacity(t),
                        ),
                )
            })
            .children(self.render_compose_strip(window, cx))
            .when(self.uploads > 0, |el| {
                el.child(div().px(px(GUTTER)).pt(px(8.)).text_size(fs(12.)).text_color(p.faint).child("Sending attachment…"))
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
                        round_button("emoji", IconName::FaceSlightlySmiling, cx)
                            .text_color(if self.emoji_open == Some(emojipick::Target::Composer) { p.text } else { p.muted })
                            .on_click(cx.listener(|this, _, window, cx| this.toggle_emoji_panel(window, cx))),
                    )
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
                            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                                // The field is already focused, so only a press tells that the keyboard is wanted back.
                                if this.emoji_open == Some(emojipick::Target::Composer) {
                                    this.close_emoji(cx);
                                    crate::show_keyboard(Keyboard::Text);
                                }
                            }))
                            .text_size(fs(16.))
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
                            .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.muted)))
                            .active(|el| el.bg(p.muted))
                            .on_click(cx.listener(|this, _, window, cx| this.send_message(window, cx)))
                            .child(Icon::new(IconName::ArrowUp).size_5()),
                    ),
            )
            .children(self.render_emoji_panel(window, cx))
    }

    /// Rows that rise from the bottom over a dimmed screen: the app's menus. Touch has no
    /// right-click, and a popup's rows are too small for a finger.
    /// `title` says what the rows act on; `shown` is how far it has risen, 0 to 1. While it falls
    /// (`live` false) it takes no input, so taps reach the screen under it.
    fn sheet(&self, title: String, rows: Vec<Stateful<Div>>, shown: f32, live: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let (_, bottom) = crate::insets();
        // Its own height, from the parts below: top padding, title, rows, bottom padding.
        let height = 8. + 36. + SHEET_ROW_H * rows.len() as f32 + bottom + 8.;
        div()
            .id("sheet")
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .justify_end()
            .items_center()
            .bg(gpui_kit::black().opacity(0.6 * shown))
            .when(live, |el| {
                el.occlude().on_click(cx.listener(|this, _, _, cx| {
                    this.chat_sheet = None;
                    cx.notify();
                }))
            })
            .child(
                v_flex()
                    .id("sheet-rows")
                    .relative()
                    .top(px(height * (1. - shown)))
                    .w_full()
                    .max_w(px(SHEET_W))
                    .pt(px(8.))
                    .pb(px(bottom + 8.))
                    .rounded_t(px(RADIUS))
                    .bg(p.surface)
                    // A tap between the rows is not a tap outside the sheet.
                    .when(live, |el| el.on_click(|_, _, cx| cx.stop_propagation()))
                    .child(div().h(px(36.)).px(px(GUTTER + 4.)).flex().items_center().truncate().text_size(fs(14.)).text_color(p.faint).child(title))
                    .children(rows),
            )
            .into_any_element()
    }

    /// A row that is not a message: a day, a line about the chat, the mark of what is new.
    fn render_line(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = &self.msgs[ix];
        let (text, ink) = match &m.kind {
            Kind::Day => (day_label(m.ts), self.on_wallpaper(p.faint, cx)),
            Kind::Unread => ("New messages".to_owned(), p.accent_ink),
            Kind::Notice => (m.body.clone(), self.on_wallpaper(p.faint, cx)),
            // Joins that follow one another are told once, by the last of them.
            Kind::Joined if self.msgs.get(ix + 1).is_none_or(|next| next.kind != Kind::Joined) => {
                let mut names: Vec<&str> =
                    self.msgs[..=ix].iter().rev().take(MAX_JOINED).take_while(|m| m.kind == Kind::Joined).map(|m| m.sender.as_str()).collect();
                names.reverse();
                (format!("{} joined", people(&names).0), self.on_wallpaper(p.faint, cx))
            }
            _ => return div().into_any_element(),
        };
        if m.kind == Kind::Unread {
            let rule = || div().h(px(1.)).flex_1().bg(p.rule);
            return h_flex().w_full().px(px(GUTTER)).pt(px(GAP)).gap(px(8.)).items_center().text_size(fs(12.)).text_color(ink).child(rule()).child(text).child(rule()).into_any_element();
        }
        div().w_full().px(px(GUTTER)).pt(px(GAP)).flex().justify_center().text_size(fs(12.)).text_color(ink).child(text).into_any_element()
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
                Delivery::Sending => "Sending…",
                Delivery::Delivered => "Delivered",
                Delivery::Sent | Delivery::Failed(_) => "Sent",
            }
            .to_owned(),
            false,
        ))
    }

    /// Scrolls to a message and tints it for a moment; one not loaded yet is looked for among
    /// older pages.
    fn reveal(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(ix) = self.msgs.iter().position(|m| m.id == id) {
            self.seek = None;
            self.timeline.scroll_to_reveal_item(ix);
            self.flash = Some((id.to_owned(), None));
            cx.notify();
        } else if self.more {
            self.seek = Some((id.to_owned(), 0));
            self.load_older(cx);
        }
    }

    /// The emoji the user just gave this message and when its badge was first drawn, once it is.
    fn reaction_since(&mut self, ix: usize) -> Option<(String, Instant)> {
        let (id, key, since) = self.fresh_reaction.as_mut()?;
        let m = &self.msgs[ix];
        if *id != m.id || !m.reactions.iter().any(|r| r.own && menu::same_emoji(&r.key, key)) {
            return None;
        }
        Some((key.clone(), *since.get_or_insert_with(Instant::now)))
    }

    /// How strongly a message a quote led to is tinted, 1 to 0; `None` when it is not.
    fn flash_level(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> Option<f32> {
        let (hold, fade) = (motion::FLASH_HOLD, motion::FLASH_FADE);
        let Some((id, since)) = &mut self.flash else { return None };
        if *id != self.msgs[ix].id {
            return None;
        }
        let since = *since.get_or_insert_with(|| {
            if cx.reduce_motion() {
                // Nothing else asks for a frame when the tint should go.
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(hold + fade).await;
                    this.update(cx, |_, cx| cx.notify()).ok();
                })
                .detach();
            }
            Instant::now()
        });
        let elapsed = since.elapsed();
        if elapsed >= hold + fade {
            self.flash = None;
            return None;
        }
        if cx.reduce_motion() {
            return Some(1.);
        }
        window.request_animation_frame();
        Some(if elapsed < hold { 1. } else { 1. - motion::ease_out((elapsed - hold).as_secs_f32() / fade.as_secs_f32()) })
    }

    /// Follows a pan to the right over a message, as in Signal and iMessage; one that travels far
    /// enough starts a reply.
    fn swipe(&mut self, id: &str, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(20.));
        let before = self.swipe.as_ref().map(|(_, far)| *far);
        let held = self.swipe.as_ref().map(|(on, _)| on.clone());
        let reply = swipe_step(&mut self.swipe, id, f32::from(delta.x), f32::from(delta.y), event.touch_phase);
        if let (Some(held), None) = (held, &self.swipe) {
            self.swipe_back = Some((held, Instant::now()));
        }
        #[cfg(target_os = "android")]
        if before.is_some_and(|far| far < SWIPE_REPLY) && self.swipe.as_ref().is_some_and(|(_, far)| *far >= SWIPE_REPLY) {
            motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Light);
        }
        if let Some(target) = reply {
            self.start_compose(Compose::Reply(target), window, cx);
        } else if before != self.swipe.as_ref().map(|(_, far)| *far) {
            cx.notify();
        }
    }

    /// Actions for the open chat. Rises with Vaul's sheet curve and falls quicker; the scrim
    /// follows the same progress, so the two never drift apart when reversed halfway.
    fn render_chat_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let live = self.chat_sheet.is_some();
        if live {
            self.sheet_last = self.chat_sheet.clone();
        } else if self.sheet_last.is_none() {
            return None;
        }
        let motion = if live {
            Transition::new(motion::SHEET_IN).easing(motion::EASE_SHEET)
        } else {
            Transition::new(motion::SHEET_OUT).easing(motion::EASE_OUT)
        };
        let shown = Presence::new("chat-sheet", live).transition(motion).sample(window, cx);
        if !shown.should_render() {
            self.sheet_last = None;
            return None;
        }
        let p = Palette::of(cx);
        let room = self.sheet_last.as_ref().and_then(|id| self.rooms.iter().find(|r| &r.id == id))?;
        let (id, title) = (room.id.clone(), room.name.clone());
        let mut rows: Vec<_> = chat_actions(room)
            .into_iter()
            .map(|(label, icon, action)| {
                let id = id.clone();
                let row = sheet_row(label, icon, label, p.text, cx);
                if !live {
                    return row;
                }
                row.on_click(cx.listener(move |this, _, _, cx| {
                    this.send(Cmd::Chat(id.clone(), action), cx);
                    this.chat_sheet = None;
                    cx.notify();
                }))
            })
            .collect();
        rows.push(self.leave_row(id, live, cx));
        Some(self.sheet(title, rows, shown.progress, live, cx))
    }

    /// What the composer is replying to or editing, with a way out. It opens and closes over the
    /// composer rather than pushing the chat at once; Escape takes it away at once.
    fn render_compose_strip(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let p = Palette::of(cx);
        let target = match &self.compose {
            Compose::New => None,
            Compose::Reply(id) | Compose::Edit(id) => Some(id),
        };
        let current = target.map(|id| {
            let found = self.msgs.iter().find(|m| &m.id == id);
            let quote = found.map_or_else(|| Quote::unknown(id.clone(), QuoteState::Missing), Msg::quote);
            let (icon, title) = match (&self.compose, found) {
                (Compose::Edit(_), _) => (Some(IconName::Pencil), "Editing".into()),
                (_, Some(m)) if m.own => (None, "Replying to yourself".into()),
                (_, Some(m)) => (None, format!("Replying to {}", m.sender).into()),
                _ => (None, "Replying".into()),
            };
            (icon, title, quote)
        });
        let present = current.is_some();
        if current.is_some() {
            self.strip_last = current;
        }
        let room = self.open.as_ref().map(ToString::to_string).unwrap_or_default();
        let shown = Presence::new((("compose-strip", self.strip_epoch), room), present)
            .transition(if present { motion::enter() } else { motion::exit() })
            .sample(window, cx);
        if !shown.should_render() {
            self.strip_last = None;
            return None;
        }
        let (icon, title, quote) = self.strip_last.clone()?;
        let thumb = self.thumbs.get(&quote.id).cloned();
        let leaving = shown.phase == PresencePhase::Exiting;
        let v = shown.progress;
        Some(
            div().flex_none().overflow_hidden().max_h(px(STRIP_H * v)).opacity(v).child(
                h_flex()
                    .mx(px(GUTTER))
                    .mt(px(8.))
                    .p(px(8.))
                    .gap(px(8.))
                    .rounded(px(RADIUS))
                    .bg(p.surface)
                    .child(quote_inset(&quote, Some((icon, title)), p, thumb).flex_1())
                    .child(round_button("cancel-compose", IconName::X, cx).when(!leaving, |el| {
                        el.on_click(cx.listener(|this, _, window, cx| {
                            this.start_compose(Compose::New, window, cx);
                        }))
                    })),
            ),
        )
    }

    fn render_banners(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        // Fades and opens in, closes and fades out; it sits above everything, so opening its
        // height moves the screen down instead of covering it.
        let offline = Presence::new("offline", self.offline).transition(if self.offline { motion::enter() } else { motion::exit() }).sample(window, cx);
        let card = || v_flex().mx(px(GUTTER)).mt(px(8.)).p(px(GAP)).gap(px(10.)).rounded(px(RADIUS)).text_size(fs(14.)).line_height(fs(20.));
        v_flex()
            // Being retried: a state, said quietly, that goes away by itself.
            .when(offline.should_render(), |el| {
                el.child(
                    div()
                        .h(px(28. * offline.progress))
                        .overflow_hidden()
                        .opacity(offline.progress)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(fs(12.))
                        .text_color(p.faint)
                        .child("Connection lost. Reconnecting…"),
                )
            })
            .when(self.need_recovery, |el| {
                el.child(
                    card()
                        .bg(p.warning_bg)
                        .child(div().text_color(p.warning).child(
                            "Encrypted messages are locked on this device. Copy your recovery code \
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
    div().text_size(fs(22.)).line_height(fs(28.)).font_weight(FontWeight::SEMIBOLD).child(title)
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
        .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.surface)))
        .active(|el| el.bg(p.raised))
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

/// A picture's size in a bubble: its own shape, within what a bubble may take. `room` is the
/// bubble's width for content.
fn picture_size(media: &Media, room: f32) -> (f32, f32) {
    let (max_w, max_h) = if media.kind == MediaKind::Sticker { (140., 140.) } else { (room.min(260.), 320.) };
    let (w, h) = media.dims.map_or((max_w, max_w * 0.75), |(w, h)| (w.max(1) as f32, h.max(1) as f32));
    let scale = (max_w / w).min(max_h / h).min(1.);
    ((w * scale).max(80.), (h * scale).max(60.))
}

/// Rounds a bubble: `top` and `bottom` are its corners on the sender's side.
fn corners<E: Styled>(el: E, own: bool, top: f32, bottom: f32) -> E {
    if own {
        el.rounded_l(px(RADIUS)).rounded_tr(px(top)).rounded_br(px(bottom))
    } else {
        el.rounded_r(px(RADIUS)).rounded_tl(px(top)).rounded_bl(px(bottom))
    }
}

/// Whether a message is one to three emoji and nothing else. Flags (two regional indicators),
/// skin tones and sequences joined by U+200D count as one.
fn emoji_only(text: &str) -> bool {
    let (mut count, mut joined, mut half_flag) = (0, false, false);
    for c in text.chars() {
        match u32::from(c) {
            0x200D => {
                joined = true;
                continue;
            }
            0xFE0F | 0x20E3 | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F => continue,
            0x1F1E6..=0x1F1FF => {
                if !half_flag {
                    count += 1;
                }
                half_flag = !half_flag;
            }
            0x1F000..=0x1FAFF | 0x2190..=0x21FF | 0x2300..=0x23FF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x3030 | 0x303D | 0x3297 | 0x3299 | 0xA9 | 0xAE | 0x2122 => {
                if !joined {
                    count += 1;
                }
            }
            _ if c.is_whitespace() => {}
            _ => return false,
        }
        joined = false;
    }
    (1..=3).contains(&count)
}

/// An attachment drawn as its picture rather than as a file's row; a video without its size is a row.
fn drawn_as_picture(media: &Media) -> bool {
    matches!(media.kind, MediaKind::Image | MediaKind::Sticker) || (media.kind == MediaKind::Video && media.dims.is_some())
}

/// Key of the thumbnail of the message a reply quotes, in the thumbnail cache.
fn quote_key(message: &str) -> String {
    format!("{message}#quote")
}

/// The palette of what is drawn inside a message's bubble: an own one may carry a colour of the
/// user's choosing.
fn inside_of(m: &Msg, p: Palette) -> Palette {
    if m.own { p.in_own_bubble() } else { p }
}

/// A quoted message: who wrote it and what it says, behind a bar that tells it from a link's card.
/// `heading` takes the place of the sender (the compose strip's "Replying to"), with one line of
/// text under it. `inside` is the palette of what it sits in.
fn quote_inset(q: &Quote, heading: Option<(Option<IconName>, SharedString)>, inside: Palette, thumb: Option<Arc<Image>>) -> Stateful<Div> {
    let line = |size: f32, height: f32| div().text_size(fs(size)).line_height(fs(height));
    let lines = if heading.is_some() { 1 } else { 2 };
    let top = match heading {
        Some((icon, title)) => Some((icon, title)),
        None => (q.state == QuoteState::Ready).then(|| (None, if q.own { "You".into() } else { SharedString::from(q.sender.clone()) })),
    };
    let top = top.map(|(icon, title)| {
        h_flex()
            .gap(px(4.))
            .min_w_0()
            .text_color(inside.text)
            .children(icon.map(|icon| Icon::new(icon).size_3p5().flex_none()))
            .child(line(13., 18.).min_w_0().font_weight(FontWeight::SEMIBOLD).truncate().child(title))
    });
    let quiet = |text: &'static str| line(14., 19.).italic().text_color(inside.faint).child(text);
    let body = match q.state {
        QuoteState::Loading => line(14., 19.).text_color(inside.faint).child("Loading…").into_any_element(),
        QuoteState::Missing => h_flex()
            .gap(px(4.))
            .text_color(inside.faint)
            .child(Icon::new(IconName::Unlink).size_3p5().flex_none())
            .child(quiet("Message unavailable"))
            .into_any_element(),
        QuoteState::Ready if q.deleted => quiet("Message deleted").into_any_element(),
        QuoteState::Ready => {
            let (icon, text) = quote_label(q);
            h_flex()
                .items_start()
                .gap(px(4.))
                .min_w_0()
                .text_color(inside.muted)
                // Centred on the first line of text.
                .children(icon.map(|icon| Icon::new(icon).size_3p5().flex_none().mt(px(2.5))))
                .child(line(14., 19.).flex_1().min_w_0().line_clamp(lines).text_ellipsis().child(text))
                .into_any_element()
        }
    };
    let pictured = q.media.as_ref().filter(|media| matches!(media.kind, MediaKind::Image | MediaKind::Video | MediaKind::Sticker));
    let picture = pictured.map(|media| {
        let sticker = media.kind == MediaKind::Sticker;
        div()
            .relative()
            .flex_none()
            .size(px(40.))
            .m(px(7.))
            .ml(px(0.))
            .overflow_hidden()
            .when(!sticker, |el| el.rounded(px(6.)))
            .map(|el| match thumb {
                Some(image) => el.child(img(image).size_full().when(!sticker, |el| el.rounded(px(6.))).object_fit(if sticker { ObjectFit::Contain } else { ObjectFit::Cover })),
                None => el.flex().items_center().justify_center().bg(inside.text.opacity(0.07)).text_color(inside.faint).child(
                    Icon::new(match media.kind {
                        MediaKind::Video => IconName::Video,
                        MediaKind::Sticker => IconName::Sticker,
                        _ => IconName::Image,
                    })
                    .size_4(),
                ),
            })
            .when(media.kind == MediaKind::Video, |el| {
                el.child(
                    div().absolute().inset_0().flex().items_center().justify_center().child(
                        div().size(px(20.)).flex().items_center().justify_center().rounded_full().bg(gpui_kit::black().opacity(0.5)).text_color(gpui_kit::white()).child(Icon::new(IconName::Play).size_3()),
                    ),
                )
            })
    });
    h_flex()
        .id(SharedString::from(format!("{}#inset", q.id)))
        .min_w_0()
        .items_stretch()
        .rounded(px(12.))
        .bg(inside.text.opacity(0.07))
        // A neutral bar inside, where a mention's accent bar stands outside the bubble.
        .child(div().flex_none().w(px(3.)).ml(px(7.)).my(px(7.)).rounded_full().bg(inside.text.opacity(0.4)))
        .child(v_flex().flex_1().min_w_0().pl(px(8.)).pr(px(10.)).py(px(7.)).children(top).child(body))
        .children(picture)
}

/// What a quote says about a message's content: an icon for what is not text, and its caption or
/// a label.
fn quote_label(q: &Quote) -> (Option<IconName>, String) {
    let or = |label: &str| if q.text.is_empty() { label.to_owned() } else { q.text.clone() };
    if q.poll {
        return (Some(IconName::ChartBar), format!("Poll: {}", q.text));
    }
    if q.place {
        return (Some(IconName::MapPin), or("Location"));
    }
    let Some(media) = &q.media else { return (None, q.text.clone()) };
    match media.kind {
        MediaKind::Image => (Some(IconName::Image), or("Photo")),
        MediaKind::Video => (Some(IconName::Video), or("Video")),
        MediaKind::Sticker => (Some(IconName::Sticker), or("Sticker")),
        MediaKind::Voice => (Some(IconName::Mic), media.duration.map_or_else(|| "Voice message".to_owned(), |d| format!("Voice message \u{b7} {}", length(d)))),
        MediaKind::Audio => (Some(IconName::Music), or(&format!("Audio \u{b7} {}", media.name))),
        MediaKind::File => (Some(IconName::File), or(&media.name)),
    }
}

/// How far a message follows a sideways pan: all the way to the reply point, then a third of
/// the rest, so the pan meets resistance instead of a wall.
fn rubber(far: f32) -> f32 {
    if far <= SWIPE_REPLY { far } else { SWIPE_REPLY + (far - SWIPE_REPLY) * 0.3 }
}

/// Whether messages added at the end of the open chat are news to show arriving: not the first
/// load (`len_before` 0), not a page of history (more than `MAX_ARRIVALS` in one go), and not
/// what the chat catches up on just after it opened.
fn arrived(len_before: usize, appended_in_batch: usize, open_for: Duration) -> bool {
    len_before > 0 && appended_in_batch <= MAX_ARRIVALS && open_for >= Duration::from_millis(500)
}

/// Eased progress of a motion that began at `since`, asking for the next frame while it lasts;
/// `None` once it is over, or at once when the system asks for less motion. A clock kept by the
/// app rather than by the element, so a row scrolled away and back does not replay it.
fn fresh(since: Instant, length: Duration, window: &mut Window, cx: &App) -> Option<f32> {
    let t = since.elapsed().as_secs_f32() / length.as_secs_f32();
    (t < 1. && !cx.reduce_motion()).then(|| {
        window.request_animation_frame();
        motion::ease_out(t)
    })
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

/// Advances a sideways pan over a message by one event; gives the message to reply to when the
/// pan has just ended far enough. Only a pan that begins on a message counts, so a mouse wheel and
/// the glide after a fling (neither has a start) never reply.
fn swipe_step(swipe: &mut Option<(String, f32)>, id: &str, dx: f32, dy: f32, phase: TouchPhase) -> Option<String> {
    let sideways = if dx.abs() > dy.abs() { dx } else { 0. };
    match phase {
        TouchPhase::Started => *swipe = Some((id.to_owned(), sideways.max(0.))),
        TouchPhase::Moved => {
            if let Some((_, far)) = swipe {
                *far = (*far + sideways).max(0.);
            }
        }
        TouchPhase::Ended => return swipe.take().filter(|(_, far)| *far >= SWIPE_REPLY).map(|(id, _)| id),
        TouchPhase::Cancelled => *swipe = None,
    }
    None
}

/// One action in a sheet.
fn sheet_row(id: &'static str, icon: IconName, label: &'static str, ink: Hsla, cx: &App) -> Stateful<Div> {
    let p = Palette::of(cx);
    h_flex()
        .id(id)
        .h(px(SHEET_ROW_H))
        .active(|el| el.bg(p.raised))
        .px(px(GUTTER + 4.))
        .gap(px(GAP))
        .text_size(fs(16.))
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

/// Digits of one width, so times and counts do not shift as they change.
fn tabular() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
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
        .text_size(fs(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .font_features(tabular())
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
        if self.settings.lock.lock && crate::applock::is_locked() {
            crate::applock::watch(cx);
            return crate::applock::screen(cx).into_any_element();
        }
        let (top, bottom) = crate::insets();
        self.apply_draft(window, cx);
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
            .when_some(crate::emoji::fallbacks(), |mut el, emoji| {
                el.text_style().font_fallbacks = Some(emoji);
                el
            })
            .bg(p.ground)
            .text_color(p.text);
        match self.screen {
            Screen::Starting => root
                .items_center()
                .justify_center()
                .child(div().text_size(fs(14.)).text_color(p.faint).child("Connecting…"))
                .into_any_element(),
            Screen::Email | Screen::Code => root.child(self.render_login(cx)).into_any_element(),
            Screen::Matrix => root.child(self.render_matrix_login(cx)).into_any_element(),
            Screen::Main => {
                let narrow = window.viewport_size().width < px(NARROW);
                let panel = self.panel.map(|panel| self.render_panel(panel, narrow, window, cx));
                let body = if narrow {
                    // One thing at a time; the destinations stay in reach wherever they lead.
                    let deep = self.open.is_some() || self.searching || matches!(self.panel, Some(Panel::Add | Panel::Login | Panel::NewChat));
                    // Android's shared-axis move: a chat comes in from the right, the list returns
                    // from the left, each 32 px with a fade. The screen it replaces is already gone.
                    let shift = self.nav.filter(|&(_, push)| panel.is_none() && push == self.open.is_some()).and_then(|(at, push)| {
                        let t = at.elapsed().as_secs_f32() / motion::PUSH.as_secs_f32();
                        (t < 1. && !cx.reduce_motion()).then(|| (motion::ease_out(t), if push { 32. } else { -32. }))
                    });
                    if shift.is_some() {
                        window.request_animation_frame();
                    }
                    let screen = if let Some(panel) = panel {
                        panel
                    } else if self.open.is_some() {
                        self.render_chat(true, window, cx).into_any_element()
                    } else {
                        self.render_rooms(cx).into_any_element()
                    };
                    v_flex()
                        .size_full()
                        .child(div().flex_1().min_h_0().when_some(shift, |el, (v, far)| el.relative().left(px(far * (1. - v))).opacity(v)).child(screen))
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
                                self.render_chat(false, window, cx).into_any_element()
                            } else {
                                div()
                                    .size_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(fs(14.))
                                    .text_color(p.faint)
                                    .child("Choose a chat")
                                    .into_any_element()
                            }))
                            .into_any_element()
                    };
                    h_flex().size_full().child(self.render_nav(false, cx)).child(main).into_any_element()
                };
                let viewer = self.render_viewer(window, cx);
                // Sampled every frame, so a falling sheet finishes even when a message menu opens.
                let chat_sheet = self.render_chat_sheet(window, cx);
                let sheet = self.render_message_menu(window, cx).or(chat_sheet).or_else(|| self.render_emoji_sheet(window, cx));
                let gallery = self.render_gallery(window, cx);
                let heart = self.render_heart(window, cx);
                let location = self.render_location(window, cx);
                let poll_form = self.render_poll_form(window, cx);
                root.relative()
                    .child(self.render_banners(window, cx))
                    .child(div().flex_1().min_h_0().child(body))
                    .children(sheet)
                    .children(gallery)
                    .children(location)
                    .children(poll_form)
                    .children(viewer)
                    .children(heart)
                    .children(self.render_confirm(window, cx))
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use super::{Duration, MAX_ARRIVALS, SWIPE_REPLY, arrived, emoji_only, rubber, swipe_step, urls};
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
    fn a_pan_past_the_reply_point_meets_resistance() {
        assert_eq!(rubber(0.), 0.);
        assert_eq!(rubber(SWIPE_REPLY), SWIPE_REPLY);
        assert!((rubber(2. * SWIPE_REPLY) - 1.3 * SWIPE_REPLY).abs() < 1e-4);
    }

    #[test]
    fn only_a_few_new_messages_in_a_chat_open_a_moment_arrive() {
        let open = Duration::from_secs(2);
        assert!(arrived(10, 1, open));
        assert!(arrived(10, MAX_ARRIVALS, open));
        assert!(!arrived(0, 1, open), "the first load");
        assert!(!arrived(10, MAX_ARRIVALS + 1, open), "a page of history");
        assert!(!arrived(10, 1, Duration::from_millis(100)), "catching up after opening");
    }

    #[test]
    fn a_few_emoji_alone_are_drawn_large() {
        for big in ["👍", "🎉🎉🎉", "❤️", "👨‍👩‍👧", "🇵🇱", "👍🏽 🙏"] {
            assert!(emoji_only(big), "{big}");
        }
        for small in ["", "k", "ok 👍", "🎉🎉🎉🎉", "🇵🇱🇩🇪🇫🇷🇮🇹", "1"] {
            assert!(!emoji_only(small), "{small}");
        }
    }

    #[test]
    fn only_a_pan_to_the_right_that_began_on_a_message_replies() {
        let run = |events: &[(f32, f32, gpui_kit::TouchPhase)]| {
            let mut swipe = None;
            events.iter().find_map(|&(dx, dy, phase)| swipe_step(&mut swipe, "$m", dx, dy, phase))
        };
        let far = SWIPE_REPLY / 2. + 1.;
        assert_eq!(run(&[(0., 0., Started), (far, 1., Moved), (far, 0., Moved), (0., 0., Ended)]).as_deref(), Some("$m"));
        assert_eq!(run(&[(0., 0., Started), (-far, 0., Moved), (-far, 2., Moved), (0., 0., Ended)]), None, "only to the right");
        assert_eq!(run(&[(0., 0., Started), (far, 0., Moved), (0., 0., Ended)]), None, "too short");
        assert_eq!(run(&[(0., 0., Started), (2., 300., Moved), (1., 300., Moved), (0., 0., Ended)]), None, "scrolling the chat");
        assert_eq!(run(&[(far, 0., Moved), (far, 0., Moved), (0., 0., Ended)]), None, "a wheel has no start");
        assert_eq!(run(&[(0., 0., Started), (far, 0., Moved), (far, 0., Moved), (0., 0., Cancelled), (0., 0., Ended)]), None);
    }
}

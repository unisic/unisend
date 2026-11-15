use std::collections::{HashMap, HashSet, VecDeque};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Datelike as _, Local, TimeZone as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _,
    scroll::ScrollableElement as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    spinner::Spinner,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::base::{Presence, PresencePhase, Transition, spring, transition};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};
use crate::i18n::{n_, t, tn, tr};
use tokio::sync::mpsc;

pub(crate) mod appearance;
mod camera;
mod caption;
mod cards;
mod emojipick;
mod contacts;
mod forward;
mod frame;
mod disappear;
mod gallery;
mod heart;
mod location;
mod gifs;
mod mentions;
mod info;
mod menu;
mod navigate;
pub(crate) mod motion;
mod panels;
mod pinpad;
mod press;
mod slide;
mod pollnew;
pub(crate) mod receipt;
mod rich;
mod search;
mod sheet;
mod signin;
mod sharein;
mod video;
mod rooms;
mod update;
pub(crate) mod rowswipe;
mod viewer;
mod voice;
mod verify;

use std::path::PathBuf;

use menu::{MenuKind, QUICK_REACTIONS};
use appearance::fs;
use panels::Panel;
use receipt::{Mark, Receipt};

use crate::{Keyboard, Palette};
use crate::bridges::{Flow, Network, Step};
use crate::backend::{ChatAction, Cmd, Delivery, Diff, Ev, Kind, Media, MediaKind, Msg, OwnRead, Quote, QuoteState, RoomSummary};

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
/// The navigation as icons alone, on a desktop window narrower than `RAIL_BELOW`.
const RAIL_W: f32 = 60.;
/// Where the labelled navigation would leave the conversation under about 800 wide.
const RAIL_BELOW: f32 = 1400.;
/// Widest a message and a sheet grow on a wide window.
const BUBBLE_W: f32 = 560.;
/// Widest the conversation runs on a wide window; beyond it the pane keeps an even margin.
const THREAD_W: f32 = 920.;
/// Widest a link's preview grows inside a message, and the tallest its picture is shown.
const LINK_W: f32 = 300.;
const LINK_PICTURE_H: f32 = 240.;
/// On a phone, the share of the row a message may take.
const BUBBLE_SHARE: f32 = 0.8;
/// What the hover buttons beside a wide bubble take: three of `menu.rs`'s 28px buttons, their gaps and the row's.
const SIDE_ROOM: f32 = 3. * 28. + 2. * 4. + 8.;
const SHEET_W: f32 = 480.;
const SHEET_ROW_H: f32 = 52.;
/// A sheet's rows as a list at the pointer, on a desktop: room for "Move to low priority" in Polish.
const POINTER_SHEET_W: f32 = 320.;
/// How long a finger rests on a chat row before the row shows the press: Android's tap timeout.
const ROW_PRESS_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
/// Sender's picture beside a run of messages in a group.
const SENDER: f32 = 28.;
/// How far a message is swiped sideways to reply to it.
const SWIPE_REPLY: f32 = 56.;
/// A swipe this far that is still moving this fast (px/s) at release replies too.
const SWIPE_FLICK: f32 = 24.;
const SWIPE_SPEED: f32 = 500.;
/// How far a pan goes before it is known to be a swipe or a scroll; until then nothing moves.
const SWIPE_SLOP: f32 = 10.;
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
/// Link and quote pictures fading in at once, at most.
const MAX_THUMB_FADES: usize = 60;
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
    (Tab::Chats, n_!("Chats"), IconName::MessageCircle),
    (Tab::Archive, n_!("Archive"), IconName::Archive),
    (Tab::Networks, n_!("Networks"), IconName::Waypoints),
    (Tab::Settings, n_!("Settings"), IconName::Settings),
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
    /// Signed in on Android with the data on this device kept plain, from before encryption.
    unencrypted: bool,
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
    /// How far the user has read the open chat, publicly and at all.
    own_read: OwnRead,
    /// Someone else's message whose status line a tap showed.
    tapped: Option<String>,
    /// What the typing line said last, drawn while it leaves.
    typing_last: SharedString,
    /// When the others were last told that the user is writing.
    typing_sent: Option<std::time::Instant>,
    /// Row of the newest message, when it is the user's own.
    tail: Option<usize>,
    /// Inline thumbnails by event ID, oldest first in `thumb_order`.
    thumbs: HashMap<String, Arc<Image>>,
    thumb_order: VecDeque<String>,
    /// When a link's or a quote's picture landed, while it fades in; at most `MAX_THUMB_FADES`.
    thumb_at: HashMap<String, Instant>,
    /// Messages whose thumbnail was requested.
    asked: HashSet<String>,
    /// Image shown full-screen; the picture arrives after the overlay opens.
    viewer: Option<(String, Option<Arc<Image>>)>,
    /// Attachments being downloaded to open.
    fetching: HashSet<String>,
    uploads: usize,
    preview: caption::Preview,
    /// The message whose reactions and actions are showing.
    menu: Option<menu::MessageMenu>,
    /// A mouse button held on a message, on its way to being a long press.
    press: Option<menu::Press>,
    /// A horizontal pan in progress over a message, with how far it has travelled.
    swipe: Option<Swipe>,
    /// The message a finger let go of, springing back into place since then.
    swipe_back: Option<(String, Instant)>,
    /// Messages that just arrived at the end of the open chat, at most `2 * MAX_ARRIVALS`.
    arrivals: VecDeque<Arrival>,
    /// When the open chat was opened: what arrives in its first moments is history, not news.
    opened_at: Option<Instant>,
    /// A finger resting on a message's bubble, before it is a long press.
    bubble_press: Option<BubblePress>,
    /// The state last shown under the user's newest message, the one it replaced, and since when.
    status_last: Option<(String, Mark, Option<Mark>, Instant)>,
    /// The message a quote led to, tinted for a moment; the tint's clock starts when it is drawn.
    flash: Option<(String, Option<Instant>)>,
    /// A message being looked for among older ones, with how many more pages it may load.
    seek: Option<(String, u8)>,
    /// The user's reaction just given: message, emoji, and when its badge was first drawn.
    fresh_reaction: Option<(String, String, Option<Instant>)>,
    /// What the compose strip showed last, drawn while it leaves.
    strip_last: Option<(Option<IconName>, SharedString, Quote)>,
    /// Bumped when the strip must go at once (Escape), so it does not play its exit.
    strip_epoch: usize,
    /// The strip closes because its message was sent: it shrinks on the new message's clock.
    strip_sent: bool,
    /// The chat whose actions are showing.
    chat_sheet: Option<OwnedRoomId>,
    /// Where a mouse opened the chat's or the attach sheet: on a desktop it opens there as a list.
    sheet_at: Option<Point<Pixels>>,
    /// The sheet's chat and its other person's page in their network's app, as `(app, link)`.
    peer_link: Option<(OwnedRoomId, &'static str, String)>,
    /// The attach sheet's photos and videos.
    gallery: gallery::Gallery,
    location: location::Locate,
    poll_form: pollnew::Form,
    gifs: gifs::Picker,
    updates: update::Updates,
    /// The `@` list over the composer.
    mention: mentions::Mentions,
    voice: voice::Voice,
    /// Attachments waiting for the one uploading now, so a batch keeps its order.
    attach_queue: VecDeque<Cmd>,
    attaching: bool,
    /// Received contact cards.
    contacts: contacts::State,
    /// The chat whose sheet is still falling after it closed.
    sheet_last: Option<OwnedRoomId>,
    /// A phone's stacked screens: a chat or deep panel pushed over the list.
    slide: slide::Slide,
    /// Rows displaced or arriving as a new message reorders the chat list.
    fly: slide::Fly,
    rows: rowswipe::RowSwipe,
    /// A finger on the chat list, since when; reordering waits for it to lift.
    finger: Option<Instant>,
    /// The chat list scrolling to its top: when it started and where from.
    to_top: Option<(Instant, f32)>,
    /// The chat list's first rows arrived: it fades in over the skeleton.
    list_in: Option<Instant>,
    /// The first sync of this session has answered.
    synced: bool,
    chat_motion: sheet::SheetMotion,
    /// The picture still fading out after the viewer closed.
    viewer_last: Option<(String, Option<Arc<Image>>)>,
    zoom: viewer::Zoom,
    find: search::Find,
    /// The reaction under the pointer in the picker.
    picker_hover: Option<usize>,
    emoji_pick: emojipick::Picker,
    emoji_open: Option<emojipick::Target>,
    compose: Compose,
    more: bool,
    loading: bool,
    timeline: ListState,
    drafts: navigate::Drafts,
    place: navigate::Place,
    rooms_scroll: UniformListScrollHandle,
    /// Kept here so the settings return to where they were after the lock keypad replaces them.
    settings_scroll: ScrollHandle,
    /// The pages open under the settings list, deepest last: a category, then a choice in it.
    settings_pages: Vec<panels::Page>,
    page_scroll: ScrollHandle,
    /// Room avatars; `None` while loading or when the room has none. One entry per room at most.
    avatars: HashMap<OwnedRoomId, Option<Arc<Image>>>,
    /// Pictures of the people writing in the open chat; `None` while loading or when they have
    /// none. At most `MAX_FACES`, 37 kB of pixels each.
    faces: HashMap<OwnedUserId, Option<Arc<Image>>>,
    /// Rooms whose missing or undecryptable preview was already re-read this session.
    peeked: HashSet<OwnedRoomId>,
    /// What another app shared in, and the "Send to" screen for it.
    share: sharein::ShareIn,
    /// Forwarding, selection mode and messages deleted on this device.
    fwd: forward::Forward,
    /// Settings' devices and the verification sheet.
    verify: verify::Ui,
    info: info::Info,
    _subs: Vec<Subscription>,
    _events: Task<()>,
    /// Leaving chats and starting new ones.
    people: rooms::People,
    disappear: disappear::Disappear,
    /// What the open chat's network takes, once read.
    features: Option<(OwnedRoomId, Option<crate::backend::Features>)>,
    /// The app PIN's keypad, on the lock screen and in the settings.
    pad: pinpad::Pad,
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
        let cookie_paste = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Paste cookies or a cURL command")));
        let updates = update::Updates::load(&dir);
        let email = cx.new(|cx| InputState::new(window, cx).placeholder(t!("you@example.com")));
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("123456"));
        let recovery = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Recovery code")));
        let composer = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Message")));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Search chats")));
        let emoji_search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Search emoji")));
        let emoji_pick = emojipick::Picker::new(emoji_search.clone(), &dir);

        for input in [&composer, &search, &emoji_search] {
            crate::ime::follow(input, window, cx);
        }
        crate::ime::set_incognito(settings.incognito_keyboard);
        let ui = cx.weak_entity();
        crate::ime::on_picture(move |path, cx| {
            let taken = ui.update(cx, |this, cx| this.open.is_some().then(|| this.attach_paths(vec![path.clone()], cx))).ok().flatten();
            if taken.is_none() {
                crate::share::discard(&path);
            }
        });
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
                // The emoji icon focuses the composer for its caret; the panel, not the keyboard, takes the keyboard's place.
                InputEvent::Focus if matches!(keyboard, Keyboard::Text) && this.emoji_open == Some(emojipick::Target::Composer) => {}
                other => keyboard_follows(other, keyboard, cx),
            }));
        }

        let matrix = signin::Form::new(window, cx, &mut subs);
        let share = sharein::ShareIn::new(window, cx, &mut subs);
        let verify = verify::Ui::new(window, cx, &mut subs);
        let find = search::Find::new(window, cx, &mut subs);
        subs.push(cx.subscribe_in(&cookie_paste, window, |_, _, ev: &InputEvent, _, cx| keyboard_follows(ev, Keyboard::Key, cx)));
        subs.push(cx.subscribe_in(&composer, window, |this, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) && this.composer_changed(cx) {
                this.typed(cx);
                this.mention_typed(cx);
                this.voice_typed(cx);
            }
        }));
        subs.push(cx.subscribe_in(&emoji_search, window, |_, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::Change => cx.notify(),
            other => keyboard_follows(other, Keyboard::Text, cx),
        }));
        subs.push(cx.subscribe_in(&search, window, |this, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::Change => {
                this.refilter(cx);
                this.find_everywhere(cx);
            }
            other => keyboard_follows(other, Keyboard::Text, cx),
        }));

        // Ahead of the focused field, which binds Up to its caret and would never let it reach the app.
        let me = cx.weak_entity();
        subs.push(cx.intercept_keystrokes(move |e, window, cx| {
            if !gpui_kit::is_mobile() && me.update(cx, |this, cx| this.shortcut(&e.keystroke, window, cx)).unwrap_or(false) {
                cx.stop_propagation();
            }
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
                this.follow_platform_language(window, cx);
                this.open_from_notification(cx);
                this.take_share(Some(window), cx);
                this.publish_share_targets();
                this.update_returned(cx);
                this.check_update_if_due(cx);
                // Android 15 ends the service after 6 hours a day in the background; being in front resets that.
                if this.user.is_some() && this.settings.stay_connected {
                    crate::stay_connected(true);
                }
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
        let fwd = forward::Forward::new(window, cx);

        // Plain until the backend says otherwise: a sealed install has no plain drafts to find.
        let drafts = navigate::Drafts::load(&dir, crate::vault::Seal::Plain);
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
            unencrypted: false,
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
            own_read: OwnRead::default(),
            tapped: None,
            typing_last: SharedString::default(),
            typing_sent: None,
            tail: None,
            thumbs: HashMap::new(),
            thumb_at: HashMap::new(),
            thumb_order: VecDeque::new(),
            asked: HashSet::new(),
            viewer: None,
            fetching: HashSet::new(),
            uploads: 0,
            preview: caption::Preview::new(window, cx),
            menu: None,
            press: None,
            swipe: None,
            swipe_back: None,
            arrivals: VecDeque::new(),
            opened_at: None,
            flash: None,
            seek: None,
            fresh_reaction: None,
            strip_last: None,
            strip_sent: false,
            bubble_press: None,
            status_last: None,
            strip_epoch: 0,
            chat_sheet: None,
            sheet_at: None,
            peer_link: None,
            gallery: gallery::Gallery::default(),
            location: location::Locate::default(),
            poll_form: pollnew::Form::default(),
            gifs: gifs::Picker::default(),
            updates,
            mention: mentions::Mentions::default(),
            voice: voice::Voice::default(),
            attach_queue: VecDeque::new(),
            attaching: false,
            contacts: contacts::State::default(),
            sheet_last: None,
            slide: slide::Slide::default(),
            fly: slide::Fly::default(),
            rows: rowswipe::RowSwipe::default(),
            finger: None,
            to_top: None,
            list_in: None,
            synced: false,
            chat_motion: sheet::SheetMotion::default(),
            viewer_last: None,
            zoom: viewer::Zoom::default(),
            find,
            picker_hover: None,
            emoji_pick,
            emoji_open: None,
            faces: HashMap::new(),
            compose: Compose::New,
            more: false,
            loading: false,
            timeline,
            rooms_scroll: UniformListScrollHandle::new(),
            settings_scroll: ScrollHandle::new(),
            settings_pages: Vec::new(),
            page_scroll: ScrollHandle::new(),
            drafts,
            place: navigate::Place::default(),
            avatars: HashMap::new(),
            peeked: HashSet::new(),
            share,
            fwd,
            verify,
            info: info::Info::default(),
            _subs: subs,
            _events: events,
            people: Default::default(),
            disappear: disappear::Disappear::default(),
            features: None,
            pad: Default::default(),
        }
    }

    fn send(&mut self, cmd: Cmd, cx: &mut Context<Self>) -> bool {
        let ok = self.cmd.try_send(cmd).is_ok();
        if !ok {
            self.error = Some(t!("Still working on the previous request, try again").into());
        }
        cx.notify();
        ok
    }

    fn on_event(&mut self, ev: Ev, cx: &mut Context<Self>) {
        match ev {
            Ev::NeedLogin => {
                // Signed out, here or by the server: the session is gone, and the app lock with it.
                // A server sign-out skips the settings button, so the connection, notices and share targets end here too.
                crate::background(false);
                if !crate::vault::signed_in(&self.dir) {
                    if self.settings.lock.mode != crate::applock::LockMode::Off {
                        crate::applock::pin::clear(&self.dir);
                        self.set_lock(Some(crate::applock::LockMode::Off), None, None, cx);
                    }
                    // The backend removed the file; what is in memory must not write it back.
                    self.drafts = navigate::Drafts::load(&self.dir, crate::vault::Seal::Plain);
                    self.unencrypted = false;
                }
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
            Ev::Ready { user, beeper, seal } => {
                self.beeper = beeper;
                self.unencrypted = cfg!(target_os = "android") && crate::vault::layout(&self.dir) == crate::vault::Layout::Legacy;
                if seal.sealed() {
                    self.drafts = navigate::Drafts::load(&self.dir, seal);
                }
                crate::background(true);
                crate::stay_connected(self.settings.stay_connected);
                // The app may have been started by a tap on a notification.
                self.open_from_notification(cx);
                self.load_hidden(&user);
                self.user = Some(user);
                self.send(Cmd::Incognito(self.settings.incognito), cx);
                let l = &self.settings.lock;
                crate::applock::configure(l.mode, l.timeout_secs(), l.secure);
                self.screen = Screen::Main;
                self.busy = false;
                self.error = None;
                self.take_share(None, cx);
                self.check_update_if_due(cx);
            }
            Ev::NeedRecovery(need) => {
                if self.need_recovery && !need {
                    // Keys just arrived: re-read what was shown as undecryptable.
                    self.peeked.clear();
                    if let Some(id) = self.open.take() {
                        self.open_room(id, cx);
                    }
                    // The device is verified now and keeps the code: its page says so at once.
                    if self.panel == Some(Panel::Settings) {
                        self.load_devices(cx);
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
                    // A fade is over in `ENTER`; only those still running are kept.
                    self.thumb_at.retain(|_, at| at.elapsed() < motion::ENTER);
                    if (id.ends_with("#link") || id.ends_with("#quote")) && self.thumb_at.len() < MAX_THUMB_FADES {
                        self.thumb_at.insert(id.clone(), Instant::now());
                    }
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
            Ev::Viewed(id, bytes) => self.viewed(id, bytes),
            Ev::Fetched(id, path) => {
                self.fetching.remove(&id);
                if let Some(path) = path.filter(|path| !self.voice_fetched(&id, path, cx)) {
                    let media = self.msgs.iter().find(|m| m.id == id).and_then(|m| m.media.as_ref()).or_else(|| self.info_media(&id));
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
                let calm = !slide::Fly::quiet(changed.len()) && !cx.reduce_motion() && !self.rooms.is_empty() && self.list_at_top() && !self.finger_down();
                let before = calm.then(|| slide::snapshot(&self.shown, &self.rooms));
                let first = self.rooms.is_empty();
                let mut by_id: HashMap<_, _> = self.rooms.drain(..).map(|r| (r.id.clone(), r)).collect();
                by_id.extend(changed.into_iter().map(|r| (r.id.clone(), r)));
                self.rooms = by_id.into_values().collect();
                self.rooms.sort_unstable_by_key(|r| (!r.pinned, std::cmp::Reverse(r.ts)));
                self.refilter(cx);
                if first && !self.rooms.is_empty() {
                    self.list_in = Some(Instant::now());
                }
                if let Some(change) = before.and_then(|b| slide::reorder(&b, &slide::snapshot(&self.shown, &self.rooms))) {
                    self.fly.start(change, ROOM_ROW_H);
                }
                self.publish_share_targets();
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
                    {
                        // The user's message reached the server: its transaction ID became the event's.
                        if let Some(arrival) = self.arrivals.iter_mut().find(|a| a.id == old.id) {
                            arrival.id = value.id.clone();
                        }
                        if let Some((on, ..)) = self.status_last.as_mut().filter(|(on, ..)| *on == old.id) {
                            *on = value.id.clone();
                        }
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
                    // Scrolled up, the user does not see the end: nothing there moves.
                    if arrived(len_before, appended, open_for) && !self.place.away {
                        self.arrivals.extend(ids.into_iter().map(|id| Arrival { id, at: now, height: None, since: None }));
                    }
                    self.count_missed(len_before, new);
                }
                self.seek_unread();
                if let Some((id, _)) = self.seek.as_ref().filter(|(id, _)| self.msgs.iter().any(|m| &m.id == id)) {
                    let id = id.clone();
                    self.reveal(&id, cx);
                }
                self.arrivals.retain(|a| now.duration_since(a.at) < Duration::from_secs(1));
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
                    } else if more && pages > 1 {
                        self.seek = Some((id, pages - 1));
                        self.load_older(cx);
                    } else if more && pages > 0 {
                        self.error = Some(t!("That message is further back than this app looks.").into());
                    } else if !more {
                        // The last page's rows can arrive after this; they are looked through then.
                        self.seek = Some((id, 0));
                    }
                }
            }
            Ev::Typing { room, names } => {
                if self.open.as_ref() != Some(&room) {
                    return;
                }
                self.typing = names;
            }
            Ev::OwnRead { room, read } => {
                if self.open.as_ref() == Some(&room) {
                    self.own_read = read;
                }
            }
            Ev::Online(online) => self.offline = !online,
            Ev::Synced => self.synced = true,
            Ev::Exported(id, path, what) => self.exported(id, path, what, cx),
            Ev::Error(e) => {
                self.row_return(cx);
                self.error = Some(crate::backend::plain(&e).into());
                self.busy = false;
                self.loading = false;
            }
            Ev::Contact(id, cards) => self.contacts.keep(id, cards),
            Ev::People(answer) => self.on_people(answer, cx),
            Ev::Gifs { query, page, found } => self.on_gifs(query, page, found),
            Ev::GifPreview(url, bytes) => self.on_gif_preview(url, bytes),
            Ev::GifSent => self.uploads = self.uploads.saturating_sub(1),
            Ev::Members { room, list } => self.members_arrived(room, list, cx),
            Ev::Forwarded(chats) => self.forwarded(chats, cx),
            ev @ (Ev::Devices(_) | Ev::DevicesFailed(_) | Ev::Verify(_) | Ev::SignOutStep(_) | Ev::RecoveryCode(_)) => self.on_verify_event(ev, cx),
            Ev::Disappear { room, setting } => self.on_disappear(room, setting),
            Ev::Features { room, features } => {
                if self.open.as_ref() == Some(&room) {
                    self.features = Some((room, features));
                }
            }
            Ev::Info(room, details) => self.info_details(room, *details, cx),
            Ev::Update(step) => self.on_update(step),
            Ev::PeerIds(room, ids) => {
                if self.chat_sheet.as_ref() == Some(&room) {
                    let network = self.rooms.iter().find(|r| r.id == room).and_then(|r| r.network.as_deref());
                    self.peer_link = crate::backend::profile_link(network, &ids).map(|(app, url)| (room, app, url));
                    cx.notify();
                }
            }
            Ev::InfoItems { room, items, scanned, done } => self.info_items(room, items, scanned, done, cx),
            Ev::Found { room, query, hits } => self.found(room, query, hits, cx),
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
            None => self.error = Some(t!("The clipboard has no text").into()),
        }
        cx.notify();
    }

    fn list_at_top(&self) -> bool {
        self.rooms_scroll.0.borrow().base_handle.offset().y >= px(-1.)
    }

    /// A touch counts as lifted after a few seconds even if its end was never seen.
    fn finger_down(&self) -> bool {
        self.finger.is_some_and(|at| at.elapsed() < Duration::from_secs(3))
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().trim().to_lowercase();
        let passes = |r: &RoomSummary| {
            // A search looks through every section.
            if !query.is_empty() {
                return r.name.to_lowercase().contains(&query);
            }
            in_view(self.view, r) && (!self.only_unread || r.is_unread())
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
            .filter(|r| r.preview.as_deref().is_none_or(|p| !self.need_recovery && p.ends_with(crate::backend::utd())))
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
        // Enter picks from the list on a keyboard; on a phone the list is tapped and Send sends.
        if self.mention.open() && !gpui_kit::is_mobile() {
            self.pick_mention(self.mention.selected, window, cx);
            return;
        }
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
        let mentions = self.picked_mentions(cx);
        if self.send(Cmd::Send { room: room.clone(), body, reply_to, edit, mentions }, cx) {
            self.strip_sent = matches!(self.compose, Compose::Reply(_));
            self.compose = Compose::New;
            self.jump_to_newest(cx);
            // `set_value` emits no change, so the draft would outlive the message it became.
            self.composer.update(cx, |c, cx| c.set_value("", window, cx));
            self.drafts.set(&room, "");
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
        let narrow = frame::viewport(window).width < px(NARROW);
        if self.verify_back(cx) {
            return true;
        }
        if self.share.open {
            self.close_share(cx);
        } else if self.viewer.is_some() {
            // Android's back is a gesture and animates; a desktop's Escape is a key and does not.
            self.dismiss_viewer(gpui_kit::is_mobile(), cx);
        } else if self.close_confirm(cx) {
        } else if self.preview.open() {
            self.close_preview(true, window, cx);
        } else if self.menu_open() {
            self.close_menu(cx);
        } else if self.chat_sheet.take().is_some() || std::mem::take(&mut self.gallery.menu) {
            cx.notify();
        } else if self.location.open {
            self.close_location(cx);
        } else if self.close_poll_form(cx) || self.close_forward_sheet(cx) {
        } else if self.gallery.open {
            self.close_gallery(cx);
        } else if self.gifs_open() {
            self.close_gifs(cx);
        } else if self.close_tone_menu(cx) {
        } else if self.emoji_open.is_some() {
            // The emoji panel and the mention list are part of the chat, under every sheet.
            self.close_emoji(cx);
        } else if self.mention.open() {
            self.mention.close();
            cx.notify();
        } else if self.selection_back(cx) {
        } else if self.info_open() {
            self.close_info(cx);
        } else if self.compose != Compose::New {
            self.strip_epoch += 1;
            self.start_compose(Compose::New, window, cx);
        } else if self.panel.is_some() {
            self.slide.jump = !cfg!(target_os = "android");
            // Beside the categories the one shown is no level of its own: Escape leaves Settings as from its list.
            let leave = !narrow && self.panel == Some(Panel::Settings) && self.settings_pages.len() == 1;
            self.back(cx);
            if leave && self.panel == Some(Panel::Settings) && self.settings_pages.is_empty() {
                self.panel = None;
            }
        } else if self.find.open {
            self.close_find(window, cx);
        } else if self.searching {
            self.set_searching(false, window, cx);
        } else if narrow && self.open.is_some() {
            if cfg!(target_os = "android") {
                self.pop_room(cx);
            } else {
                self.slide.jump = true;
                self.close_room(cx);
                cx.notify();
            }
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
        self.drop_near(cx);
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
    fn want_face(&mut self, from: OwnedUserId) {
        if self.faces.contains_key(&from) || self.faces.len() >= MAX_FACES || self.cmd.capacity() < self.cmd.max_capacity() / 2 {
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
                self.open_zoom(cx);
            }
        } else if !self.fetching.contains(&id) && self.send(Cmd::Fetch(id.clone(), media), cx) {
            self.fetching.insert(id);
        }
    }

    fn attach(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        if gallery::enabled() {
            self.open_gallery(cx);
        } else {
            (self.gallery.menu, self.sheet_at) = (true, Some(at));
            cx.notify();
        }
    }

    /// The system's picker, for documents and for anything on desktop.
    fn attach_file(&mut self, cx: &mut Context<Self>) {
        let picked = crate::pick_file(cx);
        cx.spawn(async move |this, cx| {
            let Some(path) = picked.await else { return };
            let _ = this.update(cx, |this, cx| this.attach_paths(vec![path], cx));
        })
        .detach();
    }

    /// Files for the open chat, from the picker, a drop or a paste.
    fn attach_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone().filter(|_| !paths.is_empty()) else { return };
        let reply_to = match std::mem::replace(&mut self.compose, Compose::New) {
            Compose::Reply(id) => Some(id),
            _ => None,
        };
        self.uploads += paths.len();
        self.queue_attach(room, paths, reply_to, cx);
    }

    /// Ctrl+V of a picture or of copied files: they go out as attachments. Text is the field's.
    fn paste_files(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else { return false };
        let (mut paths, mut images) = (Vec::new(), Vec::new());
        for entry in item.into_entries() {
            match entry {
                ClipboardEntry::ExternalPaths(copied) => paths.extend(copied.paths().iter().filter(|p| p.is_file()).cloned()),
                ClipboardEntry::Image(image) => images.push(image),
                ClipboardEntry::String(_) => {}
            }
        }
        if paths.is_empty() && images.is_empty() {
            return false;
        }
        let dir = self.dir.join(crate::backend::PASTED);
        cx.spawn(async move |this, cx| {
            let saved = cx.background_spawn(async move { save_pasted(&dir, images) }).await;
            let _ = this.update(cx, |this, cx| this.attach_paths(paths.into_iter().chain(saved).collect(), cx));
        })
        .detach();
        true
    }

    /// `room` is the width a bubble has for its content.
    /// `radius` is a picture's corner, concentric with the bubble around it.
    fn render_media(&self, m: &Msg, media: &Media, room: f32, radius: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
            // Fades in when it loads; one already loaded is there at once. Left out while the
            // viewer draws this picture over its place, which stays as the slot it lands in.
            let arrived = transition((SharedString::from(m.id.clone()), "in"), if thumb.is_some() { 1. } else { 0. }, Transition::new(motion::THUMB_IN).easing(motion::EASE_OUT), window, cx);
            let alpha = if self.zoom.covers(&m.id) { 0. } else { arrived };
            // Where the viewer flies this picture from and back to: its bounds as painted each frame.
            let probe = matches!(media.kind, MediaKind::Image | MediaKind::Sticker).then(|| {
                let (weak, id) = (cx.entity().downgrade(), m.id.clone());
                let radius = if media.kind == MediaKind::Sticker { 0. } else { radius };
                canvas(
                    |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                    move |bounds, hitbox, window, cx| {
                        // The viewer is laid out in the app's area, not the window.
                        let home = Bounds { origin: frame::local(window, bounds.origin), size: bounds.size };
                        let _ = weak.update(cx, |this, _| this.zoom.seen(&id, home, radius));
                        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                            if phase.bubble() && e.button == MouseButton::Left && hitbox.is_hovered(window) {
                                let _ = weak.update(cx, |this, _| this.zoom.pressed(&id, home, radius));
                            }
                        });
                    },
                )
                .absolute()
                .inset_0()
            });
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
                .children(probe)
                .map(|el| match thumb {
                    Some(image) if media.kind == MediaKind::Sticker => el.child(img(image).size_full().object_fit(ObjectFit::Contain).opacity(alpha)),
                    // The ring keeps a dark picture's edge from melting into the ground.
                    Some(image) => el.child(img(image).size_full().rounded(px(radius)).object_fit(ObjectFit::Cover).opacity(alpha)).child(
                        div().absolute().inset_0().rounded(px(radius)).border_1().border_color(Palette::of(cx).text.opacity(0.1)).opacity(alpha),
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
        if matches!(media.kind, MediaKind::Voice | MediaKind::Audio) {
            return self.render_voice(m, media, cx);
        }
        let icon = match media.kind {
            MediaKind::Voice => IconName::Mic,
            MediaKind::Audio => IconName::Music,
            MediaKind::Video => IconName::Video,
            _ => IconName::File,
        };
        let detail = if self.fetching.contains(&m.id) {
            t!("Downloading…").to_owned()
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
                        MediaKind::Voice => t!("Voice message").to_owned(),
                        _ => media.name.clone(),
                    }))
                    .when(!detail.is_empty(), |el| el.child(div().text_size(fs(12.)).font_features(tabular()).opacity(0.7).child(detail))),
            )
            .into_any_element()
    }

    /// What the open chat's network takes; everything until its bridge has said otherwise.
    fn caps(&self) -> &crate::backend::Features {
        self.published().unwrap_or(&crate::backend::Features::ALL)
    }

    /// The open chat's features as its bridge publishes them; `None` without a bridge or until read.
    fn published(&self) -> Option<&crate::backend::Features> {
        self.features.as_ref().filter(|(room, _)| self.open.as_ref() == Some(room)).and_then(|(_, f)| f.as_ref())
    }

    /// The open chat's network, by its bridge's name.
    fn open_network(&self) -> Option<&str> {
        self.open.as_ref().and_then(|id| self.rooms.iter().find(|r| &r.id == id)).and_then(|r| r.network.as_deref())
    }

    /// Adds the reaction, or removes it when it is already ours.
    fn toggle_reaction(&mut self, target: &str, key: &str, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone() else { return };
        self.close_menu(cx);
        // The SDK takes a reaction back only by its exact key; the user's own may lack or carry U+FE0F.
        let own = self.msgs.iter().find(|m| m.id == target).and_then(|m| m.reactions.iter().find(|r| r.own && menu::same_emoji(&r.key, key)));
        let adding = own.is_none();
        if adding && !self.caps().reacts_with(key) {
            self.error = Some(t!("This chat does not take that reaction").into());
            cx.notify();
            return;
        }
        let key = own.map_or(key, |r| r.key.as_str()).to_owned();
        if adding {
            self.fresh_reaction = Some((target.to_owned(), key.clone(), None));
        }
        self.send(Cmd::React { room, target: target.to_owned(), key }, cx);
    }

    /// Opens the chat a tapped notification was for.
    pub(crate) fn open_from_notification(&mut self, cx: &mut Context<Self>) {
        if self.screen != Screen::Main {
            return;
        }
        let Some(chat) = crate::pending_chat() else { return };
        match OwnedRoomId::try_from(chat) {
            Ok(id) => {
                self.panel = None;
                self.chat_sheet = None;
                self.slide.jump = true;
                self.open_room(id, cx);
                cx.notify();
            }
            Err(e) => log::warn!("notification chat: {e}"),
        }
    }

    fn open_room(&mut self, id: OwnedRoomId, cx: &mut Context<Self>) {
        self.slide.leaving = None;
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
        self.slide.leaving = None;
        self.emoji_open = None;
        self.close_gallery(cx);
        self.close_gifs(cx);
        self.mention = mentions::Mentions::default();
        self.stop_voice(cx);
        self.end_info();
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
        self.thumb_at.clear();
        for (_, image) in self.thumbs.drain() {
            image.remove_asset(cx);
        }
        self.close_viewer(cx);
        self.fetching.clear();
        (self.swipe, self.swipe_back, self.flash, self.seek, self.fresh_reaction, self.strip_last) = (None, None, None, None, None, None);
        (self.bubble_press, self.status_last, self.tapped) = (None, None, None);
        self.own_read = OwnRead::default();
        self.arrivals.clear();
        for (_, image) in self.faces.drain() {
            if let Some(image) = image {
                image.remove_asset(cx);
            }
        }
        (self.menu, self.press, self.chat_sheet) = (None, None, None);
        self.find.close();
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
            (t!("Check your email"), t!("Enter the code Beeper sent you."), &self.code, t!("Sign in"))
        } else {
            (t!("Sign in to Beeper"), t!("We will email you a sign-in code."), &self.email, t!("Send code"))
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
                    .child(div().text_size(fs(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.faint).child("Unisend"))
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
                        el.child(Button::new("login-back").ghost().label(t!("Use a different email")).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.screen = Screen::Email;
                                this.error = None;
                                cx.notify();
                            }),
                        ))
                    })
                    .when(!on_code, |el| {
                        el.child(Button::new("login-matrix").ghost().label(t!("Use another Matrix server")).on_click(
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
            if tab == Tab::Settings && !self.settings_pages.is_empty() {
                self.settings_pages.clear();
                cx.notify();
            }
            if tab == Tab::Chats && !self.searching {
                self.to_top = Some((Instant::now(), f32::from(self.rooms_scroll.0.borrow().base_handle.offset().y)));
                cx.notify();
            }
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

    /// The keys a desktop chat app answers: Ctrl+K searches, Ctrl+N starts a chat, Alt+Up and
    /// Alt+Down step through the list, Up in an empty composer edits the last message.
    fn shortcut(&mut self, k: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let m = &k.modifiers;
        let locked = self.settings.lock.mode != crate::applock::LockMode::Off && crate::applock::is_locked();
        if self.screen != Screen::Main || locked {
            return false;
        }
        let only_secondary = m.secondary() && !m.alt && !m.shift;
        let only_alt = m.alt && !m.control && !m.platform && !m.shift;
        let plain = !(m.control || m.alt || m.platform || m.shift || m.function);
        match k.key.as_str() {
            "k" if only_secondary => self.set_searching(true, window, cx),
            "n" if only_secondary => self.open_new_chat(window, cx),
            "up" | "down" if only_alt => self.step_chat(k.key == "down", cx),
            // In the composer, or with no text field focused at all (after a click into the chat).
            "v" if only_secondary
                && self.open.is_some()
                && (self.composer.read(cx).focus_handle(cx).is_focused(window) || !window.context_stack().iter().any(|c| c.contains("Input"))) =>
            {
                return self.paste_files(cx);
            }
            "up" if plain && self.composer_idle(window, cx) => match self.last_editable() {
                Some(id) => self.start_compose(Compose::Edit(id), window, cx),
                None => return false,
            },
            _ => return false,
        }
        true
    }

    /// A chat is open and its composer has the keyboard, empty, writing nothing but a new message.
    fn composer_idle(&self, window: &Window, cx: &App) -> bool {
        let composer = self.composer.read(cx);
        self.open.is_some() && matches!(self.compose, Compose::New) && composer.value().is_empty() && composer.focus_handle(cx).is_focused(window)
    }

    /// Opens the chat under (`down`) or above the open one in the list, or the first when none is open.
    fn step_chat(&mut self, down: bool, cx: &mut Context<Self>) {
        let at = self.open.as_ref().and_then(|open| self.shown.iter().position(|&ix| &self.rooms[ix].id == open));
        let next = match at {
            Some(ix) if down => ix + 1,
            Some(ix) => ix.wrapping_sub(1),
            None => 0,
        };
        if let Some(&ix) = self.shown.get(next) {
            self.open_room(self.rooms[ix].id.clone(), cx);
        }
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
    /// A desktop window too narrow for the labelled navigation beside a usable conversation.
    fn rail(window: &Window) -> bool {
        !gpui_kit::is_mobile() && frame::viewport(window).width < px(RAIL_BELOW)
    }

    fn nav_w(window: &Window) -> f32 {
        if Self::rail(window) { RAIL_W } else { NAV_W }
    }

    fn render_nav(&mut self, narrow: bool, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let current = self.tab();
        let rail = !narrow && Self::rail(window);
        let items = TABS.map(|(tab, label, icon)| {
            let ink = if tab == current { p.text } else { p.faint };
            let press = press::area(label, Duration::ZERO, p.surface, window, cx).when(!narrow, |el| el.rounded(px(RADIUS)));
            div()
                .id(label)
                .relative()
                .flex()
                .items_center()
                .text_color(ink)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| this.set_tab(tab, window, cx)))
                .child(press)
                .map(|el| {
                    if narrow {
                        el.flex_1().h_full().flex_col().justify_center().gap(px(4.)).text_size(fs(12.))
                    } else {
                        el.h(px(TAP))
                            .gap(px(GAP))
                            .rounded(px(RADIUS))
                            .map(|el| if rail { el.justify_center() } else { el.px(px(GAP)) })
                            .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.surface)).active(|el| el.bg(p.raised)))
                    }
                })
                .child(Icon::new(icon).size_5())
                .map(|el| if rail { el.aria_label(tr(label)).tooltip(move |window, cx| Tooltip::new(tr(label)).build(window, cx)) } else { el.child(tr(label)) })
        });
        if narrow {
            h_flex().flex_none().h(px(NAV_H)).border_t_1().border_color(p.rule).children(items)
        } else {
            let name = h_flex().h(px(BAR_H)).text_size(fs(16.)).font_weight(FontWeight::SEMIBOLD);
            let name = if rail { name } else { name.px(px(GAP)).child("Unisend") };
            v_flex()
                .flex_none()
                .w(px(Self::nav_w(window)))
                .h_full()
                .px(px(8.))
                .gap(px(2.))
                .child(frame::drag(name, window, cx))
                .children(items)
        }
    }

    /// Watches fingers on the chat list: reordering waits for them, and tapping the Chats tab
    /// scrolls over a list the user has taken hold of only until they do.
    fn list_touch(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let weak = cx.entity().downgrade();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let down = weak.clone();
                window.on_mouse_event(move |e: &TouchDragEvent, phase, _, cx| {
                    if phase.bubble() && e.phase == TouchPhase::Started {
                        let _ = down.update(cx, |this, _| this.finger = Some(Instant::now()));
                    }
                });
                let up = weak.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase.bubble() {
                        let _ = up.update(cx, |this, _| this.finger = None);
                    }
                });
                let scroll = weak.clone();
                window.on_mouse_event(move |_: &ScrollWheelEvent, phase, _, cx| {
                    if phase.bubble() {
                        let _ = scroll.update(cx, |this, _| this.to_top = None);
                    }
                });
            },
        )
        .absolute()
        .size_0()
    }

    /// `banners` go over the list, where a wide desktop window keeps them to the list's column.
    fn render_rooms(&mut self, banners: Option<AnyElement>, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        self.row_settle(cx);
        let list = uniform_list(
            "rooms",
            self.shown.len() + self.message_rows(),
            cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                let chats = this.shown.len();
                this.fetch_rows(range.start.min(chats)..range.end.min(chats));
                range
                    .map(|ix| {
                        if ix >= chats {
                            return this.message_row(ix - chats, cx);
                        }
                        let id = this.rooms[this.shown[ix]].id.clone();
                        let (dy, alpha) = this.fly.of(&id, window);
                        let key: ElementId = SharedString::from(id.to_string()).into();
                        // Lit while open, or while its sheet is; it fades back once it is neither.
                        let lit = this.open.as_ref() == Some(&id) || this.chat_sheet.as_ref() == Some(&id);
                        let lit_fade = if lit { Transition::new(Duration::ZERO) } else { Transition::new(motion::ROW_FADE).easing(motion::EASE_OUT) };
                        let lit_alpha = transition(ElementId::NamedChild(Arc::new(key.clone()), "lit".into()), if lit { 1.0f32 } else { 0. }, lit_fade, window, cx);
                        let press = press::area(key.clone(), ROW_PRESS_DELAY, p.raised, window, cx);
                        let (sx, armed) = this.row_offset(&id, window);
                        let backdrop = (sx != 0.).then(|| this.row_backdrop(&this.rooms[this.shown[ix]], sx, armed, window, cx)).flatten();
                        let room = &this.rooms[this.shown[ix]];
                        let selected = this.open.as_ref() == Some(&id);
                        let unread = room.is_unread();
                        let mark = |icon: IconName| Icon::new(icon).size_3p5().flex_none().text_color(p.faint);
                        // Nothing readable to preview: an undecrypted message says no more than "Message".
                        let preview = room.preview.as_deref().filter(|p| !p.ends_with(crate::backend::utd()));
                        let meta = match (room.network.as_deref().map(short_network), short_time(room.ts)) {
                            (Some(network), time) if time.is_empty() => network.to_owned(),
                            (Some(network), time) => format!("{network} · {time}"),
                            (None, time) => time,
                        };
                        let pan_id = id.clone();
                        let row = h_flex()
                            .id("row")
                            .w_full()
                            .h(px(ROOM_ROW_H))
                            .px(px(GUTTER))
                            .gap(px(GAP))
                            .relative()
                            .left(px(sx))
                            .when(backdrop.is_some(), |el| el.bg(p.ground))
                            .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(if selected { p.raised } else { p.surface })).active(|el| el.bg(p.raised)))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.open_room(id.clone(), cx)))
                            .child(div().absolute().inset_0().bg(p.raised).opacity(lit_alpha))
                            .child(press)
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
                                                            .child(div().flex_none().text_color(p.faint).child(t!("Draft:")))
                                                            .child(div().flex_1().min_w_0().truncate().child(crate::emoji::styled(draft.replace('\n', " "), Vec::new())))
                                                            .into_any_element(),
                                                        None => crate::emoji::styled(preview.unwrap_or(t!("Message")).to_owned(), Vec::new()).into_any_element(),
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
                            .child(press::long_press(cx.listener({
                                let id = room.id.clone();
                                move |this, e: &LongPressEvent, _, cx| {
                                    #[cfg(target_os = "android")]
                                    motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Heavy);
                                    // A held mouse button: the list opens where it is, as for the right button.
                                    this.open_chat_sheet(id.clone(), Some(e.start_position), cx);
                                }
                            })))
                            // The right button of a mouse.
                            .on_aux_click(cx.listener({
                                let id = room.id.clone();
                                move |this, event: &ClickEvent, _, cx| {
                                    if event.is_secondary() {
                                        this.open_chat_sheet(id.clone(), Some(event.position()), cx);
                                    }
                                }
                            }));
                        // Clipped only while swiped: a row settling after a reorder travels past its slot.
                        div()
                            .id(key)
                            .relative()
                            .w_full()
                            .h(px(ROOM_ROW_H))
                            .top(px(dy))
                            .when(alpha < 1., |el| el.opacity(alpha))
                            // A pan's events land where it began, which a swiped row may have left: the slot listens.
                            .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, window, cx| this.row_pan(&pan_id, event, window, cx)))
                            .when_some(backdrop, |el, backdrop| el.overflow_hidden().child(backdrop))
                            .child(row)
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.rooms_scroll)
        .size_full();
        if let Some((at, from)) = self.to_top {
            let t = at.elapsed().as_secs_f32() / motion::SCROLL_TOP.as_secs_f32();
            let y = if cx.reduce_motion() || t >= 1. { 0. } else { from * (1. - motion::ease_out(t)) };
            if y == 0. {
                self.to_top = None;
            } else {
                window.request_animation_frame();
            }
            let handle = self.rooms_scroll.0.borrow().base_handle.clone();
            handle.set_offset(point(handle.offset().x, px(y)));
        }
        let fade = self.list_in.map(|at| at.elapsed().as_secs_f32() / motion::ENTER.as_secs_f32());
        if fade.is_some_and(|t| t >= 1.) {
            self.list_in = None;
        } else if fade.is_some() {
            window.request_animation_frame();
        }
        let list = div()
            .size_full()
            .when_some(fade.filter(|t| *t < 1.), |el, t| el.opacity(motion::ease_out(t)))
            .child(list)
            .when(!gpui_kit::is_mobile(), |el| el.vertical_scrollbar(&self.rooms_scroll));

        let archive = self.view == View::Archive;
        let has_query = !self.search.read(cx).value().trim().is_empty();
        let state = list_state(self.synced, self.rooms.is_empty(), has_query);
        let empty = if has_query {
            t!("No chat or message matches")
        } else if self.only_unread {
            t!("Nothing unread")
        } else {
            match self.view {
                View::Inbox => t!("No chats here yet"),
                View::Archive => t!("Nothing archived"),
                View::LowPriority => t!("Nothing in low priority"),
            }
        };
        // Alone on a narrow window, the list's top row ends under the window buttons.
        let end = if frame::viewport(window).width < px(NARROW) { frame::reserve(window) } else { 0. };
        let top = if self.searching {
            h_flex()
                .flex_1()
                .h(px(TAP))
                .pl(px(GAP))
                .pr(px(4.))
                .rounded_full()
                .bg(p.surface)
                .child(Icon::new(IconName::Search).size_4().flex_none().text_color(p.faint))
                .child(div().flex_1().min_w_0().child(Input::new(&self.search).appearance(false)).child(crate::ime::marks(&self.search)))
                .child(round_button("close-search", IconName::X, cx).on_click(cx.listener(|this, _, window, cx| {
                    this.set_searching(false, window, cx);
                })))
        } else {
            h_flex()
                .flex_1()
                .child(frame::drag(
                    div().flex_1().when(!gpui_kit::is_mobile(), |el| el.h_full().flex().items_center()).child(screen_title(if archive { t!("Archive") } else { t!("Chats") })),
                    window,
                    cx,
                ))
                .child(round_button("new-chat", IconName::Plus, cx).on_click(cx.listener(|this, _, window, cx| this.open_new_chat(window, cx))))
                .child(round_button("search", IconName::Search, cx).on_click(cx.listener(|this, _, window, cx| {
                    this.set_searching(true, window, cx);
                })))
        };
        let waiting = self.rooms.iter().filter(|r| !r.archived && !r.low_priority && r.is_unread()).count();
        let seg = press::Segments::new("filters", window, cx);
        let chips = [
            (t!("All"), View::Inbox, false, 0),
            (t!("Unread"), View::Inbox, true, waiting),
            (t!("Low priority"), View::LowPriority, false, 0),
        ];
        let chosen = chips.iter().position(|&(_, view, only_unread, _)| self.view == view && self.only_unread == only_unread);
        let slider = chosen.and_then(|ix| seg.pill("filters-pill", ix, p.raised, window, cx));
        let measured = slider.is_some();
        let filters = chips.into_iter().enumerate().map(|(ix, (label, view, only_unread, count))| {
            let on = chosen == Some(ix);
            let press = (!on).then(|| press::area(label, Duration::ZERO, p.surface, window, cx).rounded_full());
            // The pill is slimmer than what a finger needs; the target around it is not.
            h_flex().id(label).flex_none().h(px(TAP)).cursor_pointer().on_click(cx.listener(move |this, _, _, cx| this.set_filter(view, only_unread, cx))).child(
                h_flex()
                    .id("pill")
                    .relative()
                    .h(px(36.))
                    .px(px(if gpui_kit::is_mobile() { GAP } else { 10. }))
                    .gap(px(6.))
                    .rounded_full()
                    .text_size(fs(14.))
                    .text_color(if on { p.text } else { p.muted })
                    .when(on && !measured, |el| el.bg(p.raised))
                    .child(seg.probe(ix))
                    .children(press)
                    .child(label)
                    .when(count > 0, |el| el.child(div().text_color(p.faint).font_features(tabular()).child(count.to_string()))),
            )
        });
        // A mouse has no finger to drag the row along: a larger text size or a longer language scrolls it under the wheel.
        let filters = h_flex().id("filter-row").flex_none().relative().px(px(GUTTER)).pb(px(4.)).gap(px(4.)).when(!gpui_kit::is_mobile(), |el| el.overflow_x_scroll()).child(seg.container()).children(slider).children(filters);
        v_flex()
            .size_full()
            .child(h_flex().flex_none().h(px(BAR_H)).pl(px(GUTTER)).pr(px(if self.searching { GUTTER } else { GUTTER - 10. } + end)).child(top))
            .when(!archive && !self.searching, |el| {
                el.child(filters)
            })
            .children(banners)
            .child(self.list_touch(cx))
            .child(div().flex_1().min_h_0().child(if state == RoomsView::Skeleton {
                skeleton(p).into_any_element()
            } else if state == RoomsView::Empty {
                let note = v_flex().size_full().items_center().justify_center().gap(px(4.)).px(px(GUTTER));
                let note = note.child(div().text_size(fs(16.)).text_color(p.muted).child(t!("No chats yet")));
                note.child(div().text_size(fs(14.)).text_color(p.faint).child(t!("Start one with +."))).into_any_element()
            } else if self.shown.is_empty() && self.message_rows() == 0 {
                div().px(px(GUTTER)).py(px(GAP)).text_size(fs(14.)).text_color(p.faint).child(empty).into_any_element()
            } else {
                list.into_any_element()
            }))
    }

    /// What the window buttons take from the right end of the chat's top row: all they need,
    /// unless the chat's info stands to its right.
    fn chat_end(&self, window: &Window) -> f32 {
        let narrow = frame::viewport(window).width < px(NARROW);
        if !narrow && self.info_open() && Self::info_beside(window) { 0. } else { frame::reserve(window) }
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
        self.tail = self.msgs.iter().rposition(|m| m.kind == Kind::Message && !self.is_hidden(&m.id)).filter(|&ix| self.msgs[ix].own);
        // A wide pane keeps the conversation to a column a glance can cross, as desktop chat apps do.
        let column = if narrow { 0. } else { ((f32::from(frame::viewport(window).width) - Self::nav_w(window) - LIST_W - self.info_aside(window) - THREAD_W) / 2.).max(0.) };
        let timeline = list(
            self.timeline.clone(),
            cx.processor(move |this, ix: usize, window, cx| {
                if this.is_hidden(&this.msgs[ix].id) {
                    return div().into_any_element();
                }
                this.want_thumb(ix);
                this.want_card(ix);
                let flash = this.flash_level(ix, window, cx);
                let (link_fade, quote_fade) = if this.thumb_at.is_empty() {
                    (None, None)
                } else {
                    (this.thumb_level(&link_key(&this.msgs[ix].id), window, cx), this.thumb_level(&quote_key(&this.msgs[ix].id), window, cx))
                };
                let pressed = this.press_level(&this.msgs[ix].id.clone(), window, cx);
                let reaction_since = this.reaction_since(ix);
                let m = &this.msgs[ix];
                if m.kind != Kind::Message {
                    return this.render_line(ix, cx);
                }
                // What a bubble has inside: its share of the row, less its padding and a mention's bar.
                let row = f32::from(frame::viewport(window).width) - 2. * GUTTER - this.info_aside(window);
                // Wide, the pane beside the lists also holds a face and the hover buttons.
                let room = if narrow { row * BUBBLE_SHARE } else { BUBBLE_W.min(row - Self::nav_w(window) - LIST_W - SENDER - 8. - SIDE_ROOM).max(TAP * 4.) } - 2. * GAP - 9.;
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
                let attachment = m.media.as_ref().filter(|_| !m.deleted).map(|media| this.render_media(m, media, room, RADIUS - pad_x, window, cx));
                // Rows that draw nothing do not come between two messages.
                let drawn = |row: &&Msg| !matches!(row.kind, Kind::Hidden | Kind::Unread | Kind::Status { .. }) && !this.is_hidden(&row.id);
                let prev = this.msgs[..ix].iter().rev().find(drawn);
                let next = this.msgs[ix + 1..].iter().find(drawn);
                // A run: one person's messages with no long pause, and no other row, between them.
                let joined = |a: &Msg, b: &Msg| {
                    b.kind == a.kind && a.from == b.from && b.ts.saturating_sub(a.ts) < RUN_PAUSE_MS && this.place.unread.as_ref() != Some(&b.id)
                };
                let first = prev.is_none_or(|p| !joined(p, m));
                let last = next.is_none_or(|n| !joined(m, n));
                let faded = m.deleted || m.body == crate::backend::utd();
                // Where a run's bubbles meet, their corners on the sender's side are tighter; a picture
                // keeps its round corners.
                let joins = !pictured;
                let top_r = if first || !joins { RADIUS } else { JOINED };
                let bottom_r = if last || !joins { RADIUS } else { JOINED };
                // A pointer's menu marks its message; the touch overlay lifts it instead.
                let picked = this.menu.as_ref().is_some_and(|menu| menu.id == m.id && menu.kind != MenuKind::Touch);
                let quote = m.reply.as_ref().map(|quote| {
                    let thumb = this.thumbs.get(&quote_key(&m.id)).cloned().map(|image| (image, quote_fade.unwrap_or(1.)));
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
                let style = this.settings.receipts;
                let asked = match this.settings.incoming {
                    receipt::IncomingStatus::Always => last,
                    receipt::IncomingStatus::OnTap => this.tapped.as_ref() == Some(&m.id),
                };
                let said = if m.own { this.delivery_line(ix, dm) } else if asked && !m.deleted { this.heard(ix) } else { None };
                let state = said.and_then(|r| Some((receipt::mark(&r, style)?, r)));
                // Still only here, not yet with the server.
                let leaving = m.delivery == Delivery::Sending && !m.id.starts_with('$');
                let (id, held) = (m.id.clone(), m.id.clone());
                // The finger's pan, followed 1:1 with resistance past the reply point; once it has let
                // go, a spring brings the message back.
                let dragged = this.swipe.as_ref().filter(|s| s.id == m.id && s.lock == Some(true)).map(|s| rubber(s.far));
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
                // The next message joining this one's run takes its time line; the line shrinks as it arrives.
                let shrink = next.filter(|n| !last && state.is_none() && joined(m, n)).and_then(|n| this.arrival_level(&n.id, cx));
                // What is drawn inside the bubble: an own one may carry a colour of the user's choosing.
                let inside = inside_of(m, p);
                let ink = HighlightStyle { color: Some(inside.accent_ink), font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
                let underline = HighlightStyle { underline: Some(UnderlineStyle { thickness: px(1.), ..Default::default() }), ..Default::default() };
                // The accent is for what addresses the user; a mention of someone else is only bold.
                let bold = HighlightStyle { font_weight: Some(FontWeight::SEMIBOLD), ..Default::default() };
                let mut marks: Vec<_> = m.mentions.iter().map(|r| (r.clone(), if m.mentioned && m.to_me.contains(r) { ink } else { bold })).collect();
                let apart = |u: &std::ops::Range<usize>| m.mentions.iter().all(|r| r.end <= u.start || u.end <= r.start);
                marks.extend(urls(&m.body).into_iter().filter(apart).map(|r| (r, underline)));
                marks.sort_by_key(|(r, _)| r.start);
                // The preview its sender attached to a link; a touch opens the link.
                let link_card = m.link.as_ref().filter(|_| !m.deleted).map(|link| {
                    let link_shown = link_fade.unwrap_or(1.);
                    let url = link.url.clone();
                    let width = room.min(LINK_W);
                    let picture = link.image.as_ref().map(|image| {
                        let (w, h) = image.dims.unwrap_or((1200, 630));
                        // The picture's own shape; one taller than the limit is cut to it, since a
                        // picture drawn to cover its box is not clipped by it.
                        let height = (width * h.max(1) as f32 / w.max(1) as f32).clamp(60., LINK_PICTURE_H);
                        div().w_full().h(px(height)).overflow_hidden().map(|el| match this.thumbs.get(&link_key(&m.id)) {
                            Some(thumb) => el.child(img(thumb.clone()).size_full().rounded_t(px(12.)).object_fit(ObjectFit::Cover).opacity(link_shown)),
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
                let poll_card = m.poll.as_ref().filter(|_| !m.deleted).map(|poll| cards::poll(m, poll, this.user.as_ref(), room, inside, pressed.and_then(|(part, level)| Some((part?, level))), window, cx));
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
                    // A finger resting on it, before the long press lifts it.
                    .when_some(pressed.filter(|(part, _)| part.is_none()), |el, (_, level)| {
                        el.relative().child(corners(div().absolute().inset_0(), m.own, top_r, bottom_r).bg(inside.text.opacity(0.08 * level)))
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
                    .when(m.edited && !m.deleted, |el| el.child(div().px(px(text_x)).text_size(fs(12.)).text_color(inside.faint).child(t!("Edited"))));
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
                    this.want_face(m.from.clone());
                }
                let arriving = this.arriving(ix, window, cx);
                let state_level = state.as_ref().filter(|_| this.tail == Some(ix)).and_then(|(mark, _)| this.status_level(ix, mark, window, cx));
                if let Some((Mark::Faces(readers, _), _)) = &state {
                    for (user, _) in readers.clone() {
                        this.want_face(user);
                    }
                }
                let m = &this.msgs[ix];
                let side = if faces { px(SENDER + 8.) } else { px(0.) };
                let row = v_flex()
                    .group("message")
                    .w_full()
                    .relative()
                    // The user's own message is already known; another's fades in as it rises.
                    .when_some(arriving.filter(|_| !m.own), |el, (v, _)| el.opacity(v))
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
                            .when(last || state.is_some() || shrink.is_some(), |el| {
                                let failed = state.as_ref().is_some_and(|(_, r)| matches!(r, Receipt::Failed(_)));
                                // Words follow the time after a dot, icons after a space.
                                let worded = state.as_ref().is_some_and(|(mark, _)| matches!(mark, Mark::Words(_) | Mark::Failed { .. }));
                                // A new state fades in where the old one was; the time stays.
                                let time = match &state {
                                    Some(_) if m.own && !last => None,
                                    Some(_) if worded => Some(format!("{} \u{b7} ", clock(m.ts))),
                                    _ => Some(clock(m.ts)),
                                };
                                let said = state.as_ref().map(|(mark, r)| this.receipt_mark(r, mark, state_level.as_ref().map(|(v, before)| (before.as_ref(), *v)), cx));
                                let ink = if failed { p.negative } else { this.on_wallpaper(p.faint, cx) };
                                el.child(
                                    h_flex()
                                        .gap(px(3.))
                                        // A little over its padding and one line at GPUI's default 1.618 leading, so it starts unclipped.
                                        .when_some(shrink, |el, v| el.max_h((px(4.) + fs(20.)) * (1. - v)).overflow_hidden().opacity(1. - v))
                                        .px(px(GAP))
                                        .pt(px(4.))
                                        .text_size(fs(12.))
                                        .font_features(tabular())
                                        .text_color(ink)
                                        .when(this.disappears(ix), |el| el.child(Icon::new(IconName::Timer).size_3().text_color(ink)))
                                        .child(h_flex().children(time).children(said)),
                                )
                            }),
                    )
                    .children(this.selection_mark(m, cx));
                let row = if this.place.unread.as_ref() == Some(&m.id) { v_flex().w_full().child(Self::unread_line(cx)).child(row) } else { row };
                // A list lays its items out across its full width, ignoring side padding, so each row keeps the column itself.
                let row = div().w_full().px(px(column)).child(row);
                match arriving {
                    // It grows from nothing over the composer, so the messages above move up with it
                    // instead of jumping; the row hangs from the top and rises as the space opens.
                    Some((v, Some(height))) => {
                        let probe = this.arrival_probe(m.id.clone(), cx);
                        div().w_full().overflow_hidden().h(px(height * v)).child(row.flex_none().child(probe)).into_any_element()
                    }
                    _ => row.into_any_element(),
                }
            }),
        )
        .size_full();
        // The send button turns between a microphone (nothing typed) and an arrow; desktop has no microphone.
        let mic = self.mic_shown(cx);
        let send_mode = transition(("send", "mode"), if mic { 0. } else { 1. }, motion::exit(), window, cx);
        let own_frame = frame::reserve(window) > 0.;
        let end = self.chat_end(window);

        let drop_tint = p.surface;
        v_flex()
            .size_full()
            .relative()
            .when(self.open.is_some(), |el| {
                el.drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_tint))
                    .on_drop(cx.listener(|this, dropped: &ExternalPaths, _, cx| this.attach_paths(dropped.paths().iter().filter(|p| p.is_file()).cloned().collect(), cx)))
            })
            .children(self.find_bar(cx).map(|el| el.pr(px(GUTTER + end))))
            .child(
                header()
                    .when(self.find.open, |el| el.hidden())
                    .when(end > 0., |el| el.pr(px(GUTTER - 10. + end)))
                    .when(narrow, |el| {
                        el.child(round_button("back", IconName::ArrowLeft, cx).on_click(cx.listener(|this, _, _, cx| this.pop_room(cx))))
                    })
                    .when(!narrow, |el| el.pl(px(GUTTER)))
                    .child(
                        h_flex()
                            .id("chat-title")
                            // Under the app's own window frame the empty rest of the row moves the window.
                            .map(|el| if own_frame { el.flex_initial() } else { el.flex_1() })
                            .min_w_0()
                            .h_full()
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_info(cx)))
                            .children(header_avatar)
                            .child(
                                v_flex()
                                    .map(|el| if own_frame { el.flex_initial() } else { el.flex_1() })
                                    .min_w_0()
                                    .pl(px(8.))
                                    .child(div().text_size(fs(16.)).line_height(fs(20.)).font_weight(FontWeight::SEMIBOLD).truncate().child(name))
                                    .children(self.header_subline(network, cx)),
                            ),
                    )
                    .when(own_frame, |el| el.child(frame::drag(div().flex_1().h_full(), window, cx)))
                    .when(room.is_some(), |el| el.child(self.find_button(cx)))
                    .when(room.is_some(), |el| {
                        el.child(round_button("chat-menu", IconName::Ellipsis, cx).on_click(cx.listener(|this, e: &ClickEvent, _, cx| {
                            if let Some(id) = this.open.clone() {
                                this.open_chat_sheet(id, Some(e.position()), cx);
                            }
                        })))
                    }),
            )
            .children(self.selection_bar(window, cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .children(self.wallpaper_layer(0., cx))
                    .child(timeline)
                    .when(!gpui_kit::is_mobile(), |el| el.vertical_scrollbar(&self.timeline))
                    .child(self.press_tracker(cx))
                    .child(self.bubble_release(cx))
                    .children(self.jump_button(window, cx))
                    .children(self.date_pill(window, cx))
                    .children(self.find_results(cx))
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
                                .child(t!("Loading messages…"))
                                // Said only when the wait is long enough to notice.
                                .with_animations(
                                    "loading",
                                    vec![Animation::new(Duration::from_millis(300)), Animation::new(motion::ENTER).with_easing(motion::ease_out)],
                                    |el, ix, t| el.opacity(if ix == 0 { 0. } else { t }),
                                ),
                        )
                    }),
            )
            .child(
                v_flex()
                    .px(px(column))
                    .children(self.render_typing(window, cx))
                    .children(self.render_compose_strip(window, cx))
                    .children(self.render_mentions(cx))
                    .when(self.uploads > 0, |el| {
                        el.child(div().px(px(GUTTER)).pt(px(8.)).text_size(fs(12.)).text_color(p.faint).child(t!("Sending attachment…")))
                    })
                    .child(
                        h_flex()
                            .items_end()
                            .gap(px(6.))
                            // The paperclip's own padding stands in for the rest of the left margin.
                            .pl(px(GUTTER - 10.))
                            .pr(px(GUTTER))
                            .py(px(8.))
                            .child(round_button("attach", IconName::Paperclip, cx).text_color(p.muted).on_click(cx.listener(|this, e: &ClickEvent, _, cx| this.attach(e.position(), cx))))
                            .child(
                                round_button("emoji", IconName::FaceSlightlySmiling, cx)
                                    .text_color(if self.emoji_open == Some(emojipick::Target::Composer) { p.text } else { p.muted })
                                    .on_click(cx.listener(|this, e: &ClickEvent, window, cx| {
                                        this.emoji_pick.at = e.position();
                                        this.toggle_emoji_panel(window, cx);
                                    })),
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
                                    .map(|el| self.mention_keys(el, cx))
                                    .child(Input::new(&self.composer).appearance(false).large().token(|t, _, cx| mentions::token(t, cx)))
                                    .child(crate::ime::marks(&self.composer)),
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
                                    .relative()
                                    .children([(IconName::ArrowUp, send_mode), (IconName::Mic, 1. - send_mode)].into_iter().filter(|(_, v)| *v > 0.).map(|(icon, v)| {
                                        let scale = 0.5 + 0.5 * v;
                                        div()
                                            .absolute()
                                            .inset_0()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .opacity(v)
                                            .child(Icon::new(icon).size_5().transform(Transformation::scale(size(scale, scale))))
                                    }))
                                    .when(mic && send_mode < 0.5, |el| el.child(self.mic_touch(cx))),
                            )
                            .relative()
                            .children(self.render_recorder(window, cx)),
                    ),
            )
            .children(self.render_emoji_panel(window, cx))
    }

    /// Rows that rise from the bottom over a dimmed screen: the app's menus. Touch has no
    /// right-click, and a popup's rows are too small for a finger.
    /// `title` says what the rows act on; a different `key` is a different set of rows, which the
    /// sheet grows or shrinks to. While it falls it takes no input, so taps reach the screen under
    /// it. The title can be dragged down; `motion` finds the sheet's motion again in the listener.
    #[expect(clippy::too_many_arguments, reason = "one call site per sheet, each with its own motion")]
    fn sheet(
        &self,
        state: &sheet::SheetMotion,
        frame: sheet::SheetFrame,
        key: usize,
        title: String,
        rows: Vec<Stateful<Div>>,
        motion: fn(&mut Self) -> &mut sheet::SheetMotion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = Palette::of(cx);
        let (_, bottom) = crate::insets();
        let live = frame.live;
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.chat_sheet = None;
            this.gallery.menu = false;
            cx.notify();
        };
        if let Some(at) = self.sheet_at.filter(|_| !gpui_kit::is_mobile()) {
            return self.pointer_sheet(at, key, title, rows, live, window, cx);
        }
        let head = div()
            .id("sheet-title")
            .h(px(44.))
            .pt(px(8.))
            .px(px(GUTTER + 4.))
            .flex()
            .items_center()
            .truncate()
            .text_size(fs(14.))
            .text_color(p.faint)
            .when(live, |el| {
                el.on_scroll_wheel(cx.listener(move |this, e: &ScrollWheelEvent, _, cx| {
                    if motion(this).drag(e) {
                        close(this, cx);
                    }
                    cx.notify();
                }))
            })
            .child(title);
        let view = cx.entity_id();
        let body = state.fit("sheet-rows", key, v_flex().child(head).children(rows), view, window, cx);
        sheet::scrim("sheet", frame)
            .when(live, |el| el.on_click(cx.listener(move |this, _, _, cx| close(this, cx))))
            .child(
                v_flex()
                    .id("sheet-rows")
                    .relative()
                    .top(px(frame.y))
                    .w_full()
                    .max_w(px(SHEET_W))
                    .pb(px(bottom + 8.))
                    .rounded_t(px(RADIUS))
                    .bg(p.surface)
                    // A tap between the rows is not a tap outside the sheet.
                    .when(live, |el| el.on_click(|_, _, cx| cx.stop_propagation()))
                    .child(state.probe(view))
                    .child(body),
            )
            .into_any_element()
    }

    /// A sheet a mouse opened on a desktop: a list where the pointer is, like a message's menu.
    /// A click or right click outside closes it, and nothing is dimmed.
    #[expect(clippy::too_many_arguments, reason = "the sheet's own arguments, handed on")]
    fn pointer_sheet(&self, at: Point<Pixels>, key: usize, title: String, rows: Vec<Stateful<Div>>, live: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !live {
            return div().into_any_element();
        }
        let p = Palette::of(cx);
        let view = frame::viewport(window);
        let (top, bottom) = crate::insets();
        let (head_h, row_h) = (32., menu::POINTER_ROW_H);
        let h = head_h + row_h * rows.len() as f32 + 12.;
        let w = f32::from(fs(POINTER_SHEET_W));
        let at = frame::local(window, at);
        let (ax, ay, vw, vh) = (f32::from(at.x), f32::from(at.y), f32::from(view.width), f32::from(view.height));
        let x = if ax + w <= vw - GUTTER { ax } else { ax - w };
        let y = if ay + h <= vh - bottom - GUTTER { ay } else { ay - h };
        let x = x.clamp(GUTTER, (vw - w - GUTTER).max(GUTTER));
        let y = y.clamp(top + GUTTER, (vh - bottom - h - GUTTER).max(top + GUTTER));
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            (this.chat_sheet, this.gallery.menu) = (None, false);
            cx.notify();
        };
        let head = div().h(px(head_h)).px(px(GUTTER)).flex().items_center().truncate().text_size(fs(13.)).text_color(p.faint).child(title);
        let rows = rows.into_iter().map(|row| row.h(px(row_h)).px(px(GUTTER)).gap(px(GUTTER)));
        let list = div()
            .absolute()
            .left(px(x))
            .top(px(y))
            .child(v_flex().id("sheet-list").w(px(w)).pb(px(6.)).rounded(px(14.)).bg(p.lifted).occlude().child(head).children(rows))
            .with_animation(SharedString::from(format!("sheet-list-{key}-{ax}-{ay}")), Animation::new(motion::ENTER).with_easing(motion::ease_out), move |el, t| {
                el.top(px(y + 4. * (1. - t))).opacity(t)
            });
        div()
            .id("sheet-list-barrier")
            .absolute()
            .inset_0()
            .occlude()
            .on_click(cx.listener(move |this, _, _, cx| close(this, cx)))
            .on_aux_click(cx.listener(move |this, _, _, cx| close(this, cx)))
            .child(list)
            .into_any_element()
    }
    fn unread_line(cx: &App) -> Div {
        let p = Palette::of(cx);
        let rule = || div().h(px(1.)).flex_1().bg(p.rule);
        h_flex().w_full().px(px(GUTTER)).pt(px(GAP)).gap(px(8.)).items_center().text_size(fs(12.)).text_color(p.accent_ink).child(rule()).child(t!("New messages")).child(rule())
    }

    /// A row that is not a message: a day, a line about the chat. The SDK's read marker draws
    /// nothing; `unread_line` stands where it was when the chat opened.
    fn render_line(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let m = &self.msgs[ix];
        let (text, ink) = match &m.kind {
            Kind::Day => (day_label(m.ts), self.on_wallpaper(p.faint, cx)),
            Kind::Notice => (m.body.clone(), self.on_wallpaper(p.faint, cx)),
            // Joins that follow one another are told once, by the last of them.
            Kind::Joined if self.msgs.get(ix + 1).is_none_or(|next| next.kind != Kind::Joined) => {
                let mut names: Vec<&str> =
                    self.msgs[..=ix].iter().rev().take(MAX_JOINED).take_while(|m| m.kind == Kind::Joined).map(|m| m.sender.as_str()).collect();
                names.reverse();
                (tn!(names.len(), "{who} joined", "{who} joined", who = people(&names).0), self.on_wallpaper(p.faint, cx))
            }
            _ => return div().into_any_element(),
        };
        div().w_full().px(px(GUTTER)).pt(px(GAP)).flex().justify_center().text_size(fs(12.)).text_color(ink).child(text).into_any_element()
    }

    /// What became of the user's message at `ix`. Said of the newest message of the chat when it
    /// is the user's, and of any that failed.
    fn delivery_line(&self, ix: usize, dm: bool) -> Option<Receipt> {
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
        if let Delivery::Failed(why) = state {
            return Some(Receipt::Failed(why));
        }
        if self.tail != Some(ix) {
            return None;
        }
        let (readers, who) = self.readers(ix);
        if !readers.is_empty() {
            return Some(Receipt::Seen { who: who.filter(|_| !dm), readers });
        }
        let waited = (chrono::Utc::now().timestamp_millis() as u64).saturating_sub(m.ts) > BRIDGE_WAIT_MS;
        Some(match state {
            Delivery::Sending if m.id.starts_with('$') && waited => Receipt::Sent,
            Delivery::Sending => Receipt::Sending,
            Delivery::Delivered => Receipt::Delivered,
            Delivery::Sent | Delivery::Failed(_) => Receipt::Sent,
        })
    }

    /// Who has read message `ix`, by the receipts on it or under it, each with a name, and all of
    /// them named as a sentence would.
    fn readers(&self, ix: usize) -> (Vec<(OwnedUserId, String)>, Option<String>) {
        let mut readers: Vec<&OwnedUserId> = Vec::new();
        for user in self.msgs[ix..].iter().flat_map(|row| &row.seen) {
            if !readers.contains(&user) {
                readers.push(user);
            }
        }
        if readers.is_empty() {
            return (Vec::new(), None);
        }
        let name = |user: &OwnedUserId| self.msgs.iter().rev().find(|m| m.from == *user && !m.sender.is_empty()).map(|m| m.sender.as_str());
        let named = readers.iter().map(|user| ((*user).clone(), name(user).map_or_else(|| user.localpart().to_owned(), str::to_owned))).collect();
        let known: Vec<&str> = readers.iter().filter_map(|user| name(user)).collect();
        (named, Some(if known.len() == readers.len() { people(&known).0 } else { readers.len().to_string() }))
    }

    /// Someone else's message `ix` as its sender's app shows it, where the network says.
    fn heard(&self, ix: usize) -> Option<Receipt> {
        let view = receipt::their_view(self.open_network(), receipt::heard(&self.msgs, ix, &self.own_read))?;
        let me = self.user.clone()?;
        let name = self.msgs.iter().rev().find(|m| m.own && !m.sender.is_empty()).map_or_else(|| me.localpart().to_owned(), |m| m.sender.clone());
        Some(Receipt::Theirs { view, me: (me, name) })
    }

    /// Scrolls to a message and tints it for a moment; one not loaded yet is looked for among
    /// older pages.
    fn reveal(&mut self, id: &str, cx: &mut Context<Self>) {
        self.reveal_within(id, SEEK_PAGES, cx);
    }

    fn reveal_within(&mut self, id: &str, pages: u8, cx: &mut Context<Self>) {
        if let Some(ix) = self.msgs.iter().position(|m| m.id == id) {
            self.seek = None;
            self.timeline.scroll_to_reveal_item(ix);
            self.flash = Some((id.to_owned(), None));
            cx.notify();
        } else if self.more {
            self.seek = Some((id.to_owned(), pages));
            self.load_older(cx);
        }
    }

    /// The emoji the user just gave this message and when its badge was first drawn, once it is.
    /// How far message `ix` has arrived: eased progress and the height its row grows to, while it
    /// grows. Its first frame has no height yet: it is drawn at none and measured. With less motion
    /// nothing grows and only another's message fades in, over `EXIT`.
    fn arriving(&mut self, ix: usize, window: &mut Window, cx: &App) -> Option<(f32, Option<f32>)> {
        let m = &self.msgs[ix];
        let still = cx.reduce_motion();
        if still && m.own {
            return None;
        }
        let arrival = self.arrivals.iter_mut().find(|a| a.id == m.id)?;
        if !still && arrival.height.is_none() {
            // Never measured (not laid out, say): drawn as it is rather than kept at no height.
            if arrival.at.elapsed() > Duration::from_millis(500) {
                return None;
            }
            window.request_animation_frame();
            return Some((0., Some(0.)));
        }
        let since = *arrival.since.get_or_insert_with(Instant::now);
        let length = if still { motion::EXIT } else { motion::ENTER };
        let t = since.elapsed().as_secs_f32() / length.as_secs_f32();
        if t >= 1. {
            return None;
        }
        window.request_animation_frame();
        Some(if still { (t, None) } else { (motion::ease_out(t), arrival.height) })
    }

    /// How far message `id` has grown in, read without starting its clock: 0 until it has.
    fn arrival_level(&self, id: &str, cx: &App) -> Option<f32> {
        let arrival = self.arrivals.iter().find(|a| a.id == id).filter(|_| !cx.reduce_motion())?;
        let Some(since) = arrival.since else { return Some(0.) };
        let t = since.elapsed().as_secs_f32() / motion::ENTER.as_secs_f32();
        (t < 1.).then(|| motion::ease_out(t))
    }

    /// How far the newest own message's state (Sending, Sent, Seen) has faded in since it last
    /// changed, and what it replaced; the first state seen for a message is shown as it is.
    fn status_level(&mut self, ix: usize, mark: &Mark, window: &mut Window, cx: &App) -> Option<(f32, Option<Mark>)> {
        let id = &self.msgs[ix].id;
        match &mut self.status_last {
            Some((on, shown, before, at)) if on == id => {
                if shown != mark {
                    *before = Some(std::mem::replace(shown, mark.clone()));
                    *at = Instant::now();
                }
                fresh(*at, motion::EXIT, window, cx).map(|v| (v, before.clone()))
            }
            last => {
                // A long-past change: nothing to fade.
                *last = Some((id.clone(), mark.clone(), None, Instant::now().checked_sub(motion::EXIT).unwrap_or_else(Instant::now)));
                None
            }
        }
    }

    /// How far the link or quote picture under `key` has faded in since it landed; None once it has.
    fn thumb_level(&mut self, key: &str, window: &mut Window, cx: &App) -> Option<f32> {
        let at = *self.thumb_at.get(key)?;
        let level = fresh(at, motion::ENTER, window, cx);
        if level.is_none() {
            self.thumb_at.remove(key);
        }
        level
    }

    fn bubble_pressed(&mut self, id: String, cx: &mut Context<Self>) {
        if self.menu_open() || self.fwd.selecting() {
            return;
        }
        let since = Instant::now();
        self.bubble_press = Some(BubblePress { id, part: None, since, released: None });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ROW_PRESS_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if this.bubble_press.as_ref().is_some_and(|b| b.since == since && b.released.is_none()) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The finger that pressed message `id` is on its answer `n`; the bubble's own probe saw it first.
    pub(super) fn part_pressed(&mut self, id: &str, n: usize) {
        if let Some(press) = self.bubble_press.as_mut().filter(|b| b.id == id && b.released.is_none()) {
            press.part = Some(n);
        }
    }

    /// Ends a bubble's press: the finger lifted (a tap), started scrolling, or let go of a long press.
    fn bubble_release(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let weak = cx.entity().downgrade();
        let release = move |cx: &mut App| {
            let _ = weak.update(cx, |this, cx| this.bubble_released(cx));
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

    /// The press ended: a tint already shown fades, one not yet shown never comes.
    fn bubble_released(&mut self, cx: &mut Context<Self>) {
        let Some(press) = self.bubble_press.as_mut().filter(|b| b.released.is_none()) else { return };
        if press.since.elapsed() < ROW_PRESS_DELAY {
            self.bubble_press = None;
        } else {
            press.released = Some(Instant::now());
        }
        cx.notify();
    }

    /// How strongly message `id`'s bubble shows a press: at once in full, then fading over `EXIT`.
    /// Also says which of its poll's answers, if any, holds the press.
    fn press_level(&mut self, id: &str, window: &mut Window, cx: &App) -> Option<(Option<usize>, f32)> {
        let press = self.bubble_press.as_ref().filter(|b| b.id == id)?;
        let part = press.part;
        let Some(released) = press.released else { return (press.since.elapsed() >= ROW_PRESS_DELAY).then_some((part, 1.)) };
        let t = released.elapsed().as_secs_f32() / motion::EXIT.as_secs_f32();
        if t >= 1. || cx.reduce_motion() {
            self.bubble_press = None;
            return None;
        }
        window.request_animation_frame();
        Some((part, 1. - motion::ease_out(t)))
    }

    /// Measures an arriving row as it is laid out, for the next frame to grow it to.
    fn arrival_probe(&self, id: String, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let weak = cx.entity().downgrade();
        canvas(
            move |bounds, _, cx| {
                let height = f32::from(bounds.size.height);
                let _ = weak.update(cx, |this, _| {
                    if let Some(arrival) = this.arrivals.iter_mut().find(|a| a.id == id) {
                        arrival.height = Some(height);
                    }
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0()
    }

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
        if self.fwd.selecting() || self.caps().reply < 1 {
            return;
        }
        let delta = event.delta.pixel_delta(px(20.));
        let far = |swipe: &Option<Swipe>| swipe.as_ref().filter(|s| s.lock == Some(true)).map(|s| s.far);
        let before = far(&self.swipe);
        let reply = swipe_step(&mut self.swipe, id, f32::from(delta.x), f32::from(delta.y), event.touch_phase, Instant::now());
        let after = far(&self.swipe);
        // Following the finger sideways, the pan is the message's: the list must not scroll with it.
        if after.is_some() {
            cx.stop_propagation();
        }
        if let (Some(_), None) = (before, after) {
            self.swipe_back = Some((id.to_owned(), Instant::now()));
        }
        #[cfg(target_os = "android")]
        if before.is_some_and(|far| far < SWIPE_REPLY) && after.is_some_and(|far| far >= SWIPE_REPLY) {
            motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Light);
        }
        if let Some(target) = reply {
            self.start_compose(Compose::Reply(target), window, cx);
        } else if before != after {
            cx.notify();
        }
    }

    fn open_chat_sheet(&mut self, id: OwnedRoomId, at: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        let network = self.rooms.iter().find(|r| r.id == id).and_then(|r| r.network.as_deref());
        // One profile lookup at the homeserver, which already holds it, so the row shows only when it leads somewhere.
        if self.peer_link.as_ref().is_none_or(|(room, ..)| *room != id) && crate::backend::links_profiles(network) {
            self.peer_link = None;
            self.send(Cmd::PeerIds(id.clone()), cx);
        }
        self.chat_sheet = Some(id);
        self.sheet_at = at;
        self.disappear.picking = false;
        cx.notify();
    }

    /// Actions for the open chat. Rises with Vaul's sheet curve and falls quicker; the scrim
    /// follows the same progress, so the two never drift apart when reversed halfway.
    fn render_chat_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.chat_sheet.is_some() {
            self.sheet_last = self.chat_sheet.clone();
        }
        let Some(frame) = self.chat_motion.sample(self.chat_sheet.is_some(), window, cx) else {
            self.sheet_last = None;
            return None;
        };
        let live = frame.live;
        let p = Palette::of(cx);
        let room = self.sheet_last.as_ref().and_then(|id| self.rooms.iter().find(|r| &r.id == id))?;
        let (id, title) = (room.id.clone(), room.name.clone());
        if self.disappear.picking
            && let Some((title, rows)) = self.disappear_sheet(&id, live, cx)
        {
            return Some(self.sheet(&self.chat_motion, frame, 1, title, rows, |this| &mut this.chat_motion, window, cx));
        }
        let mut rows: Vec<_> = chat_actions(room)
            .into_iter()
            .map(|(label, icon, action)| {
                let id = id.clone();
                let row = sheet_row(label, icon, label, p.text, window, cx);
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
        if let Some((_, app, url)) = self.peer_link.as_ref().filter(|(room, ..)| *room == id) {
            let url = url.clone();
            let row = sheet_row("open-in-app", IconName::ExternalLink, t!("Open in {app}", app = app), p.text, window, cx);
            rows.push(if live {
                row.on_click(cx.listener(move |this, _, _, cx| {
                    crate::open_link(cx, &url);
                    this.chat_sheet = None;
                    cx.notify();
                }))
            } else {
                row
            });
        }
        rows.extend(self.disappear_entry(&id, live, cx));
        rows.push(self.leave_row(id, live, window, cx));
        Some(self.sheet(&self.chat_motion, frame, 0, title, rows, |this| &mut this.chat_motion, window, cx))
    }

    /// Who is writing, over the composer. It grows from nothing and shrinks back, so the messages
    /// above it move with it instead of jumping; a second writer joining only changes the words.
    fn render_typing(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let p = Palette::of(cx);
        let present = !self.typing.is_empty();
        if present {
            let names: Vec<&str> = self.typing.iter().map(String::as_str).collect();
            let (who, many) = people(&names);
            self.typing_last = if many { t!("{who} are typing…", who = who) } else { t!("{who} is typing…", who = who) }.into();
        }
        let room = self.open.as_ref().map(ToString::to_string).unwrap_or_default();
        let shown = Presence::new(("typing", room), present).transition(if present { motion::enter() } else { motion::exit() }).sample(window, cx);
        if !shown.should_render() {
            return None;
        }
        let (v, faint) = (shown.progress, p.faint);
        // An activity indicator may loop; at 24 frames a second it costs little while it lasts.
        let pulse = Animation::new(Duration::from_millis(1200)).repeat().with_max_fps(24.);
        let dots = h_flex().gap(px(3.)).with_animation("typing-dots", pulse, move |el, t| {
            el.children((0..3).map(|n| {
                let phase = (t - n as f32 * 0.2).rem_euclid(1.);
                let level = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
                div().size(px(4.)).rounded_full().bg(faint).opacity(0.3 + 0.7 * level)
            }))
        });
        Some(
            div().flex_none().overflow_hidden().h((px(6.) + fs(16.)) * v).opacity(v).child(
                h_flex()
                    .px(px(GUTTER + GAP))
                    .pt(px(6.))
                    .gap(px(6.))
                    .text_size(fs(12.))
                    .line_height(fs(16.))
                    .text_color(faint)
                    .child(dots)
                    .child(self.typing_last.clone()),
            ),
        )
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
                (Compose::Edit(_), _) => (Some(IconName::Pencil), t!("Editing").into()),
                (_, Some(m)) if m.own => (None, t!("Replying to yourself").into()),
                (_, Some(m)) => (None, t!("Replying to {who}", who = m.sender).into()),
                _ => (None, t!("Replying").into()),
            };
            (icon, title, quote)
        });
        let present = current.is_some();
        if current.is_some() {
            self.strip_last = current;
            self.strip_sent = false;
        }
        let room = self.open.as_ref().map(ToString::to_string).unwrap_or_default();
        // Closed by sending, it shrinks as the message grows in, on the same curve and length, so
        // the two add up to one movement.
        let shown = Presence::new((("compose-strip", self.strip_epoch), room), present)
            .transition(if present || self.strip_sent { motion::enter() } else { motion::exit() })
            .sample(window, cx);
        if !shown.should_render() {
            self.strip_last = None;
            self.strip_sent = false;
            return None;
        }
        let (icon, title, quote) = self.strip_last.clone()?;
        let thumb = self.thumbs.get(&quote.id).cloned().map(|image| (image, 1.));
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
        // The card keeps its words while it leaves; its measured height lets the space open and close with it.
        let live_error = self.error.is_some();
        let error = Presence::new("error-card", live_error).transition(if live_error { motion::enter() } else { motion::exit() }).sample(window, cx);
        let probe = window.use_keyed_state("error-card-last", cx, |_, _| ErrorCard { last: None, height: Rc::new(Cell::new(0.)) });
        if let Some(e) = &self.error {
            if probe.read(cx).last.as_ref() != Some(e) {
                probe.update(cx, |c, _| c.last = Some(e.clone()));
            }
        } else if !error.should_render() && probe.read(cx).last.is_some() {
            probe.update(cx, |c, _| c.last = None);
        }
        let (text, card_h) = probe.read_with(cx, |c, _| (c.last.clone().unwrap_or_default(), c.height.clone()));
        let update = self.render_update_notice(cx);
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
                        .child(t!("Connection lost. Reconnecting…")),
                )
            })
            .children(update)
            .when(self.need_recovery, |el| {
                el.child(
                    card()
                        .bg(p.warning_bg)
                        .child(div().text_color(p.warning).child(
                            t!("Encrypted messages are locked on this device. Copy your recovery code from another device where you are signed in (in Unisend: Settings, Devices), then paste it here."),
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
                                        .label(t!("Paste"))
                                        .on_click(cx.listener(|this, _, window, cx| this.paste_recovery(window, cx))),
                                )
                                .child(
                                    Button::new("recover")
                                        .primary()
                                        .h(px(TAP))
                                        .px(px(GUTTER))
                                        .label(t!("Unlock"))
                                        .loading(self.busy)
                                        .disabled(self.busy)
                                        .on_click(cx.listener(|this, _, window, cx| this.recover(window, cx))),
                                ),
                        ),
                )
            })
            .when(error.should_render(), |el| {
                let measured = card_h.get();
                el.child(
                    div()
                        .overflow_hidden()
                        .opacity(error.progress)
                        .when(error.progress < 1., |el| el.h(px(measured * error.progress)))
                        .child(
                            card()
                                .relative()
                                .bg(p.surface)
                                .child(canvas(|_, _, _| {}, move |bounds, _, _, _| card_h.set(f32::from(bounds.size.height))).absolute().inset_0())
                                .child(div().text_color(p.negative).child(text))
                                .child(h_flex().justify_end().child(
                                    h_flex()
                                        .id("dismiss-error")
                                        .h(px(TAP))
                                        .px(px(GUTTER + 4.))
                                        .rounded_full()
                                        .bg(p.raised)
                                        .cursor_pointer()
                                        .when(live_error, |el| {
                                            el.on_click(cx.listener(|this, _, _, cx| {
                                                this.error = None;
                                                cx.notify();
                                            }))
                                        })
                                        .child(t!("Dismiss")),
                                )),
                        ),
                )
            })
    }
}

/// The error card's words, kept while it fades out, and its height as last painted.
struct ErrorCard {
    last: Option<SharedString>,
    height: Rc<Cell<f32>>,
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
/// Whether a chat belongs to a section of the list, before the unread filter and the search.
fn in_view(view: View, r: &RoomSummary) -> bool {
    match view {
        View::Inbox => !r.archived && !r.low_priority,
        View::Archive => r.archived,
        View::LowPriority => r.low_priority && !r.archived,
    }
}

fn chat_actions(room: &RoomSummary) -> [(&'static str, IconName, ChatAction); 5] {
    [
        if room.is_unread() {
            (t!("Mark as read"), IconName::MailOpen, ChatAction::Unread(false))
        } else {
            (t!("Mark as unread"), IconName::Mail, ChatAction::Unread(true))
        },
        if room.pinned { (t!("Unpin"), IconName::PinOff, ChatAction::Pin(false)) } else { (t!("Pin"), IconName::Pin, ChatAction::Pin(true)) },
        if room.muted { (t!("Unmute"), IconName::Bell, ChatAction::Mute(false)) } else { (t!("Mute"), IconName::BellOff, ChatAction::Mute(true)) },
        if room.archived {
            (t!("Unarchive"), IconName::ArchiveRestore, ChatAction::Archive(false))
        } else {
            (t!("Archive|verb"), IconName::Archive, ChatAction::Archive(true))
        },
        if room.low_priority {
            (t!("Move to chats"), IconName::Inbox, ChatAction::LowPriority(false))
        } else {
            (t!("Move to low priority"), IconName::ChevronsDown, ChatAction::LowPriority(true))
        },
    ]
}

/// Names as a phrase, and whether it stands for more than one person: "Ann", "Ann and Bob",
/// "Ann, Bob and 3 others".
fn people(names: &[&str]) -> (String, bool) {
    match names {
        [] => (String::new(), false),
        [one] => ((*one).to_owned(), *one == t!("You")),
        [one, two] => (t!("{one} and {two}", one = one, two = two), true),
        [one, two, three] => (t!("{one}, {two} and {three}", one = one, two = two, three = three), true),
        [one, two, rest @ ..] => (tn!(rest.len(), "{one}, {two} and {n} other", "{one}, {two} and {n} others", one = one, two = two), true),
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

/// Writes pasted pictures to `dir` as files to send, named only by kind and order.
fn save_pasted(dir: &std::path::Path, images: Vec<Image>) -> Vec<PathBuf> {
    if images.is_empty() {
        return Vec::new();
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    if let Err(e) = std::fs::create_dir_all(dir) {
        log::warn!("pasted picture: {e}");
        return Vec::new();
    }
    let one = images.len() == 1;
    images
        .into_iter()
        .enumerate()
        .filter_map(|(ix, image)| {
            // The name travels with a picture sent as a file; the stamp keeps two pastes apart on disk.
            let name = if one { format!("image.{}", image.format.extension()) } else { format!("image-{}.{}", ix + 1, image.format.extension()) };
            let folder = dir.join(format!("{stamp}-{ix}"));
            let path = folder.join(name);
            std::fs::create_dir_all(&folder).and_then(|()| std::fs::write(&path, &image.bytes)).map_err(|e| log::warn!("pasted picture: {e}")).ok().map(|()| path)
        })
        .collect()
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
/// `thumb` is the quoted picture with how far it has faded in.
fn quote_inset(q: &Quote, heading: Option<(Option<IconName>, SharedString)>, inside: Palette, thumb: Option<(Arc<Image>, f32)>) -> Stateful<Div> {
    let line = |size: f32, height: f32| div().text_size(fs(size)).line_height(fs(height));
    let lines = if heading.is_some() { 1 } else { 2 };
    let top = match heading {
        Some((icon, title)) => Some((icon, title)),
        None => (q.state == QuoteState::Ready).then(|| (None, if q.own { t!("You").into() } else { SharedString::from(q.sender.clone()) })),
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
        QuoteState::Loading => line(14., 19.).text_color(inside.faint).child(t!("Loading…")).into_any_element(),
        QuoteState::Missing => h_flex()
            .gap(px(4.))
            .text_color(inside.faint)
            .child(Icon::new(IconName::Unlink).size_3p5().flex_none())
            .child(quiet(t!("Message unavailable")))
            .into_any_element(),
        QuoteState::Ready if q.deleted => quiet(t!("Message deleted")).into_any_element(),
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
                Some((image, shown)) => el
                    // The placeholder's ground stays under the picture while it fades in.
                    .when(shown < 1. && !sticker, |el| el.bg(inside.text.opacity(0.07)))
                    .child(img(image).size_full().when(!sticker, |el| el.rounded(px(6.))).object_fit(if sticker { ObjectFit::Contain } else { ObjectFit::Cover }).opacity(shown)),
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
        return (Some(IconName::ChartBar), t!("Poll: {text}", text = q.text));
    }
    if q.place {
        return (Some(IconName::MapPin), or(t!("Location")));
    }
    let Some(media) = &q.media else { return (None, q.text.clone()) };
    match media.kind {
        MediaKind::Image => (Some(IconName::Image), or(t!("Photo"))),
        MediaKind::Video => (Some(IconName::Video), or(t!("Video"))),
        MediaKind::Sticker => (Some(IconName::Sticker), or(t!("Sticker"))),
        MediaKind::Voice => (Some(IconName::Mic), media.duration.map_or_else(|| t!("Voice message").to_owned(), |d| format!("{} \u{b7} {}", t!("Voice message"), length(d)))),
        MediaKind::Audio => (Some(IconName::Music), or(&format!("{} \u{b7} {}", t!("Audio"), media.name))),
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

/// A finger on a message's bubble: when it landed, and when the press ended (it then fades).
struct BubblePress {
    id: String,
    /// A poll's answer under the finger, which then shows the press instead of the whole bubble.
    part: Option<usize>,
    since: Instant,
    released: Option<Instant>,
}

/// A message that just arrived at the end of the open chat.
struct Arrival {
    id: String,
    at: Instant,
    /// Its row's height, measured the frame after it was first drawn with none.
    height: Option<f32>,
    /// When it began to grow: the first frame its height was known.
    since: Option<Instant>,
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

/// A pan that began on a message, from its first event until the finger lifts.
struct Swipe {
    id: String,
    /// The whole pan so far, both ways: it decides the axis once past `SWIPE_SLOP`.
    moved: (f32, f32),
    /// Undecided, then whether the message follows the finger or the pan is not a swipe at all.
    lock: Option<bool>,
    /// How far right the message has been pulled.
    far: f32,
    /// The last few (time, far) pairs, for the speed at release.
    samples: VecDeque<(Instant, f32)>,
}

/// Advances a pan over a message by one event; gives the message to reply to when the pan has just
/// ended far enough, or short of that but flicked. Nothing moves until the pan is `SWIPE_SLOP` long;
/// then it follows only a pan mostly to the right, and any other is ignored until the finger lifts.
/// Only a pan that begins on a message counts, so a mouse wheel and the glide after a fling (neither
/// has a start) never reply.
fn swipe_step(swipe: &mut Option<Swipe>, id: &str, dx: f32, dy: f32, phase: TouchPhase, now: Instant) -> Option<String> {
    match phase {
        TouchPhase::Started => *swipe = Some(Swipe { id: id.to_owned(), moved: (0., 0.), lock: None, far: 0., samples: VecDeque::new() }),
        TouchPhase::Moved => {}
        TouchPhase::Ended => {
            let s = swipe.take().filter(|s| s.lock == Some(true))?;
            let mut samples = s.samples;
            samples.push_back((now, s.far));
            let flicked = s.far >= SWIPE_FLICK && gallery::speed(&samples) >= SWIPE_SPEED;
            return (s.far >= SWIPE_REPLY || flicked).then_some(s.id);
        }
        TouchPhase::Cancelled => {
            *swipe = None;
            return None;
        }
    }
    let s = swipe.as_mut()?;
    match s.lock {
        None => {
            s.moved = (s.moved.0 + dx, s.moved.1 + dy);
            let (x, y) = s.moved;
            if x.hypot(y) < SWIPE_SLOP {
                return None;
            }
            let sideways = x > 0. && x.abs() > 1.5 * y.abs();
            s.lock = Some(sideways);
            // The message catches up with the finger instead of losing the slop.
            s.far = if sideways { x } else { 0. };
        }
        Some(true) => s.far = (s.far + dx).max(0.),
        Some(false) => return None,
    }
    s.samples.push_back((now, s.far));
    if s.samples.len() > 4 {
        s.samples.pop_front();
    }
    None
}

/// What the chat list shows until its first rows arrive: still, so it costs no frames.
#[derive(Debug, PartialEq)]
enum RoomsView {
    Skeleton,
    Empty,
    Filtered,
    List,
}

/// What the chat list shows: skeleton until the first sync answers, then the account's empty state.
fn list_state(synced: bool, empty: bool, filtered: bool) -> RoomsView {
    match (empty, filtered) {
        (false, _) => RoomsView::List,
        (true, true) => RoomsView::Filtered,
        (true, false) if synced => RoomsView::Empty,
        (true, false) => RoomsView::Skeleton,
    }
}

fn skeleton(p: Palette) -> Div {
    let bar = |w: f32, h: f32| div().w(relative(w)).h(px(h)).rounded_full().bg(p.surface);
    v_flex().size_full().children((0..8).map(|_| {
        h_flex()
            .h(px(ROOM_ROW_H))
            .px(px(GUTTER))
            .gap(px(GAP))
            .child(div().size(px(AVATAR)).flex_none().rounded_full().bg(p.surface))
            .child(v_flex().flex_1().gap(px(10.)).child(bar(0.45, 14.)).child(bar(0.75, 12.)))
    }))
}

/// One action in a sheet.
fn sheet_row(id: &'static str, icon: IconName, label: impl Into<SharedString>, ink: Hsla, window: &mut Window, cx: &mut App) -> Stateful<Div> {
    let press = press::area(id, Duration::ZERO, Palette::of(cx).raised, window, cx);
    sheet_row_idle(id, icon, label, ink, cx).relative().child(press)
}

/// A sheet action drawn where there is no window to read touches in: it shows only a mouse's press.
fn sheet_row_idle(id: &'static str, icon: IconName, label: impl Into<SharedString>, ink: Hsla, cx: &App) -> Stateful<Div> {
    let p = Palette::of(cx);
    h_flex()
        .id(id)
        .h(px(SHEET_ROW_H))
        .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised)))
        .active(|el| el.bg(p.raised))
        .px(px(GUTTER + 4.))
        .gap(px(GAP))
        .text_size(fs(16.))
        .text_color(ink)
        .cursor_pointer()
        .child(Icon::new(icon).size_5())
        .child(label.into())
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
        0 => t!("Today").to_owned(),
        1 => t!("Yesterday").to_owned(),
        2..=6 => crate::i18n::weekday(&t),
        _ => crate::i18n::date(&t, t.year() != today.year()),
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
        1 => return t!("Yesterday").to_owned(),
        2..=6 => return crate::i18n::weekday_short(&t),
        _ if t.year() == today.year() => return crate::i18n::date_short(&t),
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
        let locked = self.settings.lock.mode != crate::applock::LockMode::Off && crate::applock::is_locked();
        // The chat left open behind the lock is read once the lock is passed, as an opening is.
        if crate::LOCKED.swap(locked, std::sync::atomic::Ordering::Relaxed)
            && !locked
            && let Some(id) = self.open.clone()
        {
            self.send(Cmd::Chat(id, ChatAction::Unread(false)), cx);
        }
        if locked {
            let lock = self.render_lock(window, cx);
            return frame::wrap(lock, window, cx);
        }
        let screen = self.render_screen(window, cx);
        // The main screen places the window buttons itself, under its sheets and its viewer.
        if self.screen == Screen::Main { screen } else { frame::wrap(screen, window, cx) }
    }
}

impl BeeperApp {
    fn render_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let (top, bottom) = crate::insets();
        self.apply_draft(window, cx);
        if window.focused(cx).is_none() {
            window.focus(&self.focus, cx);
        }
        let root = v_flex()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let m = &event.keystroke.modifiers;
                // Nothing binds Tab, so without this a keyboard never reaches a button or its focus ring.
                if event.keystroke.key == "tab" && !gpui_kit::is_mobile() && !(m.control || m.alt || m.platform) {
                    if m.shift { window.focus_prev(cx) } else { window.focus_next(cx) }
                    cx.stop_propagation();
                } else if this.pad_keystroke(&event.keystroke.key, cx) || (event.keystroke.key == "escape" && this.go_back(window, cx)) || this.viewer_key(&event.keystroke.key, cx) {
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
                .child(div().text_size(fs(14.)).text_color(p.faint).child(t!("Connecting…")))
                .into_any_element(),
            Screen::Email | Screen::Code => root.child(self.render_login(cx)).into_any_element(),
            Screen::Matrix => root.child(self.render_matrix_login(cx)).into_any_element(),
            Screen::Main => {
                let narrow = frame::viewport(window).width < px(NARROW);
                // A wide desktop window keeps them to the column they are about, not its full width.
                let in_column = !narrow && !gpui_kit::is_mobile();
                let mut banners = in_column.then(|| self.render_banners(window, cx).into_any_element());
                let panel = self.panel.map(|panel| self.render_panel(panel, narrow, banners.take(), window, cx));
                let body = if narrow {
                    let deep_panel = matches!(self.panel, Some(Panel::Add | Panel::Login | Panel::NewChat));
                    match self.panel {
                        Some(base @ (Panel::Settings | Panel::Networks)) => self.slide.base = Some(base),
                        None => self.slide.base = None,
                        Some(_) => {}
                    }
                    let in_chat = self.panel.is_none() && self.open.is_some();
                    if deep_panel {
                        self.slide.last_panel = self.panel;
                    } else if in_chat {
                        self.slide.last_panel = None;
                    }
                    let wanted = deep_panel || (in_chat && self.slide.leaving.is_none());
                    let has_over = deep_panel || in_chat || self.slide.last_panel.is_some();
                    self.slide.jump |= !has_over;
                    let v = self.slide.value(wanted, window, cx);
                    if !wanted && v == 0. {
                        self.slide.last_panel = None;
                    }
                    // Android's shared-axis move inside a chat: its info page comes in from the right.
                    let shift = self.info_slide(cx).filter(|_| in_chat);
                    if shift.is_some() {
                        window.request_animation_frame();
                    }
                    let (mut panel_over, mut panel_base) = (None, None);
                    if let Some(el) = panel {
                        if deep_panel {
                            panel_over = Some(el);
                        } else {
                            panel_base = Some(el);
                        }
                    }
                    let over = if !(wanted || v > 0.) {
                        None
                    } else if panel_over.is_some() {
                        panel_over
                    } else if in_chat {
                        Some(if self.info_open() { self.render_info(false, window, cx) } else { self.render_chat(true, window, cx).into_any_element() })
                    } else {
                        self.slide.last_panel.map(|last| self.render_panel(last, true, None, window, cx))
                    };
                    let g = slide::layers(v, f32::from(frame::viewport(window).width), cx.reduce_motion());
                    let under = if over.is_none() || v < 1. {
                        if deep_panel && let Some(base) = self.slide.base {
                            panel_base = Some(self.render_panel(base, true, None, window, cx));
                        }
                        let screen = panel_base.unwrap_or_else(|| self.render_rooms(None, window, cx).into_any_element());
                        Some(
                            div().absolute().inset_0().child(
                                v_flex()
                                    .size_full()
                                    .relative()
                                    .left(px(g.under_x))
                                    .child(div().flex_1().min_h_0().child(screen))
                                    .when(!self.searching, |el| el.child(self.render_nav(true, window, cx)))
                                    .when(over.is_some() && v > 0., |el| el.child(div().absolute().inset_0().occlude().bg(p.ground).opacity(g.veil))),
                            ),
                        )
                    } else {
                        None
                    };
                    let leaving = self.slide.leaving.is_some();
                    div()
                        .relative()
                        .size_full()
                        .children(under)
                        .when_some(over, |el, screen| {
                            el.child(
                                div().absolute().inset_0().child(
                                    div()
                                        .size_full()
                                        .relative()
                                        .left(px(g.over_x))
                                        .opacity(g.over_alpha)
                                        .bg(p.ground)
                                        .child(div().size_full().when_some(shift, |el, (v, far)| el.relative().left(px(far * (1. - v))).opacity(v)).child(screen))
                                        .when(leaving, |el| el.child(div().absolute().inset_0().occlude())),
                                ),
                            )
                        })
                        .into_any_element()
                } else {
                    self.slide.jump = false;
                    let beside = Self::info_beside(window);
                    let main = if let Some(panel) = panel {
                        div().flex_1().min_w_0().h_full().child(panel).into_any_element()
                    } else {
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            // The one line between the panes, so the list and the conversation read as two.
                            .child(div().w(px(LIST_W)).flex_none().h_full().border_r_1().border_color(p.rule).child(self.render_rooms(banners.take(), window, cx)))
                            .child(div().flex_1().min_w_0().h_full().child(if self.info_open() && !beside {
                                self.render_info(false, window, cx)
                            } else if self.open.is_some() {
                                self.render_chat(false, window, cx).into_any_element()
                            } else {
                                let strip = frame::drag(div().absolute().top_0().left_0().w_full().h(px(BAR_H)), window, cx);
                                v_flex()
                                    .size_full()
                                    .relative()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(10.))
                                    .text_size(fs(14.))
                                    .text_color(p.faint)
                                    .child(strip)
                                    .when(!gpui_kit::is_mobile(), |el| el.child(Icon::new(IconName::MessageCircle).size_8()))
                                    .child(t!("Choose a chat"))

                                    .into_any_element()
                            }))
                            .when(self.info_open() && beside, |el| el.child(div().w(px(info::INFO_W)).flex_none().h_full().child(self.render_info(true, window, cx))))
                            .into_any_element()
                    };
                    h_flex().size_full().child(self.render_nav(false, window, cx)).child(main).into_any_element()
                };
                let body = self.render_share(window, cx).unwrap_or(body);
                let viewer = self.render_viewer(window, cx);
                // Sampled every frame, so a falling sheet finishes even when a message menu opens.
                let chat_sheet = self.render_chat_sheet(window, cx);
                let attach_menu = self.render_attach_menu(window, cx);
                let sheet = self.render_message_menu(window, cx).or(chat_sheet).or(attach_menu);
                // Its own layer over the menu's, so it can rise while the menu fades.
                let emoji_sheet = self.render_emoji_sheet(window, cx);
                let gallery = self.render_gallery(window, cx);
                let location = self.render_location(window, cx);
                let poll_form = self.render_poll_form(window, cx);
                let gifs = self.render_gifs(window, cx);
                let preview = self.render_preview(window, cx);
                let verify = self.render_verify_sheet(window, cx);
                root.relative()
                    .when(!in_column, |el| el.child(self.render_banners(window, cx)))
                    .child(div().flex_1().min_h_0().child(body))
                    .children(frame::controls(window, cx))
                    .children(sheet)
                    .children(emoji_sheet)
                    .children(self.render_emoji_popover(window, cx))
                    .children(gallery)
                    .children(location)
                    .children(poll_form)
                    .children(gifs)
                    .children(preview)
                    .children(self.render_forward_layers(window, cx))
                    .children(viewer)
                    .children(self.render_confirm(window, cx))
                    .children(verify)
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use super::{Duration, MAX_ARRIVALS, SWIPE_REPLY, arrived, emoji_only, list_state, rubber, save_pasted, swipe_step, urls, RoomsView};
    use gpui_kit::TouchPhase::{Cancelled, Ended, Moved, Started};

    #[test]
    fn pasted_pictures_land_apart_and_plainly_named() {
        let dir = std::env::temp_dir().join(format!("unisend-pasted-{}", std::process::id()));
        let image = |format| gpui_kit::Image { format, bytes: vec![1, 2, 3], id: 0 };
        let saved = save_pasted(&dir, vec![image(gpui_kit::ImageFormat::Png), image(gpui_kit::ImageFormat::Jpeg)]);
        let names: Vec<_> = saved.iter().map(|p| p.file_name().unwrap().to_str().unwrap().to_owned()).collect();
        assert_eq!(names, ["image-1.png", "image-2.jpg"]);
        assert!(saved.iter().all(|p| std::fs::read(p).unwrap() == [1, 2, 3]));
        assert_ne!(saved[0].parent(), saved[1].parent());
        assert_eq!(save_pasted(&dir, vec![image(gpui_kit::ImageFormat::Gif)])[0].file_name().unwrap(), "image.gif");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chat_list_state_follows_sync_and_filter() {
        assert_eq!(list_state(false, true, false), RoomsView::Skeleton);
        assert_eq!(list_state(true, true, false), RoomsView::Empty);
        assert_eq!(list_state(true, true, true), RoomsView::Filtered);
        assert_eq!(list_state(false, true, true), RoomsView::Filtered);
        assert_eq!(list_state(false, false, false), RoomsView::List);
        assert_eq!(list_state(true, false, true), RoomsView::List);
    }

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

    /// Feeds `(dx, dy, phase)` events 8 ms apart, or `gap` apart, and returns the reply.
    fn pan(events: &[(f32, f32, gpui_kit::TouchPhase)], gap: Duration) -> Option<String> {
        let (t0, mut swipe) = (std::time::Instant::now(), None);
        events.iter().zip(1..).find_map(|(&(dx, dy, phase), n)| swipe_step(&mut swipe, "$m", dx, dy, phase, t0 + gap * n))
    }

    #[test]
    fn only_a_pan_to_the_right_that_began_on_a_message_replies() {
        let slow = Duration::from_millis(200);
        let far = SWIPE_REPLY / 2. + 1.;
        assert_eq!(pan(&[(0., 0., Started), (far, 1., Moved), (far, 0., Moved), (0., 0., Ended)], slow).as_deref(), Some("$m"));
        assert_eq!(pan(&[(0., 0., Started), (-far, 0., Moved), (-far, 2., Moved), (0., 0., Ended)], slow), None, "only to the right");
        assert_eq!(pan(&[(0., 0., Started), (far, 0., Moved), (0., 0., Ended)], slow), None, "too short");
        assert_eq!(pan(&[(0., 0., Started), (2., 300., Moved), (1., 300., Moved), (0., 0., Ended)], slow), None, "scrolling the chat");
        assert_eq!(pan(&[(far, 0., Moved), (far, 0., Moved), (0., 0., Ended)], slow), None, "a wheel has no start");
        assert_eq!(pan(&[(0., 0., Started), (far, 0., Moved), (far, 0., Moved), (0., 0., Cancelled), (0., 0., Ended)], slow), None);
    }

    #[test]
    fn a_pan_decides_its_axis_once() {
        let slow = Duration::from_millis(200);
        // Mostly down at first: a scroll, however far right it goes after.
        assert_eq!(pan(&[(0., 0., Started), (4., 12., Moved), (80., 0., Moved), (0., 0., Ended)], slow), None);
        // Right, but not 1.5 times more than down.
        assert_eq!(pan(&[(0., 0., Started), (12., 9., Moved), (80., 0., Moved), (0., 0., Ended)], slow), None);
        // Under the slop nothing is decided; the pan that follows decides.
        assert_eq!(pan(&[(0., 0., Started), (3., 3., Moved), (70., 2., Moved), (0., 0., Ended)], slow).as_deref(), Some("$m"));
    }

    #[test]
    fn a_short_flick_replies_and_a_pause_before_letting_go_does_not() {
        let quick = Duration::from_millis(8);
        assert_eq!(pan(&[(0., 0., Started), (15., 0., Moved), (15., 0., Moved), (0., 0., Ended)], quick).as_deref(), Some("$m"));
        assert_eq!(pan(&[(0., 0., Started), (10., 0., Moved), (5., 0., Moved), (0., 0., Ended)], quick), None, "too short to flick");
        let paused = [(0., 0., Started), (15., 0., Moved), (15., 0., Moved), (0., 0., Moved), (0., 0., Moved), (0., 0., Moved), (0., 0., Moved), (0., 0., Ended)];
        assert_eq!(pan(&paused, Duration::from_millis(40)), None);
    }
}

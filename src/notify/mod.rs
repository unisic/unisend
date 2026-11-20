//! Desktop notifications: one per chat, replaced in place as messages arrive, closed once the chat
//! is read anywhere, with Mark as read, Mute and a reply box where the platform has one. A message
//! edited or deleted after it was shown is changed or taken out there too. Only the system's own
//! notification service on this machine is used: no push service, no registration.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use gpui_kit::{AnyWindowHandle, App, WeakEntity};
use tokio::sync::mpsc;

use crate::i18n::t;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use self::linux::round;
#[cfg(target_os = "linux")]
use self::linux::Sink;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use self::macos::Sink;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use self::windows::Sink;

/// Chats with a notification kept track of; past this the least recently updated one is closed.
const MAX_CHATS: usize = 64;
/// Messages a notification shows, as on Android.
const KEPT: usize = 8;
/// Characters of one message in a notification.
const MAX_LINE: usize = 300;
/// Messages a notification shows: toasts and banners have room for about four lines, and the
/// newest must be among them.
const SHOWN: usize = if cfg!(target_os = "linux") { KEPT } else { 3 };
/// Requests, button presses and UI calls waiting; more are dropped.
const QUEUE: usize = 64;
/// Pause before the notification service is asked again after it did not answer (at login the app
/// can start before the desktop's service); doubles up to `RETRY_MAX`.
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(5 * 60);
/// A message waiting for a service that is not there asks again, at most this often.
const RETRY_SOON: Duration = Duration::from_secs(5);
/// macOS takes the bundle's identifier, the same.
#[cfg(not(target_os = "macos"))]
const APP_ID: &str = "dev.gpui.beeper";

enum Req {
    Show { chat: String, name: String, icon: String, id: String, line: String, alert: bool, quiet: bool },
    /// Message `id` was edited to `line`, or deleted (`None`).
    Amend { chat: String, id: String, line: Option<String> },
    Close(String),
    /// Closes the chats of the account in this slot, signed out.
    CloseAccount(usize),
    /// Answered once every notification is closed.
    CloseAll(Option<tokio::sync::oneshot::Sender<()>>),
}

/// Which notification a platform's event is about: the server's number on Linux, the chat elsewhere.
enum Target {
    #[cfg_attr(all(not(target_os = "linux"), not(test)), expect(dead_code, reason = "only Linux numbers notifications"))]
    Id(u32),
    #[cfg_attr(target_os = "linux", expect(dead_code, reason = "Linux numbers notifications"))]
    Chat(String),
}

/// What the user did with a notification.
enum Event {
    Act(Target, Act),
    #[cfg_attr(target_os = "macos", expect(dead_code, reason = "the notification API on macOS has no reply box here"))]
    Replied(Target, String),
    #[cfg_attr(not(target_os = "linux"), expect(dead_code, reason = "only Linux says when a notification goes"))]
    Closed(Target),
}

/// What only the UI thread can do.
enum ToUi {
    Open,
    #[cfg(target_os = "macos")]
    Show(gpui_kit::SystemNotification),
    #[cfg(target_os = "macos")]
    Dismiss(String),
}

static REQUESTS: OnceLock<mpsc::Sender<Req>> = OnceLock::new();
static EVENTS: OnceLock<mpsc::Sender<Event>> = OnceLock::new();
static TO_UI: OnceLock<mpsc::Sender<ToUi>> = OnceLock::new();
/// The chat a clicked notification asked to open, taken by the UI once.
static PENDING: Mutex<Option<String>> = Mutex::new(None);

fn request(req: Req) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if REQUESTS.get().is_some_and(|tx| tx.try_send(req).is_err()) {
        if WARNED.swap(true, Ordering::Relaxed) {
            log::debug!("notification dropped");
        } else {
            log::warn!("notification dropped: {QUEUE} waiting");
        }
    }
}

/// Hands what the user did to the notification task; platforms call it from their own threads.
fn event(ev: Event) {
    if EVENTS.get().is_some_and(|tx| tx.try_send(ev).is_err()) {
        log::warn!("notification button press dropped");
    }
}

fn to_ui(msg: ToUi) {
    if TO_UI.get().is_some_and(|tx| tx.try_send(msg).is_err()) {
        log::warn!("notification click dropped");
    }
}

pub(crate) fn show(notice: &crate::Notice) {
    request(Req::Show { chat: notice.chat.into(), name: notice.name.into(), icon: notice.icon.into(), id: notice.note.id.clone(), line: line(notice), alert: notice.alert, quiet: notice.quiet });
}

/// The notice's message as its notification shows it again, edited, or with `gone` deleted.
pub(crate) fn amend(notice: &crate::Notice, gone: bool) {
    request(Req::Amend { chat: notice.chat.into(), id: notice.note.id.clone(), line: (!gone).then(|| line(notice)) });
}

/// A message's line. A direct chat's title already names the sender, as Signal and Android show it.
fn line(notice: &crate::Notice) -> String {
    if notice.group { format!("{}: {}", notice.note.sender, notice.note.text) } else { notice.note.text.clone() }
}

pub(crate) fn close(chat: &str) {
    request(Req::Close(chat.into()));
}

pub(crate) fn close_all() {
    request(Req::CloseAll(None));
}

pub(crate) fn close_account(slot: usize) {
    request(Req::CloseAccount(slot));
}

/// While the app PIN is asked, notifications say only which chat has a new message and offer no
/// buttons; the app's lock on desktop is the PIN alone.
fn locked() -> bool {
    crate::applock::is_locked()
}

pub(crate) fn take_pending() -> Option<String> {
    PENDING.lock().ok()?.take()
}

/// Lets notifications bring the window to front on their chat; called once the window is open.
pub(crate) fn attach(window: AnyWindowHandle, app: WeakEntity<crate::ui::BeeperApp>, cx: &mut App) {
    let (tx, mut rx) = mpsc::channel(QUEUE);
    if TO_UI.set(tx).is_err() {
        return;
    }
    // A window that never had the focus since it opened (started behind others) is not in front.
    let _ = window.update(cx, |_, window, _| crate::FOREGROUND.store(window.is_window_active(), Ordering::Relaxed));
    #[cfg(target_os = "windows")]
    cx.set_app_identity(APP_ID, "Unisend");
    #[cfg(target_os = "macos")]
    macos::attach(cx);
    // GPUI waits a moment for this before the process ends.
    cx.on_app_quit(|_| {
        let (tx, rx) = tokio::sync::oneshot::channel();
        request(Req::CloseAll(Some(tx)));
        async move {
            let _ = rx.await;
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Some(msg) = rx.recv().await {
            cx.update(|cx| match msg {
                ToUi::Open => {
                    #[cfg(target_os = "linux")]
                    linux::hand_token();
                    // Opens the window again when it was closed and the app ran on.
                    crate::desktop::show(cx);
                    let _ = app.update(cx, |app, cx| app.open_from_notification(cx));
                }
                #[cfg(target_os = "macos")]
                ToUi::Show(notification) => cx.show_system_notification(notification),
                #[cfg(target_os = "macos")]
                ToUi::Dismiss(tag) => cx.dismiss_system_notification(&tag),
            });
        }
    })
    .detach();
}

/// One chat's live notification.
struct Entry {
    chat: String,
    name: String,
    /// The server's number for it (Linux); 0 until it answered, and elsewhere.
    id: u32,
    /// Each line with its message's event ID; the app's own lines (a reply, "Not sent") have none.
    lines: VecDeque<(String, String)>,
    /// The chat's picture on disk, removed with the notification.
    icon: String,
}

impl Entry {
    fn push(&mut self, id: &str, line: &str) {
        if self.lines.len() == KEPT {
            self.lines.pop_front();
        }
        self.lines.push_back((id.into(), kept(line)));
    }

    /// The notification's text: the last `SHOWN` messages, or only that there is one while locked.
    fn text(&self, markup: bool, locked: bool) -> String {
        if locked {
            return t!("New message").to_owned();
        }
        body(self.lines.iter().skip(self.lines.len().saturating_sub(SHOWN)).map(|(_, line)| line), markup)
    }
}

/// Text as kept for a notification: without the control characters that end a D-Bus string or
/// make a toast's XML invalid, and cut to `MAX_LINE` characters.
fn kept(text: &str) -> String {
    let mut chars = text.chars().filter(|&c| c == '\n' || (c >= ' ' && c != '\u{7f}'));
    let mut cut: String = chars.by_ref().take(MAX_LINE).collect();
    if chars.next().is_some() {
        cut.push('…');
    }
    cut
}

/// Live notifications, least recently updated first.
#[derive(Default)]
struct Shown(VecDeque<Entry>);

impl Shown {
    /// Adds a line to the chat's notification and moves it to the back. Returns the entry dropped
    /// to stay within `MAX_CHATS`.
    fn add(&mut self, chat: &str, name: &str, id: &str, line: &str, icon: String) -> Option<Entry> {
        let mut entry = self.remove(chat).unwrap_or_else(|| Entry { chat: chat.into(), name: String::new(), id: 0, lines: VecDeque::new(), icon: String::new() });
        entry.name = kept(name);
        entry.push(id, line);
        if !icon.is_empty() {
            entry.icon = icon;
        }
        let dropped = if self.0.len() == MAX_CHATS { self.0.pop_front() } else { None };
        self.0.push_back(entry);
        dropped
    }

    fn get(&mut self, chat: &str) -> Option<&mut Entry> {
        self.0.iter_mut().find(|e| e.chat == chat)
    }

    fn remove(&mut self, chat: &str) -> Option<Entry> {
        let at = self.0.iter().position(|e| e.chat == chat)?;
        self.0.remove(at)
    }

    /// Puts message `id`'s new line in its chat's notification, or with `None` takes it out. A
    /// notification left with only the app's own lines goes.
    fn amend(&mut self, chat: &str, id: &str, line: Option<&str>) -> Amended {
        let Some(entry) = self.get(chat) else { return Amended::No };
        let Some(at) = entry.lines.iter().position(|(i, _)| !id.is_empty() && i == id) else { return Amended::No };
        match line {
            Some(line) => entry.lines[at].1 = kept(line),
            None => drop(entry.lines.remove(at)),
        }
        if entry.lines.iter().any(|(i, _)| !i.is_empty()) {
            return Amended::Changed;
        }
        self.remove(chat).map_or(Amended::No, Amended::Gone)
    }

    /// A server that started again (Plasma's shell restarts) numbers from 1 anew: another chat's
    /// entry holding the number the chat's notification just got is no longer showing, and must
    /// not take its buttons or close it.
    fn renumbered(&mut self, chat: &str) {
        let Some(id) = self.0.iter().find(|e| e.chat == chat).map(|e| e.id).filter(|&id| id != 0) else { return };
        for entry in self.0.iter_mut().filter(|e| e.id == id && e.chat != chat) {
            entry.id = 0;
        }
    }

    fn chat_of(&self, target: Target) -> Option<String> {
        match target {
            Target::Id(id) => self.0.iter().find(|e| id != 0 && e.id == id).map(|e| e.chat.clone()),
            Target::Chat(chat) => Some(chat),
        }
    }
}

/// What an edit or deletion did to the notifications.
enum Amended {
    /// The message is not in one.
    No,
    Changed,
    /// Its notification had no other message, and is to be closed.
    Gone(Entry),
}

/// The notification's text: one line per message, escaped when the server reads markup.
fn body<'a>(lines: impl IntoIterator<Item = &'a String>, markup: bool) -> String {
    let mut out = String::new();
    for line in lines {
        if !out.is_empty() {
            out.push('\n');
        }
        let mut flat = line.replace('\n', " ");
        if markup {
            flat = flat.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        }
        out.push_str(&flat);
    }
    out
}

/// `image-path` and a toast's image take a `file://` URI.
#[cfg(not(target_os = "macos"))]
fn file_uri(path: &str) -> String {
    let mut uri = String::from("file://");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/._-~:".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

#[derive(Debug, PartialEq)]
enum Act {
    Open,
    Read,
    Mute,
}

/// What a notification button asks for. A server without a reply box sends `inline-reply` as a
/// plain button: it opens the chat to answer there.
fn act(key: &str) -> Option<Act> {
    match key {
        "default" | "inline-reply" => Some(Act::Open),
        "read" => Some(Act::Read),
        "mute" => Some(Act::Mute),
        _ => None,
    }
}

/// Talks to the system's notification service for the app's lifetime: it runs on the backend's
/// runtime and ends with it. Without a service it logs once and stops; notices then go nowhere.
pub(crate) async fn serve(dir: PathBuf) {
    let (tx, mut requests) = mpsc::channel(QUEUE);
    let (etx, mut events) = mpsc::channel(QUEUE);
    if REQUESTS.set(tx).is_err() || EVENTS.set(etx).is_err() {
        return;
    }
    // Pictures left by a run that ended with notifications showing.
    let _ = std::fs::remove_dir_all(dir.join("notify"));
    #[cfg(target_os = "windows")]
    windows::register(&dir);
    let mut service = Service { sink: None, tried: Instant::now(), wait: RETRY_FIRST };
    service.open().await;
    let mut shown = Shown::default();
    loop {
        tokio::select! {
            // A notification closed before a newer message came is forgotten first, so the closing
            // cannot take the newer one's entry.
            biased;
            Some(ev) = events.recv() => {
                if let Some(sink) = service.sink.as_mut() {
                    on_event(sink, &mut shown, ev).await;
                }
            }
            () = tokio::time::sleep_until((service.tried + service.wait).into()), if service.sink.is_none() => service.open().await,
            req = requests.recv() => match req {
                Some(Req::Show { chat, name, icon, id, line, alert, quiet }) => {
                    if let Some(old) = shown.add(&chat, &name, &id, &line, icon) {
                        drop_entry(service.sink.as_mut(), old).await;
                    }
                    // Earlier messages of one sync only add their lines: the last one posts them all.
                    if alert && let Some(sink) = service.sink().await {
                        post(sink, &mut shown, &chat, !quiet).await;
                    }
                }
                Some(Req::Amend { chat, id, line }) => match shown.amend(&chat, &id, line.as_deref()) {
                    Amended::No => {}
                    Amended::Changed => {
                        if let Some(sink) = service.sink.as_mut() {
                            post(sink, &mut shown, &chat, false).await;
                        }
                    }
                    Amended::Gone(old) => drop_entry(service.sink.as_mut(), old).await,
                },
                Some(Req::Close(chat)) => {
                    if let Some(old) = shown.remove(&chat) {
                        drop_entry(service.sink.as_mut(), old).await;
                    }
                }
                Some(Req::CloseAccount(slot)) => {
                    let (gone, kept): (VecDeque<Entry>, _) = std::mem::take(&mut shown.0).into_iter().partition(|e| crate::accounts::split(&e.chat).0 == slot);
                    shown.0 = kept;
                    for old in gone {
                        drop_entry(service.sink.as_mut(), old).await;
                    }
                }
                Some(Req::CloseAll(done)) => {
                    for old in std::mem::take(&mut shown.0) {
                        drop_entry(service.sink.as_mut(), old).await;
                    }
                    #[cfg(target_os = "windows")]
                    if let Some(sink) = &service.sink {
                        sink.clear();
                    }
                    if let Some(done) = done {
                        let _ = done.send(());
                    }
                }
                None => break,
            },
        }
    }
}

/// The system's notification service, asked again with growing pauses while it is not there.
struct Service {
    sink: Option<Sink>,
    tried: Instant,
    wait: Duration,
}

impl Service {
    async fn open(&mut self) {
        self.tried = Instant::now();
        match Sink::open().await {
            Ok(sink) => {
                if self.wait > RETRY_FIRST {
                    log::info!("desktop notifications available");
                }
                self.sink = Some(sink);
            }
            Err(e) => {
                if self.wait == RETRY_FIRST {
                    log::warn!("no desktop notifications for now: {e}");
                } else {
                    log::debug!("no desktop notifications: {e}");
                }
                self.wait = (self.wait * 2).min(RETRY_MAX);
            }
        }
    }

    /// The service for a message, asked once more when the last try was a while ago.
    async fn sink(&mut self) -> Option<&mut Sink> {
        if self.sink.is_none() && self.tried.elapsed() >= RETRY_SOON {
            self.open().await;
        }
        self.sink.as_mut()
    }
}

/// Closes the notification and removes the chat's picture.
async fn drop_entry(sink: Option<&mut Sink>, entry: Entry) {
    if let Some(sink) = sink {
        sink.close(&entry).await;
    }
    if !entry.icon.is_empty() {
        let _ = std::fs::remove_file(&entry.icon);
    }
}

async fn on_event(sink: &mut Sink, shown: &mut Shown, ev: Event) {
    // A notification from before the lock can still be clicked: it opens the app, to its PIN.
    if locked() && matches!(ev, Event::Replied(..) | Event::Act(_, Act::Read | Act::Mute)) {
        return;
    }
    match ev {
        Event::Act(target, act) => {
            let Some(chat) = shown.chat_of(target) else { return };
            match act {
                Act::Open => {
                    if let Ok(mut pending) = PENDING.lock() {
                        *pending = Some(chat);
                    }
                    to_ui(ToUi::Open);
                }
                Act::Read => {
                    crate::backend::read_from_notification(&chat);
                    if let Some(old) = shown.remove(&chat) {
                        drop_entry(Some(sink), old).await;
                    }
                }
                Act::Mute if crate::backend::mute_from_notification(&chat) => {
                    if let Some(old) = shown.remove(&chat) {
                        drop_entry(Some(sink), old).await;
                    }
                }
                Act::Mute => quiet_line(sink, shown, &chat, format!("Unisend: {}", t!("Not muted"))).await,
            }
        }
        Event::Replied(target, text) => {
            let Some(chat) = shown.chat_of(target) else { return };
            if crate::backend::reply(&chat, &text) {
                // The next message's notification shows the answer above it, as Android's does.
                if let Some(entry) = shown.get(&chat) {
                    entry.push("", &format!("{}: {}", t!("You"), text.trim()));
                }
            } else {
                quiet_line(sink, shown, &chat, format!("Unisend: {}", t!("Not sent"))).await;
            }
        }
        Event::Closed(target) => {
            if let Some(chat) = shown.chat_of(target)
                && let Some(entry) = shown.remove(&chat)
                && !entry.icon.is_empty()
            {
                let _ = std::fs::remove_file(&entry.icon);
            }
        }
    }
}

/// Posts the chat's notification as it now reads.
async fn post(sink: &mut Sink, shown: &mut Shown, chat: &str, alert: bool) {
    let Some(entry) = shown.get(chat) else { return };
    // Showing nothing reads as the lock does: "New message" under the app's name, no buttons.
    let locked = locked() || crate::NotifyShows::current() == crate::NotifyShows::Nothing;
    let text = entry.text(sink.markup(), locked);
    sink.post(entry, &text, alert, !locked).await;
    shown.renumbered(chat);
}

/// Adds the app's own line to a chat's notification without a sound.
async fn quiet_line(sink: &mut Sink, shown: &mut Shown, chat: &str, line: String) {
    let Some(entry) = shown.get(chat) else { return };
    entry.push("", &line);
    let text = entry.text(sink.markup(), false);
    sink.post(entry, &text, false, true).await;
    shown.renumbered(chat);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_keeps_the_last_messages_on_one_line_each_and_escapes_markup() {
        let mut shown = Shown::default();
        for n in 0..KEPT + 2 {
            shown.add("!a:x", "Ann", &format!("$e{n}"), &format!("Ann: {n}"), String::new());
        }
        let entry = shown.get("!a:x").unwrap();
        assert_eq!(entry.lines.len(), KEPT);
        let text = entry.text(false, false);
        assert_eq!(text.lines().count(), SHOWN);
        assert!(text.ends_with("Ann: 9"), "the newest message shows");
        assert_eq!(body(&["a <b> & c\nd".to_owned()], true), "a &lt;b&gt; &amp; c d");
        assert_eq!(entry.text(false, true), t!("New message"), "no text while the app is locked");
    }

    #[test]
    fn lines_are_kept_without_control_characters_and_cut() {
        let mut shown = Shown::default();
        shown.add("!a:x", "A\0nn\u{1b}", "$e", "Ann: a\0b\u{7f}c\td\r\ne\u{1f}", String::new());
        let entry = shown.get("!a:x").unwrap();
        assert_eq!((entry.name.as_str(), entry.lines[0].1.as_str()), ("Ann", "Ann: abcd\ne"));
        let long = kept(&"é".repeat(MAX_LINE + 5));
        assert_eq!(long.chars().count(), MAX_LINE + 1);
        assert!(long.ends_with('…'));
        assert_eq!(kept(&"é".repeat(MAX_LINE)).chars().count(), MAX_LINE);
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_picture_path_becomes_a_file_uri() {
        assert_eq!(file_uri("/home/a b/notify/_x_y"), "file:///home/a%20b/notify/_x_y");
        assert_eq!(file_uri("/C:/Users/Ł/x#1"), "file:///C:/Users/%C5%81/x%231");
    }

    #[test]
    fn one_notification_per_chat_and_the_oldest_goes_past_the_cap() {
        let mut shown = Shown::default();
        assert!(shown.add("!a:x", "A", "$1", "1", "/p/a".into()).is_none());
        shown.get("!a:x").unwrap().id = 7;
        shown.add("!a:x", "A2", "$2", "2", String::new());
        let entry = shown.get("!a:x").unwrap();
        assert_eq!((entry.id, entry.icon.as_str(), entry.name.as_str()), (7, "/p/a", "A2"), "the next message replaces the chat's notification");
        assert_eq!(shown.chat_of(Target::Id(7)).as_deref(), Some("!a:x"));
        assert_eq!(shown.chat_of(Target::Id(0)), None);
        for n in 1..MAX_CHATS {
            assert!(shown.add(&format!("!{n}:x"), "n", "$m", "m", String::new()).is_none());
        }
        let dropped = shown.add("!new:x", "n", "$m", "m", String::new());
        assert_eq!(dropped.map(|e| e.chat).as_deref(), Some("!a:x"), "the least recently updated chat goes");
        assert_eq!(shown.0.len(), MAX_CHATS);
        assert_eq!(shown.remove("!new:x").map(|e| e.lines.len()), Some(1));
        assert!(shown.chat_of(Target::Id(7)).is_none());
    }

    #[test]
    fn a_number_given_again_after_a_server_restart_belongs_to_the_newer_notification() {
        let mut shown = Shown::default();
        shown.add("!old:x", "Old", "$1", "1", String::new());
        shown.get("!old:x").unwrap().id = 1;
        shown.add("!new:x", "New", "$2", "2", String::new());
        shown.get("!new:x").unwrap().id = 1;
        shown.renumbered("!new:x");
        assert_eq!(shown.chat_of(Target::Id(1)).as_deref(), Some("!new:x"));
        assert_eq!(shown.get("!old:x").unwrap().id, 0, "the old one is not closed by number");
    }

    #[test]
    fn an_edited_message_changes_its_line_and_a_deleted_one_leaves() {
        let mut shown = Shown::default();
        shown.add("!a:x", "Ann", "$1", "Ann: hi", String::new());
        shown.add("!a:x", "Ann", "$2", "Ann: my pin is 1234", String::new());
        shown.get("!a:x").unwrap().push("", "You: ok");
        assert!(matches!(shown.amend("!a:x", "$9", None), Amended::No), "a message not shown");
        assert!(matches!(shown.amend("!b:x", "$1", None), Amended::No), "a chat not shown");
        assert!(matches!(shown.amend("!a:x", "", None), Amended::No), "the app's own lines are not messages");
        assert!(matches!(shown.amend("!a:x", "$1", Some("Ann: hello")), Amended::Changed));
        assert!(matches!(shown.amend("!a:x", "$2", None), Amended::Changed));
        assert_eq!(shown.get("!a:x").unwrap().text(false, false), "Ann: hello\nYou: ok");
        let Amended::Gone(gone) = shown.amend("!a:x", "$1", None) else { panic!("only the reply is left") };
        assert_eq!(gone.chat, "!a:x");
        assert!(shown.get("!a:x").is_none());
    }

    #[test]
    fn buttons_map_to_what_they_do() {
        assert_eq!(act("default"), Some(Act::Open));
        assert_eq!(act("inline-reply"), Some(Act::Open));
        assert_eq!(act("read"), Some(Act::Read));
        assert_eq!(act("mute"), Some(Act::Mute));
        assert_eq!(act("other"), None);
    }
}

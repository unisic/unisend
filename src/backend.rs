//! Matrix side of the client. Owns a tokio runtime on its own thread; the UI
//! talks to it only through two bounded channels (`Cmd` in, `Ev` out).

use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use futures_util::StreamExt as _;
use matrix_sdk::{
    Client, Room, RoomMemberships,
    authentication::matrix::MatrixSession,
    config::{SyncSettings, SyncToken},
    deserialized_responses::{RawAnySyncOrStrippedState, TimelineEvent, TimelineEventKind},
    encryption::{
        BackupDownloadStrategy, EncryptionSettings,
        recovery::{RecoveryError, RecoveryState},
        secret_storage::SecretStorageError,
    },
    notification_settings::{NotificationSettings, RoomNotificationMode},
    attachment::{AttachmentConfig, AttachmentInfo, BaseImageInfo},
    media::{MediaFormat, MediaRequestParameters, MediaThumbnailSettings},
    reqwest,
    room::{
        Messages, MessagesOptions,
        edit::EditedContent,
        reply::{EnforceThread, Reply},
    },
    ruma::{
        EventId, OwnedEventId, OwnedRoomId, OwnedUserId, RoomId, UInt, UserId,
        api::{
            client::{
                filter::{FilterDefinition, RoomFilter},
                membership::joined_rooms,
                profile::AvatarUrl,
                receipt::create_receipt,
            },
            error::ErrorKind,
        },
        events::{
            AnySyncMessageLikeEvent, AnySyncTimelineEvent, Mentions, StateEventType, SyncMessageLikeEvent,
            receipt::ReceiptThread,
            room::{
                EncryptedFile, MediaSource,
                message::{
                    AddMentions, MessageType, Relation, RoomMessageEventContent, RoomMessageEventContentWithoutRelation,
                    sanitize::remove_plain_reply_fallback,
                },
            },
            sticker::StickerEventContent,
        },
        media::Method,
        serde::{JsonObject, Raw},
        uint,
    },
    store::RoomLoadSettings,
};
use matrix_sdk_crypto::secret_storage::DecodeError;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::beeper;
use crate::bridges::{self, Flow, Network, Step};

const CHANNEL_CAP: usize = 64;
const PAGE: u16 = 40;
const MIN_ITEMS: usize = 15;
const MAX_PAGES_PER_REQUEST: usize = 5;
const SYNC_TIMELINE_LIMIT: u32 = 10;
/// Avatar and preview fetches in flight at once.
const ROW_FETCHES: usize = 4;
/// Longest side of an inline thumbnail and of the full-screen view, in pixels. Everything shown is
/// re-encoded to at most this, so a decoded image costs at most 2.6 MB and 16 MB.
const THUMB_PX: u32 = 800;
const VIEW_PX: u32 = 2048;
/// Larger files are not fetched to make a thumbnail, and not sent.
const MAX_THUMB_SOURCE: u64 = 16 * 1024 * 1024;
const MAX_UPLOAD: u64 = 100 * 1024 * 1024;
/// A message older than this when it arrives is history, not news.
const NOTIFY_WINDOW: Duration = Duration::from_secs(300);
/// History pages read back looking for a chat's last message.
const MAX_PREVIEW_PAGES: usize = 3;
/// Chats whose last message is being looked up in the background at once, and how many may wait.
const PROBES: usize = 2;
const PROBE_QUEUE: usize = 4096;
/// Backup key downloads per history page.
const MAX_KEY_FETCHES: usize = 8;

pub enum Cmd {
    SendCode(String),
    SubmitCode(String),
    Recover(String),
    Open(OwnedRoomId),
    /// The conversation left the screen.
    Close,
    LoadOlder,
    /// The user is, or stopped, writing in the chat.
    Typing(OwnedRoomId, bool),
    Chat(OwnedRoomId, ChatAction),
    Avatars(Vec<OwnedRoomId>),
    /// The picture of a person in a chat, as that chat shows them.
    Face(OwnedRoomId, OwnedUserId),
    /// Inline thumbnails for these messages (event ID, media).
    Thumbs(Vec<(String, Media)>),
    /// The image at viewing size.
    View(String, Media),
    /// Download to the media cache so the system can open it.
    Fetch(String, Media),
    Attach { room: OwnedRoomId, path: PathBuf, reply_to: Option<String> },
    /// Chat networks on the account.
    Networks,
    /// Ways to log in to a bridge; starts the bridge when the account does not run it yet.
    Flows(String),
    LoginStart { bridge: String, flow: String },
    /// Answers a login step; the values are empty for a step that only waits.
    LoginSubmit(Step, HashMap<String, String>),
    Logout { bridge: String, login: String },
    /// Read receipts stay private while on.
    Incognito(bool),
    SignOut,
    /// Re-read the latest message of rooms whose preview is missing or undecryptable.
    Previews(Vec<OwnedRoomId>),
    /// `reply_to` and `edit` are event IDs; an edit replaces that message's text.
    Send { room: OwnedRoomId, body: String, reply_to: Option<String>, edit: Option<String> },
    React { room: OwnedRoomId, target: String, key: String },
    /// Sets the user's vote on a poll to `answers`; empty withdraws it.
    Vote { room: OwnedRoomId, target: String, unstable: bool, answers: Vec<String> },
    /// Deletes a message for everyone; one that has not left yet is not sent.
    Delete { room: OwnedRoomId, target: String },
    /// Sends again a message that failed to leave.
    Retry { room: OwnedRoomId, target: String },
}

pub enum Ev {
    NeedLogin,
    CodeSent,
    /// Signed in as this user.
    Ready(OwnedUserId),
    /// The connection to the server is lost (false) or back (true).
    Online(bool),
    /// True while this device lacks the secrets needed to decrypt history.
    NeedRecovery(bool),
    Rooms(Vec<RoomSummary>),
    RoomGone(OwnedRoomId),
    /// Changes to the rows of the open conversation, in order.
    Timeline { room: OwnedRoomId, diffs: Vec<Diff> },
    /// A request for older messages is done; `more` says whether there are older ones still.
    Paged { room: OwnedRoomId, more: bool },
    /// Who is writing in the open conversation right now.
    Typing { room: OwnedRoomId, names: Vec<String> },
    /// Thumbnail bytes as served (PNG, JPEG, ...).
    Avatar(OwnedRoomId, Vec<u8>),
    Face(OwnedUserId, Vec<u8>),
    /// JPEG or PNG, at most `THUMB_PX` on its longest side.
    Thumb(String, Vec<u8>),
    /// JPEG or PNG, at most `VIEW_PX`.
    Viewed(String, Vec<u8>),
    /// The file is in the media cache, or could not be fetched.
    Fetched(String, Option<PathBuf>),
    /// An attachment finished uploading, or failed.
    Attached,
    Networks(Vec<Network>),
    Flows(String, Vec<Flow>),
    LoginStep(Step),
    /// A request made for the settings pages failed; `Error` carries the reason.
    PanelFailed,
    Error(String),
}

/// Chat-list actions, stored the way Beeper's own clients store them so they sync both ways.
#[derive(Clone, Copy, Debug)]
pub enum ChatAction {
    /// `com.beeper.inbox.done` room account data.
    Archive(bool),
    /// `m.favourite` tag.
    Pin(bool),
    /// `m.lowpriority` tag.
    LowPriority(bool),
    /// Room push rule without a notify action.
    Mute(bool),
    /// `m.marked_unread`; clearing it also sends a read receipt.
    Unread(bool),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RoomSummary {
    pub id: OwnedRoomId,
    pub name: String,
    pub preview: Option<String>,
    pub ts: u64,
    /// Bridge network, e.g. "WhatsApp"; none for native Matrix rooms.
    #[serde(default)]
    pub network: Option<String>,
    #[serde(default)]
    pub unread: u64,
    /// Unread messages that mention the user.
    #[serde(default)]
    pub mentions: u64,
    /// One-to-one chat: bubbles need no sender names.
    #[serde(default)]
    pub dm: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub low_priority: bool,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub marked_unread: bool,
    #[serde(default)]
    pub archived: bool,
    /// Beeper's server-side order of the newest message from someone else; 0 when unknown.
    #[serde(default)]
    pub order: i64,
    /// `ts` is known to be the last message's time, or 0 because there is no message: either a
    /// message was seen, or the history was looked at. Without it `ts` may be a guess (caches from
    /// builds that took any event's time), and the chat is queued for a look.
    #[serde(default)]
    pub dated: bool,
}

impl RoomSummary {
    pub fn is_unread(&self) -> bool {
        self.unread > 0 || self.marked_unread
    }
}

#[derive(Clone)]
pub struct Msg {
    /// The event's ID; a message still on its way out has its transaction ID instead.
    pub id: String,
    pub kind: Kind,
    pub sender: String,
    pub from: OwnedUserId,
    pub body: String,
    pub own: bool,
    /// Origin server time, ms since the epoch.
    pub ts: u64,
    /// `com.beeper.hs.order`; 0 on other homeservers.
    pub order: i64,
    /// The message this one replies to.
    pub reply: Option<Quote>,
    pub deleted: bool,
    pub edited: bool,
    /// Attachment; `body` is then its caption, possibly empty.
    pub media: Option<Media>,
    pub poll: Option<Poll>,
    pub place: Option<Place>,
    /// The account's push rules ask for a notification (false when they were not evaluated).
    pub notify: bool,
    /// Byte ranges of `body` that mention someone.
    pub mentions: Vec<std::ops::Range<usize>>,
    /// Someone else mentions the user, by name or with the whole room.
    pub mentioned: bool,
    /// Preview of a link in the message, when the sender's client or bridge attached one.
    pub link: Option<Link>,
    /// One per emoji, in the order they were first used.
    pub reactions: Vec<Reaction>,
    /// Where a message of the user's is on its way to the others.
    pub delivery: Delivery,
    /// The other people whose read receipt stands on this row: they have read this far.
    pub seen: Vec<OwnedUserId>,
}

/// What a row of a conversation is.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Message,
    /// A line about the chat itself, in `body`: someone left, the name changed.
    Notice,
    /// `sender` joined. Joins that follow one another are told on one line.
    Joined,
    /// The day the rows under it belong to.
    Day,
    /// The rows under it are new since the user last read the chat.
    Unread,
    /// A bridge's word on what the network did with the user's message `target`.
    Status { target: String, delivery: Delivery },
    /// Takes a place in the list and shows nothing.
    Hidden,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Delivery {
    /// Not confirmed yet: by the homeserver, or by the bridge to the network.
    Sending,
    Sent,
    /// The network handed it to the people in the chat.
    Delivered,
    /// With the reason, when one was given.
    Failed(String),
}

/// The message a reply answers, as far as it is known.
#[derive(Clone)]
pub struct Quote {
    pub id: String,
    /// Empty when the message could not be loaded.
    pub sender: String,
    pub text: String,
}

impl Msg {
    /// A row with nothing on it yet.
    fn bare(id: String, kind: Kind, from: OwnedUserId) -> Self {
        Self {
            id,
            kind,
            sender: String::new(),
            from,
            body: String::new(),
            own: false,
            ts: 0,
            order: 0,
            reply: None,
            deleted: false,
            edited: false,
            media: None,
            poll: None,
            place: None,
            notify: false,
            mentions: Vec::new(),
            mentioned: false,
            link: None,
            reactions: Vec::new(),
            delivery: Delivery::Sent,
            seen: Vec::new(),
        }
    }
}

/// A link's preview as it came with the message (`com.beeper.linkpreviews`, MSC4095). Nothing is
/// fetched from the linked site.
#[derive(Clone)]
pub struct Link {
    /// Always `http://` or `https://`.
    pub url: String,
    pub title: String,
    pub description: String,
    /// The site's name, or its host.
    pub site: String,
    pub image: Option<Media>,
}

impl Msg {
    /// One-line description for previews and quotes: Beeper's label for media, plus the caption.
    pub fn summary(&self) -> String {
        if let Some(poll) = &self.poll {
            return format!("📊 Poll: {}", poll.question);
        }
        if let Some(place) = &self.place {
            return if place.description.is_empty() { "📍 Location".to_owned() } else { format!("📍 Location: {}", place.description) };
        }
        let Some(media) = &self.media else { return self.body.clone() };
        let label = match media.kind {
            MediaKind::Image => "📷 Photo",
            MediaKind::Sticker => "🖼 Sticker",
            MediaKind::Video => "🎬 Video",
            MediaKind::Voice => "🎤 Voice Note",
            MediaKind::Audio => "🎵 Audio",
            MediaKind::File => return format!("📎 {}", media.name),
        };
        if self.body.is_empty() { label.to_owned() } else { format!("{label}: {}", self.body) }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MediaKind {
    Image,
    Sticker,
    Video,
    Audio,
    Voice,
    File,
}

#[derive(Clone)]
pub struct Media {
    pub kind: MediaKind,
    pub name: String,
    pub size: Option<u64>,
    pub mime: Option<String>,
    /// Pixel size, so the placeholder has the image's shape before it loads.
    pub dims: Option<(u32, u32)>,
    pub duration: Option<Duration>,
    source: MediaSource,
    thumb: Option<MediaSource>,
}

#[derive(Clone)]
pub struct Reaction {
    pub key: String,
    /// How many people gave it.
    pub count: usize,
    pub own: bool,
}

/// One change to the rows of the open conversation, as the SDK's timeline reports it.
pub type Diff = matrix_sdk_ui::eyeball_im::VectorDiff<Msg>;

/// Applies a change to `rows`. Returns the rows it replaced and how many stand there now, for a
/// list that mirrors them; `None` when all of them changed.
pub fn apply(rows: &mut Vec<Msg>, diff: Diff) -> Option<(std::ops::Range<usize>, usize)> {
    let len = rows.len();
    Some(match diff {
        Diff::Append { values } => {
            rows.extend(values);
            (len..len, rows.len() - len)
        }
        Diff::PushBack { value } => {
            rows.push(value);
            (len..len, 1)
        }
        Diff::PushFront { value } => {
            rows.insert(0, value);
            (0..0, 1)
        }
        Diff::Insert { index, value } if index <= len => {
            rows.insert(index, value);
            (index..index, 1)
        }
        Diff::Set { index, value } if index < len => {
            rows[index] = value;
            (index..index + 1, 1)
        }
        Diff::Remove { index } if index < len => {
            rows.remove(index);
            (index..index + 1, 0)
        }
        Diff::PopBack if len > 0 => {
            rows.pop();
            (len - 1..len, 0)
        }
        Diff::PopFront if len > 0 => {
            rows.remove(0);
            (0..1, 0)
        }
        Diff::Truncate { length } if length <= len => {
            rows.truncate(length);
            (length..len, 0)
        }
        Diff::Reset { values } => {
            *rows = values.into_iter().collect();
            return None;
        }
        Diff::Clear => {
            rows.clear();
            return None;
        }
        // An index past the end: the two sides disagree, which a full redraw at least shows.
        _ => return None,
    })
}

/// The command channel, for what happens outside the UI: a reply typed into a notification.
#[cfg(target_os = "android")]
static COMMANDS: std::sync::OnceLock<mpsc::Sender<Cmd>> = std::sync::OnceLock::new();

/// Sends a message from a notification's reply box. False when the app is not signed in.
#[cfg(target_os = "android")]
pub(crate) fn reply(chat: &str, body: &str) -> bool {
    log::info!("notification: reply to {chat}");
    let (body, Ok(room)) = (body.trim(), OwnedRoomId::try_from(chat)) else { return false };
    !body.is_empty() && COMMANDS.get().is_some_and(|tx| tx.try_send(Cmd::Send { room, body: body.to_owned(), reply_to: None, edit: None }).is_ok())
}

/// Marks a chat read from its notification.
#[cfg(target_os = "android")]
pub(crate) fn read_from_notification(chat: &str) {
    log::info!("notification: mark {chat} read");
    if let (Some(tx), Ok(room)) = (COMMANDS.get(), OwnedRoomId::try_from(chat)) {
        let _ = tx.try_send(Cmd::Chat(room, ChatAction::Unread(false)));
    }
}

pub fn start(dir: PathBuf) -> (mpsc::Sender<Cmd>, mpsc::Receiver<Ev>) {
    let (cmd_tx, cmd_rx) = mpsc::channel(CHANNEL_CAP);
    #[cfg(target_os = "android")]
    let _ = COMMANDS.set(cmd_tx.clone());
    let (ev_tx, ev_rx) = mpsc::channel(CHANNEL_CAP);
    std::thread::Builder::new()
        .name("matrix".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("tokio runtime");
            // Returns once the UI drops its sender; dropping `rt` then cancels sync.
            rt.block_on(run(dir, cmd_rx, ev_tx));
        })
        .expect("spawn matrix thread");
    (cmd_tx, ev_rx)
}

fn http_client() -> Result<reqwest::Client> {
    let builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(15));
    // shortcut: bundled Mozilla roots on Android (ignores user-installed CAs); switch to
    // rustls-platform-verifier's Android init once its Kotlin component is packaged.
    #[cfg(target_os = "android")]
    let builder = builder.tls_certs_only(
        webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .filter_map(|der| reqwest::Certificate::from_der(der).ok()),
    );
    Ok(builder.build()?)
}

struct State {
    dir: PathBuf,
    homeserver: String,
    http: reqwest::Client,
    ev: mpsc::Sender<Ev>,
    client: Option<Client>,
    login_request: Option<String>,
    /// The conversation on screen.
    open: Option<timeline::Open>,
    rooms: Rooms,
    fetches: Arc<tokio::sync::Semaphore>,
    /// Push rules, kept current by the SDK. One instance: each registers an event handler.
    notif: Option<NotificationSettings>,
    /// Read by the tasks that follow an open conversation.
    incognito: Arc<AtomicBool>,
    /// Sync and recovery watchers, stopped on sign-out.
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

/// Room summaries, shared by the sync loop and preview refreshes. Never held across an await.
type Rooms = Arc<Mutex<HashMap<OwnedRoomId, RoomSummary>>>;

async fn run(dir: PathBuf, mut cmds: mpsc::Receiver<Cmd>, ev: mpsc::Sender<Ev>) {
    let http = match http_client() {
        Ok(h) => h,
        Err(e) => {
            let _ = ev.send(Ev::Error(format!("HTTP client: {e:#}"))).await;
            return;
        }
    };
    // Debug builds can point at a local homeserver with a pre-seeded session.json.
    let homeserver = std::env::var("BEEPER_GPUI_HOMESERVER")
        .ok()
        .filter(|_| cfg!(debug_assertions))
        .unwrap_or_else(|| beeper::HOMESERVER_URL.to_owned());
    let _ = std::fs::remove_dir_all(media_dir(&dir));
    let mut st = State { dir, homeserver, http, ev, client: None, login_request: None, open: None,
        rooms: Rooms::default(), fetches: Arc::new(tokio::sync::Semaphore::new(ROW_FETCHES)), notif: None,
        incognito: Arc::new(AtomicBool::new(true)), tasks: Vec::new() };
    match st.restore().await {
        Ok(true) => {}
        Ok(false) => st.emit(Ev::NeedLogin).await,
        Err(e) => {
            st.emit(Ev::Error(format!("Could not restore session: {e:#}"))).await;
            st.emit(Ev::NeedLogin).await;
        }
    }
    while let Some(cmd) = cmds.recv().await {
        // Boxed: the handler's future holds every command's state and is too large for the stack.
        if let Err(e) = Box::pin(st.handle(cmd)).await {
            log::warn!("{e:#}");
            st.emit(Ev::Error(format!("{e:#}"))).await;
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SavedSession {
    session: MatrixSession,
}

impl State {
    async fn emit(&self, ev: Ev) {
        // Only fails when the UI is gone, and then nobody is listening.
        let _ = self.ev.send(ev).await;
    }

    fn session_path(&self) -> PathBuf {
        self.dir.join("session.json")
    }

    async fn build_client(&self) -> Result<Client> {
        // Fetch a missing room key from the server-side backup when an event fails to decrypt,
        // instead of downloading the whole backup after recovery.
        let e2ee = EncryptionSettings {
            backup_download_strategy: BackupDownloadStrategy::AfterDecryptionFailure,
            ..Default::default()
        };
        Ok(Client::builder()
            .homeserver_url(&self.homeserver)
            .sqlite_store(self.dir.join("store"), None)
            .http_client(self.http.clone())
            .with_encryption_settings(e2ee)
            .build()
            .await?)
    }

    async fn restore(&mut self) -> Result<bool> {
        let Ok(raw) = std::fs::read(self.session_path()) else { return Ok(false) };
        let saved: SavedSession = serde_json::from_slice(&raw).context("session.json")?;
        let client = self.build_client().await?;
        client.matrix_auth().restore_session(saved.session, RoomLoadSettings::default()).await?;
        if let Some(rooms) = load_rooms(&self.dir) {
            let list = rooms.values().cloned().collect();
            *self.rooms.lock().unwrap() = rooms;
            self.emit(Ev::Rooms(list)).await;
        }
        self.go_live(client).await;
        Ok(true)
    }

    async fn go_live(&mut self, client: Client) {
        // The conversations' store of events; it has to see every sync from the first one on.
        if let Err(e) = client.event_cache().subscribe() {
            log::warn!("event cache: {e}");
        }
        let notif = client.notification_settings().await;
        self.tasks = vec![
            tokio::spawn(watch_recovery(client.clone(), self.ev.clone())),
            tokio::spawn(sync_loop(client.clone(), notif.clone(), self.dir.clone(), self.rooms.clone(), self.ev.clone())),
        ];
        self.notif = Some(notif);
        let user = client.user_id().map(ToOwned::to_owned);
        self.client = Some(client);
        if let Some(user) = user {
            self.emit(Ev::Ready(user)).await;
        }
    }

    async fn login_jwt(&mut self, jwt: String) -> Result<()> {
        // No session file means any store left behind belongs to a device that never finished logging in.
        let store = self.dir.join("store");
        if store.exists() {
            log::warn!("removing orphaned store at {}", store.display());
            std::fs::remove_dir_all(&store)?;
        }
        let client = self.build_client().await?;
        let mut data = JsonObject::new();
        data.insert("token".into(), jwt.into());
        client
            .matrix_auth()
            .login_custom("org.matrix.login.jwt", data)?
            .initial_device_display_name("Beeper GPUI")
            .await?;
        let session = client.matrix_auth().session().ok_or_else(|| anyhow!("Login returned no session"))?;
        write_private(&self.session_path(), &serde_json::to_vec(&SavedSession { session })?)?;
        self.go_live(client).await;
        Ok(())
    }

    fn client(&self) -> Result<&Client> {
        self.client.as_ref().ok_or_else(|| anyhow!("Not signed in"))
    }

    async fn handle(&mut self, cmd: Cmd) -> Result<()> {
        match cmd {
            Cmd::SendCode(email) => {
                self.login_request = Some(beeper::send_code(&self.http, email.trim()).await?);
                self.emit(Ev::CodeSent).await;
            }
            Cmd::SubmitCode(code) => {
                let request = self.login_request.clone().ok_or_else(|| anyhow!("Request a code first"))?;
                let jwt = beeper::submit_code(&self.http, &request, code.trim()).await?;
                self.login_request = None;
                self.login_jwt(jwt).await?;
            }
            Cmd::Recover(key) => {
                let recovery = self.client()?.encryption().recovery();
                if let Err(e) = recovery.recover(key.trim()).await {
                    return Err(match e {
                        RecoveryError::SecretStorage(SecretStorageError::SecretStorageKey(DecodeError::Mac(_))) => {
                            anyhow!(
                                "This recovery code is valid but does not match your account's current key. \
                                 It may be an older code; get the current one from a Beeper app where you are signed in."
                            )
                        }
                        RecoveryError::SecretStorage(SecretStorageError::SecretStorageKey(_)) => {
                            anyhow!("This is not a recovery code. Check it for typos: {e}")
                        }
                        e => anyhow::Error::new(e).context("Recovery code was not accepted"),
                    });
                }
                if let Some(open) = &self.open {
                    timeline::unlock(&open.timeline).await;
                }
                let incomplete = recovery.state() == RecoveryState::Incomplete;
                self.emit(Ev::NeedRecovery(incomplete)).await;
                if incomplete {
                    self.emit(Ev::Error("Recovery code accepted, but the account's key storage is incomplete".into()))
                        .await;
                }
            }
            Cmd::Open(room_id) => {
                // The old conversation stops being followed before the new one starts.
                self.open = None;
                crate::clear_notification(room_id.as_str());
                let room = self.room(&room_id)?;
                let open = timeline::open(&room, &self.ev, self.incognito.clone()).await?;
                self.older(&open, true);
                self.open = Some(open);
            }
            Cmd::Close => self.open = None,
            Cmd::LoadOlder => {
                if let Some(open) = &self.open {
                    self.older(open, false);
                }
            }
            Cmd::Typing(room_id, on) => {
                // Not telling who reads includes not telling who is about to answer.
                if !self.incognito.load(Ordering::Relaxed) {
                    let room = self.room(&room_id)?;
                    tokio::spawn(async move {
                        if let Err(e) = room.typing_notice(on).await {
                            log::debug!("typing notice: {e}");
                        }
                    });
                }
            }
            Cmd::Chat(room_id, action) => {
                let room = self.room(&room_id)?;
                match action {
                    ChatAction::Pin(on) => room.set_is_favourite(on, Some(1.0)).await?,
                    ChatAction::LowPriority(on) => room.set_is_low_priority(on, Some(0.0)).await?,
                    ChatAction::Unread(true) => room.set_unread_flag(true).await?,
                    ChatAction::Unread(false) => {
                        let resp = room.messages(MessagesOptions::backward()).await?;
                        mark_read(&room, resp.chunk.first().and_then(|e| e.event_id()).map(ToOwned::to_owned), self.incognito.load(Ordering::Relaxed)).await?;
                    }
                    ChatAction::Mute(on) => {
                        let notif = self.notif.as_ref().context("Not signed in")?;
                        if on {
                            notif.set_room_notification_mode(&room_id, RoomNotificationMode::MentionsAndKeywordsOnly).await?;
                        } else {
                            notif.delete_user_defined_room_rules(&room_id).await?;
                        }
                    }
                    ChatAction::Archive(false) => {
                        room.set_account_data_raw(DONE.into(), Raw::new(&Done::default())?.cast_unchecked()).await?;
                    }
                    ChatAction::Archive(true) => {
                        if room.is_favourite() {
                            return Err(anyhow!("Pinned chats can't be archived"));
                        }
                        // Like Beeper: archived up to the newest event, by the server's own order.
                        let resp = room.messages(MessagesOptions::backward()).await?;
                        let newest = resp.chunk.first();
                        let order = newest.and_then(hs_order).filter(|o| *o != 0);
                        let done = Done { updated_ts: Some(now_ms()), at_order: order };
                        room.set_account_data_raw(DONE.into(), Raw::new(&done)?.cast_unchecked()).await?;
                        // Without a server order the rule compares timestamps, which never archive an unread chat.
                        if order.is_none() || mark_read_on_archive(self.client()?).await {
                            mark_read(&room, newest.and_then(|e| e.event_id()).map(ToOwned::to_owned), self.incognito.load(Ordering::Relaxed)).await?;
                        }
                    }
                }
            }
            Cmd::Face(room_id, user) => {
                let client = self.client()?.clone();
                let (ev, permits) = (self.ev.clone(), self.fetches.clone());
                tokio::spawn(async move {
                    let Ok(_permit) = permits.acquire().await else { return };
                    match face(&client, &room_id, &user).await {
                        Ok(Some(bytes)) => {
                            let _ = ev.send(Ev::Face(user, bytes)).await;
                        }
                        Ok(None) => {}
                        Err(e) => log::debug!("face {user}: {e:#}"),
                    }
                });
            }
            Cmd::Avatars(ids) => {
                for room_id in ids {
                    let client = self.client()?.clone();
                    let (ev, permits) = (self.ev.clone(), self.fetches.clone());
                    tokio::spawn(async move {
                        let Ok(_permit) = permits.acquire().await else { return };
                        match avatar(&client, &room_id).await {
                            Ok(Some(bytes)) => {
                                let _ = ev.send(Ev::Avatar(room_id, bytes)).await;
                            }
                            Ok(None) => {}
                            Err(e) => log::debug!("avatar {room_id}: {e:#}"),
                        }
                    });
                }
            }
            Cmd::Thumbs(wanted) => {
                for (id, media) in wanted {
                    let client = self.client()?.clone();
                    let (ev, permits) = (self.ev.clone(), self.fetches.clone());
                    tokio::spawn(async move {
                        let Ok(_permit) = permits.acquire().await else { return };
                        match thumbnail(&client, &media).await {
                            Ok(Some(bytes)) => {
                                let _ = ev.send(Ev::Thumb(id, bytes)).await;
                            }
                            Ok(None) => {}
                            Err(e) => log::debug!("thumbnail {id}: {e:#}"),
                        }
                    });
                }
            }
            Cmd::View(id, media) => {
                let client = self.client()?.clone();
                let ev = self.ev.clone();
                tokio::spawn(async move {
                    let bytes = async {
                        let request = MediaRequestParameters { source: media.source.clone(), format: MediaFormat::File };
                        let bytes = client.media().get_media_content(&request, true).await?;
                        tokio::task::spawn_blocking(move || shrink(&bytes, VIEW_PX)).await?
                    };
                    let _ = match bytes.await {
                        Ok(bytes) => ev.send(Ev::Viewed(id, bytes)).await,
                        Err(e) => ev.send(Ev::Error(format!("Could not load the image: {e:#}"))).await,
                    };
                });
            }
            Cmd::Fetch(id, media) => {
                let client = self.client()?.clone();
                let (ev, dir) = (self.ev.clone(), media_dir(&self.dir));
                tokio::spawn(async move {
                    let saved = async {
                        let request = MediaRequestParameters { source: media.source.clone(), format: MediaFormat::File };
                        let bytes = client.media().get_media_content(&request, false).await?;
                        // One directory per event keeps the file's own name without collisions.
                        let folder = dir.join(safe_name(&id));
                        std::fs::create_dir_all(&folder)?;
                        let path = folder.join(safe_name(&media.name));
                        write_private(&path, &bytes)?;
                        anyhow::Ok(path)
                    };
                    let path = match saved.await {
                        Ok(path) => Some(path),
                        Err(e) => {
                            let _ = ev.send(Ev::Error(format!("Could not download {}: {e:#}", media.name))).await;
                            None
                        }
                    };
                    let _ = ev.send(Ev::Fetched(id, path)).await;
                });
            }
            Cmd::Attach { room, path, reply_to } => {
                let room = self.room(&room)?;
                let ev = self.ev.clone();
                tokio::spawn(async move {
                    if let Err(e) = attach(&room, &path, reply_to).await {
                        let _ = ev.send(Ev::Error(format!("Attachment not sent: {e:#}"))).await;
                    }
                    let _ = ev.send(Ev::Attached).await;
                });
            }
            Cmd::Incognito(on) => self.incognito.store(on, Ordering::Relaxed),
            Cmd::Networks => {
                let (api, ev) = (self.api()?, self.ev.clone());
                tokio::spawn(async move { panel(&ev, api.networks().await.map(Ev::Networks)).await });
            }
            Cmd::Flows(bridge) => {
                let (api, ev) = (self.api()?, self.ev.clone());
                tokio::spawn(async move {
                    panel(&ev, api.flows(&bridge).await.map(|flows| Ev::Flows(bridge, flows))).await;
                });
            }
            Cmd::LoginStart { bridge, flow } => {
                let (api, ev) = (self.api()?, self.ev.clone());
                tokio::spawn(async move { panel(&ev, api.start(&bridge, &flow).await.map(Ev::LoginStep)).await });
            }
            Cmd::LoginSubmit(step, values) => {
                let (api, ev) = (self.api()?, self.ev.clone());
                tokio::spawn(async move { panel(&ev, api.submit(&step, &values).await.map(Ev::LoginStep)).await });
            }
            Cmd::Logout { bridge, login } => {
                let (api, ev) = (self.api()?, self.ev.clone());
                tokio::spawn(async move {
                    let done = async {
                        api.logout(&bridge, &login).await?;
                        api.networks().await
                    };
                    panel(&ev, done.await.map(Ev::Networks)).await;
                });
            }
            Cmd::SignOut => {
                for task in self.tasks.drain(..) {
                    task.abort();
                }
                if let Some(client) = self.client.take() {
                    // The device is gone locally either way; the server forgets it when it can.
                    if let Err(e) = client.matrix_auth().logout().await {
                        log::warn!("logout: {e}");
                    }
                }
                self.notif = None;
                self.open = None;
                self.rooms.lock().unwrap().clear();
                let _ = std::fs::remove_file(self.session_path());
                let _ = std::fs::remove_file(rooms_path(&self.dir));
                let _ = std::fs::remove_dir_all(self.dir.join("store"));
                let _ = std::fs::remove_dir_all(media_dir(&self.dir));
                self.emit(Ev::NeedLogin).await;
            }
            Cmd::Previews(ids) => {
                for room_id in ids {
                    let room = self.room(&room_id)?;
                    let (ev, permits, rooms) = (self.ev.clone(), self.fetches.clone(), self.rooms.clone());
                    tokio::spawn(async move {
                        let Ok(_permit) = permits.acquire().await else { return };
                        if let Some(summary) = probe(&room, &rooms).await {
                            let _ = ev.send(Ev::Rooms(vec![summary])).await;
                        }
                    });
                }
            }
            Cmd::Send { room, body, reply_to, edit } => {
                let text = RoomMessageEventContentWithoutRelation::text_plain(body);
                match self.open.as_ref().filter(|open| open.room == room) {
                    // The timeline shows the message at once and keeps trying to send it.
                    Some(open) => match (edit, reply_to) {
                        (Some(id), _) => open.timeline.edit(&timeline::item_id(&id)?, EditedContent::RoomMessage(text)).await?,
                        (None, Some(id)) => drop(open.timeline.send_reply(text, EventId::parse(id)?).await?),
                        (None, None) => drop(open.timeline.send(RoomMessageEventContent::from(text).into()).await?),
                    },
                    // A reply typed into a notification, for a chat that is not on screen.
                    None => drop(self.room(&room)?.send_queue().send(RoomMessageEventContent::from(text).into()).await?),
                }
            }
            Cmd::React { room, target, key } => {
                let open = self.open.as_ref().filter(|open| open.room == room).context("That chat is not open")?;
                open.timeline.toggle_reaction(&timeline::item_id(&target)?, &key).await?;
            }
            Cmd::Delete { room, target } => {
                let open = self.open.as_ref().filter(|open| open.room == room).context("That chat is not open")?;
                open.timeline.redact(&timeline::item_id(&target)?, None).await?;
            }
            Cmd::Retry { room, target } => {
                let open = self.open.as_ref().filter(|open| open.room == room).context("That chat is not open")?;
                // A failed send stops the chat's queue; trying again starts it.
                open.timeline.room().send_queue().set_enabled(true);
                timeline::retry(&open.timeline, &target).await?;
            }
            Cmd::Vote { room, target, unstable, answers } => {
                let (kind, content) = polls::response(unstable, &target, answers);
                self.room(&room)?.send_raw(kind, content).await?;
            }
        }
        Ok(())
    }

    /// Asks for older messages in the background; the rows arrive as changes to the timeline. The
    /// first time, also until there is something to read: a page may hold nothing that is shown.
    fn older(&self, open: &timeline::Open, first: bool) {
        let (timeline, room, ev, incognito) = (open.timeline.clone(), open.room.clone(), self.ev.clone(), self.incognito.clone());
        tokio::spawn(async move {
            let mut more = true;
            for _ in 0..if first { MAX_PAGES_PER_REQUEST } else { 1 } {
                match timeline.paginate_backwards(PAGE).await {
                    Ok(start) => more = !start,
                    Err(e) => {
                        let _ = ev.send(Ev::Error(format!("Could not load messages: {e}"))).await;
                        break;
                    }
                }
                if !more || timeline.items().await.iter().filter(|item| item.as_event().is_some()).count() >= MIN_ITEMS {
                    break;
                }
            }
            let _ = ev.send(Ev::Paged { room, more }).await;
            if first && let Err(e) = mark_read(timeline.room(), timeline.latest_event_id().await, incognito.load(Ordering::Relaxed)).await {
                log::warn!("mark read: {e:#}");
            }
        });
    }

    /// Access to Beeper's account API and the bridges behind the homeserver.
    fn api(&self) -> Result<bridges::Api> {
        let client = self.client()?;
        let debug = |var: &str| std::env::var(var).ok().filter(|_| cfg!(debug_assertions));
        Ok(bridges::Api {
            http: self.http.clone(),
            api: debug("BEEPER_GPUI_API").unwrap_or_else(|| beeper::API_URL.to_owned()),
            homeserver: debug("BEEPER_GPUI_BRIDGES").unwrap_or_else(|| self.homeserver.clone()),
            token: client.access_token().context("Not signed in")?,
            user_id: client.user_id().context("Not signed in")?.to_string(),
        })
    }

    fn room(&self, id: &OwnedRoomId) -> Result<Room> {
        self.client()?.get_room(id).ok_or_else(|| anyhow!("Unknown room {id}"))
    }
}

/// One backward `/messages` page. Undecryptable events get their keys from the server-side backup
/// (a no-op until recovery has imported the backup key), then the page is fetched again.
async fn messages(room: &Room, from: Option<&str>, limit: u16) -> Result<Messages> {
    let opts = || {
        let mut o = MessagesOptions::backward().from(from);
        o.limit = UInt::from(limit);
        o
    };
    let resp = room.messages(opts()).await?;
    let sessions: BTreeSet<String> = resp
        .chunk
        .iter()
        .filter_map(|e| match &e.kind {
            TimelineEventKind::UnableToDecrypt { utd_info, .. } => utd_info.session_id.clone(),
            _ => None,
        })
        .collect();
    let backups = room.client().encryption().backups();
    let mut found = false;
    for session in sessions.iter().take(MAX_KEY_FETCHES) {
        match backups.download_room_key(room.room_id(), session).await {
            Ok(f) => found |= f,
            Err(e) => {
                log::warn!("backup key for {}: {e}", room.room_id());
                break;
            }
        }
    }
    Ok(if found { room.messages(opts()).await? } else { resp })
}

/// List preview: the newest message on one line, prefixed with its sender outside one-to-one chats.
fn preview(msgs: &[Msg], dm: bool) -> Option<String> {
    let m = msgs.last()?;
    let text = match m {
        m if m.own => format!("You: {}", m.summary()),
        m if dm => m.summary(),
        m => format!("{}: {}", m.sender, m.summary()),
    };
    Some(text.replace('\n', " "))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Files downloaded to be opened by other apps. Emptied at startup, so it holds one session's worth.
fn media_dir(dir: &Path) -> PathBuf {
    dir.join("media")
}

/// A file name with nothing that could leave its directory.
fn safe_name(name: &str) -> String {
    let clean: String = name.chars().map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') { c } else { '_' }).collect();
    let clean = clean.trim_matches(['.', ' ']);
    if clean.is_empty() { "file".to_owned() } else { clean.chars().take(120).collect() }
}

/// Re-encodes an image to fit `max` pixels on its longest side: JPEG, or PNG when it has alpha.
fn shrink(bytes: &[u8], max: u32) -> Result<Vec<u8>> {
    let image = image::load_from_memory(bytes)?;
    let image = if image.width().max(image.height()) > max { image.thumbnail(max, max) } else { image };
    let mut out = std::io::Cursor::new(Vec::new());
    if image.color().has_alpha() {
        image.write_to(&mut out, image::ImageFormat::Png)?;
    } else {
        image.to_rgb8().write_to(&mut out, image::ImageFormat::Jpeg)?;
    }
    Ok(out.into_inner())
}

/// An inline thumbnail: the sender's own thumbnail when there is one, else the server's for a
/// plain image, else the image itself. `None` when there is nothing reasonable to fetch.
async fn thumbnail(client: &Client, media: &Media) -> Result<Option<Vec<u8>>> {
    let pictured = matches!(media.kind, MediaKind::Image | MediaKind::Sticker);
    let request = match (&media.thumb, &media.source) {
        (Some(thumb), _) => MediaRequestParameters { source: thumb.clone(), format: MediaFormat::File },
        (None, MediaSource::Plain(_)) if pictured => MediaRequestParameters {
            source: media.source.clone(),
            format: MediaFormat::Thumbnail(MediaThumbnailSettings::new(THUMB_PX.into(), THUMB_PX.into())),
        },
        (None, _) if pictured && media.size.is_none_or(|s| s <= MAX_THUMB_SOURCE) => {
            MediaRequestParameters { source: media.source.clone(), format: MediaFormat::File }
        }
        _ => return Ok(None),
    };
    let bytes = client.media().get_media_content(&request, true).await?;
    Ok(Some(tokio::task::spawn_blocking(move || shrink(&bytes, THUMB_PX)).await??))
}

/// Uploads a file as the message type its content calls for.
async fn attach(room: &Room, path: &Path, reply_to: Option<String>) -> Result<()> {
    let size = tokio::fs::metadata(path).await?.len();
    if size > MAX_UPLOAD {
        return Err(anyhow!("the file is larger than {} MB", MAX_UPLOAD / 1024 / 1024));
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".to_owned());
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let data = tokio::fs::read(path).await?;
    let mut config = AttachmentConfig::new();
    if mime.type_() == mime_guess::mime::IMAGE {
        // Bridges and other clients lay the bubble out from these.
        if let Ok((width, height)) = image::ImageReader::new(std::io::Cursor::new(&data)).with_guessed_format()?.into_dimensions() {
            config = config.info(AttachmentInfo::Image(BaseImageInfo {
                width: Some(width.into()),
                height: Some(height.into()),
                size: UInt::new(size),
                ..Default::default()
            }));
        }
    }
    if let Some(id) = reply_to {
        let reply = Reply {
            event_id: EventId::parse(id)?,
            enforce_thread: EnforceThread::MaybeThreaded,
            add_mentions: AddMentions::Yes,
        };
        config = config.reply(Some(reply));
    }
    room.send_attachment(name, &mime, data, config).await?;
    Ok(())
}

/// One message to put in a chat's notification.
#[cfg_attr(not(target_os = "android"), expect(dead_code, reason = "only Android shows notifications"))]
pub(crate) struct Note {
    pub sender: String,
    pub text: String,
    pub ts: u64,
    pub mention: bool,
}

/// Most messages of one sync a notification is given; it shows the last few of a chat anyway.
const MAX_NOTES: usize = 6;
/// How long to wait for a chat's picture before notifying without it.
const ICON_WAIT: Duration = Duration::from_secs(3);

/// The messages these new items call for a notification of, oldest first: those from someone else
/// that the account's push rules ask to be notified of (they cover mutes and mentions-only chats).
/// Old messages do not count: a bridge bringing in history sends them through sync too.
fn notes(msgs: &[Msg], now_ms: i64) -> Vec<Note> {
    let recent = |m: &Msg| now_ms - (m.ts as i64) < NOTIFY_WINDOW.as_millis() as i64;
    let mut found: Vec<Note> = msgs
        .iter()
        .rev()
        .filter(|m| m.notify && !m.own && recent(m))
        .map(|m| Note { sender: m.sender.clone(), text: m.summary(), ts: m.ts, mention: m.mentioned })
        .take(MAX_NOTES)
        .collect();
    found.reverse();
    found
}

/// Notifies while the window is not in front; clears the chat's notification once it has been
/// read somewhere. Only Android has notifications.
fn announce(client: &Client, dir: &Path, summary: &RoomSummary, prev: Option<&RoomSummary>, msgs: &[Msg]) {
    if !cfg!(target_os = "android") {
        return;
    }
    let chat = summary.id.as_str();
    if summary.unread == 0 && prev.is_some_and(|p| p.unread > 0) {
        crate::clear_notification(chat);
    }
    if crate::FOREGROUND.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let notes = notes(msgs, now_ms());
    if notes.is_empty() {
        return;
    }
    let (client, dir, summary) = (client.clone(), dir.to_owned(), summary.clone());
    // Off the sync loop: the picture may have to be fetched.
    tokio::spawn(async move {
        let icon = tokio::time::timeout(ICON_WAIT, notification_icon(&client, &dir, &summary.id)).await.ok().flatten();
        let icon = icon.as_deref().and_then(Path::to_str).unwrap_or_default();
        for (n, note) in notes.iter().enumerate() {
            let alert = n + 1 == notes.len();
            crate::notify(&crate::Notice { chat: summary.id.as_str(), name: &summary.name, group: !summary.dm, icon, note, alert });
        }
    });
}

/// The chat's picture as a file Android can read, one small file per chat that has notified.
async fn notification_icon(client: &Client, dir: &Path, room: &RoomId) -> Option<PathBuf> {
    let bytes = avatar(client, room).await.ok()??;
    let folder = dir.join("notify");
    std::fs::create_dir_all(&folder).ok()?;
    let name: String = room.as_str().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let path = folder.join(name);
    write_private(&path, &bytes).ok()?;
    Some(path)
}

/// Unread messages that mention the user, counted here because a server cannot see into
/// encrypted messages. The last `unread` messages from others are the unread ones; `msgs` are the
/// newest, oldest first, and `before` is the count from earlier ones.
fn unread_mentions(before: u64, unread: u64, msgs: &[Msg]) -> u64 {
    let theirs: Vec<&Msg> = msgs.iter().filter(|m| !m.own).collect();
    let fresh = theirs.iter().rev().take(usize::try_from(unread).unwrap_or(usize::MAX)).filter(|m| m.mentioned).count() as u64;
    // When the unread ones reach back past these items, what was counted before still stands.
    let earlier = if (theirs.len() as u64) < unread { before } else { 0 };
    (earlier + fresh).min(unread)
}

/// Ends a background task with the task that owns it.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Delivers the answer to a settings-page request, or its error and the cue to stop waiting.
async fn panel(ev: &mpsc::Sender<Ev>, result: Result<Ev>) {
    match result {
        Ok(answer) => {
            let _ = ev.send(answer).await;
        }
        Err(e) => {
            let _ = ev.send(Ev::Error(format!("{e:#}"))).await;
            let _ = ev.send(Ev::PanelFailed).await;
        }
    }
}

fn rooms_path(dir: &Path) -> PathBuf {
    dir.join("rooms.json")
}

fn load_rooms(dir: &Path) -> Option<HashMap<OwnedRoomId, RoomSummary>> {
    serde_json::from_slice(&std::fs::read(rooms_path(dir)).ok()?).ok()
}

async fn watch_recovery(client: Client, ev: mpsc::Sender<Ev>) {
    let mut states = client.encryption().recovery().state_stream();
    while let Some(state) = states.next().await {
        if state == RecoveryState::Unknown {
            continue;
        }
        if ev.send(Ev::NeedRecovery(state == RecoveryState::Incomplete)).await.is_err() {
            break;
        }
    }
}

const DONE: &str = "com.beeper.inbox.done";

/// Beeper's archive mark: the chat is done up to this point of its timeline.
#[derive(Default, Serialize, Deserialize)]
struct Done {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_ts: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    at_order: Option<i64>,
}

/// Clears the unread state. A private receipt does that without telling the other side, or the
/// bridged network, that the message was read.
async fn mark_read(room: &Room, newest: Option<OwnedEventId>, private: bool) -> Result<()> {
    room.set_unread_flag(false).await?;
    if let Some(id) = newest {
        let kind = if private { create_receipt::v3::ReceiptType::ReadPrivate } else { create_receipt::v3::ReceiptType::Read };
        room.send_single_receipt(kind, ReceiptThread::Unthreaded, id).await?;
    }
    Ok(())
}

/// Beeper's "mark as read when archiving" setting; on unless the account turned it off.
async fn mark_read_on_archive(client: &Client) -> bool {
    #[derive(Deserialize)]
    struct Pref {
        enabled: Option<bool>,
    }
    match client.account().account_data_raw("com.beeper.mark_read_when_archive".into()).await {
        Ok(Some(raw)) => raw.deserialize_as_unchecked::<Pref>().ok().and_then(|p| p.enabled).unwrap_or(false),
        _ => true,
    }
}

fn is_redacted(event: &TimelineEvent) -> bool {
    #[derive(Deserialize)]
    struct Unsigned {
        redacted_because: Option<serde::de::IgnoredAny>,
    }
    event.raw().get_field::<Unsigned>("unsigned").ok().flatten().is_some_and(|u| u.redacted_because.is_some())
}

fn hs_order(event: &TimelineEvent) -> Option<i64> {
    #[derive(Deserialize)]
    struct Unsigned {
        #[serde(rename = "com.beeper.hs.order")]
        order: Option<i64>,
    }
    event.raw().get_field::<Unsigned>("unsigned").ok().flatten()?.order
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Same rule as Beeper Desktop: a pinned chat is never archived, and a message newer than the mark
/// brings the chat back. `at_order` wins when both sides know the server order; else timestamps.
fn is_done(done: &Done, s: &RoomSummary, now: i64) -> bool {
    if s.pinned {
        return false;
    }
    if let Some(at) = done.at_order.filter(|o| *o != 0) {
        return at >= if s.order != 0 { s.order } else { i64::MIN };
    }
    done.updated_ts.is_some_and(|upto| upto > 0 && upto >= s.ts as i64 && upto < now && !s.is_unread())
}

/// A probe changed a summary since rooms.json was last written.
static UNSAVED_PROBES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The chat's last message as a preview and its time; `None` when the newest pages hold none.
/// A room a bridge has just set up ends in a run of state events, hence more than one page.
async fn last_message(room: &Room) -> Result<Option<(String, u64)>> {
    let dm = other_person(room).await.is_some();
    let mut from = None;
    for _ in 0..MAX_PREVIEW_PAGES {
        let resp = messages(room, from.as_deref(), PAGE).await?;
        let mut chunk = resp.chunk;
        chunk.reverse();
        let msgs = to_msgs(room, &chunk).await;
        if let (Some(text), Some(sent)) = (preview(&msgs, dm), msgs.last().map(|m| m.ts)) {
            return Ok(Some((text, sent)));
        }
        from = resp.end;
        if from.is_none() {
            break;
        }
    }
    Ok(None)
}

/// Looks the chat's last message up and records it. Returns the summary when it changed.
/// rooms.json picks the change up on the sync loop's next save.
async fn probe(room: &Room, rooms: &Rooms) -> Option<RoomSummary> {
    let found = match last_message(room).await {
        Ok(found) => found,
        // Left undated, so the next start tries again.
        Err(e) => {
            log::debug!("last message of {}: {e:#}", room.room_id());
            return None;
        }
    };
    let mut rooms = rooms.lock().unwrap();
    let r = rooms.get_mut(room.room_id())?;
    UNSAVED_PROBES.store(true, std::sync::atomic::Ordering::Relaxed);
    r.dated = true;
    match found {
        Some((text, sent)) => (r.preview, r.ts) = (Some(text), sent),
        // No message and no preview: whatever time it had came from a state event.
        None if r.preview.is_none() => r.ts = 0,
        None => {}
    }
    Some(r.clone())
}

/// Looks up the last message of chats where it is not known, a few at a time, as their IDs come in.
async fn probe_queue(client: Client, rooms: Rooms, ev: mpsc::Sender<Ev>, queue: mpsc::Receiver<OwnedRoomId>) {
    let ids = futures_util::stream::unfold(queue, |mut queue| async { queue.recv().await.map(|id| (id, queue)) });
    ids.for_each_concurrent(PROBES, |id| {
        let (client, rooms, ev) = (&client, &rooms, &ev);
        async move {
            let Some(room) = client.get_room(&id) else { return };
            if let Some(summary) = probe(&room, rooms).await {
                let _ = ev.send(Ev::Rooms(vec![summary])).await;
            }
        }
    })
    .await;
}

/// A cached preview in today's form. Caches written by earlier builds held bracketed media labels,
/// which are dropped so the row reads the message again, and previews of several paragraphs.
fn current(preview: Option<String>) -> Option<String> {
    let stale = |p: &String| ["[Photo]", "[Video]", "[Audio]", "[File]", "[Location]"].iter().any(|old| p.contains(old));
    preview.filter(|p| !stale(p)).map(|p| p.replace('\n', " "))
}

/// Everything the local store knows about a room. `preview`, `ts` and `order` come from the
/// timeline, so the caller passes what it saw; the rest is read here.
async fn summarize(
    room: &Room,
    notif: &NotificationSettings,
    peer: Option<OwnedUserId>,
    seen: RoomSummary,
) -> RoomSummary {
    let name = match &peer {
        // A bridged one-to-one room also holds the bridge bot, which the SDK's computed name
        // counts: "Ann, and 2 others". Beeper titles it with the other person alone.
        Some(peer) if room.name().is_none() => display_name(room, peer).await,
        _ => room.display_name().await.map(|n| n.to_string()).unwrap_or_else(|_| seen.id.to_string()),
    };
    let network = match seen.network {
        Some(n) => Some(n),
        None => network(room).await,
    };
    let mode = notif.get_user_defined_room_notification_mode(room.room_id()).await;
    let done = match room.account_data(DONE.into()).await {
        Ok(Some(raw)) => raw.get_field::<Done>("content").ok().flatten().unwrap_or_default(),
        _ => Done::default(),
    };
    let counts = room.unread_notification_counts();
    let mut s = RoomSummary {
        name,
        network,
        dm: peer.is_some(),
        preview: current(seen.preview),
        unread: counts.notification_count,
        // Once the chat is read nothing in it is an unread mention.
        mentions: seen.mentions.max(counts.highlight_count).min(counts.notification_count),
        pinned: room.is_favourite(),
        low_priority: room.is_low_priority(),
        muted: matches!(mode, Some(RoomNotificationMode::Mute | RoomNotificationMode::MentionsAndKeywordsOnly)),
        marked_unread: room.is_marked_unread(),
        ..seen
    };
    s.archived = is_done(&done, &s, now_ms());
    s
}

/// Removes cached chats the account is no longer in. The sync filter does not ask for left rooms
/// (a first sync would then carry every chat ever left), so a chat left on another device stays
/// "joined" here; the server's own list settles it with one request. Skipped when offline.
async fn drop_left(client: &Client, rooms: &Rooms, ev: &mpsc::Sender<Ev>) {
    let joined: std::collections::HashSet<OwnedRoomId> = match client.send(joined_rooms::v3::Request::new()).await {
        Ok(response) => response.joined_rooms.into_iter().collect(),
        Err(e) => return log::debug!("joined rooms: {e}"),
    };
    let left: Vec<OwnedRoomId> = rooms.lock().unwrap().keys().filter(|id| !joined.contains(*id)).cloned().collect();
    for id in left {
        rooms.lock().unwrap().remove(&id);
        let _ = ev.send(Ev::RoomGone(id)).await;
    }
}

/// Rooms without new events never pass through the sync loop, so their cached summaries miss
/// fields added since and changes made on other devices. Store reads only.
async fn refresh_cached(client: &Client, notif: &NotificationSettings, rooms: &Rooms) -> Vec<RoomSummary> {
    let cached: Vec<RoomSummary> = rooms.lock().unwrap().values().cloned().collect();
    let mut out = Vec::with_capacity(cached.len());
    for seen in cached {
        let Some(room) = client.get_room(&seen.id) else { continue };
        let fresh = summarize(&room, notif, other_person(&room).await, seen).await;
        if let Some(r) = rooms.lock().unwrap().get_mut(&fresh.id) {
            // A preview refresh may have updated the entry meanwhile.
            *r = RoomSummary { preview: current(r.preview.take()), ts: r.ts, dated: r.dated, ..fresh };
            // An unverified time on a chat with no preview came from a state event: it has no
            // business near the top while it waits for its look.
            if !r.dated && r.preview.is_none() {
                r.ts = 0;
            }
            out.push(r.clone());
        }
    }
    out
}

async fn sync_loop(client: Client, notif: NotificationSettings, dir: PathBuf, rooms: Rooms, ev: mpsc::Sender<Ev>) {
    drop_left(&client, &rooms, &ev).await;
    let cached = refresh_cached(&client, &notif, &rooms).await;
    // Chats whose last message is not known are looked up in the background. A full queue drops
    // the rest; they stay undated and are queued again at the next start.
    let (to_probe, queue) = mpsc::channel(PROBE_QUEUE);
    let prober = tokio::spawn(probe_queue(client.clone(), rooms.clone(), ev.clone(), queue));
    let _stop_prober = AbortOnDrop(prober);
    // Top of the list first: those are the rows in view.
    let mut undated: Vec<&RoomSummary> = cached.iter().filter(|r| !r.dated).collect();
    undated.sort_unstable_by_key(|r| std::cmp::Reverse(r.ts));
    for unknown in undated {
        let _ = to_probe.try_send(unknown.id.clone());
    }
    if !cached.is_empty() && ev.send(Ev::Rooms(cached)).await.is_err() {
        return;
    }
    let mut filter = RoomFilter::with_lazy_loading();
    filter.timeline.limit = Some(UInt::from(SYNC_TIMELINE_LIMIT));
    let mut def = FilterDefinition::default();
    def.room = filter;
    // Without a summary cache the stored sync token would skip rooms with no new events.
    let token = if rooms.lock().unwrap().is_empty() { SyncToken::NoToken } else { SyncToken::ReusePrevious };
    let settings = SyncSettings::default().filter(def.into()).token(token);

    let mut stream = std::pin::pin!(client.sync_stream(settings).await);
    // Requests failed in a row.
    let mut failed = 0u32;
    while let Some(resp) = stream.next().await {
        let resp = match resp {
            Ok(r) => r,
            Err(e) if matches!(e.client_api_error_kind(), Some(ErrorKind::UnknownToken(_))) => {
                let _ = std::fs::remove_file(dir.join("session.json"));
                let _ = ev.send(Ev::Error(format!("Signed out by server: {e}"))).await;
                let _ = ev.send(Ev::NeedLogin).await;
                return;
            }
            Err(e) => {
                failed += 1;
                log::warn!("sync: {}", plain(&e));
                // One failed request is a blip (a tunnel, a change of network); two in a row is
                // being offline. Either way it is retried, so it is a state to show, not an error.
                if failed == OFFLINE_AFTER && ev.send(Ev::Online(false)).await.is_err() {
                    return;
                }
                tokio::time::sleep(retry_wait(failed)).await;
                continue;
            }
        };
        if std::mem::take(&mut failed) >= OFFLINE_AFTER {
            // A message that failed while offline stopped its chat's queue.
            client.send_queue().set_enabled(true).await;
            if ev.send(Ev::Online(true)).await.is_err() {
                return;
            }
        }
        let mut changed = Vec::new();
        for (id, update) in &resp.rooms.joined {
            let Some(room) = client.get_room(id) else { continue };
            if room.is_space() {
                continue;
            }
            let msgs = to_msgs(&room, &update.timeline.events).await;
            let prev = rooms.lock().unwrap().get(id).cloned();
            // A chat's time is its last message's. A room a bridge has just created has only state
            // events, which say nothing about when the chat was last used: it stays at 0, sorts
            // last, and is queued for a look at its history.
            let last_message = msgs.iter().map(|m| m.ts).max();
            let dated = last_message.is_some() || prev.as_ref().is_some_and(|p| p.dated);
            if !dated {
                let _ = to_probe.try_send(id.clone());
            }
            let newest = last_message.max(prev.as_ref().map(|p| p.ts));
            let peer = other_person(&room).await;
            let dm = peer.is_some();
            let order = msgs.iter().filter(|m| !m.own).map(|m| m.order);
            let seen = RoomSummary {
                id: id.clone(),
                name: String::new(),
                preview: preview(&msgs, dm).or_else(|| prev.as_ref().and_then(|p| p.preview.clone())),
                ts: newest.unwrap_or(0),
                network: prev.as_ref().and_then(|p| p.network.clone()),
                order: order.max().max(prev.as_ref().map(|p| p.order)).unwrap_or(0),
                unread: 0,
                dm,
                mentions: prev.as_ref().map_or(0, |p| p.mentions),
                pinned: false,
                low_priority: false,
                muted: false,
                marked_unread: false,
                archived: false,
                dated,
            };
            let mut summary = summarize(&room, &notif, peer, seen).await;
            summary.mentions = unread_mentions(summary.mentions, summary.unread, &msgs);
            announce(&client, &dir, &summary, prev.as_ref(), &msgs);
            rooms.lock().unwrap().insert(id.clone(), summary.clone());
            changed.push(summary);
        }
        let mut gone = false;
        for id in resp.rooms.left.keys() {
            if rooms.lock().unwrap().remove(id).is_some() {
                gone = true;
                let _ = ev.send(Ev::RoomGone(id.clone())).await;
            }
        }
        // Probes change summaries between syncs; only this loop writes the file.
        if !changed.is_empty() || gone || UNSAVED_PROBES.swap(false, std::sync::atomic::Ordering::Relaxed) {
            let saved = serde_json::to_vec(&*rooms.lock().unwrap()).map_err(anyhow::Error::from);
            if let Err(e) = saved.and_then(|b| write_private(&rooms_path(&dir), &b)) {
                log::warn!("rooms.json: {e:#}");
            }
        }
        // Mutes are push rules: one global event changes any number of rooms.
        let rules_changed = resp.account_data.iter().any(|e| e.get_field::<String>("type").ok().flatten().as_deref() == Some("m.push_rules"));
        if rules_changed {
            changed = refresh_cached(&client, &notif, &rooms).await;
        }
        if !changed.is_empty() && ev.send(Ev::Rooms(changed)).await.is_err() {
            return;
        }
    }
}

/// Failed syncs in a row after which the UI is told the connection is lost.
const OFFLINE_AFTER: u32 = 2;

/// How long to wait before the sync after `failed` failures in a row: 5 s doubling to a minute,
/// so a phone without a network does not wake its radio every few seconds.
fn retry_wait(failed: u32) -> Duration {
    Duration::from_secs((5u64 << failed.saturating_sub(1).min(4)).min(60))
}

/// An error as text for a person: without the address of the request, which an HTTP error
/// carries in full and nobody on the other side of the screen can use.
pub(crate) fn plain(error: &dyn std::fmt::Display) -> String {
    let mut text = error.to_string();
    while let Some(start) = text.find(" for url (") {
        let end = text[start..].find(')').map_or(text.len(), |at| start + at + 1);
        text.replace_range(start..end, "");
    }
    text
}

pub(crate) const UTD: &str = "Unable to decrypt this message yet";
pub(crate) const DELETED: &str = "Message deleted";

/// The messages among timeline events, oldest first, as the chat list and notifications need
/// them: what each says, by whom and when. Edits, reactions and deletions belong to the open
/// conversation, which the SDK's timeline keeps.
async fn to_msgs(room: &Room, events: &[TimelineEvent]) -> Vec<Msg> {
    let own = room.own_user_id().to_owned();
    let mut names: HashMap<OwnedUserId, String> = HashMap::new();
    let mut out = Vec::new();
    for event in events {
        let Some(id) = event.event_id().map(|e| e.to_string()) else { continue };
        let Some(sender) = event.sender() else { continue };
        let mut m = Msg::bare(id, Kind::Message, sender.clone());
        // A deleted event in an encrypted room is an empty `m.room.encrypted`: nothing says whether
        // it was a message or a reaction, so it gets no placeholder.
        if event.kind.is_utd() {
            if is_redacted(event) {
                continue;
            }
            m.body = UTD.to_owned();
        } else if let Some(poll) = poll_start(event) {
            m.poll = Some(poll);
        } else {
            match event.raw().deserialize() {
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(SyncMessageLikeEvent::Original(e)))) => {
                    let reply = match e.content.relates_to {
                        Some(Relation::Replacement(_)) => continue,
                        Some(Relation::Reply(_)) => true,
                        _ => false,
                    };
                    let (media, text) = content(&e.content.msgtype);
                    m.media = media;
                    if let MessageType::Location(l) = &e.content.msgtype {
                        m.place = Place::parse(&l.geo_uri, &l.body);
                    }
                    // Older clients and bridges prepend the quoted message to the body.
                    let text = if reply { remove_plain_reply_fallback(&text).to_owned() } else { text };
                    // The place's description is told with the place.
                    m.body = if m.place.is_some() { String::new() } else { text };
                    (m.mentions, m.mentioned) = marks(&m.body, &e.content.msgtype, e.content.mentions.as_ref(), &own);
                }
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(SyncMessageLikeEvent::Redacted(_)))) => {
                    (m.body, m.deleted) = (DELETED.to_owned(), true);
                }
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::Sticker(SyncMessageLikeEvent::Original(s)))) => {
                    m.media = Some(sticker(&s.content));
                }
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomEncrypted(_))) if !is_redacted(event) => {
                    m.body = UTD.into();
                }
                _ => continue,
            }
        }
        m.sender = match names.get(&sender) {
            Some(n) => n.clone(),
            None => {
                let n = display_name(room, &sender).await;
                names.insert(sender.clone(), n.clone());
                n
            }
        };
        m.ts = event.timestamp().map_or(0, |t| u64::from(t.0));
        m.order = hs_order(event).unwrap_or(0);
        m.own = sender == own;
        m.mentioned &= !m.own;
        m.notify = event.push_actions().is_some_and(|actions| actions.iter().any(|a| a.should_notify()));
        out.push(m);
    }
    out
}

fn sticker(content: &StickerEventContent) -> Media {
    let info = &content.info;
    Media {
        kind: MediaKind::Sticker,
        name: content.body.clone(),
        size: info.size.map(Into::into),
        mime: info.mimetype.clone(),
        dims: dims(info.width, info.height),
        duration: None,
        source: content.source.clone().into(),
        thumb: info.thumbnail_source.clone(),
    }
}

/// A poll's start, whichever flavour of event type.
fn poll_start(event: &TimelineEvent) -> Option<Poll> {
    let kind = event.raw().get_field::<String>("type").ok().flatten().filter(|k| polls::is_start(k))?;
    let content = event.raw().get_field::<serde_json::Value>("content").ok().flatten()?;
    Poll::parse(&content, kind.starts_with("org."))
}

/// Longest title or description kept from a preview: it is someone else's text.
const LINK_TEXT_MAX: usize = 300;

/// The first usable link preview in a message's content.
fn link_preview(content: &serde_json::Value) -> Option<Link> {
    use serde_json::Value;
    let previews = content.get("com.beeper.linkpreviews")?.as_array()?;
    previews.iter().find_map(|p| {
        let text = |key: &str| {
            let value = p.get(key).and_then(Value::as_str).unwrap_or_default().trim();
            value.chars().take(LINK_TEXT_MAX).collect::<String>()
        };
        // Widths come as numbers or as strings.
        let number = |key: &str| p.get(key).and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok())).and_then(|n| u32::try_from(n).ok());
        let url = [text("matched_url"), text("og:url")].into_iter().find(|u| u.starts_with("https://") || u.starts_with("http://"))?;
        let (title, description) = (text("og:title"), text("og:description"));
        let mxc = p.get("og:image").and_then(Value::as_str).filter(|u| u.starts_with("mxc://"));
        // In an encrypted room the picture is an encrypted file, whose address may be given once.
        let encrypted = p.get("beeper:image:encryption").filter(|v| v.is_object()).and_then(|file| {
            let mut file = file.clone();
            if file.get("url").is_none() {
                file["url"] = mxc?.into();
            }
            serde_json::from_value::<EncryptedFile>(file).ok()
        });
        let source = match encrypted {
            Some(file) => Some(MediaSource::Encrypted(Box::new(file))),
            None => mxc.map(|uri| MediaSource::Plain(uri.into())),
        };
        if title.is_empty() && description.is_empty() && source.is_none() {
            return None;
        }
        let host = url.split("://").nth(1).and_then(|rest| rest.split(['/', '?', '#']).next()).unwrap_or_default();
        let site = Some(text("og:site_name")).filter(|s| !s.is_empty()).unwrap_or_else(|| host.trim_start_matches("www.").to_owned());
        let image = source.map(|source| Media {
            kind: MediaKind::Image,
            name: String::new(),
            size: None,
            mime: Some(text("og:image:type")).filter(|m| !m.is_empty()),
            dims: number("og:image:width").zip(number("og:image:height")),
            duration: None,
            source,
            thumb: None,
        });
        Some(Link { url, title, description, site, image })
    })
}

/// Where `body` mentions someone, and whether it mentions `own`. Matrix writes a mention twice:
/// as a link to the user in the HTML form, and in `m.mentions`.
fn marks(body: &str, msgtype: &MessageType, mentions: Option<&Mentions>, own: &UserId) -> (Vec<std::ops::Range<usize>>, bool) {
    let html = match msgtype {
        MessageType::Text(c) => c.formatted.as_ref(),
        MessageType::Notice(c) => c.formatted.as_ref(),
        MessageType::Emote(c) => c.formatted.as_ref(),
        _ => None,
    };
    let linked = html.map(|f| linked_users(&f.body)).unwrap_or_default();
    let room = mentions.is_some_and(|m| m.room);
    let mentioned = room || mentions.is_some_and(|m| m.user_ids.contains(own)) || linked.iter().any(|(user, _)| user == own.as_str());
    let names: Vec<&str> = linked.iter().map(|(_, name)| name.as_str()).collect();
    (mention_ranges(body, &names, room), mentioned)
}

/// Users linked in a message's HTML as (user ID, link text).
fn linked_users(html: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("<a ") {
        rest = &rest[at + 3..];
        let Some(close) = rest.find('>') else { break };
        let (tag, after) = (&rest[..close], &rest[close + 1..]);
        let Some(end) = after.find("</a>") else { break };
        rest = &after[end..];
        let href = tag.split_once("href=\"").and_then(|(_, h)| h.split('"').next()).unwrap_or_default();
        let Some((_, target)) = href.split_once("matrix.to/#/") else { continue };
        let user = target.split(['?', '/']).next().unwrap_or_default().replace("%40", "@").replace("%3A", ":").replace("%3a", ":");
        if !user.starts_with('@') {
            continue;
        }
        // Link text may carry markup of its own.
        let mut name = String::new();
        let mut in_tag = false;
        for ch in after[..end].chars() {
            match ch {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => name.push(ch),
                _ => {}
            }
        }
        let name = name.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&");
        out.push((user, name.trim().to_owned()));
    }
    out
}

/// Byte ranges of `body` to draw as mentions: each linked name with the `@` before it, and, when
/// the whole room is addressed, every `@word` (networks name it differently: @room, @everyone).
fn mention_ranges(body: &str, names: &[&str], room: bool) -> Vec<std::ops::Range<usize>> {
    let word = |c: char| c.is_alphanumeric();
    let mut found: Vec<std::ops::Range<usize>> = Vec::new();
    for name in names.iter().filter(|n| !n.is_empty()) {
        for (at, _) in body.match_indices(name) {
            let end = at + name.len();
            let start = if body[..at].ends_with('@') { at - 1 } else { at };
            // A name inside a longer word is not a mention of it.
            if !body[..start].ends_with(word) && !body[end..].starts_with(word) {
                found.push(start..end);
            }
        }
    }
    if room {
        for (at, _) in body.match_indices('@') {
            if !body[..at].ends_with(word) {
                let len = body[at..].find(char::is_whitespace).unwrap_or(body.len() - at);
                if len > 1 {
                    found.push(at..at + len);
                }
            }
        }
    }
    found.sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
    let mut out: Vec<std::ops::Range<usize>> = Vec::with_capacity(found.len());
    for r in found {
        match out.last() {
            Some(last) if r.start < last.end => {}
            _ => out.push(r),
        }
    }
    out
}

async fn display_name(room: &Room, user: &UserId) -> String {
    match room.get_member_no_sync(user).await {
        Ok(Some(m)) => m.name().to_owned(),
        _ => user.localpart().to_owned(),
    }
}

/// The other person in a one-to-one chat. Bridged DMs also hold a bridge bot, listed as a service member.
async fn other_person(room: &Room) -> Option<OwnedUserId> {
    let own = room.own_user_id();
    let service = room.service_members().unwrap_or_default();
    // Lazy loading leaves most members unloaded, so the size comes from the room summary.
    if room.joined_members_count() > 2 + service.len() as u64 {
        return None;
    }
    // Synapse sends heroes only for unnamed rooms; bridged DMs are named.
    let mut ids: Vec<OwnedUserId> = room.clone_info().heroes().iter().map(|h| h.user_id.clone()).collect();
    if ids.is_empty() {
        ids = room.members_no_sync(RoomMemberships::JOIN).await.ok()?.iter().map(|m| m.user_id().to_owned()).collect();
    }
    let mut people = ids.into_iter().filter(|u| u != own && !service.contains(u));
    let first = people.next()?;
    people.next().is_none().then_some(first)
}

async fn network(room: &Room) -> Option<String> {
    #[derive(Deserialize)]
    struct Protocol {
        id: String,
        displayname: Option<String>,
    }
    let events = room.get_state_events(StateEventType::from("m.bridge")).await.ok()?;
    events.iter().find_map(|e| {
        let RawAnySyncOrStrippedState::Sync(raw) = e else { return None };
        let content: serde_json::Value = raw.get_field("content").ok()??;
        let p: Protocol = serde_json::from_value(content.get("protocol")?.clone()).ok()?;
        Some(p.displayname.unwrap_or(p.id))
    })
}

/// Room avatar, or for a one-to-one chat the other person's, as a small thumbnail.
async fn avatar(client: &Client, room_id: &RoomId) -> Result<Option<Vec<u8>>> {
    let Some(room) = client.get_room(room_id) else { return Ok(None) };
    let url = match room.avatar_url() {
        Some(url) => url,
        None => {
            let Some(person) = other_person(&room).await else { return Ok(None) };
            let member = room.get_member_no_sync(&person).await?.and_then(|m| m.avatar_url().map(ToOwned::to_owned));
            let url = match member {
                Some(url) => Some(url),
                None => client.account().fetch_user_profile_of(&person).await?.get_static::<AvatarUrl>()?,
            };
            let Some(url) = url else { return Ok(None) };
            url
        }
    };
    Ok(Some(picture(client, url).await?))
}

/// A member's picture in a chat; bridges set it per chat, so the member event is asked, not the
/// profile.
async fn face(client: &Client, room_id: &RoomId, user: &UserId) -> Result<Option<Vec<u8>>> {
    let Some(room) = client.get_room(room_id) else { return Ok(None) };
    let Some(url) = room.get_member_no_sync(user).await?.and_then(|m| m.avatar_url().map(ToOwned::to_owned)) else { return Ok(None) };
    Ok(Some(picture(client, url).await?))
}

/// A small square thumbnail of an avatar, from the media cache when it is there.
async fn picture(client: &Client, url: matrix_sdk::ruma::OwnedMxcUri) -> Result<Vec<u8>> {
    let format = MediaFormat::Thumbnail(MediaThumbnailSettings::with_method(Method::Crop, uint!(96), uint!(96)));
    let request = MediaRequestParameters { source: MediaSource::Plain(url), format };
    Ok(square(client.media().get_media_content(&request, true).await?))
}

/// Side of a stored avatar, in pixels.
const AVATAR_PX: u32 = 96;
/// Largest picture decoded to make an avatar: some servers answer a thumbnail request with the
/// original upload.
const AVATAR_SOURCE_PX: u32 = 4096;

/// A picture as an upright square for a round frame. Servers return some avatars uncropped and
/// with their rotation in EXIF, which the renderer does not read. Anything that cannot be decoded
/// is passed on as it came.
fn square(bytes: Vec<u8>) -> Vec<u8> {
    use image::ImageDecoder as _;
    let upright = || {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().ok()?;
        let mut limits = image::Limits::default();
        (limits.max_image_width, limits.max_image_height) = (Some(AVATAR_SOURCE_PX), Some(AVATAR_SOURCE_PX));
        reader.limits(limits);
        let mut decoder = reader.into_decoder().ok()?;
        let orientation = decoder.orientation().ok()?;
        let mut picture = image::DynamicImage::from_decoder(decoder).ok()?;
        picture.apply_orientation(orientation);
        Some((picture, orientation != image::metadata::Orientation::NoTransforms))
    };
    let Some((picture, turned)) = upright() else { return bytes };
    let (w, h) = (picture.width(), picture.height());
    if w == h && w <= AVATAR_PX && !turned {
        return bytes;
    }
    let side = w.min(h);
    let cropped = picture.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    // `thumbnail` also enlarges.
    let cropped = if side > AVATAR_PX { cropped.thumbnail(AVATAR_PX, AVATAR_PX) } else { cropped };
    let mut out = Vec::new();
    match cropped.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png) {
        Ok(()) => out,
        Err(_) => bytes,
    }
}

fn dims(width: Option<UInt>, height: Option<UInt>) -> Option<(u32, u32)> {
    Some((u64::from(width?).try_into().ok()?, u64::from(height?).try_into().ok()?))
}

/// A message's attachment and text. For media the text is the caption: the body is only a
/// caption when a separate file name says so, otherwise it is the file name.
fn content(msgtype: &MessageType) -> (Option<Media>, String) {
    let media = match msgtype {
        MessageType::Image(c) => {
            let info = c.info.as_deref();
            Media {
                kind: MediaKind::Image,
                name: c.filename().to_owned(),
                size: info.and_then(|i| i.size).map(Into::into),
                mime: info.and_then(|i| i.mimetype.clone()),
                dims: info.and_then(|i| dims(i.width, i.height)),
                duration: None,
                source: c.source.clone(),
                thumb: info.and_then(|i| i.thumbnail_source.clone()),
            }
        }
        MessageType::Video(c) => {
            let info = c.info.as_deref();
            Media {
                kind: MediaKind::Video,
                name: c.filename().to_owned(),
                size: info.and_then(|i| i.size).map(Into::into),
                mime: info.and_then(|i| i.mimetype.clone()),
                dims: info.and_then(|i| dims(i.width, i.height)),
                duration: info.and_then(|i| i.duration),
                source: c.source.clone(),
                thumb: info.and_then(|i| i.thumbnail_source.clone()),
            }
        }
        MessageType::Audio(c) => {
            let info = c.info.as_deref();
            Media {
                kind: if c.voice.is_some() { MediaKind::Voice } else { MediaKind::Audio },
                name: c.filename().to_owned(),
                size: info.and_then(|i| i.size).map(Into::into),
                mime: info.and_then(|i| i.mimetype.clone()),
                dims: None,
                duration: info.and_then(|i| i.duration),
                source: c.source.clone(),
                thumb: None,
            }
        }
        MessageType::File(c) => {
            let info = c.info.as_deref();
            Media {
                kind: MediaKind::File,
                name: c.filename().to_owned(),
                size: info.and_then(|i| i.size).map(Into::into),
                mime: info.and_then(|i| i.mimetype.clone()),
                dims: None,
                duration: None,
                source: c.source.clone(),
                thumb: info.and_then(|i| i.thumbnail_source.clone()),
            }
        }
        other => return (None, other.body().to_owned()),
    };
    let caption = match msgtype {
        MessageType::Image(c) => c.caption(),
        MessageType::Video(c) => c.caption(),
        MessageType::Audio(c) => c.caption(),
        MessageType::File(c) => c.caption(),
        _ => None,
    };
    (Some(media), caption.unwrap_or_default().to_owned())
}

mod polls;
pub use polls::{Place, Poll};

mod timeline;

#[cfg(test)]
mod tests;

//! Matrix side of the client. Owns a tokio runtime on its own thread; the UI
//! talks to it only through two bounded channels (`Cmd` in, `Ev` out).

use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
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
            AnyMessageLikeEventContent,
            reaction::ReactionEventContent,
            relation::Annotation,
            room::{
                MediaSource,
                message::{
                    AddMentions, MessageType, Relation, RoomMessageEventContent,
                    sanitize::remove_plain_reply_fallback,
                },
            },
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
const PAGE: u32 = 40;
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
    LoadOlder,
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
    /// Redacts a message or a reaction.
    Delete { room: OwnedRoomId, target: String },
}

pub enum Ev {
    NeedLogin,
    CodeSent,
    /// Signed in as this user.
    Ready(OwnedUserId),
    /// True while this device lacks the secrets needed to decrypt history.
    NeedRecovery(bool),
    Rooms(Vec<RoomSummary>),
    RoomGone(OwnedRoomId),
    /// History page for `room`, oldest first. `reset` replaces the timeline.
    History { room: OwnedRoomId, items: Vec<Item>, reset: bool, more: bool },
    Live { room: OwnedRoomId, items: Vec<Item> },
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
    pub id: String,
    pub sender: String,
    pub from: OwnedUserId,
    pub body: String,
    pub own: bool,
    /// Origin server time, ms since the epoch.
    pub ts: u64,
    /// `com.beeper.hs.order`; 0 on other homeservers.
    pub order: i64,
    /// Event ID this message replies to.
    pub reply_to: Option<String>,
    pub deleted: bool,
    pub edited: bool,
    /// Attachment; `body` is then its caption, possibly empty.
    pub media: Option<Media>,
    /// The account's push rules ask for a notification (false when they were not evaluated).
    pub notify: bool,
    /// Byte ranges of `body` that mention someone.
    pub mentions: Vec<std::ops::Range<usize>>,
    /// Someone else mentions the user, by name or with the whole room.
    pub mentioned: bool,
    /// Filled by the UI from `Item::Reaction`.
    pub reactions: Vec<Reaction>,
}

impl Msg {
    /// One-line description for previews and quotes: Beeper's label for media, plus the caption.
    pub fn summary(&self) -> String {
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
    /// The reaction event, which is what removing it redacts.
    pub id: String,
    pub key: String,
    pub own: bool,
}

// Nearly every item is a message, so boxing it would add an allocation to the common case.
#[allow(clippy::large_enum_variant)]
pub enum Item {
    Msg(Msg),
    Edit { target: String, body: String, mentions: Vec<std::ops::Range<usize>> },
    Reaction { target: String, reaction: Reaction },
    /// A message or a reaction was deleted.
    Redact { target: String },
}

pub fn start(dir: PathBuf) -> (mpsc::Sender<Cmd>, mpsc::Receiver<Ev>) {
    let (cmd_tx, cmd_rx) = mpsc::channel(CHANNEL_CAP);
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
    /// Room whose history is open and its pagination token.
    open: Option<(OwnedRoomId, Option<String>)>,
    rooms: Rooms,
    fetches: Arc<tokio::sync::Semaphore>,
    /// Push rules, kept current by the SDK. One instance: each registers an event handler.
    notif: Option<NotificationSettings>,
    incognito: bool,
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
        incognito: true, tasks: Vec::new() };
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
                let incomplete = recovery.state() == RecoveryState::Incomplete;
                self.emit(Ev::NeedRecovery(incomplete)).await;
                if incomplete {
                    self.emit(Ev::Error("Recovery code accepted, but the account's key storage is incomplete".into()))
                        .await;
                }
            }
            Cmd::Open(room_id) => {
                self.open = Some((room_id.clone(), None));
                crate::clear_notification(room_id.as_str());
                let newest = self.page(room_id.clone(), true).await?;
                if let Err(e) = mark_read(&self.room(&room_id)?, newest, self.incognito).await {
                    log::warn!("mark {room_id} read: {e:#}");
                }
            }
            Cmd::LoadOlder => {
                if let Some((room_id, Some(_))) = &self.open {
                    self.page(room_id.clone(), false).await?;
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
                        mark_read(&room, resp.chunk.first().and_then(|e| e.event_id()).map(ToOwned::to_owned), self.incognito).await?;
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
                            mark_read(&room, newest.and_then(|e| e.event_id()).map(ToOwned::to_owned), self.incognito).await?;
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
            Cmd::Incognito(on) => self.incognito = on,
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
                let room = self.room(&room)?;
                let text = RoomMessageEventContent::text_plain(body);
                let content: AnyMessageLikeEventContent = match (edit, reply_to) {
                    (Some(id), _) => {
                        room.make_edit_event(&EventId::parse(id)?, EditedContent::RoomMessage(text.into())).await?
                    }
                    (None, Some(id)) => {
                        let reply = Reply {
                            event_id: EventId::parse(id)?,
                            enforce_thread: EnforceThread::MaybeThreaded,
                            add_mentions: AddMentions::Yes,
                        };
                        room.make_reply_event(text.into(), reply).await?.into()
                    }
                    (None, None) => text.into(),
                };
                let ev = self.ev.clone();
                tokio::spawn(async move {
                    if let Err(e) = room.send(content).await {
                        let _ = ev.send(Ev::Error(format!("Message not sent: {e}"))).await;
                    }
                });
            }
            Cmd::React { room, target, key } => {
                let content = ReactionEventContent::new(Annotation::new(EventId::parse(target)?, key));
                self.room(&room)?.send(content).await?;
            }
            Cmd::Delete { room, target } => {
                self.room(&room)?.redact(&EventId::parse(target)?, None, None).await?;
            }
        }
        Ok(())
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

    /// Emits the next older page; returns the newest message's ID.
    async fn page(&mut self, room_id: OwnedRoomId, reset: bool) -> Result<Option<OwnedEventId>> {
        let room = self.room(&room_id)?;
        let mut token = self.open.as_ref().and_then(|(_, t)| t.clone());
        let mut items = Vec::new();
        // Pages of only state events would leave the view blank with nothing to scroll.
        for _ in 0..MAX_PAGES_PER_REQUEST {
            let resp = messages(&room, token.as_deref(), PAGE).await?;
            let mut chunk = resp.chunk;
            chunk.reverse();
            let mut older = to_items(&room, &chunk).await;
            older.append(&mut items);
            items = older;
            token = resp.end;
            if token.is_none() || items.len() >= MIN_ITEMS {
                break;
            }
        }
        let more = token.is_some();
        self.open = Some((room_id.clone(), token));
        let newest = items.iter().rev().find_map(|i| match i {
            Item::Msg(m) => EventId::parse(&m.id).ok(),
            _ => None,
        });
        self.emit(Ev::History { room: room_id, items, reset, more }).await;
        Ok(newest)
    }
}

/// One backward `/messages` page. Undecryptable events get their keys from the server-side backup
/// (a no-op until recovery has imported the backup key), then the page is fetched again.
async fn messages(room: &Room, from: Option<&str>, limit: u32) -> Result<Messages> {
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
fn preview(items: &[Item], dm: bool) -> Option<String> {
    let text = items.iter().rev().find_map(|i| match i {
        Item::Msg(m) if m.own => Some(format!("You: {}", m.summary())),
        Item::Msg(m) if dm => Some(m.summary()),
        Item::Msg(m) => Some(format!("{}: {}", m.sender, m.summary())),
        _ => None,
    })?;
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

/// Text of the notification these new items call for: the newest message from someone else that
/// the account's push rules ask to be notified of (they cover mutes and mentions-only chats).
/// Old messages do not count: a bridge bringing in history sends them through sync too.
fn notification(summary: &RoomSummary, items: &[Item], now_ms: i64) -> Option<String> {
    let recent = |m: &Msg| now_ms - (m.ts as i64) < NOTIFY_WINDOW.as_millis() as i64;
    let m = items.iter().rev().find_map(|i| match i {
        Item::Msg(m) if m.notify && !m.own && recent(m) => Some(m),
        _ => None,
    })?;
    Some(if summary.dm { m.summary() } else { format!("{}: {}", m.sender, m.summary()) })
}

/// Notifies while the window is not in front; clears the chat's notification once it has been
/// read somewhere.
fn announce(summary: &RoomSummary, prev: Option<&RoomSummary>, items: &[Item]) {
    let chat = summary.id.as_str();
    if summary.unread == 0 && prev.is_some_and(|p| p.unread > 0) {
        crate::clear_notification(chat);
    }
    if crate::FOREGROUND.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Some(text) = notification(summary, items, now_ms()) {
        crate::notify(chat, &summary.name, &text);
    }
}

/// Unread messages that mention the user, counted here because a server cannot see into
/// encrypted messages. The last `unread` messages from others are the unread ones; `items` are the
/// newest, oldest first, and `before` is the count from earlier items.
fn unread_mentions(before: u64, unread: u64, items: &[Item]) -> u64 {
    let theirs: Vec<&Msg> = items
        .iter()
        .filter_map(|i| match i {
            Item::Msg(m) if !m.own => Some(m),
            _ => None,
        })
        .collect();
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
        let items = to_items(room, &chunk).await;
        let sent = items.iter().rev().find_map(|i| match i {
            Item::Msg(m) => Some(m.ts),
            _ => None,
        });
        if let (Some(text), Some(sent)) = (preview(&items, dm), sent) {
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
                if ev.send(Ev::Error(format!("Sync failed, retrying: {e}"))).await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let mut changed = Vec::new();
        for (id, update) in &resp.rooms.joined {
            let Some(room) = client.get_room(id) else { continue };
            if room.is_space() {
                continue;
            }
            let items = to_items(&room, &update.timeline.events).await;
            let prev = rooms.lock().unwrap().get(id).cloned();
            // A chat's time is its last message's. A room a bridge has just created has only state
            // events, which say nothing about when the chat was last used: it stays at 0, sorts
            // last, and is queued for a look at its history.
            let last_message = items.iter().filter_map(|i| match i {
                Item::Msg(m) => Some(m.ts),
                _ => None,
            });
            let last_message = last_message.max();
            let dated = last_message.is_some() || prev.as_ref().is_some_and(|p| p.dated);
            if !dated {
                let _ = to_probe.try_send(id.clone());
            }
            let newest = last_message.max(prev.as_ref().map(|p| p.ts));
            let peer = other_person(&room).await;
            let dm = peer.is_some();
            let order = items.iter().filter_map(|i| match i {
                Item::Msg(m) if !m.own => Some(m.order),
                _ => None,
            });
            let seen = RoomSummary {
                id: id.clone(),
                name: String::new(),
                preview: preview(&items, dm).or_else(|| prev.as_ref().and_then(|p| p.preview.clone())),
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
            summary.mentions = unread_mentions(summary.mentions, summary.unread, &items);
            announce(&summary, prev.as_ref(), &items);
            rooms.lock().unwrap().insert(id.clone(), summary.clone());
            changed.push(summary);
            if !items.is_empty() && ev.send(Ev::Live { room: id.clone(), items }).await.is_err() {
                return;
            }
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

pub(crate) const UTD: &str = "Unable to decrypt this message yet";
pub(crate) const DELETED: &str = "Message deleted";

/// Converts timeline events (oldest first) into displayable items.
async fn to_items(room: &Room, events: &[TimelineEvent]) -> Vec<Item> {
    let own = room.own_user_id().to_owned();
    let mut names: HashMap<OwnedUserId, String> = HashMap::new();
    let mut out = Vec::new();
    for event in events {
        let Some(id) = event.event_id().map(|e| e.to_string()) else { continue };
        let Some(sender) = event.sender() else { continue };
        let (mut reply_to, mut deleted, mut media) = (None, false, None);
        let (mut mentions, mut mentioned) = (Vec::new(), false);
        // A deleted event in an encrypted room is an empty `m.room.encrypted`: nothing says whether
        // it was a message or a reaction, so it gets no placeholder.
        let body = if event.kind.is_utd() {
            if is_redacted(event) {
                continue;
            }
            UTD.to_owned()
        } else {
            match event.raw().deserialize() {
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(
                    SyncMessageLikeEvent::Original(m),
                ))) => match m.content.relates_to {
                    Some(Relation::Replacement(r)) => {
                        let body = r.new_content.msgtype.body().to_owned();
                        let (mentions, _) = marks(&body, &r.new_content.msgtype, r.new_content.mentions.as_ref(), &own);
                        out.push(Item::Edit { target: r.event_id.to_string(), body, mentions });
                        continue;
                    }
                    relation => {
                        if let Some(Relation::Reply(reply)) = relation {
                            reply_to = Some(reply.in_reply_to.event_id.to_string());
                        }
                        let (found, text) = content(&m.content.msgtype);
                        media = found;
                        // Older clients and bridges prepend the quoted message to the body.
                        let text = if reply_to.is_some() { remove_plain_reply_fallback(&text).to_owned() } else { text };
                        (mentions, mentioned) = marks(&text, &m.content.msgtype, m.content.mentions.as_ref(), &own);
                        text
                    }
                },
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(
                    SyncMessageLikeEvent::Redacted(_),
                ))) => {
                    deleted = true;
                    DELETED.to_owned()
                }
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::Reaction(
                    SyncMessageLikeEvent::Original(r),
                ))) => {
                    let Annotation { event_id, key, .. } = r.content.relates_to;
                    let reaction = Reaction { id, key, own: sender == own };
                    out.push(Item::Reaction { target: event_id.to_string(), reaction });
                    continue;
                }
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomRedaction(_))) => {
                    // The target moved from the event to its content in room version 11.
                    #[derive(Deserialize)]
                    struct Redacts {
                        redacts: Option<String>,
                    }
                    let raw = event.raw();
                    let top = raw.get_field::<String>("redacts").ok().flatten();
                    let inner = || raw.get_field::<Redacts>("content").ok().flatten().and_then(|c| c.redacts);
                    if let Some(target) = top.or_else(inner) {
                        out.push(Item::Redact { target });
                    }
                    continue;
                }
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::Sticker(
                    SyncMessageLikeEvent::Original(s),
                ))) => {
                    let info = s.content.info;
                    media = Some(Media {
                        kind: MediaKind::Sticker,
                        name: s.content.body,
                        size: info.size.map(Into::into),
                        mime: info.mimetype,
                        dims: dims(info.width, info.height),
                        duration: None,
                        source: s.content.source.into(),
                        thumb: info.thumbnail_source,
                    });
                    String::new()
                }
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomEncrypted(_))) if !is_redacted(event) => {
                    UTD.into()
                }
                _ => continue,
            }
        };
        let name = match names.get(&sender) {
            Some(n) => n.clone(),
            None => {
                let n = display_name(room, &sender).await;
                names.insert(sender.clone(), n.clone());
                n
            }
        };
        let ts = event.timestamp().map_or(0, |t| u64::from(t.0));
        let order = hs_order(event).unwrap_or(0);
        let own = sender == own;
        out.push(Item::Msg(Msg {
            id,
            sender: name,
            own,
            from: sender,
            body,
            ts,
            order,
            reply_to,
            deleted,
            edited: false,
            media,
            notify: event.push_actions().is_some_and(|actions| actions.iter().any(|a| a.should_notify())),
            mentions,
            mentioned: mentioned && !own,
            reactions: Vec::new(),
        }));
    }
    out
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
        MessageType::Location(_) => return (None, "📍 Location".to_owned()),
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

#[cfg(test)]
mod tests;

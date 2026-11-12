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
                profile::AvatarUrl,
                receipt::create_receipt,
            },
            error::ErrorKind,
        },
        events::{
            AnySyncMessageLikeEvent, AnySyncTimelineEvent, StateEventType, SyncMessageLikeEvent,
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

const CHANNEL_CAP: usize = 64;
const PAGE: u32 = 40;
const MIN_ITEMS: usize = 15;
const MAX_PAGES_PER_REQUEST: usize = 5;
const SYNC_TIMELINE_LIMIT: u32 = 10;
/// Avatar and preview fetches in flight at once.
const ROW_FETCHES: usize = 4;
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
    Ready,
    /// True while this device lacks the secrets needed to decrypt history.
    NeedRecovery(bool),
    Rooms(Vec<RoomSummary>),
    RoomGone(OwnedRoomId),
    /// History page for `room`, oldest first. `reset` replaces the timeline.
    History { room: OwnedRoomId, items: Vec<Item>, reset: bool, more: bool },
    Live { room: OwnedRoomId, items: Vec<Item> },
    /// Thumbnail bytes as served (PNG, JPEG, ...).
    Avatar(OwnedRoomId, Vec<u8>),
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
    /// Filled by the UI from `Item::Reaction`.
    pub reactions: Vec<Reaction>,
}

#[derive(Clone)]
pub struct Reaction {
    /// The reaction event, which is what removing it redacts.
    pub id: String,
    pub key: String,
    pub own: bool,
}

pub enum Item {
    Msg(Msg),
    Edit { target: String, body: String },
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
    let mut st = State { dir, homeserver, http, ev, client: None, login_request: None, open: None,
        rooms: Rooms::default(), fetches: Arc::new(tokio::sync::Semaphore::new(ROW_FETCHES)), notif: None };
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
        tokio::spawn(watch_recovery(client.clone(), self.ev.clone()));
        let notif = client.notification_settings().await;
        tokio::spawn(sync_loop(client.clone(), notif.clone(), self.dir.clone(), self.rooms.clone(), self.ev.clone()));
        self.notif = Some(notif);
        self.client = Some(client);
        self.emit(Ev::Ready).await;
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
                let newest = self.page(room_id.clone(), true).await?;
                if let Err(e) = mark_read(&self.room(&room_id)?, newest).await {
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
                        mark_read(&room, resp.chunk.first().and_then(|e| e.event_id()).map(ToOwned::to_owned)).await?;
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
                            mark_read(&room, newest.and_then(|e| e.event_id()).map(ToOwned::to_owned)).await?;
                        }
                    }
                }
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
            Cmd::Previews(ids) => {
                for room_id in ids {
                    let room = self.room(&room_id)?;
                    let (ev, permits, rooms) = (self.ev.clone(), self.fetches.clone(), self.rooms.clone());
                    tokio::spawn(async move {
                        let Ok(_permit) = permits.acquire().await else { return };
                        let resp = match messages(&room, None, SYNC_TIMELINE_LIMIT * 2).await {
                            Ok(r) => r,
                            Err(e) => return log::debug!("preview {room_id}: {e:#}"),
                        };
                        let mut chunk = resp.chunk;
                        chunk.reverse();
                        let Some(text) = preview(&to_items(&room, &chunk).await, other_person(&room).await.is_some()) else {
                            return;
                        };
                        // rooms.json picks this up on the next sync that changes anything.
                        let summary = rooms.lock().unwrap().get_mut(&room_id).map(|r| {
                            r.preview = Some(text);
                            r.clone()
                        });
                        if let Some(summary) = summary {
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

/// List preview: the newest message, prefixed with its sender outside one-to-one chats.
fn preview(items: &[Item], dm: bool) -> Option<String> {
    items.iter().rev().find_map(|i| match i {
        Item::Msg(m) if m.own => Some(format!("You: {}", m.body)),
        Item::Msg(m) if dm => Some(m.body.clone()),
        Item::Msg(m) => Some(format!("{}: {}", m.sender, m.body)),
        _ => None,
    })
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

/// Clears the unread state. The receipt is private: the other side, or the bridged network, is not told.
async fn mark_read(room: &Room, newest: Option<OwnedEventId>) -> Result<()> {
    room.set_unread_flag(false).await?;
    if let Some(id) = newest {
        room.send_single_receipt(create_receipt::v3::ReceiptType::ReadPrivate, ReceiptThread::Unthreaded, id).await?;
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

/// Everything the local store knows about a room. `preview`, `ts`, `order` and `dm` are the
/// caller's (the preview's wording depends on `dm`); the rest is read here.
async fn summarize(room: &Room, notif: &NotificationSettings, seen: RoomSummary) -> RoomSummary {
    let name = room.display_name().await.map(|n| n.to_string()).unwrap_or_else(|_| seen.id.to_string());
    let network = match seen.network {
        Some(n) => Some(n),
        None => network(room).await,
    };
    let mode = notif.get_user_defined_room_notification_mode(room.room_id()).await;
    let done = match room.account_data(DONE.into()).await {
        Ok(Some(raw)) => raw.get_field::<Done>("content").ok().flatten().unwrap_or_default(),
        _ => Done::default(),
    };
    let mut s = RoomSummary {
        name,
        network,
        unread: room.unread_notification_counts().notification_count,
        pinned: room.is_favourite(),
        low_priority: room.is_low_priority(),
        muted: matches!(mode, Some(RoomNotificationMode::Mute | RoomNotificationMode::MentionsAndKeywordsOnly)),
        marked_unread: room.is_marked_unread(),
        ..seen
    };
    s.archived = is_done(&done, &s, now_ms());
    s
}

/// Rooms without new events never pass through the sync loop, so their cached summaries miss
/// fields added since and changes made on other devices. Store reads only.
async fn refresh_cached(client: &Client, notif: &NotificationSettings, rooms: &Rooms) -> Vec<RoomSummary> {
    let cached: Vec<RoomSummary> = rooms.lock().unwrap().values().cloned().collect();
    let mut out = Vec::with_capacity(cached.len());
    for seen in cached {
        let Some(room) = client.get_room(&seen.id) else { continue };
        let dm = other_person(&room).await.is_some();
        let fresh = summarize(&room, notif, RoomSummary { dm, ..seen }).await;
        if let Some(r) = rooms.lock().unwrap().get_mut(&fresh.id) {
            // A preview refresh may have updated the entry meanwhile.
            *r = RoomSummary { preview: r.preview.clone(), ..fresh };
            out.push(r.clone());
        }
    }
    out
}

async fn sync_loop(client: Client, notif: NotificationSettings, dir: PathBuf, rooms: Rooms, ev: mpsc::Sender<Ev>) {
    let cached = refresh_cached(&client, &notif, &rooms).await;
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
            let newest = update.timeline.events.iter().filter_map(|e| e.timestamp()).map(|t| u64::from(t.0)).max();
            let dm = other_person(&room).await.is_some();
            let order = items.iter().filter_map(|i| match i {
                Item::Msg(m) if !m.own => Some(m.order),
                _ => None,
            });
            let seen = RoomSummary {
                id: id.clone(),
                name: String::new(),
                preview: preview(&items, dm).or_else(|| prev.as_ref().and_then(|p| p.preview.clone())),
                ts: newest.max(prev.as_ref().map(|p| p.ts)).unwrap_or(0),
                network: prev.as_ref().and_then(|p| p.network.clone()),
                order: order.max().max(prev.as_ref().map(|p| p.order)).unwrap_or(0),
                unread: 0,
                dm,
                pinned: false,
                low_priority: false,
                muted: false,
                marked_unread: false,
                archived: false,
            };
            let summary = summarize(&room, &notif, seen).await;
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
        if !changed.is_empty() || gone {
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
        let (mut reply_to, mut deleted) = (None, false);
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
                        out.push(Item::Edit { target: r.event_id.to_string(), body });
                        continue;
                    }
                    Some(Relation::Reply(reply)) => {
                        reply_to = Some(reply.in_reply_to.event_id.to_string());
                        // Older clients and bridges prepend the quoted message to the body.
                        remove_plain_reply_fallback(&describe(&m.content.msgtype)).to_owned()
                    }
                    _ => describe(&m.content.msgtype),
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
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::Sticker(_))) => "Sticker".into(),
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
            reactions: Vec::new(),
        }));
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
    let format = MediaFormat::Thumbnail(MediaThumbnailSettings::with_method(Method::Crop, uint!(96), uint!(96)));
    let request = MediaRequestParameters { source: MediaSource::Plain(url), format };
    Ok(Some(client.media().get_media_content(&request, true).await?))
}

fn describe(msgtype: &MessageType) -> String {
    let label = match msgtype {
        MessageType::Image(_) => "Photo",
        MessageType::Video(_) => "Video",
        MessageType::Audio(_) => "Audio",
        MessageType::File(_) => "File",
        MessageType::Location(_) => "Location",
        _ => return msgtype.body().to_owned(),
    };
    format!("[{label}] {}", msgtype.body())
}

#[cfg(test)]
mod tests;

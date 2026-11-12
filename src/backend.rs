//! Matrix side of the client. Owns a tokio runtime on its own thread; the UI
//! talks to it only through two bounded channels (`Cmd` in, `Ev` out).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use futures_util::StreamExt as _;
use matrix_sdk::{
    Client, Room,
    authentication::matrix::MatrixSession,
    config::{SyncSettings, SyncToken},
    deserialized_responses::TimelineEvent,
    encryption::{BackupDownloadStrategy, EncryptionSettings, recovery::RecoveryState},
    reqwest,
    room::MessagesOptions,
    ruma::{
        OwnedRoomId, OwnedUserId, UInt, UserId,
        api::{
            client::filter::{FilterDefinition, RoomFilter},
            error::ErrorKind,
        },
        events::{
            AnySyncMessageLikeEvent, AnySyncTimelineEvent, SyncMessageLikeEvent,
            room::message::{MessageType, Relation, RoomMessageEventContent},
        },
        serde::JsonObject,
    },
    store::RoomLoadSettings,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::beeper;

const CHANNEL_CAP: usize = 64;
const PAGE: u32 = 40;
const MIN_ITEMS: usize = 15;
const MAX_PAGES_PER_REQUEST: usize = 5;
const SYNC_TIMELINE_LIMIT: u32 = 10;

pub enum Cmd {
    SendCode(String),
    SubmitCode(String),
    Recover(String),
    Open(OwnedRoomId),
    LoadOlder,
    Send(OwnedRoomId, String),
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
    Error(String),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RoomSummary {
    pub id: OwnedRoomId,
    pub name: String,
    pub preview: Option<String>,
    pub ts: u64,
}

#[derive(Clone)]
pub struct Msg {
    pub id: String,
    pub sender: String,
    pub body: String,
    pub own: bool,
}

pub enum Item {
    Msg(Msg),
    Edit { target: String, body: String },
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
}

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
    let mut st = State { dir, homeserver, http, ev, client: None, login_request: None, open: None };
    match st.restore().await {
        Ok(true) => {}
        Ok(false) => st.emit(Ev::NeedLogin).await,
        Err(e) => {
            st.emit(Ev::Error(format!("Could not restore session: {e:#}"))).await;
            st.emit(Ev::NeedLogin).await;
        }
    }
    while let Some(cmd) = cmds.recv().await {
        if let Err(e) = st.handle(cmd).await {
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
            self.emit(Ev::Rooms(rooms.into_values().collect())).await;
        }
        self.go_live(client).await;
        Ok(true)
    }

    async fn go_live(&mut self, client: Client) {
        tokio::spawn(watch_recovery(client.clone(), self.ev.clone()));
        tokio::spawn(sync_loop(client.clone(), self.dir.clone(), self.ev.clone()));
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
                recovery.recover(key.trim()).await.context("Recovery code was not accepted")?;
                let incomplete = recovery.state() == RecoveryState::Incomplete;
                self.emit(Ev::NeedRecovery(incomplete)).await;
                if incomplete {
                    self.emit(Ev::Error("Recovery code accepted, but the account's key storage is incomplete".into()))
                        .await;
                }
            }
            Cmd::Open(room_id) => {
                self.open = Some((room_id.clone(), None));
                self.page(room_id, true).await?;
            }
            Cmd::LoadOlder => {
                if let Some((room_id, Some(_))) = &self.open {
                    self.page(room_id.clone(), false).await?;
                }
            }
            Cmd::Send(room_id, body) => {
                let room = self.room(&room_id)?;
                let ev = self.ev.clone();
                tokio::spawn(async move {
                    if let Err(e) = room.send(RoomMessageEventContent::text_plain(body)).await {
                        let _ = ev.send(Ev::Error(format!("Message not sent: {e}"))).await;
                    }
                });
            }
        }
        Ok(())
    }

    fn room(&self, id: &OwnedRoomId) -> Result<Room> {
        self.client()?.get_room(id).ok_or_else(|| anyhow!("Unknown room {id}"))
    }

    async fn page(&mut self, room_id: OwnedRoomId, reset: bool) -> Result<()> {
        let room = self.room(&room_id)?;
        let mut token = self.open.as_ref().and_then(|(_, t)| t.clone());
        let mut items = Vec::new();
        // Pages of only state events would leave the view blank with nothing to scroll.
        for _ in 0..MAX_PAGES_PER_REQUEST {
            let mut opts = MessagesOptions::backward().from(token.as_deref());
            opts.limit = UInt::from(PAGE);
            let resp = room.messages(opts).await?;
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
        self.emit(Ev::History { room: room_id, items, reset, more }).await;
        Ok(())
    }
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

async fn sync_loop(client: Client, dir: PathBuf, ev: mpsc::Sender<Ev>) {
    let mut rooms = load_rooms(&dir).unwrap_or_default();
    let mut filter = RoomFilter::with_lazy_loading();
    filter.timeline.limit = Some(UInt::from(SYNC_TIMELINE_LIMIT));
    let mut def = FilterDefinition::default();
    def.room = filter;
    // Without a summary cache the stored sync token would skip rooms with no new events.
    let token = if rooms.is_empty() { SyncToken::NoToken } else { SyncToken::ReusePrevious };
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
            let prev = rooms.get(id);
            let newest = update.timeline.events.iter().filter_map(|e| e.timestamp()).max();
            let ts = newest.map(|t| u64::from(t.0)).max(prev.map(|p| p.ts)).unwrap_or(0);
            let preview = items
                .iter()
                .rev()
                .find_map(|i| match i {
                    Item::Msg(m) => Some(format!("{}: {}", m.sender, m.body)),
                    Item::Edit { .. } => None,
                })
                .or_else(|| prev.and_then(|p| p.preview.clone()));
            let name = room.display_name().await.map(|n| n.to_string()).unwrap_or_else(|_| id.to_string());
            let summary = RoomSummary { id: id.clone(), name, preview, ts };
            rooms.insert(id.clone(), summary.clone());
            changed.push(summary);
            if !items.is_empty() && ev.send(Ev::Live { room: id.clone(), items }).await.is_err() {
                return;
            }
        }
        let mut gone = false;
        for id in resp.rooms.left.keys() {
            if rooms.remove(id).is_some() {
                gone = true;
                let _ = ev.send(Ev::RoomGone(id.clone())).await;
            }
        }
        if !changed.is_empty() || gone {
            let saved = serde_json::to_vec(&rooms).map_err(anyhow::Error::from);
            if let Err(e) = saved.and_then(|b| write_private(&rooms_path(&dir), &b)) {
                log::warn!("rooms.json: {e:#}");
            }
        }
        if !changed.is_empty() && ev.send(Ev::Rooms(changed)).await.is_err() {
            return;
        }
    }
}

const UTD: &str = "Unable to decrypt this message yet";

/// Converts timeline events (oldest first) into displayable items.
async fn to_items(room: &Room, events: &[TimelineEvent]) -> Vec<Item> {
    let own = room.own_user_id().to_owned();
    let mut names: HashMap<OwnedUserId, String> = HashMap::new();
    let mut out = Vec::new();
    for event in events {
        let Some(id) = event.event_id().map(|e| e.to_string()) else { continue };
        let Some(sender) = event.sender() else { continue };
        let item = if event.kind.is_utd() {
            Some(Item::Msg(Msg { id, sender: String::new(), body: UTD.into(), own: sender == own }))
        } else {
            match event.raw().deserialize() {
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(
                    SyncMessageLikeEvent::Original(m),
                ))) => Some(match m.content.relates_to {
                    Some(Relation::Replacement(r)) => {
                        Item::Edit { target: r.event_id.to_string(), body: r.new_content.msgtype.body().to_owned() }
                    }
                    _ => Item::Msg(Msg { id, sender: String::new(), body: describe(&m.content.msgtype), own: sender == own }),
                }),
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::Sticker(_))) => {
                    Some(Item::Msg(Msg { id, sender: String::new(), body: "Sticker".into(), own: sender == own }))
                }
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomEncrypted(_))) => {
                    Some(Item::Msg(Msg { id, sender: String::new(), body: UTD.into(), own: sender == own }))
                }
                _ => None,
            }
        };
        let Some(mut item) = item else { continue };
        if let Item::Msg(m) = &mut item {
            m.sender = match names.get(&sender) {
                Some(n) => n.clone(),
                None => {
                    let n = display_name(room, &sender).await;
                    names.insert(sender.clone(), n.clone());
                    n
                }
            };
        }
        out.push(item);
    }
    out
}

async fn display_name(room: &Room, user: &UserId) -> String {
    match room.get_member_no_sync(user).await {
        Ok(Some(m)) => m.name().to_owned(),
        _ => user.localpart().to_owned(),
    }
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

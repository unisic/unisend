//! Matrix side of the client. Owns a tokio runtime on its own thread; the UI
//! talks to it only through two bounded channels (`Cmd` in, `Ev` out).

use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use futures_util::StreamExt as _;
use matrix_sdk::{
    Client, Room, RoomMemberships, RoomState,
    authentication::matrix::MatrixSession,
    config::{SyncSettings, SyncToken},
    deserialized_responses::{RawAnySyncOrStrippedState, TimelineEvent, TimelineEventKind},
    encryption::{
        BackupDownloadStrategy, EncryptionSettings,
        recovery::{RecoveryError, RecoveryState},
        secret_storage::SecretStorageError,
    },
    notification_settings::{NotificationSettings, RoomNotificationMode},
    attachment::{AttachmentConfig, AttachmentInfo, BaseImageInfo, BaseVideoInfo, Thumbnail},
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
                session::get_login_types::v3::LoginType,
            },
            error::ErrorKind,
        },
        events::{
            AnySyncEphemeralRoomEvent, AnySyncMessageLikeEvent, AnySyncTimelineEvent, Mentions, StateEventType, SyncMessageLikeEvent,
            room::{
                EncryptedFile, MediaSource,
                message::{
                    AddMentions, MessageFormat, MessageType, Relation, RoomMessageEventContent, TextMessageEventContent,
                    sanitize::remove_plain_reply_fallback,
                },
            },
            sticker::StickerEventContent,
        },
        media::Method,
        room_version_rules::RoomVersionRules,
        serde::{JsonObject, Raw},
        uint,
    },
    store::RoomLoadSettings,
};
use matrix_sdk_crypto::secret_storage::DecodeError;
use matrix_sdk_crypto::types::events::UtdCause;
use crate::vault::{self, KeyError, Seal};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::beeper;
use crate::bridges::{self, Flow, Guild, Network, Step};
use crate::i18n::{n_, t, tn, tr_in};

const CHANNEL_CAP: usize = 64;
const PAGE: u16 = 40;
const MIN_ITEMS: usize = 15;
const MAX_PAGES_PER_REQUEST: usize = 5;
const SYNC_TIMELINE_LIMIT: u32 = 10;
/// Pictures (avatars, thumbnails) fetched at once. Previews have their own permits: a list that
/// asked for many must not hold back the pictures of the chat opened after it.
const ROW_FETCHES: usize = 4;
/// Longest side of an inline thumbnail and of the full-screen view, in pixels. Everything shown is
/// re-encoded to at most this, so a decoded image costs at most 2.6 MB and 16 MB.
const THUMB_PX: u32 = 800;
const VIEW_PX: u32 = 2048;
/// Larger files are not fetched to make a thumbnail, and not sent.
const MAX_THUMB_SOURCE: u64 = 16 * 1024 * 1024;
pub(crate) const MAX_UPLOAD: u64 = 100 * 1024 * 1024;
/// Pictures pasted into the composer, under the data directory: each goes once it is sent, and
/// what a closed preview left goes at the next start.
pub(crate) const PASTED: &str = "pasted";
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
    /// Sign in to any Matrix homeserver. `server` is a server name or URL, as typed.
    PasswordLogin { server: String, user: String, password: String },
    /// Make an account with a user name and password, and an invite code when the server asks one.
    Register { server: String, user: String, password: String, code: String },
    /// What the server typed in the sign-in form offers to sign in with.
    LoginMethods(String),
    /// Sign in on the server's own page, in a browser.
    BrowserLogin { server: String, way: Way },
    /// The browser came back to `back` at `url`, or the page was closed (none).
    BrowserDone { back: String, url: Option<String> },
    BrowserCancel,
    Recover(String),
    /// The recovery code kept on this device, for the settings to show.
    RecoveryCode,
    /// Replaces the account's recovery code with a new one and keeps it here.
    NewRecoveryCode,
    /// A notification as a message brings one, with the user's picture, to try its look and buttons.
    TestNotice,
    Open(OwnedRoomId),
    /// The conversation left the screen.
    Close,
    LoadOlder,
    /// The user is, or stopped, writing in the chat.
    Typing(OwnedRoomId, bool),
    Chat(OwnedRoomId, ChatAction),
    /// Marks these chats read, one after another, as `ChatAction::Unread(false)` does each.
    ReadAll(Vec<OwnedRoomId>),
    Avatars(Vec<OwnedRoomId>),
    /// The picture of a person in a chat, as that chat shows them.
    Face(OwnedRoomId, OwnedUserId),
    /// Inline thumbnails for these messages (event ID, media).
    Thumbs(Vec<(String, Media)>),
    /// The image at viewing size.
    View(String, Media),
    /// A chat's picture (`user: None`) or a member's, at viewing size, as `Ev::Viewed(id, ..)`.
    ViewPicture { id: String, room: OwnedRoomId, user: Option<OwnedUserId> },
    /// The user opened view-once media (the event of the open chat): the bridge is told.
    ViewedOnce(OwnedRoomId, String),
    /// Download to the media cache so the system can open it.
    Fetch(String, Media),
    /// Fetch into the SDK's media cache only, so a later `Fetch` needs no network (auto-download).
    Prefetch(Media),
    Attach { room: OwnedRoomId, path: PathBuf, reply_to: Option<String> },
    /// Chat networks on the account.
    Networks,
    /// Ways to log in to a bridge; starts the bridge when the account does not run it yet.
    Flows(String),
    /// `relogin`: the login signed in again, in place of a new one.
    LoginStart { bridge: String, flow: String, relogin: Option<String> },
    /// Answers a login step; the values are empty for a step that only waits.
    LoginSubmit(Step, HashMap<String, String>),
    /// The user left a login before it completed: the bridge stops waiting for it.
    LoginCancel(Step),
    Logout { bridge: String, login: String },
    /// A Discord login's servers.
    Guilds { bridge: String, login: String },
    /// Starts or stops bringing in a Discord server, then lists the servers again.
    BridgeGuild { bridge: String, login: String, guild: String, on: bool },
    /// Read receipts stay private while on.
    Incognito(bool),
    SignOut,
    /// Re-read the latest message of rooms whose preview is missing or undecryptable.
    Previews(Vec<OwnedRoomId>),
    /// The interface language changed: previews and the open chat's rows are built again in it.
    Relabel,
    /// `reply_to` and `edit` are event IDs; an edit replaces that message's text. `mentions` are
    /// the people picked from the mention list, as (name, user). `preview` is the link whose
    /// preview, made by `LinkPreview`, goes with a new message.
    Send { room: OwnedRoomId, body: String, reply_to: Option<String>, edit: Option<String>, mentions: Vec<(String, OwnedUserId)>, preview: Option<String> },
    /// Makes the preview of a link being typed, or with `None` drops it.
    LinkPreview(Option<String>),
    React { room: OwnedRoomId, target: String, key: String },
    /// Sets the user's vote on a poll to `answers`; empty withdraws it.
    Vote { room: OwnedRoomId, target: String, unstable: bool, answers: Vec<String> },
    /// Deletes a message for everyone; one that has not left yet is not sent.
    Delete { room: OwnedRoomId, target: String },
    /// Sends again a message that failed to leave.
    Retry { room: OwnedRoomId, target: String },
    /// Starts the sync again after `Ev::SyncStopped`.
    Resync,
    /// Sends the whole chat list again: the window turned to this account.
    Replay,
    /// Sends the user's position as an `m.location` message.
    SendLocation { room: OwnedRoomId, lat: f64, lon: f64, accuracy: Option<f64>, reply_to: Option<String> },
    /// Starts a poll; `answers` are checked again before they go out.
    SendPoll { room: OwnedRoomId, question: String, answers: Vec<String>, multiple: bool },
    /// Pins or unpins a message that reached the server.
    Pin { room: OwnedRoomId, target: String, pin: bool },
    /// Ends the user's own poll.
    EndPoll { room: OwnedRoomId, target: String },
    /// Reads the contact card in a `.vcf` attachment.
    Contact(String, Media),
    /// Leaving chats and starting new ones.
    People(rooms::Ask),
    /// Downloads the original file for the viewer's Save or Share; answered by `Exported`.
    Export(String, Media, Export),
    /// A page of KLIPY GIFs: trending when `query` is empty. Page 1 stops the previous search.
    GifSearch { query: String, page: u32 },
    /// The GIF picker closed.
    GifStop,
    /// `query` is the search the GIF was found with, empty from trending.
    SendGif { room: OwnedRoomId, gif: gifs::Gif, query: String, reply_to: Option<String> },
    /// The people in a chat, for the mention list.
    Members(OwnedRoomId),
    /// A message's earlier versions, for its edit history.
    EditHistory { room: OwnedRoomId, id: String },
    /// The sticker packs offered in a chat.
    Stickers(OwnedRoomId),
    SendSticker { room: OwnedRoomId, sticker: stickers::Sticker, reply_to: Option<String> },
    /// A sticker's picture for the picker's grid; answered by `StickerThumb`.
    StickerThumb(matrix_sdk::ruma::OwnedMxcUri),
    /// A photo or video from the preview: with its caption, and as the original when `hd`.
    AttachMedia { room: OwnedRoomId, path: PathBuf, reply_to: Option<String>, caption: String, hd: bool },
    /// A video `OfferMp4` named, made into an MP4 with ffmpeg and sent; `Attached` follows.
    Mp4 { room: OwnedRoomId, path: PathBuf, reply_to: Option<String>, caption: String },
    /// A recording to send as a voice message; `levels` are its loudness, 0 to 1, per tick.
    Voice { room: OwnedRoomId, path: PathBuf, duration: Duration, levels: Vec<f32>, reply_to: Option<String> },
    /// Sends these messages of the open chat (event IDs, oldest first) again in each chat of `to`.
    Forward { room: OwnedRoomId, targets: Vec<String>, to: Vec<OwnedRoomId> },
    /// The account's devices and how far they are trusted.
    Devices,
    RenameDevice(String),
    /// Asks the user's other devices to verify this one.
    VerifyStart,
    /// Asks this person to verify each other.
    VerifyPerson(OwnedUserId),
    /// Takes this person's changed keys as theirs.
    AcceptIdentity(OwnedUserId),
    VerifyAccept,
    /// The emoji match (true) or do not.
    VerifyMatch(bool),
    VerifyCancel,
    /// One step of signing out another device.
    SignOutDevice { device: String, proof: verify::Proof },
    /// Sets the chat's disappearing message timer, in ms; 0 turns it off.
    SetDisappear { room: OwnedRoomId, ms: u64 },
    /// Details, members and history of a chat for its info screen; replaces an earlier one.
    Info(OwnedRoomId),
    /// The info screen closed: stop reading history for it.
    InfoClose,
    /// How the other person in a one-to-one chat is known on its network; answered with `PeerIds`.
    PeerIds(OwnedRoomId),
    /// Blocks (true) or unblocks the person; the new list comes as `Ev::Blocked`, a failure as `Ev::Error`.
    SetBlocked(OwnedUserId, bool),
    /// Has the one-to-one chat's bridge block (true) or unblock the person on the network; the
    /// bridge's answer comes with the chat, a failure as `Ev::Error`.
    BlockOnNetwork(OwnedRoomId, bool),
    /// Changes a group; answered with `Ev::Group` once the server took it, else `Ev::Error`.
    Group { room: OwnedRoomId, change: group::Change },
    /// People who could be added to a group; answered with `Candidates`.
    Candidates(OwnedRoomId),
    /// How a member of a group is known on its network; answered with `MemberIds`.
    MemberIds(OwnedRoomId, OwnedUserId),
    /// The user's own name and picture; answered with `Profile`.
    Profile,
    /// Changes the user's own name or picture; answered with `Profile`, else `Ev::Error`.
    SetProfile(group::Mine),
    /// How long messages stay in this device's caches (`storage::Keep`).
    Keep(storage::Keep),
    /// What the account takes on this device; answered with `Ev::Storage`.
    Storage,
    /// Empties the media cache and the downloads; answered with `Ev::Storage`, else `Ev::Error`.
    ClearMedia,
    /// Messages matching `query` and `filter` in one room, or in every room; answered with `Found`.
    Search { room: Option<OwnedRoomId>, query: String, filter: Filter },
    /// Looks for a newer release; answered with `Ev::Update`.
    UpdateCheck,
    /// Downloads, checks and installs this release; replaces a download still running.
    UpdateInstall(update::Release),
    /// Android did not install the update, for this reason.
    #[cfg(target_os = "android")]
    UpdateFailed(String),
    /// Downloads emoji font `emoji::SETS[set]` to `to`, checked against its SHA-256; answered by
    /// `Ev::EmojiSet`.
    EmojiSet { set: usize, to: PathBuf },
    /// The chat of the open merged chat that the composer sends to: (merged chat, chat).
    Target(OwnedRoomId, OwnedRoomId),
    /// The user has had the open chat on screen down to this event, as Signal reads: the chat (or
    /// a chat of the open merged chat) is read up to it. A receipt that would go back is dropped.
    Seen(OwnedRoomId, OwnedEventId),
    /// Asks the account's device with this ID for its settings, through the server; answered by
    /// `Ev::SettingsArrived`, else `Ev::Error`.
    AskSettings(String),
    /// Stops waiting for the answer to `AskSettings`.
    CancelAsk,
    /// Sends `settings` (an exported settings file) to the device that asked with `request`.
    GiveSettings { device: String, request: String, settings: Vec<u8> },
    /// Offers `settings` on the local network; followed by `Ev::Handoff`.
    HandoffOffer(Vec<u8>),
    /// Looks for a device offering its settings on the local network; followed by `Ev::Handoff`.
    HandoffSeek,
    /// Takes the settings of the device found at this place in `handoff::Step::Seeking`.
    HandoffPick(usize),
    /// The user saw the same code on both screens: the offering side sends.
    HandoffConfirm,
    /// Ends the local-network exchange.
    HandoffStop,
}

pub enum Ev {
    NeedLogin,
    CodeSent,
    LoginMethods(Methods),
    /// The server's sign-in page to show; it sends the browser back to `back`.
    SignInPage { url: String, back: String },
    /// Signed in as this user; `beeper` says the account is Beeper's, with its bridges and account API.
    /// `seal` keeps the UI's own files the way the store is kept.
    Ready { user: OwnedUserId, beeper: bool, seal: Seal },
    /// The connection to the server is lost (false) or back (true).
    Online(bool),
    /// The first sync answered: a list still empty is the account's, not one still loading.
    Synced,
    /// The sync stopped for good (a panic), not for a lost connection, which it retries itself.
    SyncStopped,
    /// True while this device lacks the secrets needed to decrypt history.
    NeedRecovery(bool),
    /// The recovery code kept on this device, `None` when none is kept.
    RecoveryCode(Result<Option<String>, String>),
    Rooms(Vec<RoomSummary>),
    RoomGone(OwnedRoomId),
    /// Changes to the rows of the open conversation, in order.
    Timeline { room: OwnedRoomId, diffs: Vec<Diff> },
    /// A request for older messages is done; `more` says whether there are older ones still.
    Paged { room: OwnedRoomId, more: bool },
    /// Who is writing in the open conversation right now.
    Typing { room: OwnedRoomId, names: Vec<String> },
    /// Who is writing in any chat, for its row in the list: from the typing notices every sync
    /// already carries. Empty once they stop.
    ListTyping { room: OwnedRoomId, names: Vec<String> },
    /// How far the user has read the open conversation, and how far others can see that.
    OwnRead { room: OwnedRoomId, read: OwnRead },
    /// Thumbnail bytes as served (PNG, JPEG, ...).
    Avatar(OwnedRoomId, Vec<u8>),
    Face(OwnedUserId, Vec<u8>),
    /// JPEG or PNG, at most `THUMB_PX` on its longest side, or a moving GIF.
    Thumb(String, Vec<u8>),
    /// The preview of the link being typed; `None` when the page gave none.
    LinkPreview(String, Option<LinkDraft>),
    /// JPEG or PNG, at most `VIEW_PX`.
    Viewed(String, Vec<u8>),
    /// The file is in the media cache, or could not be fetched.
    Fetched(String, Option<PathBuf>),
    /// An attachment is in the send queue, or failed before it.
    Attached,
    /// A video that cannot go as it is, `why`; ffmpeg is here to make an MP4 of it (`Cmd::Mp4`).
    OfferMp4 { room: OwnedRoomId, path: PathBuf, reply_to: Option<String>, caption: String, why: mp4::Unfit },
    Networks(Vec<Network>),
    /// Logins that need signing in again, as (bridge, login name); checked now and then.
    Attention(Vec<(String, String)>),
    /// How many merged chats the Beeper account keeps active (`max-merge-chats`); -1 for no limit.
    MergeLimit(i64),
    Flows(String, Vec<Flow>),
    /// A login's Discord servers, by login ID.
    Guilds(String, Vec<Guild>),
    LoginStep(Step),
    /// A request made for the settings pages failed; `Error` carries the reason.
    PanelFailed,
    Error(String),
    /// The cards of a contact file; empty when it could not be read.
    Contact(String, Vec<crate::vcard::Card>),
    People(rooms::Answer),
    /// The original file is in the media cache (or could not be fetched), for what the user asked.
    Exported(String, Option<PathBuf>, Export),
    /// A page of GIFs for the picker; `None` when it could not be loaded.
    Gifs { query: String, page: u32, found: Option<gifs::Found> },
    /// A GIF's preview (JPEG) by its URL.
    GifPreview(String, Vec<u8>),
    /// A GIF, or a message with a link's preview, finished sending or failed.
    Uploaded,
    /// Joined members of a chat other than the user, as (user, name), at most `MAX_MEMBERS`.
    Members { room: OwnedRoomId, list: Vec<(OwnedUserId, String)> },
    /// The versions of message `id`, oldest first; `None` when the server could not give them.
    EditHistory { id: String, versions: Option<Vec<edits::Version>> },
    /// The sticker packs offered in a chat, at most `stickers::MAX_PACKS`.
    Stickers { room: OwnedRoomId, packs: Vec<stickers::Pack> },
    /// A sticker's picture by its `mxc://` URL, PNG or JPEG of at most `stickers::THUMB_PX`.
    StickerThumb(String, Vec<u8>),
    /// A forward was queued in this many chats.
    Forwarded(usize),
    Devices(verify::Devices),
    /// Loading or renaming the devices failed, with the reason.
    DevicesFailed(String),
    Verify(verify::Verify),
    /// The people of this chat whose keys changed since this device first saw them; empty when
    /// none (any more).
    Identities(OwnedRoomId, Vec<verify::Changed>),
    /// What signing out a device needs next; `Ok(None)` when it is signed out.
    SignOutStep(Result<Option<verify::Auth>, String>),
    /// The open chat's disappearing messages, when it opens and whenever its timer changes.
    Disappear { room: OwnedRoomId, setting: Disappearing },
    /// What the open chat's network takes, when it opens and whenever its bridge says anew.
    /// `None`: no bridge says, so the chat is plain Matrix and takes everything.
    /// `moderate`: the user's power level lets them delete other people's messages.
    Features { room: OwnedRoomId, features: Option<Features>, moderate: bool },
    /// The open chat's pinned messages and whether the user may change them, when it opens and
    /// whenever its pins or power levels change.
    Pins { room: OwnedRoomId, pins: Pins },
    /// A chat's details for its info screen; sent again as more is known.
    Info(OwnedRoomId, Box<info::Details>),
    Group { room: OwnedRoomId, change: group::Change },
    Candidates { room: OwnedRoomId, list: Vec<(OwnedUserId, String)> },
    MemberIds(OwnedUserId, Vec<String>),
    Profile(group::Profile),
    /// What checking for or installing an update did.
    Update(update::Step),
    EmojiSet(crate::emoji::Fetch),
    /// The people this account ignores: when signed in, and whenever the list changes anywhere.
    Blocked(Vec<block::Blocked>),
    /// The other person of an opened one-to-one chat.
    Peer(OwnedRoomId, OwnedUserId),
    /// The answer to `Cmd::PeerIds`; never sent for a group.
    PeerIds(OwnedRoomId, Vec<String>),
    /// Messages with media, files or links, newest first; `scanned` counts the events read so far.
    InfoItems { room: OwnedRoomId, items: Vec<Msg>, scanned: usize, done: bool },
    /// The answer to `Cmd::Storage` and `Cmd::ClearMedia`.
    Storage(storage::Usage),
    /// The answer to `Cmd::Search` for this room, query and filter.
    Found { room: Option<OwnedRoomId>, query: String, filter: Filter, hits: Vec<Hit> },
    /// Another device of the account asks for this one's settings; the UI answers with
    /// `Cmd::GiveSettings`.
    SettingsAsked { device: String, request: String },
    /// Settings from another device of the account, as an exported settings file.
    SettingsArrived(Vec<u8>),
    /// How the local-network exchange goes.
    Handoff(handoff::Step),
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
    /// The room's push rules, as matrix-sdk sets them: `Mute` an override rule without a notify
    /// action, `MentionsAndKeywordsOnly` a room rule without one (Beeper's mute), `AllMessages` neither.
    Mute(RoomNotificationMode),
    /// `m.marked_unread`; clearing it also sends a read receipt.
    Unread(bool),
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomSummary {
    pub id: OwnedRoomId,
    pub name: String,
    pub preview: Option<String>,
    /// The event the preview was made from, so that its edit or deletion can change the preview.
    #[serde(default)]
    pub preview_id: Option<String>,
    pub ts: u64,
    /// Bridge network, e.g. "WhatsApp"; none for native Matrix rooms.
    #[serde(default)]
    pub network: Option<String>,
    /// The server, workspace, forum or community the chat belongs to, from `m.bridge` `network`.
    #[serde(default)]
    pub parent: Option<String>,
    /// A Telegram forum topic: the forum's id from `m.bridge` `network` (`forum::forum_id`).
    #[serde(default)]
    pub forum: Option<String>,
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
    /// The chat carries an archive mark that a newer message has overridden: archived again where
    /// muted chats stay archived (Signal's "Keep muted chats archived"), a choice of this device.
    #[serde(default)]
    pub archive_mark: bool,
    /// Beeper's server-side order of the newest message from someone else; 0 when unknown.
    #[serde(default)]
    pub order: i64,
    /// `ts` is known to be the last message's time, or 0 because there is no message: either a
    /// message was seen, or the history was looked at. Without it `ts` may be a guess (caches from
    /// builds that took any event's time), and the chat is queued for a look.
    #[serde(default)]
    pub dated: bool,
    /// The chat waits for the user to take it or turn it down.
    #[serde(default)]
    pub pending: Option<Pending>,
    /// The room was upgraded: the room that replaces it.
    #[serde(default)]
    pub moved_to: Option<OwnedRoomId>,
    /// The preview is the user's own message.
    #[serde(default)]
    pub own_last: bool,
    /// Who has read that message, as their public receipts on it say; a few, named.
    #[serde(default)]
    pub read_by: Vec<(OwnedUserId, String)>,
    /// The user's note to self (`com.beeper.rooms.note_to_self`): only they are in it.
    #[serde(default)]
    pub note: bool,
    /// A merged chat (`com.beeper.union`) and the chats it stands for.
    #[serde(default)]
    pub union: Option<union::Union>,
    /// The bridge blocked the other person of this one-to-one chat on the network (`block.rs`).
    #[serde(default)]
    pub net_blocked: bool,
    /// The other person of a one-to-one chat.
    #[serde(default)]
    pub peer: Option<OwnedUserId>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub enum Pending {
    /// An invitation, and who sent it.
    Invite { by: Option<OwnedUserId>, name: String },
    /// A bridged network's message request (`com.beeper.message_request` in `m.bridge`): the other
    /// side learns nothing of the user until it is accepted.
    Request,
}

impl RoomSummary {
    pub fn is_unread(&self) -> bool {
        self.unread > 0 || self.marked_unread || matches!(self.pending, Some(Pending::Invite { .. }))
    }
}

#[derive(Clone)]
pub struct Msg {
    /// The event's ID; a message still on its way out has its transaction ID instead.
    pub id: String,
    pub kind: Kind,
    pub sender: String,
    pub from: OwnedUserId,
    /// The text as plain text; the formatted body's, when there is one.
    pub body: String,
    /// `body` as the sender formatted it. Its runs point into `body`.
    pub rich: Option<Rich>,
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
    /// The ranges of `mentions` that name the user or the whole room.
    pub to_me: Vec<std::ops::Range<usize>>,
    /// Preview of a link in the message, when the sender's client or bridge attached one.
    pub link: Option<Link>,
    /// One per emoji, in the order they were first used.
    pub reactions: Vec<Reaction>,
    /// Where a message of the user's is on its way to the others.
    pub delivery: Delivery,
    /// The other people whose read receipt stands on this row, with when they sent it (ms, 0 when
    /// unknown): they have read this far.
    pub seen: Vec<(OwnedUserId, u64)>,
    /// The message disappears after this many ms (`com.beeper.disappearing_timer`); 0 when it
    /// stays. On a bridge's status row: the timer the bridge gave the user's message.
    pub disappear: u64,
    /// The Meta bridge's placeholder for a message it could not decrypt (`fi.mau.meta.undecryptable`).
    pub undecryptable: bool,
    /// In a merged chat, the chat the row comes from; `None` elsewhere.
    pub lane: Option<OwnedRoomId>,
}

/// What a row of a conversation is.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Message,
    /// A line about the chat itself, in `body`: someone left, the name changed.
    Notice,
    /// `sender` joined. Joins that follow one another are told on one line.
    Joined,
    /// `by` (the user's "You" when `own`) invited `sender`, also told in `body`. Invitations one
    /// person sends one after another are told on one line.
    Invited { by: String },
    /// The day the rows under it belong to.
    Day,
    /// The rows under it are new since the user last read the chat.
    Unread,
    /// A bridge's word on what the network did with the user's message `target`.
    Status { target: String, delivery: Delivery },
    /// Takes a place in the list and shows nothing.
    Hidden,
}

/// The user's own read receipts in the open conversation: each as its event and, when the
/// timeline hides that event, the shown row it falls back to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OwnRead {
    /// The newest of them, public or private.
    pub read: Option<(String, Option<String>)>,
    /// The public one (`m.read`), which the others see, and when it was sent.
    pub public: Option<(String, Option<String>, u64)>,
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

/// Whether the message a reply answers is known.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum QuoteState {
    Ready,
    /// Being fetched from the server.
    Loading,
    /// Not found, or not fetched.
    Missing,
}

/// The message a reply answers, as far as it is known.
#[derive(Clone)]
pub struct Quote {
    pub id: String,
    pub state: QuoteState,
    /// Empty unless `state` is `Ready`.
    pub sender: String,
    pub own: bool,
    /// The text or caption; a poll's question; a place's description.
    pub text: String,
    pub deleted: bool,
    pub media: Option<Media>,
    pub poll: bool,
    pub place: bool,
}

impl Quote {
    /// A message that is not, or not yet, known.
    pub fn unknown(id: String, state: QuoteState) -> Self {
        Self { id, state, sender: String::new(), own: false, text: String::new(), deleted: false, media: None, poll: false, place: false }
    }
}

impl Msg {
    /// A row with nothing on it yet.
    pub(crate) fn bare(id: String, kind: Kind, from: OwnedUserId) -> Self {
        Self {
            id,
            kind,
            sender: String::new(),
            from,
            body: String::new(),
            rich: None,
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
            to_me: Vec::new(),
            link: None,
            reactions: Vec::new(),
            delivery: Delivery::Sent,
            seen: Vec::new(),
            disappear: 0,
            undecryptable: false,
            lane: None,
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
    /// For a GIF, `looped`: it may move.
    pub image: Option<Media>,
    /// The page's colour as `0xrrggbb`, which the card's bar takes.
    pub colour: Option<u32>,
    /// A video page: its picture gets a play mark.
    pub video: bool,
}

impl Msg {
    /// This message as a reply quotes it.
    pub fn quote(&self) -> Quote {
        let text = match (&self.poll, &self.place) {
            (Some(poll), _) => poll.question.clone(),
            (_, Some(place)) => place.description.clone(),
            _ => rich::masked(&self.body, self.rich.as_ref()),
        };
        Quote {
            id: self.id.clone(),
            state: QuoteState::Ready,
            sender: self.sender.clone(),
            own: self.own,
            text,
            deleted: self.deleted,
            media: self.media.clone().filter(|_| !self.deleted),
            poll: self.poll.is_some(),
            place: self.place.is_some(),
        }
    }

    /// One-line description for previews: Beeper's label for media, plus the caption.
    pub fn summary(&self) -> String {
        if let Some(poll) = &self.poll {
            return t!("📊 Poll: {question}", question = poll.question);
        }
        if let Some(place) = &self.place {
            return if place.description.is_empty() { t!("📍 Location").to_owned() } else { t!("📍 Location: {place}", place = place.description) };
        }
        let body = rich::masked(&self.body, self.rich.as_ref());
        let Some(media) = &self.media else { return body };
        let label = match media.kind {
            MediaKind::Image => t!("📷 Photo"),
            MediaKind::Sticker => t!("🖼 Sticker"),
            MediaKind::Video => t!("🎬 Video"),
            MediaKind::Voice => t!("🎤 Voice Note"),
            MediaKind::Audio => t!("🎵 Audio"),
            MediaKind::File => return format!("📎 {}", media.name),
        };
        if body.is_empty() { label.to_owned() } else { format!("{label}: {body}") }
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
    /// A video the sender's network plays as a GIF: on repeat, without sound.
    pub looped: bool,
    /// Media the sender's network lets the user open once (`com.beeper.view_limited`): shown as a
    /// placeholder, never cached, saved or forwarded.
    pub once: bool,
    /// A voice message's loudness as `voice::BARS` heights, 0 to 255; empty when it has none.
    pub waveform: Vec<u8>,
}

#[derive(Clone)]
pub struct Reaction {
    pub key: String,
    /// How many people gave it.
    pub count: usize,
    pub own: bool,
    /// The others who gave it, in the order they did; at most `MAX_SEEN`.
    pub by: Vec<OwnedUserId>,
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

/// Each account's command channel, by slot, for what happens outside the UI: a reply typed into a
/// notification.
#[cfg(target_os = "android")]
type Commands = mpsc::Sender<Cmd>;
/// Weak: desktop notifications must not keep the backend running once the UI is gone.
#[cfg(not(target_os = "android"))]
type Commands = mpsc::WeakSender<Cmd>;
static COMMANDS: std::sync::Mutex<[Option<Commands>; crate::accounts::MAX]> = std::sync::Mutex::new([const { None }; crate::accounts::MAX]);

fn commands(slot: usize) -> Option<mpsc::Sender<Cmd>> {
    let all = COMMANDS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    #[cfg(target_os = "android")]
    return all.get(slot)?.clone();
    #[cfg(not(target_os = "android"))]
    all.get(slot)?.as_ref()?.upgrade()
}

/// Lets go of the account's channel, so the backend ends once the UI drops its sender too.
pub(crate) fn forget(slot: usize) {
    if let Some(tx) = COMMANDS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_mut(slot) {
        *tx = None;
    }
}

/// The chat of the test notification: no room, so its buttons send nothing.
pub(crate) const TEST_CHAT: &str = "test";

/// Shows the test notification through the same path a message takes, the user's picture as its face.
async fn test_notice(client: Client, dir: PathBuf) {
    let face = async {
        let bytes = picture(&client, client.account().get_avatar_url().await.ok()??).await.ok()?;
        let path = dir.join("notify").join(TEST_CHAT);
        std::fs::create_dir_all(path.parent()?).ok()?;
        write_private(&path, &notification_face(bytes)).ok()?;
        Some(path)
    };
    let file = tokio::time::timeout(ICON_WAIT, face).await.ok().flatten();
    let icon = file.as_deref().and_then(Path::to_str).unwrap_or_default();
    let note = Note { id: String::new(), sender: "Unisend".into(), text: t!("Test notification. Reply, mark as read or mute to try the buttons: nothing is sent.").into(), ts: now_ms().max(0) as u64, mention: false };
    let chat = crate::accounts::key(crate::accounts::slot_of(&dir), TEST_CHAT);
    crate::notify(&crate::Notice { chat: &chat, name: "Unisend", group: false, icon, note: &note, alert: true, quiet: false });
    // Android has decoded the picture by now; desktop removes it with the notification.
    if cfg!(target_os = "android")
        && let Some(file) = file
    {
        let _ = std::fs::remove_file(file);
    }
}

/// Whether a link typed into the composer may get a preview: see `linkprev::allowed`.
pub(crate) fn link_previewable(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| linkprev::allowed(&u))
}

/// Sends a message from a notification's reply box, through the account `chat`'s key names.
/// False when that account is not signed in.
pub(crate) fn reply(chat: &str, body: &str) -> bool {
    log::debug!("notification: reply");
    let (slot, chat) = crate::accounts::split(chat);
    if chat == TEST_CHAT {
        log::info!("test notification: reply");
        return !body.trim().is_empty();
    }
    let (body, Ok(room)) = (body.trim(), OwnedRoomId::try_from(chat)) else { return false };
    !body.is_empty() && commands(slot).is_some_and(|tx| tx.try_send(Cmd::Send { room, body: body.to_owned(), reply_to: None, edit: None, mentions: Vec::new(), preview: None }).is_ok())
}

/// Android's answer when it did not install an update: shown by the account on screen.
#[cfg(target_os = "android")]
pub(crate) fn update_failed(why: String) {
    if commands(crate::accounts::active()).is_none_or(|tx| tx.try_send(Cmd::UpdateFailed(why)).is_err()) {
        log::warn!("update failure not shown");
    }
}

/// Marks a chat read from its notification.
pub(crate) fn read_from_notification(chat: &str) {
    log::debug!("notification: mark read");
    let (slot, chat) = crate::accounts::split(chat);
    if chat == TEST_CHAT {
        log::info!("test notification: mark read");
    }
    if let (Some(tx), Ok(room)) = (commands(slot), OwnedRoomId::try_from(chat)) {
        let _ = tx.try_send(Cmd::Chat(room, ChatAction::Unread(false)));
    }
}

/// Mutes a chat from its notification. False when the app is not signed in.
pub(crate) fn mute_from_notification(chat: &str) -> bool {
    log::debug!("notification: mute");
    let (slot, chat) = crate::accounts::split(chat);
    if chat == TEST_CHAT {
        log::info!("test notification: mute");
        return true;
    }
    let Ok(room) = OwnedRoomId::try_from(chat) else { return false };
    commands(slot).is_some_and(|tx| tx.try_send(Cmd::Chat(room, ChatAction::Mute(RoomNotificationMode::MentionsAndKeywordsOnly))).is_ok())
}

/// Stack of the backend's threads. Opening a chat polls deep matrix-sdk futures whose debug-build
/// frames outgrow the default 2 MiB; only the pages actually touched cost memory.
const BACKEND_STACK: usize = 8 << 20;

/// A running backend: its channels, and its thread, which ends once `cmd` and every clone of it
/// are dropped (see `forget`) and its tasks have stopped.
pub struct Backend {
    pub cmd: mpsc::Sender<Cmd>,
    pub events: mpsc::Receiver<Ev>,
    pub thread: std::thread::JoinHandle<()>,
}

/// Starts the backend of the account whose data is in `dir`.
pub fn start(dir: PathBuf) -> Backend {
    let (cmd_tx, cmd_rx) = mpsc::channel(CHANNEL_CAP);
    let slot = crate::accounts::slot_of(&dir);
    #[cfg(target_os = "android")]
    let registered = cmd_tx.clone();
    #[cfg(not(target_os = "android"))]
    let registered = cmd_tx.downgrade();
    COMMANDS.lock().unwrap_or_else(std::sync::PoisonError::into_inner)[slot] = Some(registered);
    let (ev_tx, ev_rx) = mpsc::channel(CHANNEL_CAP);
    let thread = std::thread::Builder::new()
        .name(format!("matrix-{slot}"))
        .stack_size(BACKEND_STACK)
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(BACKEND_STACK)
                .enable_all()
                .build()
                .expect("tokio runtime");
            // Returns once the UI drops its sender; dropping `rt` then cancels sync and waits for
            // the worker threads.
            rt.block_on(run(dir, cmd_rx, ev_tx));
        })
        .expect("spawn matrix thread");
    Backend { cmd: cmd_tx, events: ev_rx, thread }
}

fn http_client() -> Result<reqwest::Client> {
    Ok(http_builder().build()?)
}

fn http_builder() -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(15));
    // shortcut: bundled Mozilla roots on Android (ignores user-installed CAs); switch to
    // rustls-platform-verifier's Android init once its Kotlin component is packaged.
    #[cfg(target_os = "android")]
    let builder = builder.tls_certs_only(
        webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .filter_map(|der| reqwest::Certificate::from_der(der).ok()),
    );
    builder
}

struct State {
    dir: PathBuf,
    homeserver: String,
    /// Signed in through Beeper: only then may the access token reach Beeper's account API.
    beeper: bool,
    http: reqwest::Client,
    ev: mpsc::Sender<Ev>,
    /// How the store and the files with chat content are kept; plain until a key is read or made.
    seal: Seal,
    client: Option<Client>,
    login_request: Option<String>,
    /// The conversation on screen.
    open: Option<timeline::Open>,
    /// The merged chat on screen, instead of `open`.
    merged: Option<union::Merged>,
    /// The account's `max-merge-chats` once Beeper said it; `i64::MIN` before.
    merge_limit: Arc<AtomicI64>,
    rooms: Rooms,
    /// The blocked people last told to the UI, told again when the window turns to this account.
    blocked: Arc<Mutex<Vec<block::Blocked>>>,
    fetches: Arc<tokio::sync::Semaphore>,
    previews: Arc<tokio::sync::Semaphore>,
    /// Push rules, kept current by the SDK. One instance: each registers an event handler.
    notif: Option<NotificationSettings>,
    /// Read by the tasks that follow an open conversation.
    incognito: Arc<AtomicBool>,
    /// Sync and recovery watchers, stopped on sign-out.
    tasks: Vec<tokio::task::JoinHandle<()>>,
    verify: verify::Verifier,
    /// Gathers what the info screen shows.
    info: Option<AbortOnDrop>,
    /// Looks up the person behind a chat's sheet; a newer sheet replaces it.
    peer: Option<AbortOnDrop>,
    /// The network login's request in flight; leaving the login cancels it.
    login: Option<AbortOnDrop>,
    /// A sign-in waiting for the browser to come back.
    browser: Option<signin::Pending>,
    /// The account on disk cannot be opened here (key gone, session unreadable), so a sign-in may
    /// replace it; otherwise a sign-in never deletes a session that may still open.
    replaceable: bool,
    /// The update being downloaded and installed.
    update: Option<AbortOnDrop>,
    /// The preview of the link being typed: in the making, then made. One at a time.
    link_task: Option<AbortOnDrop>,
    link_made: Arc<Mutex<Option<linkprev::Preview>>>,
    handoff: handoff::Handoff,
}

/// What the composer shows of a link's preview before it is sent.
pub struct LinkDraft {
    pub title: String,
    pub site: String,
    pub picture: Option<Vec<u8>>,
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
    let _ = std::fs::remove_dir_all(media_dir(&dir));
    let _ = std::fs::remove_dir_all(dir.join(PASTED));
    let _ = std::fs::remove_dir_all(update::folder(&dir));
    // Older builds kept KLIPY's customer ID on disk; it now lives only in memory.
    let _ = std::fs::remove_file(dir.join("klipy-id"));
    let mut st = State { dir, homeserver: beeper_homeserver(), beeper: true, http, ev, seal: Seal::Plain, client: None, login_request: None, open: None, merged: None, merge_limit: Arc::new(AtomicI64::new(i64::MIN)),
        rooms: Rooms::default(), blocked: Arc::default(), fetches: Arc::new(tokio::sync::Semaphore::new(ROW_FETCHES)), previews: Arc::new(tokio::sync::Semaphore::new(PROBES)), notif: None,
        incognito: Arc::new(AtomicBool::new(true)), tasks: Vec::new(), verify: verify::Verifier::default(), info: None, peer: None, login: None, browser: None, replaceable: false, update: None, link_task: None, link_made: Arc::default(), handoff: Default::default() };
    tokio::spawn(sweep_media_every(media_dir(&st.dir)));
    // One service shows every account's notifications; the first account's backend, which runs
    // as long as the app, keeps it.
    #[cfg(not(target_os = "android"))]
    if crate::accounts::slot_of(&st.dir) == 0 {
        tokio::spawn(crate::notify::serve(st.dir.clone()));
    }
    let restored = if st.unlock().await { st.restore_retrying().await } else { false };
    if !restored {
        st.emit(Ev::NeedLogin).await;
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
    /// The homeserver of an account signed in with a password; none for Beeper, which older files
    /// always are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    homeserver: Option<String>,
    /// The OAuth 2.0 client ID of a session signed in through the server's authorization service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    oauth: Option<String>,
}

fn beeper_homeserver() -> String {
    // Debug builds can point at a local homeserver with a pre-seeded session.json.
    std::env::var("BEEPER_GPUI_HOMESERVER")
        .ok()
        .filter(|_| cfg!(debug_assertions))
        .unwrap_or_else(|| beeper::HOMESERVER_URL.to_owned())
}

/// The machine this app runs on, where plain http is allowed (a homeserver under test).
fn is_local(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

/// What the homeserver field (or a full user ID, `@name:server`) names, in the form the SDK's
/// discovery takes: a server name or an https URL. Plain http only for this machine.
pub(crate) fn server_from_input(input: &str) -> Result<String, &'static str> {
    let input = input.trim();
    let input = match input.strip_prefix('@') {
        Some(user) => user.split_once(':').map(|(_, server)| server).ok_or(t!("Enter a server, like matrix.org."))?,
        None => input,
    };
    if input.is_empty() {
        return Err(t!("Enter a server, like matrix.org."));
    }
    if input.contains(char::is_whitespace) {
        return Err(t!("A server address has no spaces."));
    }
    let lower = input.to_ascii_lowercase();
    let (scheme, rest) = match lower.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, lower.as_str()),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().map_or("", |h| if h == "::1" { "[::1]" } else { h }),
        None => authority.split(':').next().unwrap_or_default(),
    };
    if host.is_empty() {
        return Err(t!("Enter a server, like matrix.org."));
    }
    match scheme {
        Some("https") => Ok(input.to_owned()),
        Some("http") if is_local(host) => Ok(input.to_owned()),
        Some("http") => Err(t!("This address is not encrypted. Use https.")),
        Some(_) => Err(t!("Enter a server, like matrix.org, or an https address.")),
        None if is_local(host) => Ok(format!("http://{input}")),
        None => Ok(input.to_owned()),
    }
}

/// True when the error, or one it wraps, is a connection that never got an answer.
fn unreachable(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut next = Some(error);
    while let Some(e) = next {
        if let Some(e) = e.downcast_ref::<reqwest::Error>()
            && (e.is_connect() || e.is_timeout())
        {
            return true;
        }
        next = e.source();
    }
    false
}

impl State {
    async fn emit(&self, ev: Ev) {
        // Only fails when the UI is gone, and then nobody is listening.
        let _ = self.ev.send(ev).await;
    }

    fn client_builder(&self) -> matrix_sdk::ClientBuilder {
        // Fetch a missing room key from the server-side backup when an event fails to decrypt,
        // instead of downloading the whole backup after recovery.
        let e2ee = EncryptionSettings {
            backup_download_strategy: BackupDownloadStrategy::AfterDecryptionFailure,
            ..Default::default()
        };
        let store = matrix_sdk::SqliteStoreConfig::new(self.dir.join("store")).key(self.seal.key());
        Client::builder()
            .sqlite_store_with_config_and_cache_path(store, None::<PathBuf>)
            .http_client(self.http.clone())
            .with_encryption_settings(e2ee)
            .handle_refresh_tokens()
    }

    /// Reads the store key. A Keystore that fails for now is asked again, with nothing deleted; a
    /// key that is gone leaves the device signed out (false).
    async fn unlock(&mut self) -> bool {
        let mut wait = Duration::from_secs(2);
        let mut told = false;
        loop {
            match vault::load(&self.dir, vault::keystore()) {
                Ok(seal) => {
                    self.seal = seal;
                    return true;
                }
                Err(KeyError::Lost(e)) => {
                    log::warn!("{e}");
                    self.lost().await;
                    return false;
                }
                Err(KeyError::Transient(e)) => {
                    log::warn!("{e}");
                    if !std::mem::replace(&mut told, true) {
                        self.emit(Ev::Error(t!("Could not open the encrypted data on this device. Trying again.").into())).await;
                    }
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(Duration::from_secs(60));
                }
            }
        }
    }

    /// The key to this device's data is gone for good: the device is signed out. Nothing is
    /// deleted here, in case the key comes back; the next sign-in replaces the files.
    async fn lost(&mut self) {
        self.emit(Ev::Error(t!("The key that encrypts this device's data is gone, so it was signed out. Sign in again; your recovery key brings back message history.").into())).await;
        (self.seal, self.replaceable) = (Seal::Plain, true);
    }

    /// The saved session is damaged or from a newer version of the app: signed out here, with the
    /// files kept for the version that can read them until a sign-in replaces them.
    async fn unreadable(&mut self) {
        self.emit(Ev::Error(t!("The sign-in saved on this device could not be read. Sign in again.").into())).await;
        (self.seal, self.replaceable) = (Seal::Plain, true);
    }

    /// `restore`, asked again after 2 s, doubling up to 60 s, while a session exists and fails to
    /// open for a reason that may pass; only a lost key or session leads to the sign-in screen.
    async fn restore_retrying(&mut self) -> bool {
        let mut wait = Duration::from_secs(2);
        let mut told = false;
        loop {
            match self.restore().await {
                Ok(restored) => return restored,
                Err(e) if vault::signed_in(&self.dir) => {
                    log::warn!("restore: {e:#}");
                    if !std::mem::replace(&mut told, true) {
                        self.emit(Ev::Error(t!("Could not open the data on this device: {error}. Trying again.", error = format!("{e:#}")))).await;
                    }
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(Duration::from_secs(60));
                }
                Err(e) => {
                    self.emit(Ev::Error(t!("Could not restore session: {error}", error = format!("{e:#}")))).await;
                    return false;
                }
            }
        }
    }

    async fn build_client(&self) -> Result<Client> {
        Ok(self.client_builder().homeserver_url(&self.homeserver).build().await?)
    }

    async fn restore(&mut self) -> Result<bool> {
        let raw = match self.seal.read(&self.dir, vault::SESSION) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            // A key that unwraps but does not open the session is not this device's key.
            Err(e) if vault::broken(&e) == Some(&vault::Broken::Tag) => {
                log::warn!("{e}");
                self.lost().await;
                return Ok(false);
            }
            Err(e) if vault::broken(&e).is_some() => {
                log::warn!("{e}");
                self.unreadable().await;
                return Ok(false);
            }
            Err(e) => return Err(e).context("session"),
        };
        let saved: SavedSession = match serde_json::from_slice(&raw) {
            Ok(saved) => saved,
            Err(e) => {
                log::warn!("session: {e}");
                self.unreadable().await;
                return Ok(false);
            }
        };
        let beeper = saved.homeserver.is_none();
        let homeserver = saved.homeserver.clone().unwrap_or_else(|| self.homeserver.clone());
        let client = match self.client_builder().homeserver_url(&homeserver).build().await {
            Ok(client) => client,
            Err(e) if key_rejected(&e) => {
                log::warn!("store: {e}");
                self.lost().await;
                return Ok(false);
            }
            Err(e) => return Err(e.into()),
        };
        if saved.session.tokens.refresh_token.is_some() {
            signin::keep_tokens(&client, &self.dir, &self.seal, saved.homeserver.clone())?;
        }
        signin::restore(&client, saved).await?;
        (self.homeserver, self.beeper) = (homeserver, beeper);
        if let Some(rooms) = load_rooms(&self.dir, &self.seal) {
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
        // Left by an account the server signed out; detached, they would follow its client still.
        for task in self.tasks.drain(..) {
            task.abort();
        }
        // Sends left in the queue by the last run go out now, not once their chat is opened.
        client.send_queue().respawn_tasks_for_rooms_with_unsent_requests().await;
        self.tasks = vec![tokio::spawn(watch_recovery(client.clone(), self.ev.clone())), tokio::spawn(block::watch(client.clone(), self.ev.clone(), self.blocked.clone())), self.sync_task(&client, &notif)];
        self.notif = Some(notif);
        self.verify.listen(&client, &self.ev);
        self.handoff.listen(&client, &self.ev);
        list_typing(&client, &self.ev);
        search::start(&self.dir, &client, &self.seal);
        let user = client.user_id().map(ToOwned::to_owned);
        self.client = Some(client);
        // Tests sign in to a local server as if it were Beeper's; its token stays off Beeper's API.
        if let Some(api) = self.api().ok().filter(|_| !cfg!(test)) {
            self.tasks.push(tokio::spawn(watch_networks(api, self.ev.clone(), self.merge_limit.clone())));
        }
        if let Some(user) = user {
            self.emit(Ev::Ready { user, beeper: self.beeper, seal: self.seal.clone() }).await;
        }
    }

    /// The sync loop, last in `tasks` so `Cmd::Resync` finds it.
    fn sync_task(&self, client: &Client, notif: &NotificationSettings) -> tokio::task::JoinHandle<()> {
        tokio::spawn(sync_watched(client.clone(), notif.clone(), self.dir.clone(), self.seal.clone(), self.rooms.clone(), self.ev.clone()))
    }

    /// A sign-in starts clean: a store left behind belongs to a device that never finished logging
    /// in. On Android it gets a key of its own, so the store is encrypted from its first byte.
    /// A session that may still open is never replaced: only signing out deletes it.
    fn fresh_store(&mut self) -> Result<()> {
        if vault::signed_in(&self.dir) && !self.replaceable {
            return Err(anyhow!(t!("Already signed in on this device. Sign out first.")));
        }
        self.rooms.lock().unwrap().clear();
        let store = self.dir.join("store");
        if store.exists() {
            log::warn!("removing orphaned store");
            std::fs::remove_dir_all(&store)?;
        }
        wipe_account(&self.dir);
        self.seal = match vault::keystore() {
            Some(keystore) => vault::create(&self.dir, keystore).map_err(|e| {
                log::warn!("{e}");
                anyhow!(t!("Could not create the key that encrypts this device's data. Try again."))
            })?,
            None => Seal::Plain,
        };
        self.replaceable = false;
        Ok(())
    }

    async fn login_jwt(&mut self, jwt: String) -> Result<()> {
        self.fresh_store()?;
        self.beeper = true;
        let client = self.build_client().await?;
        let mut data = JsonObject::new();
        data.insert("token".into(), jwt.into());
        client
            .matrix_auth()
            .login_custom("org.matrix.login.jwt", data)?
            .initial_device_display_name("Unisend")
            .await?;
        let session = client.matrix_auth().session().ok_or_else(|| anyhow!(t!("Login returned no session")))?;
        self.seal.write(&self.dir, vault::SESSION, &serde_json::to_vec(&SavedSession { session, homeserver: None, oauth: None })?)?;
        self.go_live(client).await;
        Ok(())
    }

    /// Password sign-in to the homeserver `server` names, found the way the SDK discovers it.
    async fn login_password(&mut self, server: &str, user: &str, password: &str) -> Result<()> {
        let server = server_from_input(server).map_err(|e| anyhow!(e))?;
        let user = user.trim();
        if user.is_empty() || password.is_empty() {
            return Err(anyhow!(t!("Enter your user name and password.")));
        }
        self.browser = None;
        self.fresh_store()?;
        let client = self.find(self.client_builder(), &server).await?;
        let homeserver = client.homeserver();
        let auth = client.matrix_auth();
        let types = auth.get_login_types().await.map_err(|e| match unreachable(&e) {
            true => anyhow!(t!("Could not reach {server}. Check your connection.", server = server)),
            false => anyhow!(t!("{server} did not answer like a Matrix server: {error}", server = server, error = plain(&e))),
        })?;
        if !types.flows.iter().any(|flow| matches!(flow, LoginType::Password(_))) {
            return Err(anyhow!(t!(
                "{server} signs people in another way (such as single sign-on) that this app does not support yet.",
                server = server
            )));

        }
        auth.login_username(user, password).initial_device_display_name("Unisend").await.map_err(|e| {
            match e.client_api_error_kind() {
                Some(ErrorKind::Forbidden) => anyhow!(t!("Wrong user name or password.")),
                Some(ErrorKind::UserDeactivated) => anyhow!(t!("This account has been deactivated.")),
                Some(ErrorKind::LimitExceeded(_)) => anyhow!(t!("Too many attempts. Wait a minute and try again.")),
                _ if unreachable(&e) => anyhow!(t!("Could not reach {server}. Check your connection.", server = server)),
                _ => anyhow!(t!("Could not sign in: {error}", error = plain(&e))),
            }
        })?;
        let session = auth.session().ok_or_else(|| anyhow!(t!("Login returned no session")))?;
        self.homeserver = homeserver.to_string();
        self.beeper = false;
        let saved = SavedSession { session, homeserver: Some(self.homeserver.clone()), oauth: None };
        self.seal.write(&self.dir, vault::SESSION, &serde_json::to_vec(&saved)?)?;
        self.go_live(client).await;
        Ok(())
    }

    fn client(&self) -> Result<&Client> {
        self.client.as_ref().ok_or_else(|| anyhow!(t!("Not signed in")))
    }

    /// A chat-list action on one room. `pinned_stops` keeps a pinned chat from being archived;
    /// the chats of a merged chat are archived with it whatever their own pin says.
    async fn chat_action(&self, room: &Room, room_id: OwnedRoomId, action: ChatAction, pinned_stops: bool) -> Result<()> {
        match action {
            ChatAction::Pin(on) => room.set_is_favourite(on, Some(1.0)).await?,
            ChatAction::LowPriority(on) => room.set_is_low_priority(on, Some(0.0)).await?,
            ChatAction::Unread(true) => room.set_unread_flag(true).await?,
            ChatAction::Unread(false) => read_newest(room, self.incognito.load(Ordering::Relaxed)).await?,
            ChatAction::Mute(mode) => {
                let notif = self.notif.as_ref().context(t!("Not signed in"))?;
                if mode == RoomNotificationMode::AllMessages {
                    notif.delete_user_defined_room_rules(&room_id).await?;
                } else {
                    notif.set_room_notification_mode(&room_id, mode).await?;
                }
            }
            ChatAction::Archive(false) => {
                room.set_account_data_raw(DONE.into(), Raw::new(&Done::default())?.cast_unchecked()).await?;
            }
            ChatAction::Archive(true) => {
                if pinned_stops && room.is_favourite() {
                    return Err(anyhow!(t!("Pinned chats can't be archived")));
                }
                // Like Beeper: archived up to the newest event, by the server's own order.
                let resp = room.messages(MessagesOptions::backward()).await?;
                let newest = resp.chunk.first();
                let order = newest.and_then(hs_order).filter(|o| *o != 0);
                let done = Done { updated_ts: Some(now_ms()), at_order: order };
                room.set_account_data_raw(DONE.into(), Raw::new(&done)?.cast_unchecked()).await?;
                // Without a server order the rule compares timestamps, which never archive an unread chat.
                if order.is_none() || mark_read_on_archive(self.client()?).await {
                    mark_read(room, newest.and_then(|e| e.event_id()).map(ToOwned::to_owned), self.incognito.load(Ordering::Relaxed)).await?;
                }
            }
        }
        Ok(())
    }

    /// The timelines on screen: the open chat's, or each of the open merged chat's.
    fn opens(&self) -> impl Iterator<Item = &timeline::Open> {
        self.open.iter().chain(self.merged.iter().flat_map(|m| m.lanes.iter()))
    }

    /// The timeline of `room`, when it is on screen alone or in the open merged chat.
    fn opened(&self, room: &RoomId) -> Option<&timeline::Open> {
        self.opens().find(|open| open.room == room)
    }

    /// A command for the open merged chat goes to one of its chats: the one holding the message it
    /// is about (reply, edit, reaction), else the one picked under the composer.
    fn route(&self, cmd: Cmd) -> Result<Cmd> {
        let Some(m) = self.merged.as_ref() else { return Ok(cmd) };
        let at = |room: OwnedRoomId, about: Option<&str>| -> Result<OwnedRoomId> {
            if room != m.id {
                return Ok(room);
            }
            about.and_then(|id| m.lane_of(id)).or_else(|| m.target.clone()).context(t!("This merged chat has no chats on this device"))
        };
        Ok(match cmd {
            Cmd::Send { room, body, reply_to, edit, mentions, preview } => {
                Cmd::Send { room: at(room, edit.as_deref().or(reply_to.as_deref()))?, body, reply_to, edit, mentions, preview }
            }
            Cmd::Attach { room, path, reply_to } => Cmd::Attach { room: at(room, reply_to.as_deref())?, path, reply_to },
            Cmd::AttachMedia { room, path, reply_to, caption, hd } => Cmd::AttachMedia { room: at(room, reply_to.as_deref())?, path, reply_to, caption, hd },
            Cmd::Mp4 { room, path, reply_to, caption } => Cmd::Mp4 { room: at(room, reply_to.as_deref())?, path, reply_to, caption },
            Cmd::Voice { room, path, duration, levels, reply_to } => Cmd::Voice { room: at(room, reply_to.as_deref())?, path, duration, levels, reply_to },
            Cmd::SendLocation { room, lat, lon, accuracy, reply_to } => Cmd::SendLocation { room: at(room, reply_to.as_deref())?, lat, lon, accuracy, reply_to },
            Cmd::SendGif { room, gif, query, reply_to } => Cmd::SendGif { room: at(room, reply_to.as_deref())?, gif, query, reply_to },
            Cmd::SendPoll { room, question, answers, multiple } => Cmd::SendPoll { room: at(room, None)?, question, answers, multiple },
            Cmd::React { room, target, key } => Cmd::React { room: at(room, Some(&target))?, target, key },
            Cmd::Vote { room, target, unstable, answers } => Cmd::Vote { room: at(room, Some(&target))?, target, unstable, answers },
            Cmd::Delete { room, target } => Cmd::Delete { room: at(room, Some(&target))?, target },
            Cmd::Retry { room, target } => Cmd::Retry { room: at(room, Some(&target))?, target },
            Cmd::Pin { room, target, pin } => Cmd::Pin { room: at(room, Some(&target))?, target, pin },
            Cmd::EndPoll { room, target } => Cmd::EndPoll { room: at(room, Some(&target))?, target },
            Cmd::ViewedOnce(room, id) => Cmd::ViewedOnce(at(room, Some(&id))?, id),
            Cmd::ViewPicture { id, room, user } => Cmd::ViewPicture { room: at(room, Some(&id))?, id, user },
            Cmd::Forward { room, targets, to } => Cmd::Forward { room: at(room, targets.first().map(String::as_str))?, targets, to },
            Cmd::Typing(room, on) => Cmd::Typing(at(room, None)?, on),
            Cmd::Face(room, user) => Cmd::Face(at(room, None)?, user),
            Cmd::SetDisappear { room, ms } => Cmd::SetDisappear { room: at(room, None)?, ms },
            Cmd::Members(room) => Cmd::Members(at(room, None)?),
            Cmd::PeerIds(room) => Cmd::PeerIds(at(room, None)?),
            Cmd::Candidates(room) => Cmd::Candidates(at(room, None)?),
            Cmd::MemberIds(room, user) => Cmd::MemberIds(at(room, None)?, user),
            Cmd::Group { room, change } => Cmd::Group { room: at(room, None)?, change },
            // shortcut: a search in a merged chat looks through the chat picked under the composer;
            // search each chat and merge the hits if people miss them.
            Cmd::Search { room: Some(room), query, filter } => Cmd::Search { room: Some(at(room, None)?), query, filter },
            other => other,
        })
    }

    async fn handle(&mut self, cmd: Cmd) -> Result<()> {
        let cmd = self.route(cmd)?;
        match cmd {
            Cmd::SendCode(email) => {
                self.login_request = Some(beeper::send_code(&self.http, email.trim()).await?);
                self.emit(Ev::CodeSent).await;
            }
            Cmd::SubmitCode(code) => {
                let request = self.login_request.clone().ok_or_else(|| anyhow!(t!("Request a code first")))?;
                let jwt = beeper::submit_code(&self.http, &request, code.trim()).await?;
                self.login_request = None;
                self.login_jwt(jwt).await?;
            }
            Cmd::PasswordLogin { server, user, password } => self.login_password(&server, &user, &password).await?,
            Cmd::Register { server, user, password, code } => self.register(&server, &user, &password, &code).await?,
            Cmd::LoginMethods(server) => {
                let methods = self.methods(server).await?;
                self.emit(Ev::LoginMethods(methods)).await;
            }
            Cmd::BrowserLogin { server, way } => self.browser_login(&server, way).await?,
            Cmd::BrowserDone { back, url } => self.browser_done(&back, url).await?,
            Cmd::BrowserCancel => self.browser = None,
            Cmd::Recover(key) => {
                let recovery = self.client()?.encryption().recovery();
                if let Err(e) = recovery.recover(key.trim()).await {
                    return Err(match e {
                        RecoveryError::SecretStorage(SecretStorageError::SecretStorageKey(DecodeError::Mac(_))) => {
                            anyhow!(t!(
                                "This recovery code is valid but does not match your account's current key. It may be an older code; get the current one from another app where you are signed in."
                            ))
                        }
                        RecoveryError::SecretStorage(SecretStorageError::SecretStorageKey(_)) => {
                            // The decoder's detail ("invalid character 'O' at byte 0") means nothing to a person.
                            log::debug!("recovery code: {e}");
                            anyhow!(t!("This is not a recovery code. It is 48 letters and digits in groups of four; check it for typos."))
                        }
                        e => anyhow::Error::new(e).context(t!("Recovery code was not accepted")),
                    });
                }
                if let Err(e) = self.seal.write(&self.dir, vault::RECOVERY, key.trim().as_bytes()) {
                    log::warn!("keep recovery code: {e}");
                }
                for open in self.opens() {
                    timeline::unlock(&open.timeline).await;
                }
                let incomplete = recovery.state() == RecoveryState::Incomplete;
                self.emit(Ev::NeedRecovery(incomplete)).await;
                if incomplete {
                    self.emit(Ev::Error(t!("Recovery code accepted, but the account's key storage is incomplete").into()))
                        .await;
                }
            }
            Cmd::RecoveryCode => {
                let kept = match self.seal.read(&self.dir, vault::RECOVERY) {
                    Ok(bytes) => Ok(Some(String::from_utf8_lossy(&zeroize::Zeroizing::new(bytes)).into_owned())),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.to_string()),
                };
                self.emit(Ev::RecoveryCode(kept)).await;
            }
            // Off the command loop: the picture may have to be fetched.
            Cmd::TestNotice => drop(tokio::spawn(test_notice(self.client()?.clone(), self.dir.clone()))),
            Cmd::NewRecoveryCode => {
                let enc = self.client()?.encryption();
                let recovery = enc.recovery();
                // The new code replaces the old one everywhere, so it must lock away every secret the old one did.
                let whole = recovery.state() == RecoveryState::Enabled
                    && enc.cross_signing_status().await.is_some_and(|s| s.is_complete())
                    && enc.backups().are_enabled().await;
                let made = if !whole {
                    Err(t!("This device does not hold all of the account's keys, so it cannot make a new recovery code.").to_owned())
                } else {
                    match recovery.reset_key().await {
                        Ok(code) => {
                            if let Err(e) = self.seal.write(&self.dir, vault::RECOVERY, code.as_bytes()) {
                                log::warn!("keep recovery code: {e}");
                            }
                            Ok(Some(code))
                        }
                        Err(e) => Err(format!("{:#}", anyhow::Error::new(e).context(t!("Could not make a new recovery code")))),
                    }
                };
                self.emit(Ev::RecoveryCode(made)).await;
            }
            Cmd::Open(room_id) => {
                // The old conversation stops being followed before the new one starts.
                (self.open, self.merged) = (None, None);
                crate::clear_notification(&crate::accounts::key(crate::accounts::slot_of(&self.dir), room_id.as_str()));
                let room = self.room(&room_id)?;
                let ev = self.ev.clone();
                let asked = room.clone();
                tokio::spawn(async move {
                    if let Some(peer) = other_person(&asked).await {
                        let _ = ev.send(Ev::Peer(asked.room_id().to_owned(), peer)).await;
                    }
                });
                // An invitation has no history to read until it is taken.
                if room.state() == RoomState::Invited {
                    let _ = self.ev.send(Ev::Paged { room: room_id, more: false }).await;
                    return Ok(());
                }
                if union::is_union(&room) {
                    let merged = union::open(self.client()?, &room, &self.ev).await?;
                    self.older_merged(&merged, true);
                    self.merged = Some(merged);
                    return Ok(());
                }
                let open = timeline::open(&room, &self.ev).await?;
                self.older(&open, true);
                self.open = Some(open);
            }
            Cmd::Close => (self.open, self.merged) = (None, None),
            Cmd::LoadOlder => {
                if let Some(open) = &self.open {
                    self.older(open, false);
                }
                if let Some(merged) = &self.merged {
                    self.older_merged(merged, false);
                }
            }
            Cmd::Seen(room, event) => {
                let lanes = self.merged.iter().flat_map(|m| &m.lanes);
                if let Some(open) = self.open.iter().chain(lanes).find(|o| o.room == room) {
                    let (timeline, private) = (open.timeline.clone(), self.incognito.load(Ordering::Relaxed));
                    tokio::spawn(async move {
                        if let Err(e) = read_to(&timeline, event, private).await {
                            log::warn!("mark read: {e:#}");
                        }
                    });
                }
            }
            Cmd::Target(union, lane) => {
                if let Some(merged) = self.merged.as_mut().filter(|m| m.id == union && m.lanes.iter().any(|o| o.room == lane)) {
                    merged.target = Some(lane);
                }
            }
            Cmd::Typing(room_id, on) => {
                // Do not announce typing while private; allow stop to clear an earlier notice.
                // A stop notice is allowed even after the privacy switch changes, so an already
                // visible typing notice can be cleared immediately.
                if !on || !self.incognito.load(Ordering::Relaxed) {
                    let room = self.room(&room_id)?;
                    tokio::spawn(async move {
                        if is_note(&room).await {
                            return;
                        }
                        if let Err(e) = room.typing_notice(on).await {
                            log::debug!("typing notice: {e}");
                        }
                    });
                }
            }
            Cmd::Chat(room_id, action) => {
                let room = self.room(&room_id)?;
                // A merged chat's row is its chats: archiving, reading and muting it reach each of
                // them, as in Beeper; pinning and marking unread stay on the merged chat's own room.
                if union::is_union(&room) && matches!(action, ChatAction::Archive(_) | ChatAction::Mute(_) | ChatAction::Unread(false)) {
                    for member in union::members(self.client()?, &room).await {
                        if let Err(e) = self.chat_action(&member, member.room_id().to_owned(), action, false).await {
                            log::warn!("merged chat action: {e:#}");
                        }
                    }
                }
                self.chat_action(&room, room_id, action, true).await?;
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
                    let (dir, generation) = (media_dir(&self.dir), media_generation());
                    tokio::spawn(async move {
                        let Ok(_permit) = permits.acquire().await else { return };
                        let looped = match media.kind == MediaKind::Video && media.looped {
                            true => looping(&client, &media, &id, &dir, generation).await.unwrap_or_else(|e| {
                                log::debug!("loop: {e:#}");
                                None
                            }),
                            false => None,
                        };
                        match if looped.is_some() { Ok(looped) } else { thumbnail(&client, &media).await } {
                            Ok(Some(bytes)) => {
                                let _ = ev.send(Ev::Thumb(id, bytes)).await;
                            }
                            Ok(None) => {}
                            Err(e) => log::debug!("thumbnail {id}: {e:#}"),
                        }
                    });
                }
            }
            Cmd::Prefetch(media) => {
                let (client, permits) = (self.client()?.clone(), self.fetches.clone());
                tokio::spawn(async move {
                    let Ok(_permit) = permits.acquire().await else { return };
                    let request = MediaRequestParameters { source: media.source.clone(), format: MediaFormat::File };
                    if let Err(e) = client.media().get_media_content(&request, true).await {
                        log::debug!("prefetch: {e:#}");
                    }
                });
            }
            Cmd::ViewedOnce(room_id, event) => {
                // Only Beeper's bridges take the report; elsewhere there is nobody to tell.
                if let Ok(api) = self.api() {
                    let room = self.room(&room_id)?;
                    tokio::spawn(async move {
                        if let Err(e) = rooms::report_once(&api, &room, &event).await {
                            log::debug!("view-once report: {e:#}");
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
                        // View-once media stays out of the on-disk media cache.
                        let bytes = client.media().get_media_content(&request, !media.once).await?;
                        tokio::task::spawn_blocking(move || shrink(&bytes, VIEW_PX)).await?
                    };
                    let _ = match bytes.await {
                        Ok(bytes) => ev.send(Ev::Viewed(id, bytes)).await,
                        Err(e) => ev.send(Ev::Error(t!("Could not load the image: {error}", error = format!("{e:#}")))).await,
                    };
                });
            }
            Cmd::ViewPicture { id, room, user } => {
                let client = self.client()?.clone();
                let ev = self.ev.clone();
                tokio::spawn(async move {
                    let bytes = async {
                        let url = match &user {
                            Some(user) => face_url(&client, &room, user).await?,
                            None => avatar_url(&client, &room).await?,
                        };
                        let Some(url) = url else { return Ok(None) };
                        let request = MediaRequestParameters { source: MediaSource::Plain(url), format: MediaFormat::File };
                        let bytes = client.media().get_media_content(&request, true).await?;
                        anyhow::Ok(Some(tokio::task::spawn_blocking(move || shrink(&bytes, VIEW_PX)).await??))
                    };
                    let _ = match bytes.await {
                        Ok(Some(bytes)) => ev.send(Ev::Viewed(id, bytes)).await,
                        Ok(None) => Ok(()),
                        Err(e) => ev.send(Ev::Error(t!("Could not load the image: {error}", error = format!("{e:#}")))).await,
                    };
                });
            }
            Cmd::Fetch(id, media) => {
                let client = self.client()?.clone();
                let (ev, dir, generation) = (self.ev.clone(), media_dir(&self.dir), media_generation());
                tokio::spawn(async move {
                    let saved = async {
                        let request = MediaRequestParameters { source: media.source.clone(), format: MediaFormat::File };
                        // From the media cache where auto-download put it; view-once media stays out.
                        let bytes = client.media().get_media_content(&request, !media.once).await?;
                        save_media(&dir, generation, &id, &media.name, &bytes)
                    };
                    let path = match saved.await {
                        Ok(path) => Some(path),
                        Err(e) => {
                            let _ = ev.send(Ev::Error(t!("Could not download {name}: {error}", name = media.name, error = format!("{e:#}")))).await;
                            None
                        }
                    };
                    let _ = ev.send(Ev::Fetched(id, path)).await;
                });
            }
            cmd @ (Cmd::Attach { .. } | Cmd::AttachMedia { .. }) => {
                let (room, path, reply_to, caption, hd) = match cmd {
                    Cmd::Attach { room, path, reply_to } => (room, path, reply_to, String::new(), true),
                    Cmd::AttachMedia { room, path, reply_to, caption, hd } => (room, path, reply_to, caption, hd),
                    _ => unreachable!("matched above"),
                };
                // `Attached` follows every attachment, sent or not: the next one waits for it.
                let target = self.room(&room);
                let ev = self.ev.clone();
                let pasted = self.pasted_folder(&path);
                tokio::spawn(async move {
                    let sent = match target {
                        Ok(target) => attach(&target, &path, reply_to.clone(), &caption, hd).await,
                        Err(e) => Err(e),
                    };
                    let why = sent.as_ref().err().and_then(|e| e.downcast_ref::<mp4::Unfit>().copied());
                    let offer = match why {
                        Some(why) if tokio::task::spawn_blocking(mp4::available).await.unwrap_or(false) => Some(why),
                        _ => None,
                    };
                    // The file stays for the MP4 the user may still ask for.
                    if let (Some(folder), None) = (pasted, offer) {
                        let _ = tokio::fs::remove_dir_all(folder).await;
                    }
                    match (sent, offer) {
                        (_, Some(why)) => {
                            let _ = ev.send(Ev::OfferMp4 { room, path, reply_to, caption, why }).await;
                        }
                        (Err(e), None) => {
                            let _ = ev.send(Ev::Error(t!("Attachment not sent: {error}", error = format!("{e:#}")))).await;
                        }
                        (Ok(()), None) => {}
                    }
                    let _ = ev.send(Ev::Attached).await;
                });
            }
            Cmd::Mp4 { room, path, reply_to, caption } => {
                let target = self.room(&room);
                let ev = self.ev.clone();
                let pasted = self.pasted_folder(&path);
                let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                let scratch = self.dir.join(PASTED).join(format!("mp4-{stamp}"));
                tokio::spawn(async move {
                    let sent = async {
                        let target = target?;
                        tokio::fs::create_dir_all(&scratch).await?;
                        let out = scratch.join("video.mp4");
                        let (source, made) = (path.clone(), out.clone());
                        tokio::task::spawn_blocking(move || mp4::convert(&source, &made)).await??;
                        attach(&target, &out, reply_to, &caption, true).await
                    }
                    .await;
                    for folder in std::iter::once(scratch).chain(pasted) {
                        let _ = tokio::fs::remove_dir_all(folder).await;
                    }
                    if let Err(e) = sent {
                        log::warn!("mp4: {e:#}");
                        let _ = ev.send(Ev::Error(t!("Attachment not sent: {error}", error = format!("{e:#}")))).await;
                    }
                    let _ = ev.send(Ev::Attached).await;
                });
            }
            Cmd::ReadAll(ids) => {
                let (client, ev, incognito) = (self.client()?.clone(), self.ev.clone(), self.incognito.clone());
                // One chat at a time: two requests each, and the command loop stays free meanwhile.
                tokio::spawn(async move {
                    let mut failed = 0u32;
                    for id in ids {
                        let Some(room) = client.get_room(&id) else { continue };
                        if let Err(e) = read_newest(&room, incognito.load(Ordering::Relaxed)).await {
                            log::warn!("read all: {e:#}");
                            failed += 1;
                        }
                    }
                    if failed > 0 {
                        let _ = ev.send(Ev::Error(tn!(failed, "{n} chat could not be marked as read", "{n} chats could not be marked as read"))).await;
                    }
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
            Cmd::LoginStart { bridge, flow, relogin } => {
                let (api, ev) = (self.api()?, self.ev.clone());
                self.login = Some(AbortOnDrop(tokio::spawn(async move { panel(&ev, api.start(&bridge, &flow, relogin.as_deref()).await.map(Ev::LoginStep)).await })));
            }
            Cmd::LoginSubmit(step, values) => {
                let (api, ev) = (self.api()?, self.ev.clone());
                self.login = Some(AbortOnDrop(tokio::spawn(async move { panel(&ev, api.submit(&step, &values).await.map(Ev::LoginStep)).await })));
            }
            Cmd::LoginCancel(step) => {
                self.login = None;
                let api = self.api()?;
                tokio::spawn(async move {
                    // Unknown to the bridge by now (finished, timed out) is as good as cancelled.
                    if let Err(e) = api.cancel(&step.bridge, &step.login_id).await {
                        log::debug!("login cancel: {e:#}");
                    }
                });
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
            Cmd::Replay => {
                let list: Vec<RoomSummary> = self.rooms.lock().unwrap().values().cloned().collect();
                if !list.is_empty() {
                    self.emit(Ev::Rooms(list)).await;
                }
                let limit = self.merge_limit.load(Ordering::Relaxed);
                if limit != i64::MIN {
                    self.emit(Ev::MergeLimit(limit)).await;
                }
                let blocked = self.blocked.lock().unwrap().clone();
                if !blocked.is_empty() {
                    self.emit(Ev::Blocked(blocked)).await;
                }
            }
            Cmd::Resync => {
                if let (Some(client), Some(notif)) = (&self.client, &self.notif)
                    && self.tasks.last().is_some_and(tokio::task::JoinHandle::is_finished)
                {
                    let task = self.sync_task(client, notif);
                    self.tasks.pop();
                    self.tasks.push(task);
                }
            }
            Cmd::Guilds { bridge, login } => {
                let (api, ev) = (self.api()?, self.ev.clone());
                tokio::spawn(async move { panel(&ev, api.guilds(&bridge, &login).await.map(|g| Ev::Guilds(login, g))).await });
            }
            Cmd::BridgeGuild { bridge, login, guild, on } => {
                let (api, ev) = (self.api()?, self.ev.clone());
                tokio::spawn(async move {
                    let done = async {
                        api.bridge_guild(&bridge, &login, &guild, on).await?;
                        api.guilds(&bridge, &login).await
                    };
                    panel(&ev, done.await.map(|g| Ev::Guilds(login, g))).await;
                });
            }
            Cmd::SignOut => {
                // Waited for, so the sync loop cannot write the chat list after the wipe below.
                for task in self.tasks.drain(..) {
                    task.abort();
                    let _ = task.await;
                }
                self.verify.stop();
                if let Some(client) = self.client.take() {
                    // The device is gone locally either way; the server forgets it when it can.
                    if let Err(e) = client.logout().await {
                        log::warn!("logout: {e}");
                    }
                }
                self.notif = None;
                (self.open, self.merged, self.info) = (None, None, None);
                self.rooms.lock().unwrap().clear();
                wipe_account(&self.dir);
                // One posted between the window's sign-out and the wipe; other accounts keep theirs.
                crate::clear_account_notifications(crate::accounts::slot_of(&self.dir));
                (self.homeserver, self.beeper, self.seal) = (beeper_homeserver(), true, Seal::Plain);
                self.emit(Ev::NeedLogin).await;
            }
            Cmd::Previews(ids) => {
                for room_id in ids {
                    let room = self.room(&room_id)?;
                    let (ev, permits, rooms) = (self.ev.clone(), self.previews.clone(), self.rooms.clone());
                    tokio::spawn(async move {
                        let Ok(_permit) = permits.acquire().await else { return };
                        if let Some(summary) = probe(&room, &rooms).await {
                            let _ = ev.send(Ev::Rooms(vec![summary])).await;
                        }
                    });
                }
            }
            Cmd::Relabel => {
                for open in self.opens() {
                    open.relabel.notify_one();
                }
                let changed = relabel_rooms(&mut self.rooms.lock().unwrap(), crate::i18n::polish());
                if !changed.is_empty() {
                    UNSAVED_PROBES.store(true, std::sync::atomic::Ordering::Relaxed);
                    self.emit(Ev::Rooms(changed)).await;
                }
            }
            Cmd::Send { room, body, reply_to, edit, mentions, preview } => {
                let text = mentions::content(&body, &mentions);
                // The interface counts a message sent with a preview as an upload until `Uploaded`.
                let asked = preview.is_some();
                let made = preview.filter(|_| edit.is_none()).and_then(|url| {
                    self.link_task = None;
                    self.link_made.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take().filter(|p| p.url == url)
                });
                if let Some(made) = made {
                    let (room, ev) = (self.room(&room)?, self.ev.clone());
                    // shortcut: a message sent while this picture uploads can arrive before it; upload
                    // as the preview is made if users hit that, at the cost of unsent pictures on the server.
                    tokio::spawn(async move {
                        let sent = async {
                            let content = match reply_to {
                                Some(id) => {
                                    let reply = matrix_sdk::room::reply::Reply { event_id: EventId::parse(id)?, enforce_thread: matrix_sdk::room::reply::EnforceThread::MaybeThreaded, add_mentions: matrix_sdk::ruma::events::room::message::AddMentions::Yes };
                                    room.make_reply_event(text, reply).await?
                                }
                                None => RoomMessageEventContent::from(text),
                            };
                            let mut content = serde_json::to_value(content)?;
                            content["com.beeper.linkpreviews"] = serde_json::json!([linkprev::entry(&room, &made).await]);
                            let raw: Raw<matrix_sdk::ruma::events::AnyMessageLikeEventContent> = Raw::new(&content)?.cast_unchecked();
                            room.send_queue().send_raw(raw, "m.room.message".to_owned()).await?;
                            anyhow::Ok(())
                        };
                        if let Err(e) = sent.await {
                            log::warn!("{e:#}");
                            let _ = ev.send(Ev::Error(format!("{e:#}"))).await;
                        }
                        let _ = ev.send(Ev::Uploaded).await;
                    });
                    return Ok(());
                }
                if asked {
                    self.emit(Ev::Uploaded).await;
                }
                match self.opened(&room) {
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
                let open = self.opened(&room).context(t!("That chat is not open"))?;
                open.timeline.toggle_reaction(&timeline::item_id(&target)?, &key).await?;
            }
            Cmd::Delete { room, target } => {
                let open = self.opened(&room).context(t!("That chat is not open"))?;
                open.timeline.redact(&timeline::item_id(&target)?, None).await?;
            }
            Cmd::Retry { room, target } => {
                let open = self.opened(&room).context(t!("That chat is not open"))?;
                // A failed send stops the chat's queue; trying again starts it.
                open.timeline.room().send_queue().set_enabled(true);
                timeline::retry(&open.timeline, &target).await?;
            }
            Cmd::AskSettings(device) => self.handoff.ask(self.client()?, &device).await?,
            Cmd::CancelAsk => self.handoff.cancel_ask(),
            Cmd::GiveSettings { device, request, settings } => {
                let client = self.client()?.clone();
                tokio::spawn(async move {
                    if let Err(e) = handoff::Handoff::give(&client, &device, request, settings).await {
                        log::warn!("settings handoff: {}", plain(&e));
                    }
                });
            }
            Cmd::HandoffOffer(settings) => {
                let client = self.client()?.clone();
                self.handoff.offer(&client, settings, &self.ev)?;
            }
            Cmd::HandoffSeek => self.handoff.seek(&self.ev),
            Cmd::HandoffPick(ix) => {
                let client = self.client()?.clone();
                self.handoff.pick(&client, ix, &self.ev)?;
            }
            Cmd::HandoffConfirm => self.handoff.confirm(),
            Cmd::HandoffStop => self.handoff.stop(),
            Cmd::Devices => {
                let (client, ev) = (self.client()?.clone(), self.ev.clone());
                tokio::spawn(async move {
                    let _ = ev.send(match verify::devices(&client).await {
                        Ok(list) => Ev::Devices(list),
                        Err(e) => Ev::DevicesFailed(format!("{e:#}")),
                    }).await;
                });
            }
            Cmd::RenameDevice(name) => {
                let client = self.client()?;
                let done = match verify::rename(client, &name).await {
                    Ok(()) => verify::devices(client).await.map(Ev::Devices),
                    Err(e) => Err(e),
                };
                self.emit(done.unwrap_or_else(|e| Ev::DevicesFailed(format!("{e:#}")))).await;
            }
            Cmd::AcceptIdentity(user) => {
                let client = self.client()?.clone();
                tokio::spawn(async move {
                    if let Err(e) = verify::accept_identity(&client, &user).await {
                        log::warn!("accept identity: {e:#}");
                    }
                });
            }
            Cmd::VerifyStart | Cmd::VerifyPerson(_) | Cmd::VerifyAccept | Cmd::VerifyMatch(_) | Cmd::VerifyCancel => {
                let done = match cmd {
                    Cmd::VerifyStart => self.verify.start(self.client()?, &self.ev, None).await,
                    Cmd::VerifyPerson(user) => self.verify.start(self.client()?, &self.ev, Some(&user)).await,
                    Cmd::VerifyAccept => self.verify.accept().await,
                    Cmd::VerifyMatch(matched) => self.verify.answer(matched).await,
                    _ => self.verify.cancel().await,
                };
                if let Err(e) = done {
                    self.emit(Ev::Verify(verify::Verify::Failed(format!("{e:#}")))).await;
                }
            }
            Cmd::SignOutDevice { device, proof } => {
                let client = self.client()?.clone();
                let api = self.api().ok();
                let step = self.verify.sign_out(&client, api.as_ref(), device, proof).await.map_err(|e| format!("{e:#}"));
                let gone = matches!(step, Ok(None));
                self.emit(Ev::SignOutStep(step)).await;
                if gone && let Ok(list) = verify::devices(&client).await {
                    self.emit(Ev::Devices(list)).await;
                }
            }
            Cmd::Vote { room, target, unstable, answers } => {
                let (kind, content) = polls::response(unstable, &target, answers);
                self.room(&room)?.send_raw(kind, content).await?;
            }
            Cmd::SendLocation { room, lat, lon, accuracy, reply_to } => {
                let place = location::content(lat, lon, accuracy);
                match self.opened(&room) {
                    Some(open) => match reply_to {
                        Some(id) => drop(open.timeline.send_reply(place, EventId::parse(id)?).await?),
                        None => drop(open.timeline.send(RoomMessageEventContent::from(place).into()).await?),
                    },
                    None => drop(self.room(&room)?.send_queue().send(RoomMessageEventContent::from(place).into()).await?),
                }
            }
            Cmd::SendPoll { room, question, answers, multiple } => {
                let (kind, content) = pollnew::NewPoll::check(&question, &answers, multiple).map_err(anyhow::Error::msg)?.event();
                self.room(&room)?.send_raw(kind, content).await?;
            }
            Cmd::Pin { room, target, pin } => pins::set(&self.room(&room)?, &target, pin).await?,
            Cmd::EndPoll { room, target } => {
                let (kind, content) = pollnew::end(&target);
                self.room(&room)?.send_raw(kind, content).await?;
            }
            Cmd::Contact(id, media) => {
                let client = self.client()?.clone();
                let (ev, permits) = (self.ev.clone(), self.fetches.clone());
                tokio::spawn(async move {
                    let Ok(_permit) = permits.acquire().await else { return };
                    let cards = contact::load(&client, &media).await.unwrap_or_else(|e| {
                        log::debug!("contact {id}: {e:#}");
                        Vec::new()
                    });
                    let _ = ev.send(Ev::Contact(id, cards)).await;
                });
            }
            Cmd::People(ask) => rooms::handle(self.client()?.clone(), self.api().ok(), self.rooms.clone(), self.ev.clone(), ask),
            Cmd::Export(id, media, what) => {
                let (client, ev, dir) = (self.client()?.clone(), self.ev.clone(), media_dir(&self.dir));
                tokio::spawn(export::run(client, ev, dir, media_generation(), id, media, what));
            }
            Cmd::GifSearch { query, page } => gifs::search(self.http.clone(), self.ev.clone(), query, page),
            Cmd::GifStop => gifs::stop(),
            Cmd::LinkPreview(url) => {
                *self.link_made.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                self.link_task = url.map(|url| {
                    let (ev, made) = (self.ev.clone(), self.link_made.clone());
                    AbortOnDrop(tokio::spawn(async move {
                        let preview = linkprev::fetch(&url).await.inspect_err(|e| log::debug!("link preview: {e:#}")).ok();
                        let draft = preview.as_ref().map(|p| LinkDraft {
                            title: if p.meta.title.is_empty() { p.meta.description.clone() } else { p.meta.title.clone() },
                            site: p.meta.site.clone(),
                            picture: p.picture.as_ref().map(|pic| pic.bytes.clone()),
                        });
                        *made.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = preview;
                        let _ = ev.send(Ev::LinkPreview(url, draft)).await;
                    }))
                });
            }
            Cmd::UpdateCheck => {
                tokio::spawn(update::run_check(self.http.clone(), self.ev.clone()));
            }
            Cmd::UpdateInstall(release) => {
                self.update = Some(AbortOnDrop(tokio::spawn(update::run_install(self.http.clone(), self.ev.clone(), self.dir.clone(), release))));
            }
            #[cfg(target_os = "android")]
            Cmd::UpdateFailed(why) => self.emit(Ev::Update(update::Step::Failed(why))).await,
            Cmd::EmojiSet { set, to } => {
                let Some(set) = crate::emoji::SETS.get(set) else { return Ok(()) };
                let (http, ev) = (self.http.clone(), self.ev.clone());
                // One at a time: the settings offer no other while one runs.
                tokio::spawn(async move {
                    let progress = |p| {
                        let _ = ev.try_send(Ev::EmojiSet(crate::emoji::Fetch::Progress(p)));
                    };
                    let done = match update::file(&http, set.url, set.sha256, set.size, &to, progress).await {
                        Ok(()) => crate::emoji::Fetch::Done,
                        Err(e) => crate::emoji::Fetch::Failed(format!("{e:#}")),
                    };
                    let _ = ev.send(Ev::EmojiSet(done)).await;
                });
            }
            Cmd::SendGif { room, gif, query, reply_to } => {
                let network = self.rooms.lock().unwrap().get(&room).and_then(|r| r.network.clone());
                let room = self.room(&room);
                let (http, ev) = (self.http.clone(), self.ev.clone());
                tokio::spawn(async move {
                    let sent = match room {
                        Ok(room) => gifs::send(&http, &room, &gif, &query, network.as_deref(), reply_to).await,
                        Err(e) => Err(e),
                    };
                    if let Err(e) = sent {
                        let _ = ev.send(Ev::Error(t!("GIF not sent: {error}", error = format!("{e:#}")))).await;
                    }
                    let _ = ev.send(Ev::Uploaded).await;
                });
            }
            Cmd::Stickers(room_id) => {
                let (client, room, ev) = (self.client()?.clone(), self.room(&room_id)?, self.ev.clone());
                tokio::spawn(async move {
                    let packs = stickers::packs(&client, &room).await;
                    let _ = ev.send(Ev::Stickers { room: room_id, packs }).await;
                });
            }
            Cmd::SendSticker { room, sticker, reply_to } => {
                let room = self.room(&room)?;
                stickers::send(&room, &sticker, reply_to).await?;
            }
            Cmd::StickerThumb(url) => {
                let client = self.client()?.clone();
                let (ev, permits) = (self.ev.clone(), self.fetches.clone());
                tokio::spawn(async move {
                    let Ok(_permit) = permits.acquire().await else { return };
                    match stickers::thumb(&client, url.clone()).await {
                        Ok(bytes) => drop(ev.send(Ev::StickerThumb(url.to_string(), bytes)).await),
                        Err(e) => log::debug!("sticker thumbnail: {e:#}"),
                    }
                });
            }
            Cmd::EditHistory { room, id } => {
                let room = self.room(&room)?;
                let ev = self.ev.clone();
                tokio::spawn(async move {
                    // A message still on its way out has no event ID, and so no edits on the server.
                    let versions = match OwnedEventId::try_from(id.as_str()) {
                        Ok(event) => edits::history(&room, &event).await.inspect_err(|e| log::warn!("edit history: {e:#}")).ok(),
                        Err(_) => None,
                    };
                    let _ = ev.send(Ev::EditHistory { id, versions }).await;
                });
            }
            Cmd::Members(room_id) => {
                let room = self.room(&room_id)?;
                let own = self.client()?.user_id().context(t!("Not signed in"))?.to_owned();
                let ev = self.ev.clone();
                // Fetching a big chat's members can take a while; the command loop goes on meanwhile.
                tokio::spawn(async move {
                    match mentions::members(&room, &own).await {
                        Ok(list) => drop(ev.send(Ev::Members { room: room_id, list }).await),
                        Err(e) => log::warn!("members: {e:#}"),
                    }
                });
            }
            Cmd::Voice { room, path, duration, levels, reply_to } => {
                // Sent through the attachment queue: `Attached` follows it as well.
                let room = self.room(&room);
                let ev = self.ev.clone();
                tokio::spawn(async move {
                    let sent = match room {
                        Ok(room) => voice::send(&room, &path, duration, levels, reply_to).await,
                        Err(e) => Err(e),
                    };
                    if let Err(e) = sent {
                        let _ = ev.send(Ev::Error(t!("Voice message not sent: {error}", error = format!("{e:#}")))).await;
                    }
                    let _ = ev.send(Ev::Attached).await;
                });
            }
            Cmd::Forward { room, targets, to } => {
                let open = self.opened(&room).context(t!("That chat is not open"))?;
                let rooms = to.iter().map(|id| self.room(id)).collect::<Result<Vec<_>>>()?;
                let done = forward::forward(&open.timeline, &targets, &rooms).await?;
                self.emit(Ev::Forwarded(done)).await;
            }
            Cmd::SetDisappear { room, ms } => {
                let room = self.room(&room)?;
                let mut setting = disappear::read(&room).await;
                let content = setting.content(ms).filter(|_| setting.allowed).context(t!("This chat does not take that timer"))?;
                room.send_state_event_raw(disappear::TIMER, "", content).await?;
                // The bridge answers with the new state, which reads it again; until then, this.
                setting.ms = ms;
                self.emit(Ev::Disappear { room: room.room_id().to_owned(), setting }).await;
            }
            Cmd::Info(room_id) => {
                let room = self.room(&room_id)?;
                self.info = Some(AbortOnDrop(tokio::spawn(info::run(room, self.ev.clone()))));
            }
            Cmd::InfoClose => self.info = None,
            Cmd::Group { room: room_id, change } => {
                let (room, client, ev) = (self.room(&room_id)?, self.client()?.clone(), self.ev.clone());
                tokio::spawn(async move {
                    match group::change(&room, &change).await {
                        Ok(()) => {
                            if matches!(change, group::Change::Photo(_))
                                && let Ok(Some(bytes)) = avatar(&client, &room_id).await
                            {
                                let _ = ev.send(Ev::Avatar(room_id.clone(), bytes)).await;
                            }
                            let _ = ev.send(Ev::Group { room: room_id, change }).await;
                        }
                        Err(e) => {
                            log::warn!("group change: {e:#}");
                            let _ = ev.send(Ev::Error(t!("Could not change the group: {error}", error = format!("{e:#}")))).await;
                        }
                    }
                });
            }
            Cmd::Candidates(room_id) => {
                let (room, client, ev) = (self.room(&room_id)?, self.client()?.clone(), self.ev.clone());
                tokio::spawn(async move {
                    let list = group::candidates(&client, &room).await;
                    let _ = ev.send(Ev::Candidates { room: room_id, list }).await;
                });
            }
            Cmd::MemberIds(room_id, user) => {
                let (room, ev) = (self.room(&room_id)?, self.ev.clone());
                tokio::spawn(async move {
                    let ids = info::identifiers_of(&room, &user).await;
                    let _ = ev.send(Ev::MemberIds(user, ids)).await;
                });
            }
            Cmd::BlockOnNetwork(room, on) => {
                let (client, ev) = (self.client()?.clone(), self.ev.clone());
                tokio::spawn(async move {
                    if let Err(e) = block::set_on_network(&client, &room, on).await {
                        log::warn!("network block: {e:#}");
                        let _ = ev.send(Ev::Error(t!("Could not change who you have blocked: {error}", error = plain(&format!("{e:#}"))))).await;
                    }
                });
            }
            Cmd::SetBlocked(user, on) => {
                let (client, ev) = (self.client()?.clone(), self.ev.clone());
                tokio::spawn(async move {
                    if let Err(e) = block::set(&client, &user, on).await {
                        log::warn!("block: {e:#}");
                        let _ = ev.send(Ev::Error(t!("Could not change who you have blocked: {error}", error = plain(&format!("{e:#}"))))).await;
                    }
                });
            }
            Cmd::Profile => {
                let (client, ev) = (self.client()?.clone(), self.ev.clone());
                tokio::spawn(async move {
                    match group::profile(&client).await {
                        Ok(profile) => {
                            let _ = ev.send(Ev::Profile(profile)).await;
                        }
                        Err(e) => log::debug!("own profile: {e:#}"),
                    }
                });
            }
            Cmd::SetProfile(mine) => {
                let (client, ev) = (self.client()?.clone(), self.ev.clone());
                tokio::spawn(async move {
                    let done = match group::set_profile(&client, mine).await {
                        Ok(()) => group::profile(&client).await.map(Ev::Profile),
                        Err(e) => Err(e),
                    };
                    let ev_out = done.unwrap_or_else(|e| {
                        log::warn!("profile change: {e:#}");
                        Ev::Error(t!("Could not change your profile: {error}", error = format!("{e:#}")))
                    });
                    let _ = ev.send(ev_out).await;
                });
            }
            Cmd::PeerIds(room_id) => {
                let room = self.room(&room_id)?;
                self.peer = Some(AbortOnDrop(tokio::spawn(info::peer(room, self.ev.clone()))));
            }
            Cmd::Keep(keep) => {
                let client = self.client()?.clone();
                tokio::spawn(async move { storage::keep(&client, keep).await });
            }
            cmd @ (Cmd::Storage | Cmd::ClearMedia) => {
                let (client, ev, dir) = (self.client()?.clone(), self.ev.clone(), self.dir.clone());
                tokio::spawn(async move {
                    if matches!(cmd, Cmd::ClearMedia)
                        && let Err(e) = storage::clear_media(&client, &dir).await
                    {
                        let _ = ev.send(Ev::Error(t!("Could not clear the media cache: {error}", error = format!("{e:#}")))).await;
                    }
                    if let Ok(usage) = tokio::task::spawn_blocking(move || storage::usage(&dir)).await {
                        let _ = ev.send(Ev::Storage(usage)).await;
                    }
                });
            }
            Cmd::Search { room, query, filter } => {
                let (client, ev) = (self.client()?.clone(), self.ev.clone());
                tokio::spawn(async move {
                    let hits = search::search(&client, room.as_deref(), &query, filter.clone()).await;
                    let _ = ev.send(Ev::Found { room, query, filter, hits }).await;
                });
            }
        }
        Ok(())
    }

    /// Asks for older messages in the background; the rows arrive as changes to the timeline. The
    /// first time, also until there is something to read: a page may hold nothing that is shown.
    fn older(&self, open: &timeline::Open, first: bool) {
        let (timeline, room, ev) = (open.timeline.clone(), open.room.clone(), self.ev.clone());
        tokio::spawn(async move {
            let mut more = true;
            for _ in 0..if first { MAX_PAGES_PER_REQUEST } else { 1 } {
                match timeline.paginate_backwards(PAGE).await {
                    Ok(start) => more = !start,
                    Err(e) => {
                        let _ = ev.send(Ev::Error(t!("Could not load messages: {error}", error = e))).await;
                        break;
                    }
                }
                if !more || timeline.items().await.iter().filter(|item| timeline::drawn(item)).count() >= MIN_ITEMS {
                    break;
                }
            }
            // Read as far as it shows, by the UI's `Cmd::Seen`, not on opening.
            let _ = ev.send(Ev::Paged { room, more }).await;
        });
    }

    /// `older` for each chat of the open merged chat; the merged chat has more while any chat has.
    fn older_merged(&self, merged: &union::Merged, first: bool) {
        let lanes: Vec<_> = merged.lanes.iter().map(|o| o.timeline.clone()).collect();
        let (id, ev) = (merged.id.clone(), self.ev.clone());
        tokio::spawn(async move {
            let mut more = false;
            for timeline in &lanes {
                let mut lane_more = true;
                for _ in 0..if first { MAX_PAGES_PER_REQUEST } else { 1 } {
                    match timeline.paginate_backwards(PAGE).await {
                        Ok(start) => lane_more = !start,
                        Err(e) => {
                            let _ = ev.send(Ev::Error(t!("Could not load messages: {error}", error = e))).await;
                            break;
                        }
                    }
                    if !lane_more || timeline.items().await.iter().filter(|item| timeline::drawn(item)).count() >= MIN_ITEMS {
                        break;
                    }
                }
                more |= lane_more;
            }
            let _ = ev.send(Ev::Paged { room: id, more }).await;
        });
    }

    /// Access to Beeper's account API and the bridges behind the homeserver.
    fn api(&self) -> Result<bridges::Api> {
        // Another homeserver's access token must never reach Beeper's servers.
        if !self.beeper {
            return Err(anyhow!(t!("Networks are a Beeper feature")));
        }
        let client = self.client()?;
        let debug = |var: &str| std::env::var(var).ok().filter(|_| cfg!(debug_assertions));
        Ok(bridges::Api {
            http: self.http.clone(),
            api: debug("BEEPER_GPUI_API").unwrap_or_else(|| beeper::API_URL.to_owned()),
            homeserver: debug("BEEPER_GPUI_BRIDGES").unwrap_or_else(|| self.homeserver.clone()),
            token: client.access_token().context(t!("Not signed in"))?,
            user_id: client.user_id().context(t!("Not signed in"))?.to_string(),
        })
    }

    /// The folder of its own a pasted or edited file sits in under `pasted/`, which goes with it.
    fn pasted_folder(&self, path: &Path) -> Option<PathBuf> {
        path.parent().filter(|folder| folder.parent() == Some(self.dir.join(PASTED).as_path())).map(Path::to_path_buf)
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
    let backups = &room.client().encryption().backups();
    // A key the backup does not hold yet says nothing about the others: none stops the rest.
    let asked = futures_util::future::join_all(sessions.iter().take(MAX_KEY_FETCHES).map(|session| backups.download_room_key(room.room_id(), session))).await;
    let found = asked.into_iter().fold(false, |found, got| found | got.inspect_err(|e| log::debug!("backup key: {e}")).unwrap_or(false));
    let resp = if found { room.messages(opts()).await? } else { resp };
    search::page(room, &resp.chunk).await;
    Ok(resp)
}

/// An emote: the sender's name ahead of the action, as other apps show it. Needs `sender` set.
fn act(m: &mut Msg) {
    let n = rich::emote(&m.sender, &mut m.body, &mut m.rich);
    for r in m.mentions.iter_mut().chain(m.to_me.iter_mut()) {
        *r = r.start + n..r.end + n;
    }
}

/// List preview: the newest message on one line, prefixed with its sender outside one-to-one chats.
fn preview(m: &Msg, dm: bool) -> String {
    let text = match m {
        // An emote names its sender already.
        m if m.media.is_none() && m.body.starts_with(&format!("* {} ", m.sender)) => m.summary(),
        m if m.own => format!("{}: {}", t!("You"), m.summary()),
        m if dm => m.summary(),
        m => format!("{}: {}", m.sender, m.summary()),
    };
    text.replace('\n', " ")
}

/// How the app's own words sit in a preview's summary.
#[derive(Clone, Copy)]
enum Fit {
    /// Alone, or before ": " and a caption.
    Label,
    /// Before the sender's text, such as a poll's question.
    Prefix,
    /// The whole summary.
    Whole,
}

/// What `preview` writes in the interface language, by English key.
const PREVIEW_TEXTS: [(&str, Fit); 9] = [
    (n_!("📷 Photo"), Fit::Label),
    (n_!("🖼 Sticker"), Fit::Label),
    (n_!("🎬 Video"), Fit::Label),
    (n_!("🎤 Voice Note"), Fit::Label),
    (n_!("🎵 Audio"), Fit::Label),
    (n_!("📍 Location"), Fit::Label),
    (n_!("📊 Poll: {question}"), Fit::Prefix),
    (n_!("Unable to decrypt this message yet"), Fit::Whole),
    (n_!("Message deleted"), Fit::Whole),
];

/// A preview written in either language, in the given one: the app's own words (the "You" prefix,
/// a media label, a placeholder) are swapped, the sender's name and text stay. No IO.
fn relabel(preview: &str, polish: bool) -> String {
    let langs = [false, true];
    let mut out = String::with_capacity(preview.len() + 8);
    let mut rest = preview;
    if let Some(after) = langs.iter().find_map(|&l| rest.strip_prefix(tr_in(n_!("You"), l))?.strip_prefix(": ")) {
        out.push_str(tr_in(n_!("You"), polish));
        out.push_str(": ");
        rest = after;
    }
    // The summary starts the preview or follows a sender's name and ": ", which a name may also hold.
    let starts = std::iter::once(0).chain(rest.match_indices(": ").map(|(at, _)| at + 2));
    for start in starts {
        let summary = &rest[start..];
        for (key, fit) in PREVIEW_TEXTS {
            let words = |l| tr_in(key, l).split('{').next().unwrap_or_default();
            let found = langs.iter().find_map(|&l| {
                let tail = summary.strip_prefix(words(l))?;
                let fits = match fit {
                    Fit::Label => tail.is_empty() || tail.starts_with(": "),
                    Fit::Prefix => true,
                    Fit::Whole => tail.is_empty(),
                };
                fits.then_some(tail)
            });
            if let Some(tail) = found {
                out.push_str(&rest[..start]);
                out.push_str(words(polish));
                out.push_str(tail);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Every cached preview in the given language; returns the summaries that changed.
fn relabel_rooms(rooms: &mut HashMap<OwnedRoomId, RoomSummary>, polish: bool) -> Vec<RoomSummary> {
    let mut changed = Vec::new();
    for r in rooms.values_mut() {
        let Some(old) = &r.preview else { continue };
        let new = relabel(old, polish);
        if &new != old {
            r.preview = Some(new);
            changed.push(r.clone());
        }
    }
    changed
}

/// Everything this account left on the device: store, key, session, caches, downloads, index.
fn wipe_account(dir: &Path) {
    vault::wipe(dir);
    let _ = std::fs::remove_file(rooms_lang_path(dir));
    {
        let mut generation = MEDIA.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *generation += 1;
        let _ = std::fs::remove_dir_all(media_dir(dir));
    }
    let _ = std::fs::remove_dir_all(dir.join("notify"));
    search::wipe(dir);
}

/// The store refused its key: it was made with another one, or none.
fn key_rejected(e: &matrix_sdk::ClientBuildError) -> bool {
    matches!(e, matrix_sdk::ClientBuildError::SqliteStore(matrix_sdk_sqlite::OpenStoreError::InitCipher(_)))
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

/// Files downloaded to be opened by other apps, decrypted. Emptied at startup and sign-out, and
/// each event's folder goes `MEDIA_TTL` after it was written.
fn media_dir(dir: &Path) -> PathBuf {
    dir.join("media")
}

/// How long a downloaded attachment stays: long enough for the app it went to, or a voice note or
/// video being played, to read it (an open file outlives its deletion), short enough that the sync
/// service keeping the process alive for days does not keep the plain copies with it.
const MEDIA_TTL: Duration = Duration::from_secs(10 * 60);

/// Moved on by `wipe_account`, under this lock, so a download that finishes after a sign-out
/// writes nothing.
static MEDIA: std::sync::Mutex<u64> = std::sync::Mutex::new(0);

fn media_generation() -> u64 {
    *MEDIA.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Writes a downloaded attachment, one directory per event so it keeps its own name, unless the
/// account it was asked for is gone.
fn save_media(dir: &Path, generation: u64, id: &str, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let current = MEDIA.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if *current != generation {
        return Err(anyhow!(t!("Not signed in")));
    }
    sweep_media(dir, MEDIA_TTL);
    let folder = dir.join(safe_name(id));
    std::fs::create_dir_all(&folder)?;
    let path = folder.join(safe_name(name));
    write_private(&path, bytes)?;
    Ok(path)
}

/// Deletes the event folders under `dir` written more than `ttl` ago. One directory listing and a
/// stat per folder (a few dozen at most).
fn sweep_media(dir: &Path, ttl: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|at| at.elapsed().is_ok_and(|age| age > ttl));
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Sweeps `dir` every half `MEDIA_TTL` for the life of the backend, also when nothing is fetched.
async fn sweep_media_every(dir: PathBuf) {
    let mut tick = tokio::time::interval(MEDIA_TTL / 2);
    loop {
        tick.tick().await;
        let dir = dir.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _held = MEDIA.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            sweep_media(&dir, MEDIA_TTL);
        })
        .await;
    }
}

/// Deletes every download now, for Settings' Storage page.
fn sweep_now(dir: &Path) {
    let _held = MEDIA.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    sweep_media(dir, Duration::ZERO);
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
/// plain image, else the image itself. A moving GIF a link's preview shows (`looped`) stays
/// moving within `linkprev::MAX_MOVING`. `None` when there is nothing reasonable to fetch.
async fn thumbnail(client: &Client, media: &Media) -> Result<Option<Vec<u8>>> {
    let pictured = matches!(media.kind, MediaKind::Image | MediaKind::Sticker);
    let moving = pictured && media.looped && media.thumb.is_none();
    let request = match (&media.thumb, &media.source) {
        (Some(thumb), _) => MediaRequestParameters { source: thumb.clone(), format: MediaFormat::File },
        (None, MediaSource::Plain(_)) if pictured && !moving => MediaRequestParameters {
            source: media.source.clone(),
            format: MediaFormat::Thumbnail(MediaThumbnailSettings::new(THUMB_PX.into(), THUMB_PX.into())),
        },
        (None, _) if pictured && media.size.is_none_or(|s| s <= MAX_THUMB_SOURCE) => {
            MediaRequestParameters { source: media.source.clone(), format: MediaFormat::File }
        }
        _ => return Ok(None),
    };
    let bytes = client.media().get_media_content(&request, true).await?;
    if moving && linkprev::gif_frames(&bytes).is_some_and(|(w, h, frames)| frames > 1 && u64::from(w) * u64::from(h) * 4 * frames as u64 <= linkprev::MAX_MOVING) {
        return Ok(Some(bytes));
    }
    Ok(Some(tokio::task::spawn_blocking(move || shrink(&bytes, THUMB_PX)).await??))
}

/// A bridged GIF as a loop for its bubble (`video::looping`), made once and kept in memory.
/// `None` for a clip without a size or over `video::MAX_LOOP_SOURCE`, and where it does not decode.
async fn looping(client: &Client, media: &Media, id: &str, dir: &Path, generation: u64) -> Result<Option<Vec<u8>>> {
    if let Some(gif) = video::kept(id) {
        return Ok(Some(gif));
    }
    let Some(dims) = media.dims else { return Ok(None) };
    if media.size.is_some_and(|s| s > video::MAX_LOOP_SOURCE) {
        return Ok(None);
    }
    let request = MediaRequestParameters { source: media.source.clone(), format: MediaFormat::File };
    let bytes = client.media().get_media_content(&request, true).await?;
    if bytes.len() as u64 > video::MAX_LOOP_SOURCE {
        return Ok(None);
    }
    let (dir, key, name, duration) = (dir.to_owned(), format!("{id}#loop"), media.name.clone(), media.duration);
    let _one = video::DECODING.acquire().await?;
    // The decoders read a file: it is there in `media/` only while they do.
    let gif = tokio::task::spawn_blocking(move || {
        let path = save_media(&dir, generation, &key, &name, &bytes)?;
        let gif = video::looping(&path, dims, duration);
        if let Some(folder) = path.parent() {
            let _ = std::fs::remove_dir_all(folder);
        }
        anyhow::Ok(gif)
    })
    .await??;
    if let Some(gif) = &gif {
        video::keep(id, gif.clone());
    }
    Ok(gif)
}

/// Queues a file as the message type its content calls for: the chat shows it at once, and the
/// send queue uploads it, keeps it across restarts and offers a retry when it fails. A picture
/// goes out scaled down and without metadata unless `hd`; `caption` becomes the message's body
/// (MSC2530), or a text message right after it where the network drops captions of that type.
async fn attach(room: &Room, path: &Path, reply_to: Option<String>, caption: &str, hd: bool) -> Result<()> {
    let size = tokio::fs::metadata(path).await?.len();
    if size > MAX_UPLOAD {
        return Err(anyhow!(t!("the file is larger than {mb} MB", mb = MAX_UPLOAD / 1024 / 1024)));

    }
    let mut name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".to_owned());
    let mut mime = mime_guess::from_path(path).first_or_octet_stream();
    // The registered type; the guess is the older `text/x-vcard`, which bridges pass on as it is.
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("vcf")) {
        mime = "text/vcard".parse()?;
    }
    // SVG is markup the user wrote or chose, sent as a document.
    let pictured = mime.type_() == mime_guess::mime::IMAGE && mime.subtype() != "svg";
    let filmed = mime.type_() == mime_guess::mime::VIDEO;
    let mut dims = None;
    let downscale = |source: PathBuf| async move { anyhow::Ok(tokio::task::spawn_blocking(move || caption::downscale(&source)).await??) };
    let data = if pictured && !hd && caption::recodable(&mime) {
        let sent = downscale(path.to_owned()).await?;
        (name, mime, dims) = (sent.name, sent.mime, Some((sent.width, sent.height)));
        sent.bytes
    } else if pictured {
        let raw = tokio::fs::read(path).await?;
        match tokio::task::spawn_blocking(move || strip::image(raw)).await? {
            Ok(clean) => clean,
            // A format the walk does not know (TIFF, BMP) or a file it finds broken: a re-encode carries no metadata.
            Err(e) => {
                log::debug!("strip: {e:#}");
                let sent = downscale(path.to_owned()).await.map_err(|_| anyhow!(t!("could not remove the location from this photo")))?;
                (name, mime, dims) = (sent.name, sent.mime, Some((sent.width, sent.height)));
                sent.bytes
            }
        }
    } else if filmed {
        if features::read(room).await.is_some_and(|f| !f.takes_video(mime.essence_str())) {
            return Err(mp4::Unfit::Refused.into());
        }
        let mut raw = tokio::fs::read(path).await?;
        tokio::task::spawn_blocking(move || strip::video(&mut raw).map(|()| raw)).await?.map_err(|e| {
            log::debug!("strip: {e:#}");
            mp4::Unfit::Unclean
        })?
    } else {
        tokio::fs::read(path).await?
    };
    if pictured || filmed {
        // The file's own name tells when it was taken (IMG_20250101_120000) or more.
        let ext = Path::new(&name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        name = format!("{}.{ext}", if filmed { "video" } else { "image" });
    }
    let mut config = AttachmentConfig::new();
    // Other clients show this until they fetch the file; a video has one only where a still can be read.
    let (data, thumb) = if pictured {
        tokio::task::spawn_blocking(move || {
            let thumb = caption::thumbnail(&data).map_err(|e| log::debug!("thumbnail: {e:#}")).ok().flatten();
            (data, thumb)
        })
        .await?
    } else if filmed {
        let source = path.to_owned();
        (data, tokio::task::spawn_blocking(move || video_thumbnail(&source)).await?)
    } else {
        (data, None)
    };
    if let Some(thumb) = thumb {
        config = config.thumbnail(Some(Thumbnail {
            size: UInt::new(thumb.bytes.len() as u64).unwrap_or_default(),
            data: thumb.bytes,
            content_type: thumb.mime,
            width: thumb.width.into(),
            height: thumb.height.into(),
        }));
    }
    if pictured {
        // Bridges and other clients lay the bubble out from these.
        let known = match dims {
            Some(dims) => Some(dims),
            None => image::ImageReader::new(std::io::Cursor::new(&data)).with_guessed_format()?.into_dimensions().ok(),
        };
        if let Some((width, height)) = known {
            config = config.info(AttachmentInfo::Image(BaseImageInfo {
                width: Some(width.into()),
                height: Some(height.into()),
                size: UInt::new(data.len() as u64),
                ..Default::default()
            }));
        }
    } else if let Some(shape) = filmed.then(|| strip::shape(&data)).flatten() {
        // Without a size, clients (this one too) draw a video as a file.
        config = config.info(AttachmentInfo::Video(BaseVideoInfo {
            width: Some(shape.width.into()),
            height: Some(shape.height.into()),
            duration: shape.millis.map(Duration::from_millis),
            size: UInt::new(data.len() as u64),
            blurhash: None,
        }));
    }
    let caption = caption.trim();
    let apart = !caption.is_empty() && !features::read(room).await.is_none_or(|f| f.keeps_caption(mime.essence_str()));
    if !caption.is_empty() && !apart {
        config = config.caption(Some(TextMessageEventContent::plain(caption)));
    }
    if let Some(id) = reply_to {
        let reply = Reply {
            event_id: EventId::parse(id)?,
            enforce_thread: EnforceThread::MaybeThreaded,
            add_mentions: AddMentions::Yes,
        };
        config = config.reply(Some(reply));
    }
    let queue = room.send_queue();
    queue.send_attachment(name, mime, data, config).await?;
    if apart {
        queue.send(RoomMessageEventContent::text_plain(caption).into()).await.context(t!("the caption was not sent"))?;
    }
    Ok(())
}

/// A still from a video for its thumbnail. Only Android can read one.
fn video_thumbnail(path: &Path) -> Option<caption::Prepared> {
    let bytes = crate::ui::video_frame(path, caption::THUMB_PX)?;
    let (width, height) = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
    Some(caption::Prepared { bytes, name: "thumbnail.jpg".to_owned(), mime: mime_guess::mime::IMAGE_JPEG, width, height })
}

/// One message to put in a chat's notification.
pub(crate) struct Note {
    /// The message's event ID, by which its edit or deletion finds it; empty where there is none.
    pub id: String,
    pub sender: String,
    pub text: String,
    pub ts: u64,
    pub mention: bool,
}

/// Most messages of one sync a notification is given; it shows the last few of a chat anyway.
const MAX_NOTES: usize = 6;
/// How long to wait for a chat's picture before notifying without it.
const ICON_WAIT: Duration = Duration::from_secs(3);
/// How long a notification waits for the SDK to count an encrypted chat's new message as unread.
const COUNT_WAIT: Duration = Duration::from_secs(3);

/// The messages these new items call for a notification of, oldest first: those from someone else
/// that the account's push rules ask to be notified of (they cover mutes and mentions-only chats).
/// Old messages do not count: a bridge bringing in history sends them through sync too.
fn notes(msgs: &[Msg], now_ms: i64) -> Vec<Note> {
    let mut found: Vec<Note> = msgs
        .iter()
        .rev()
        .filter(|m| m.notify && !m.own && recent(m, now_ms))
        // A message not decrypted yet says only that it came, as one the settings hide does.
        .map(|m| Note { id: m.id.clone(), sender: m.sender.clone(), text: if undecrypted(&m.body) { t!("New message").into() } else { m.summary() }, ts: m.ts, mention: m.mentioned })
        .take(MAX_NOTES)
        .collect();
    found.reverse();
    found
}

fn recent(m: &Msg, now_ms: i64) -> bool {
    now_ms - (m.ts as i64) < NOTIFY_WINDOW.as_millis() as i64
}

/// Edits and deletions of messages the chat's notification may show, made there too: a deleted
/// message leaves it, as in Signal. Older ones are history a bridge brings in.
fn amend_notes(dir: &Path, summary: &RoomSummary, amends: &[Msg]) {
    let now = now_ms();
    for m in amends.iter().filter(|m| recent(m, now)) {
        let chat = crate::accounts::key(crate::accounts::slot_of(dir), summary.id.as_str());
        let note = Note { id: m.id.clone(), sender: m.sender.clone(), text: m.summary(), ts: m.ts, mention: false };
        crate::amend_notification(&crate::Notice { chat: &chat, name: &summary.name, group: !summary.dm, icon: "", note: &note, alert: false, quiet: false }, m.deleted);
    }
}

/// Whether the window shows this account's chats now, so it needs no notification of them.
fn on_screen(dir: &Path) -> bool {
    crate::FOREGROUND.load(std::sync::atomic::Ordering::Relaxed) && crate::accounts::slot_of(dir) == crate::accounts::active()
}

/// Where the user's own read receipt in a chat stands, the last time each chat came by.
type ReadMarks = HashMap<OwnedRoomId, OwnedEventId>;

/// Whether the user read the chat somewhere since it last came by: this sync brings their receipt
/// (`fresh`; a reply moves it too) and it differs from the last one seen, or at a chat's first
/// sync from the one the SDK kept (`stored`). A chat with neither has nothing to compare, so a
/// full sync at start leaves the notifications standing.
fn read_moved(marks: &mut ReadMarks, room: &RoomId, fresh: Option<&EventId>, stored: Option<&EventId>) -> bool {
    let Some(fresh) = fresh else { return false };
    let before = marks.insert(room.to_owned(), fresh.to_owned());
    before.as_deref().or(stored).is_some_and(|before| before != fresh)
}

/// The user's own read receipt that a sync brings for a chat, from any of their devices: public or
/// private, on the main timeline, the newest when there are several. Read here and not from the
/// SDK's room, whose receipt the event cache moves only after the sync loop has seen the chat.
fn own_receipt(ephemeral: &[Raw<AnySyncEphemeralRoomEvent>], own: &UserId) -> Option<OwnedEventId> {
    use matrix_sdk::ruma::events::receipt::{ReceiptThread, ReceiptType};
    let receipts = ephemeral.iter().filter_map(|raw| match raw.deserialize().ok()? {
        AnySyncEphemeralRoomEvent::Receipt(ev) => Some(ev.content.0),
        _ => None,
    });
    let mine = receipts.flatten().flat_map(|(event, kinds)| {
        kinds
            .into_iter()
            .filter(|(kind, _)| matches!(kind, ReceiptType::Read | ReceiptType::ReadPrivate))
            .filter_map(|(_, users)| users.get(own).filter(|r| matches!(r.thread, ReceiptThread::Unthreaded | ReceiptThread::Main)).map(|r| r.ts))
            .map(move |ts| (ts, event.clone()))
            .collect::<Vec<_>>()
    });
    mine.max_by_key(|(ts, _)| *ts).map(|(_, event)| event)
}

/// The messages after the one the user has read up to, when it is among them.
fn after_read<'a>(msgs: &'a [Msg], read: Option<&EventId>) -> &'a [Msg] {
    match read.and_then(|read| msgs.iter().position(|m| m.id == read.as_str())) {
        Some(at) => &msgs[at + 1..],
        None => msgs,
    }
}

/// Notifies while the window does not show the account; clears the chat's notification once it
/// has been read somewhere, so a notification holds only messages not read yet.
/// `msgs`: those after the user's read receipt.
fn announce(client: &Client, dir: &Path, rooms: &Rooms, summary: &RoomSummary, prev: Option<&RoomSummary>, msgs: &[Msg], moved: bool) {
    // The count alone misses a read that came with a newer message in one sync.
    if moved || summary.unread == 0 && prev.is_some_and(|p| p.unread > 0) {
        crate::clear_notification(&crate::accounts::key(crate::accounts::slot_of(dir), summary.id.as_str()));
    }
    let theirs = msgs.iter().filter(|m| !m.own).count();
    if on_screen(dir) {
        if theirs > 0 {
            log::info!(target: "notify", "{theirs} new: the window shows the account");
        }
        return;
    }
    let notes = notes(msgs, now_ms());
    if notes.is_empty() {
        if theirs > 0 {
            let flagged = msgs.iter().filter(|m| !m.own && m.notify).count();
            log::info!(target: "notify", "{theirs} new, {flagged} notify by push rules, none of them from the last {} s", NOTIFY_WINDOW.as_secs());
        }
        return;
    }
    // Read on another device already. The server cannot count a mention in an encrypted chat that
    // notifies of mentions only, so a mention is not judged by the count.
    let mention = notes.iter().any(|n| n.mention);
    let waiting = move |r: &RoomSummary| r.is_unread() || mention;
    show_notes(client, dir, rooms, summary, notes, waiting);
}

/// Shows the notes as the chat's notification while `waiting` still holds for it.
fn show_notes(client: &Client, dir: &Path, rooms: &Rooms, summary: &RoomSummary, notes: Vec<Note>, waiting: impl Fn(&RoomSummary) -> bool + Send + Sync + 'static) {
    // The chat's own settings and the notification profiles, on this device only.
    let loud = match crate::quiet::judge(&summary.id, summary.muted, !summary.dm, notes.iter().any(|n| n.mention)) {
        crate::quiet::Verdict::Show { alert } => alert,
        held => {
            log::info!(target: "notify", "not shown: {held:?}");
            return;
        }
    };
    let (client, dir, rooms, summary) = (client.clone(), dir.to_owned(), rooms.clone(), summary.clone());
    let generation = media_generation();
    // Off the sync loop: the picture may have to be fetched.
    tokio::spawn(async move {
        let file = tokio::time::timeout(ICON_WAIT, notification_icon(&client, &dir, &summary.id, generation)).await.ok().flatten();
        let icon = file.as_deref().and_then(Path::to_str).unwrap_or_default();
        // An encrypted chat's count is the SDK's, worked out after the sync loop has summed the chat
        // up: a new message there counts as unread only once the SDK has caught up.
        let unread = || rooms.lock().unwrap().get(&summary.id).is_some_and(&waiting) || counted(&client, &summary.id);
        let deadline = tokio::time::Instant::now() + COUNT_WAIT;
        while !unread() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        {
            // Under the lock a sign-out moves the generation on with: nothing is shown after it.
            let current = MEDIA.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            // The chat may have been read while the picture was on its way.
            let still = unread();
            if !still {
                log::info!(target: "notify", "not shown: read already ({})", counts(&client, &summary.id));
            }
            if *current == generation && still {
                log::info!(target: "notify", "shown ({})", counts(&client, &summary.id));
                let chat = crate::accounts::key(crate::accounts::slot_of(&dir), summary.id.as_str());
                for (n, note) in notes.iter().enumerate() {
                    let alert = n + 1 == notes.len();
                    crate::notify(&crate::Notice { chat: &chat, name: &summary.name, group: !summary.dm, icon, note, alert, quiet: !loud });
                }
            }
        }
        // `Notify` has decoded and kept the picture by now; a chat's face does not stay on disk in clear.
        // A desktop notification reads it later: `notify` removes it with the notification.
        if cfg!(target_os = "android")
            && let Some(file) = file
        {
            let _ = std::fs::remove_file(file);
        }
    });
}

/// Whether the SDK counts unread messages that notify in this chat, where its count is the one used.
fn counted(client: &Client, id: &RoomId) -> bool {
    client.get_room(id).is_some_and(|room| {
        let receipts = room.read_receipts();
        room.encryption_state().is_encrypted() && receipts.latest_active.is_some() && receipts.num_notifications > 0
    })
}

/// Where a chat's unread count comes from, for the notify debug log.
fn counts(client: &Client, id: &RoomId) -> String {
    let Some(room) = client.get_room(id) else { return "no room".into() };
    let (server, receipts) = (room.unread_notification_counts(), room.read_receipts());
    format!(
        "encrypted {}, server {}, own receipt {}, sdk {}",
        room.encryption_state().is_encrypted(),
        server.notification_count,
        receipts.latest_active.is_some(),
        receipts.num_notifications
    )
}

/// Notifies of a new invitation once, unless the account's push rules turned invitations off.
fn announce_invite(client: &Client, dir: &Path, rooms: &Rooms, notif: &NotificationSettings, summary: &RoomSummary) {
    let Some(Pending::Invite { name, .. }) = &summary.pending else { return };
    if on_screen(dir) {
        return;
    }
    let text = if summary.dm { t!("Wants to chat with you") } else { t!("Invited you to the group") };
    let note = Note { id: String::new(), sender: name.clone(), text: text.to_owned(), ts: now_ms() as u64, mention: false };
    let (client, dir, rooms, notif, summary) = (client.clone(), dir.to_owned(), rooms.clone(), notif.clone(), summary.clone());
    tokio::spawn(async move {
        use matrix_sdk::ruma::push::{PredefinedOverrideRuleId, RuleKind};
        if notif.is_push_rule_enabled(RuleKind::Override, PredefinedOverrideRuleId::InviteForMe.as_str()).await.unwrap_or(true) {
            show_notes(&client, &dir, &rooms, &summary, vec![note], |r| r.pending.is_some());
        }
    });
}

/// A chat the user is invited to, from the stripped state the invitation carries.
/// `live`: it came in while the app was running, not in the backlog of a first sign-in.
async fn invitation(room: &Room, prev: Option<&RoomSummary>, live: bool) -> RoomSummary {
    let details = room.invite_details().await.ok();
    let by = details.as_ref().map(|d| d.inviter_id.clone());
    let inviter = match &details {
        Some(d) => d.inviter.as_ref().map_or_else(|| d.inviter_id.localpart().to_owned(), |m| m.name().to_owned()),
        None => String::new(),
    };
    let dm = room.is_direct().await.unwrap_or(false);
    let name = match room.name().filter(|n| !n.is_empty()) {
        Some(name) => name,
        None if dm && !inviter.is_empty() => inviter.clone(),
        None => room.display_name().await.map(|n| n.to_string()).unwrap_or_else(|_| room.room_id().to_string()),
    };
    RoomSummary {
        id: room.room_id().to_owned(),
        name,
        preview: None,
        preview_id: None,
        // Stripped state carries no time (Synapse 1.165 sends none): an invitation is dated when it
        // arrives. One from before the first sign-in has no known time and sorts below the chats.
        ts: prev.map_or(if live { now_ms() as u64 } else { 0 }, |p| p.ts),
        network: None,
        parent: None,
        forum: None,
        unread: 0,
        mentions: 0,
        dm,
        pinned: false,
        low_priority: false,
        muted: false,
        marked_unread: false,
        archived: false,
        archive_mark: false,
        order: 0,
        dated: true,
        pending: Some(Pending::Invite { by, name: inviter }),
        moved_to: None,
        own_last: false,
        read_by: Vec::new(),
        note: false,
        union: None,
        net_blocked: false,
        peer: None,
    }
}

/// A chat's face as its notification shows it: Linux servers show the picture as given, the other
/// systems crop it round themselves.
fn notification_face(bytes: Vec<u8>) -> Vec<u8> {
    #[cfg(target_os = "linux")]
    return crate::notify::round(bytes);
    #[cfg(not(target_os = "linux"))]
    bytes
}

/// The chat's picture as a file Android can read, one small file per chat that has notified.
/// Desktop keeps it while the chat's notification shows: then it is not fetched again.
async fn notification_icon(client: &Client, dir: &Path, room: &RoomId, generation: u64) -> Option<PathBuf> {
    let folder = dir.join("notify");
    let name: String = room.as_str().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let path = folder.join(name);
    if path.exists() {
        return Some(path);
    }
    let bytes = avatar(client, room).await.ok()??;
    let current = MEDIA.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if *current != generation {
        return None;
    }
    std::fs::create_dir_all(&folder).ok()?;
    write_private(&path, &notification_face(bytes)).ok()?;
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

/// How often Beeper is asked whether a network's login broke: one GET of its account API.
const NETWORK_CHECK: Duration = Duration::from_secs(15 * 60);

/// Notices a login that broke while Unisend runs, so the chat list can say so, and the plan's limit
/// on merged chats; the first check is at once.
async fn watch_networks(api: bridges::Api, ev: mpsc::Sender<Ev>, merge_limit: Arc<AtomicI64>) {
    loop {
        match api.whoami().await {
            Ok(whoami) => {
                let limit = union::limit(&whoami);
                if merge_limit.swap(limit, Ordering::Relaxed) != limit && ev.send(Ev::MergeLimit(limit)).await.is_err() {
                    return;
                }
                if ev.send(Ev::Attention(bridges::attention(&whoami))).await.is_err() {
                    return;
                }
            }
            Err(e) => log::debug!("network check: {e:#}"),
        }
        tokio::time::sleep(NETWORK_CHECK).await;
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

const ROOMS: &str = "rooms.json";

/// The interface language the cached previews were written in; caches without it are English.
fn rooms_lang_path(dir: &Path) -> PathBuf {
    dir.join("rooms-lang.txt")
}

fn rooms_lang() -> &'static str {
    if crate::i18n::polish() { "pl" } else { "en" }
}

fn load_rooms(dir: &Path, seal: &Seal) -> Option<HashMap<OwnedRoomId, RoomSummary>> {
    let mut rooms: HashMap<OwnedRoomId, RoomSummary> = serde_json::from_slice(&seal.read(dir, ROOMS).inspect_err(|e| if e.kind() != std::io::ErrorKind::NotFound { log::warn!("{e}") }).ok()?).ok()?;
    // Previews are text in the interface language, written again after a switch.
    if std::fs::read_to_string(rooms_lang_path(dir)).unwrap_or_else(|_| "en".into()) != rooms_lang() {
        relabel_rooms(&mut rooms, crate::i18n::polish());
    }
    Some(rooms)
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
/// bridged network, that the message was read. The read marker moves with it, so the chat opens
/// again without the unread line.
/// Nothing is sent before the user takes the chat: an invitation or a message request is not
/// told it was seen.
/// Marks the chat read up to its newest event.
async fn read_newest(room: &Room, private: bool) -> Result<()> {
    let resp = room.messages(MessagesOptions::backward()).await?;
    mark_read(room, resp.chunk.first().and_then(|e| e.event_id()).map(ToOwned::to_owned), private).await
}

async fn mark_read(room: &Room, newest: Option<OwnedEventId>, private: bool) -> Result<()> {
    if room.state() != RoomState::Joined {
        return Ok(());
    }
    room.set_unread_flag(false).await?;
    if request(room).await {
        return Ok(());
    }
    if let Some(id) = newest {
        // `m.fully_read` is the user's own account data, never shown to others, so it suits private receipts too.
        let receipts = matrix_sdk::room::Receipts::new().fully_read_marker(id.clone());
        // No one else is in a note to self; nothing about it is sent.
        let private = private || is_note(room).await;
        let receipts = if private { receipts.private_read_receipt(id) } else { receipts.public_read_receipt(id) };
        room.send_multiple_receipts(receipts).await?;
    }
    Ok(())
}

/// `mark_read` for the open chat, up to what the user has seen. The timeline sends a receipt only
/// past the one it has, so scrolling back up never unreads; it clears the unread mark either way.
async fn read_to(timeline: &matrix_sdk_ui::Timeline, id: OwnedEventId, private: bool) -> Result<()> {
    let room = timeline.room();
    if room.state() != RoomState::Joined {
        return Ok(());
    }
    if request(room).await {
        room.set_unread_flag(false).await?;
        return Ok(());
    }
    let receipts = matrix_sdk::room::Receipts::new().fully_read_marker(id.clone());
    let private = private || is_note(room).await;
    let receipts = if private { receipts.private_read_receipt(id) } else { receipts.public_read_receipt(id) };
    timeline.send_multiple_receipts(receipts).await?;
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

impl Done {
    /// Archived at some point and not unarchived since, which writes `{}`.
    fn marked(&self) -> bool {
        self.at_order.is_some_and(|o| o != 0) || self.updated_ts.is_some_and(|t| t > 0)
    }
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
async fn last_message(room: &Room) -> Result<Option<(String, u64, String)>> {
    let dm = other_person(room).await.is_some();
    let mut from = None;
    for _ in 0..MAX_PREVIEW_PAGES {
        let resp = messages(room, from.as_deref(), PAGE).await?;
        let mut chunk = resp.chunk;
        chunk.reverse();
        let (msgs, _) = to_msgs(room, &chunk).await;
        if let Some(m) = msgs.last() {
            return Ok(Some((preview(m, dm), m.ts, m.id.clone())));
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
        Some((text, sent, id)) => (r.preview, r.ts, r.preview_id) = (Some(text), sent, Some(id)),
        // No message and no preview: whatever time it had came from a state event.
        None if r.preview.is_none() => r.ts = 0,
        None => {}
    }
    Some(r.clone())
}

/// Unread counts of encrypted chats are the SDK's, which it works out from the events and receipts
/// after the sync loop has summed the chat up: the row is summed up again once they changed.
/// rooms.json picks the change up on the sync loop's next save.
async fn recount(client: Client, notif: NotificationSettings, rooms: Rooms, ev: mpsc::Sender<Ev>) {
    let mut updates = client.room_info_notable_update_receiver();
    loop {
        let id = match updates.recv().await {
            Ok(update) => update.room_id,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };
        let Some(room) = client.get_room(&id) else { continue };
        let receipts = room.read_receipts();
        if receipts.latest_active.is_none() {
            continue;
        }
        let encrypted = room.encryption_state().is_encrypted();
        let counted = |s: &RoomSummary| if s.muted { receipts.num_unread } else { receipts.num_notifications };
        let Some(seen) = rooms.lock().unwrap().get(&id).cloned().filter(|s| (encrypted || s.muted) && s.unread != counted(s)) else { continue };
        let fresh = summarize(&room, &notif, other_person(&room).await, seen).await;
        let changed = rooms.lock().unwrap().get_mut(&id).map(|r| {
            (r.unread, r.mentions, r.archived, r.archive_mark) = (fresh.unread, fresh.mentions, fresh.archived, fresh.archive_mark);
            r.clone()
        });
        if let Some(summary) = changed {
            UNSAVED_PROBES.store(true, std::sync::atomic::Ordering::Relaxed);
            if ev.send(Ev::Rooms(vec![summary])).await.is_err() {
                return;
            }
        }
    }
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
    // A markdown link: a bridge's plain body, cached before formatted bodies were read.
    // shortcut: a message whose formatted form keeps "](http" is re-read at every start, one request each; drop the rule once old caches are gone.
    let stale = |p: &String| ["[Photo]", "[Video]", "[Audio]", "[File]", "[Location]", "](http"].iter().any(|old| p.contains(old));
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
    let note = is_note(room).await;
    let name = match &peer {
        _ if note => t!("Note to self").to_owned(),
        // A bridged one-to-one room also holds the bridge bot, which the SDK's computed name
        // counts: "Ann, and 2 others". Beeper titles it with the other person alone.
        Some(peer) if room.name().is_none() => display_name(room, peer).await,
        _ if room.name().is_none() && room.canonical_alias().is_none() && let Some(title) = unnamed_group(room).await => title,
        _ => room.display_name().await.map(|n| n.to_string()).unwrap_or_else(|_| seen.id.to_string()),
    };
    // A chat with a parent and no forum is read again: a cache from before forums lacks `forum`.
    let (network, parent, forum) = match (seen.network, seen.parent, seen.forum) {
        (Some(n), Some(p), Some(f)) => (Some(n), Some(p), Some(f)),
        (n, p, f) => {
            let (found, parent, forum) = bridge_names(room).await;
            (n.or(found), p.or(parent), f.or(forum))
        }
    };
    let mode = notif.get_user_defined_room_notification_mode(room.room_id()).await;
    let done = match room.account_data(DONE.into()).await {
        Ok(Some(raw)) => raw.get_field::<Done>("content").ok().flatten().unwrap_or_default(),
        _ => Done::default(),
    };
    let server = room.unread_notification_counts();
    let receipts = room.read_receipts();
    let client = receipts.latest_active.is_some().then_some((receipts.num_unread, receipts.num_notifications, receipts.num_mentions));
    let muted = matches!(mode, Some(RoomNotificationMode::Mute | RoomNotificationMode::MentionsAndKeywordsOnly));
    let (unread, highlights) = unread(room.encryption_state().is_encrypted(), muted, (server.notification_count, server.highlight_count), client);
    let mut s = RoomSummary {
        name,
        network,
        parent,
        forum,
        dm: peer.is_some() || note,
        note,
        peer: peer.clone(),
        preview: current(seen.preview),
        preview_id: seen.preview_id,
        unread,
        // Once the chat is read nothing in it is an unread mention.
        mentions: seen.mentions.max(highlights).min(unread),
        pinned: room.is_favourite(),
        low_priority: room.is_low_priority(),
        muted,
        marked_unread: room.is_marked_unread(),
        ..seen
    };
    s.archived = is_done(&done, &s, now_ms());
    s.archive_mark = !s.archived && !s.pinned && done.marked();
    s.pending = request(room).await.then_some(Pending::Request);
    s.net_blocked = s.network.is_some() && block::on_network(room).await;
    s.moved_to = room.successor_room().map(|next| next.room_id);
    s.union = union::read(room).await;
    if s.own_last && let Some(id) = s.preview_id.as_deref().and_then(|id| EventId::parse(id).ok()) {
        s.read_by = readers(room, &id).await;
    }
    s
}

/// Readers named in a chat row's status, at most.
const ROW_READERS: usize = 3;

/// Who has read an event, by the public receipts the store holds on it. No network.
// shortcut: a receipt already moved past the event (onto a reaction or a state event) is not seen
// here, so such a chat shows Sent; walk the newer events if that shows up in practice.
async fn readers(room: &Room, event: &EventId) -> Vec<(OwnedUserId, String)> {
    use matrix_sdk::ruma::events::receipt::{ReceiptThread, ReceiptType};
    let own = room.own_user_id();
    let service = room.service_members().unwrap_or_default();
    let mut out = Vec::new();
    let receipts = room.load_event_receipts(ReceiptType::Read, &ReceiptThread::Unthreaded, event).await.unwrap_or_default();
    for (user, _) in receipts.into_iter().filter(|(u, _)| u != own && !service.contains(u)).take(ROW_READERS) {
        let name = display_name(room, &user).await;
        out.push((user, name));
    }
    out
}

/// Names shown typing in a chat row, at most.
const ROW_TYPING: usize = 3;

/// Tells the list who is writing in every chat, from the typing notices syncs bring anyway.
fn list_typing(client: &Client, ev: &mpsc::Sender<Ev>) {
    let ev = ev.clone();
    client.add_event_handler(move |e: matrix_sdk::ruma::events::typing::SyncTypingEvent, room: Room| {
        let ev = ev.clone();
        async move {
            let own = room.own_user_id().to_owned();
            let service = room.service_members().unwrap_or_default();
            let mut names = Vec::new();
            for user in e.content.user_ids.iter().filter(|u| **u != own && !service.contains(*u)).take(ROW_TYPING) {
                names.push(display_name(&room, user).await);
            }
            let _ = ev.send(Ev::ListTyping { room: room.room_id().to_owned(), names }).await;
        }
    });
}

/// A chat's unread messages that notify, and its mentions among them. In an encrypted chat the
/// server counts every event it cannot read, reactions and edits too, and cannot see mentions or
/// mutes; the SDK's own count from the decrypted events is used once it has found the user's read
/// receipt among them. A muted chat notifies of nothing, so it counts every unread message
/// instead, from the SDK's count, as Signal shows it: still there, but grey.
/// `client`: the SDK's (messages, notifying, mentions).
fn unread(encrypted: bool, muted: bool, server: (u64, u64), client: Option<(u64, u64, u64)>) -> (u64, u64) {
    let (notifying, mentions) = match client {
        Some((_, n, h)) if encrypted => (n, h),
        _ => server,
    };
    match client {
        Some((all, ..)) if muted => (all.max(notifying), mentions),
        _ => (notifying, mentions),
    }
}

/// Removes cached chats the account is no longer in. The sync filter does not ask for left rooms
/// (a first sync would then carry every chat ever left), so a chat left on another device stays
/// "joined" here; the server's own list settles it with one request. Skipped when offline.
async fn drop_left(client: &Client, rooms: &Rooms, ev: &mpsc::Sender<Ev>) {
    // Only rooms known before the request: one the sync adds meanwhile may be missing from its answer.
    let known: Vec<OwnedRoomId> = rooms.lock().unwrap().keys().cloned().collect();
    let joined: std::collections::HashSet<OwnedRoomId> = match client.send(joined_rooms::v3::Request::new()).await {
        Ok(response) => response.joined_rooms.into_iter().collect(),
        Err(e) => return log::debug!("joined rooms: {e}"),
    };
    let invited = |id: &OwnedRoomId| client.get_room(id).is_some_and(|r| r.state() == RoomState::Invited);
    let left: Vec<OwnedRoomId> = known.into_iter().filter(|id| !joined.contains(id) && !invited(id)).collect();
    for id in left {
        rooms.lock().unwrap().remove(&id);
        let _ = ev.send(Ev::RoomGone(id)).await;
    }
}

/// Rooms without new events never pass through the sync loop, so their cached summaries miss
/// fields added since and changes made on other devices. Store reads only.
async fn refresh_cached(client: &Client, notif: &NotificationSettings, rooms: &Rooms) -> Vec<RoomSummary> {
    let ids: Vec<OwnedRoomId> = rooms.lock().unwrap().keys().cloned().collect();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        // Read as late as possible: the sync loop may have updated it since the walk began.
        let Some(seen) = rooms.lock().unwrap().get(&id).cloned() else { continue };
        let Some(room) = client.get_room(&id) else { continue };
        // An invitation has only the stripped state it came with: its summary was made from that.
        if room.state() == RoomState::Invited {
            out.push(seen);
            continue;
        }
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

/// Whether the SDK holds no events of the newest chat: an upgrade of its store emptied it, or the
/// chat has not changed since the store was made.
async fn forgotten(client: &Client, rooms: &Rooms) -> bool {
    let newest = rooms.lock().unwrap().values().max_by_key(|r| r.ts).map(|r| r.id.clone());
    let Some(room) = newest.and_then(|id| client.get_room(&id)) else { return false };
    let Ok((cache, _)) = room.event_cache().await else { return false };
    cache.events().await.is_ok_and(|events| events.is_empty())
}

/// The sync loop, caught when it panics so the UI can say so and offer a restart. Not a second
/// task: aborting this one still stops the loop before a sign-out wipes what it writes.
async fn sync_watched(client: Client, notif: NotificationSettings, dir: PathBuf, seal: Seal, rooms: Rooms, ev: mpsc::Sender<Ev>) {
    use futures_util::FutureExt as _;
    // The panic hook has already logged where it happened.
    if let Err(panic) = std::panic::AssertUnwindSafe(sync_loop(client, notif, dir, seal, rooms.clone(), ev.clone())).catch_unwind().await {
        let why = panic.downcast_ref::<&str>().copied().or_else(|| panic.downcast_ref::<String>().map(String::as_str)).unwrap_or("panic");
        log::error!("sync stopped: {why}");
        // A list the panic left half updated beats one no task can lock again; the next sync rewrites it.
        rooms.clear_poison();
        let _ = ev.send(Ev::SyncStopped).await;
    }
}

async fn sync_loop(client: Client, notif: NotificationSettings, dir: PathBuf, seal: Seal, rooms: Rooms, ev: mpsc::Sender<Ev>) {
    // A first sign-in brings every invitation ever left unanswered: those are not news.
    let first_run = rooms.lock().unwrap().is_empty();
    // Chats whose last message is not known are looked up in the background. A full queue drops
    // the rest; they stay undated and are queued again at the next start.
    let (to_probe, queue) = mpsc::channel(PROBE_QUEUE);
    let prober = tokio::spawn(probe_queue(client.clone(), rooms.clone(), ev.clone(), queue));
    let _stop_prober = AbortOnDrop(prober);
    let _stop_recount = AbortOnDrop(tokio::spawn(recount(client.clone(), notif.clone(), rooms.clone(), ev.clone())));
    // The cached list is on screen already: new messages need not wait for a request and a walk
    // over every stored room.
    let _stop_settle = AbortOnDrop(tokio::spawn(settle_cached(client.clone(), notif.clone(), rooms.clone(), ev.clone(), to_probe.clone())));
    let mut filter = RoomFilter::with_lazy_loading();
    filter.timeline.limit = Some(UInt::from(SYNC_TIMELINE_LIMIT));
    let mut def = FilterDefinition::default();
    def.room = filter;
    // Without a summary cache the stored sync token would skip rooms with no new events. So it
    // would when the SDK has lost their events, and a chat it holds none of waits 3 s for a sync
    // that mentions it before asking the server for its history.
    // shortcut: the SDK drops a full answer at the token it already holds, so the resync lands at
    // the first change on the account; clear the stored token if a quiet account ever needs it.
    let token = if rooms.lock().unwrap().is_empty() || forgotten(&client, &rooms).await { SyncToken::NoToken } else { SyncToken::ReusePrevious };
    // Syncing marks the account online by default; nothing in the app shows presence, so never share it.
    let settings = SyncSettings::default().filter(def.into()).token(token).set_presence(matrix_sdk::ruma::presence::PresenceState::Offline);

    let mut stream = std::pin::pin!(client.sync_stream(settings).await);
    // Requests failed in a row.
    let mut failed = 0u32;
    let mut synced = false;
    // Bounded by the account's chats.
    let mut read_marks = ReadMarks::new();
    while let Some(resp) = stream.next().await {
        let resp = match resp {
            Ok(r) => r,
            Err(e) if matches!(e.client_api_error_kind(), Some(ErrorKind::UnknownToken(_))) => {
                // The server dropped this device: nothing kept here can be used again.
                wipe_account(&dir);
                let _ = ev.send(Ev::Error(t!("Signed out by server: {error}", error = e))).await;
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
            if room.is_space() || rooms::leaving(id) {
                continue;
            }
            let (msgs, amends) = to_msgs(&room, &update.timeline.events).await;
            let prev = rooms.lock().unwrap().get(id).cloned();
            // A chat's time is its last message's. A room a bridge has just created has only state
            // events, which say nothing about when the chat was last used: it stays at 0, sorts
            // last, and is queued for a look at its history.
            let last_message = msgs.iter().map(|m| m.ts).max();
            // An invitation's time is when it arrived: once taken, the chat goes by its messages.
            let timed = prev.as_ref().filter(|p| !matches!(p.pending, Some(Pending::Invite { .. })));
            let dated = last_message.is_some() || timed.is_some_and(|p| p.dated);
            if !dated {
                let _ = to_probe.try_send(id.clone());
            }
            let newest = last_message.max(timed.map(|p| p.ts));
            let peer = other_person(&room).await;
            let dm = peer.is_some();
            let order = msgs.iter().filter(|m| !m.own).map(|m| m.order);
            let preview_id = msgs.last().map(|m| m.id.clone()).or_else(|| prev.as_ref().and_then(|p| p.preview_id.clone()));
            // An edit or deletion of the message the row shows changes the row.
            let amended = amends.iter().rev().find(|m| Some(&m.id) == preview_id.as_ref());
            let seen = RoomSummary {
                id: id.clone(),
                name: String::new(),
                preview: amended.or(msgs.last()).map(|m| preview(m, dm)).or_else(|| prev.as_ref().and_then(|p| p.preview.clone())),
                preview_id,
                ts: newest.unwrap_or(0),
                network: prev.as_ref().and_then(|p| p.network.clone()),
                parent: prev.as_ref().and_then(|p| p.parent.clone()),
                forum: prev.as_ref().and_then(|p| p.forum.clone()),
                order: order.max().max(prev.as_ref().map(|p| p.order)).unwrap_or(0),
                unread: 0,
                dm,
                mentions: prev.as_ref().map_or(0, |p| p.mentions),
                pinned: false,
                low_priority: false,
                muted: false,
                marked_unread: false,
                archived: false,
                archive_mark: false,
                dated,
                pending: None,
                moved_to: None,
                own_last: amended.or(msgs.last()).map(|m| m.own).or_else(|| prev.as_ref().map(|p| p.own_last)).unwrap_or(false),
                read_by: Vec::new(),
                note: false,
                union: None,
                net_blocked: false,
                peer: None,
            };
            let mut summary = summarize(&room, &notif, peer, seen).await;
            summary.mentions = unread_mentions(summary.mentions, summary.unread, &msgs);
            rooms.lock().unwrap().insert(id.clone(), summary.clone());
            // The SDK's receipt is the last sync's: its event cache takes this one after the loop.
            let (fresh, stored) = (own_receipt(&update.ephemeral, room.own_user_id()), room.read_receipts().latest_active.map(|r| r.event_id));
            let read = fresh.clone().or_else(|| read_marks.get(id).cloned()).or_else(|| stored.clone());
            let moved = read_moved(&mut read_marks, id, fresh.as_deref(), stored.as_deref());
            announce(&client, &dir, &rooms, &summary, prev.as_ref(), after_read(&msgs, read.as_deref()), moved);
            amend_notes(&dir, &summary, &amends);
            // Typing, receipts and room account data bring a room here without changing its row.
            if prev.as_ref() != Some(&summary) {
                changed.push(summary);
            }
        }
        for id in resp.rooms.invited.keys() {
            let Some(room) = client.get_room(id) else { continue };
            if room.is_space() || rooms::leaving(id) {
                continue;
            }
            let prev = rooms.lock().unwrap().get(id).cloned();
            let live = synced || !first_run;
            let summary = invitation(&room, prev.as_ref(), live).await;
            rooms.lock().unwrap().insert(id.clone(), summary.clone());
            if prev.is_none() && live {
                announce_invite(&client, &dir, &rooms, &notif, &summary);
            }
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
            let saved = saved.and_then(|b| Ok(seal.write(&dir, ROOMS, &b)?)).and_then(|()| write_private(&rooms_lang_path(&dir), rooms_lang().as_bytes()));
            if let Err(e) = saved {
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
        if !std::mem::replace(&mut synced, true) && ev.send(Ev::Synced).await.is_err() {
            return;
        }
        // A test of the stopped-sync card: a debug build's sync dies once at the next answer after
        // this file appears in the data directory.
        #[cfg(debug_assertions)]
        if std::fs::remove_file(dir.join("test-sync-panic")).is_ok() {
            panic!("test-sync-panic");
        }
    }
}

/// Brings the cached list up to date with the server and the store, next to the first sync.
async fn settle_cached(client: Client, notif: NotificationSettings, rooms: Rooms, ev: mpsc::Sender<Ev>, to_probe: mpsc::Sender<OwnedRoomId>) {
    drop_left(&client, &rooms, &ev).await;
    let cached = refresh_cached(&client, &notif, &rooms).await;
    // The sync loop writes rooms.json; this marks what changed here for its next save.
    UNSAVED_PROBES.store(true, std::sync::atomic::Ordering::Relaxed);
    // Top of the list first: those are the rows in view.
    let mut undated: Vec<&RoomSummary> = cached.iter().filter(|r| !r.dated).collect();
    undated.sort_unstable_by_key(|r| std::cmp::Reverse(r.ts));
    for unknown in undated {
        let _ = to_probe.try_send(unknown.id.clone());
    }
    // Read again: the sync loop may have sent a newer summary of a room while the walk went on.
    let cached: Vec<RoomSummary> = {
        let map = rooms.lock().unwrap();
        cached.iter().filter_map(|r| map.get(&r.id).cloned()).collect()
    };
    if !cached.is_empty() {
        let _ = ev.send(Ev::Rooms(cached)).await;
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

/// What a person is told of an error: a request that never reached the server says only that.
pub(crate) fn told(error: &str) -> String {
    match error.find(UNREACHABLE) {
        Some(at) => format!("{}{}", &error[..at], t!("No connection to the server")),
        None => plain(&error),
    }
}

/// How the HTTP client words a request that never reached the server (refused, no DNS, timed out).
pub(crate) const UNREACHABLE: &str = "error sending request";

pub(crate) fn utd() -> &'static str {
    t!("Unable to decrypt this message yet")
}

/// What a message that could not be decrypted says, by what the SDK knows of why: "yet" only
/// where its keys may still come.
pub(crate) fn utd_because(cause: UtdCause) -> &'static str {
    match cause {
        UtdCause::Unknown => utd(),
        UtdCause::SentBeforeWeJoined => t!("Sent before you joined this chat"),
        UtdCause::HistoricalMessageAndBackupIsDisabled => t!("Sent before this device signed in, and the message backup is off"),
        UtdCause::HistoricalMessageAndDeviceIsUnverified => t!("Verify this device to read this message"),
        UtdCause::VerificationViolation | UtdCause::UnsignedDevice | UtdCause::UnknownDevice => t!("Sent from a device that is not verified"),
        UtdCause::WithheldForUnverifiedOrInsecureDevice => t!("The sender does not share messages with unverified devices"),
        UtdCause::WithheldBySender => t!("The sender's device did not share the key to this message"),
    }
}

/// Whether a message's text is that of one that could not be decrypted.
pub(crate) fn undecrypted(body: &str) -> bool {
    use UtdCause::*;
    let causes = [Unknown, SentBeforeWeJoined, HistoricalMessageAndBackupIsDisabled, HistoricalMessageAndDeviceIsUnverified, VerificationViolation, UnsignedDevice, UnknownDevice];
    causes.into_iter().chain([WithheldForUnverifiedOrInsecureDevice, WithheldBySender]).any(|cause| utd_because(cause) == body)
}
pub(crate) fn deleted() -> &'static str {
    t!("Message deleted")
}

/// The messages among timeline events, oldest first, as the chat list and notifications need
/// them: what each says, by whom and when. Edits, reactions and deletions belong to the open
/// conversation, which the SDK's timeline keeps.
/// Messages of a batch of events, and the changes the batch makes to messages it may not hold:
/// each edited or deleted message as it now reads, under its own id.
async fn to_msgs(room: &Room, events: &[TimelineEvent]) -> (Vec<Msg>, Vec<Msg>) {
    let own = room.own_user_id().to_owned();
    let rules = room.version().and_then(|v| v.rules()).unwrap_or(RoomVersionRules::V11);
    let mut names: HashMap<OwnedUserId, String> = HashMap::new();
    let (mut out, mut amends) = (Vec::new(), Vec::new());
    for event in events {
        let Some(id) = event.event_id().map(|e| e.to_string()) else { continue };
        let Some(sender) = event.sender() else { continue };
        let mut m = Msg::bare(id, Kind::Message, sender.clone());
        let mut amends_id = None;
        let mut emote = false;
        // A deleted event in an encrypted room is an empty `m.room.encrypted`: nothing says whether
        // it was a message or a reaction, so it gets no placeholder.
        if event.kind.is_utd() {
            if is_redacted(event) {
                continue;
            }
            m.body = utd().to_owned();
        } else if let Some(poll) = poll_start(event) {
            m.poll = Some(poll);
        } else {
            match event.raw().deserialize() {
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(SyncMessageLikeEvent::Original(e)))) => {
                    let (msgtype, mentions, reply) = match &e.content.relates_to {
                        Some(Relation::Replacement(r)) => {
                            (amends_id, m.edited) = (Some(r.event_id.to_string()), true);
                            (&r.new_content.msgtype, r.new_content.mentions.as_ref(), false)
                        }
                        relation => (&e.content.msgtype, e.content.mentions.as_ref(), matches!(relation, Some(Relation::Reply(_)))),
                    };
                    // A verification request's body is a fallback, not words for the list.
                    if matches!(msgtype, MessageType::VerificationRequest(_)) {
                        continue;
                    }
                    let (media, text) = content(msgtype);
                    m.media = media;
                    if let MessageType::Location(l) = msgtype {
                        m.place = Place::parse(&l.geo_uri, &l.body);
                    }
                    // Older clients and bridges prepend the quoted message to the body.
                    let text = if reply { remove_plain_reply_fallback(&text).to_owned() } else { text };
                    // The place's description is told with the place.
                    m.body = if m.place.is_some() { String::new() } else { text };
                    format(&mut m, msgtype);
                    (m.mentions, m.mentioned, m.to_me) = marks(&m.body, msgtype, mentions, &own);
                    emote = matches!(msgtype, MessageType::Emote(_));
                }
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomRedaction(r))) => {
                    let Some(target) = r.redacts(&rules.redaction) else { continue };
                    amends_id = Some(target.to_string());
                    (m.body, m.deleted) = (deleted().to_owned(), true);
                }
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(SyncMessageLikeEvent::Redacted(_)))) => {
                    (m.body, m.deleted) = (deleted().to_owned(), true);
                }
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::Sticker(SyncMessageLikeEvent::Original(s)))) => {
                    m.media = Some(sticker(&s.content));
                }
                Ok(AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomEncrypted(_))) if !is_redacted(event) => {
                    m.body = utd().into();
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
        if emote {
            act(&mut m);
        }
        m.ts = event.timestamp().map_or(0, |t| u64::from(t.0));
        m.order = hs_order(event).unwrap_or(0);
        m.own = sender == own;
        m.mentioned &= !m.own;
        m.notify = event.push_actions().is_some_and(|actions| actions.iter().any(|a| a.should_notify()));
        match amends_id {
            Some(target) => amends.push(Msg { id: target, notify: false, mentioned: false, ..m }),
            None => out.push(m),
        }
    }
    (out, amends)
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
        looped: false,
        once: false,
        waveform: Vec::new(),
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
        let mime = Some(text("og:image:type")).filter(|m| !m.is_empty());
        let image = source.map(|source| Media {
            kind: MediaKind::Image,
            name: String::new(),
            size: None,
            looped: mime.as_deref() == Some("image/gif"),
            mime,
            dims: number("og:image:width").zip(number("og:image:height")),
            duration: None,
            source,
            thumb: None,
            once: false,
            waveform: Vec::new(),
        });
        let colour = p.get(linkprev::COLOUR_KEY).and_then(Value::as_str).and_then(linkprev::colour).and_then(|hex| u32::from_str_radix(&hex[1..], 16).ok());
        let video = text("og:type").starts_with("video") || ["og:video", "og:video:url", "og:video:secure_url"].iter().any(|k| !text(k).is_empty());
        Some(Link { url, title, description, site, image, colour, video })
    })
}

/// The HTML form of a text message, when the sender gave one.
fn html(msgtype: &MessageType) -> Option<&str> {
    let f = match msgtype {
        MessageType::Text(c) => c.formatted.as_ref(),
        MessageType::Notice(c) => c.formatted.as_ref(),
        MessageType::Emote(c) => c.formatted.as_ref(),
        // A caption: an Instagram story reply is a picture whose caption is the quote and link.
        MessageType::Image(c) => c.formatted.as_ref(),
        MessageType::Video(c) => c.formatted.as_ref(),
        MessageType::Audio(c) => c.formatted.as_ref(),
        MessageType::File(c) => c.formatted.as_ref(),
        _ => None,
    }?;
    (f.format == MessageFormat::Html).then_some(f.body.as_str())
}

/// Reads the message's formatted body: `body` becomes its plain text (no markdown, no reply
/// fallback), `rich` its styled form. Without a formatted body `body` stays as it is.
fn format(m: &mut Msg, msgtype: &MessageType) {
    // Media without a caption has nothing to format, whatever HTML came with it.
    if m.place.is_some() || (m.media.is_some() && m.body.is_empty()) {
        return;
    }
    if let Some((plain, rich)) = html(msgtype).and_then(rich::parse) {
        (m.body, m.rich) = (plain, Some(rich));
    }
}

/// Where `body` mentions someone, whether it mentions `own`, and where it names `own`. Matrix
/// writes a mention twice: as a link to the user in the HTML form, and in `m.mentions`.
fn marks(body: &str, msgtype: &MessageType, mentions: Option<&Mentions>, own: &UserId) -> (Vec<std::ops::Range<usize>>, bool, Vec<std::ops::Range<usize>>) {
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
    let mine: Vec<&str> = linked.iter().filter(|(user, _)| user == own.as_str()).map(|(_, name)| name.as_str()).collect();
    (mention_ranges(body, &names, room), mentioned, mention_ranges(body, &mine, room))
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

/// The chat is the user's note to self. Beeper marks it with an empty-keyed state event of this type.
async fn is_note(room: &Room) -> bool {
    let events = room.get_state_events(StateEventType::from(NOTE_TO_SELF)).await.unwrap_or_default();
    events.iter().any(|e| matches!(e, RawAnySyncOrStrippedState::Sync(raw) if raw.get_field::<String>("state_key").ok().flatten().is_some_and(|k| k.is_empty())))
}

pub(crate) const NOTE_TO_SELF: &str = "com.beeper.rooms.note_to_self";

/// The title of an unnamed group that has a bridge bot: the SDK's computed one counts the bot
/// ("Ann and Bridge bot"), this names the people alone. `None` when the room has no service
/// member, or no one is known to name, and the SDK's title stands.
async fn unnamed_group(room: &Room) -> Option<String> {
    let mut service = room.service_members().unwrap_or_default();
    // A bridge that lists no functional members still names its bot in `m.bridge`.
    service.extend(disappear::state(room, "m.bridge").await.iter().filter_map(|c| UserId::parse(c.get("bridgebot")?.as_str()?).ok()));
    if service.is_empty() {
        return None;
    }
    let own = room.own_user_id();
    let heroes: Vec<OwnedUserId> = room.clone_info().heroes().iter().map(|h| h.user_id.clone()).filter(|u| u != own && !service.contains(u)).collect();
    let mut names = Vec::new();
    for user in heroes.iter().take(GROUP_NAMED) {
        names.push(display_name(room, user).await);
    }
    let people = (room.joined_members_count() + room.invited_members_count()).saturating_sub(1 + service.len() as u64);
    group_title(&names, people)
}

/// Names an unnamed group's title lists, at most.
const GROUP_NAMED: usize = 3;

/// Up to `GROUP_NAMED` of the people, then how many more there are; `people` counts everyone but the user.
fn group_title(names: &[String], people: u64) -> Option<String> {
    let shown = &names[..names.len().min(GROUP_NAMED)];
    if shown.is_empty() {
        return None;
    }
    let list = shown.join(", ");
    Some(match people.saturating_sub(shown.len() as u64) {
        0 => list,
        more => tn!(more, "{names} and {n} other", "{names} and {n} others", names = list),
    })
}

async fn display_name(room: &Room, user: &UserId) -> String {
    match room.get_member_no_sync(user).await {
        Ok(Some(m)) => m.name().to_owned(),
        _ => user.localpart().to_owned(),
    }
}

/// The other person in a one-to-one chat. Bridged DMs also hold a bridge bot, listed as a service member.
async fn other_person(room: &Room) -> Option<OwnedUserId> {
    let marked = room.is_direct().await.unwrap_or(false);
    if !marked && !one_to_one(bridge_kind(room).await, room.name().is_some()) {
        return None;
    }
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

/// The bridge holds the chat as a message request: `com.beeper.message_request` in the channel of
/// its `m.bridge` state (mautrix-go event/state.go).
pub(crate) async fn request(room: &Room) -> bool {
    let events = room.get_state_events(StateEventType::from("m.bridge")).await.unwrap_or_default();
    events.iter().any(|e| {
        let RawAnySyncOrStrippedState::Sync(raw) = e else { return false };
        let content: Option<serde_json::Value> = raw.get_field("content").ok().flatten();
        content.and_then(|c| c.get("channel")?.get("com.beeper.message_request")?.as_bool()).unwrap_or(false)
    })
}

/// Whether a chat not marked one-to-one in `m.direct` may still be one, before its members are
/// counted. `bridge` is its bridge's word on the kind of chat (`Some(None)`: bridged, kind not
/// told); a chat no bridge made is a group once it has a name, even with two people in it.
fn one_to_one(bridge: Option<Option<String>>, named: bool) -> bool {
    match bridge {
        Some(Some(kind)) => kind == "dm",
        // Older bridges name a one-to-one chat after the person and do not say what it is.
        Some(None) => true,
        None => !named,
    }
}

/// The kind of chat as its bridge tells it (`com.beeper.room_type.v2`, else the older
/// `com.beeper.room_type`, which says "dm" for groups of people too); `None` when no bridge made it.
async fn bridge_kind(room: &Room) -> Option<Option<String>> {
    let events = room.get_state_events(StateEventType::from("m.bridge")).await.ok()?;
    let contents: Vec<serde_json::Value> = events
        .iter()
        .filter_map(|e| match e {
            RawAnySyncOrStrippedState::Sync(raw) => raw.get_field("content").ok().flatten(),
            // An invitation holds the state stripped.
            RawAnySyncOrStrippedState::Stripped(raw) => raw.get_field("content").ok().flatten(),
        })
        .collect();
    let kind = |key: &str| contents.iter().find_map(|c| c.get(key)?.as_str().map(ToOwned::to_owned));
    (!contents.is_empty()).then(|| kind("com.beeper.room_type.v2").or_else(|| kind("com.beeper.room_type")))
}

/// The bridge's protocol id for the chat (`signal`, `whatsapp`), from `m.bridge`.
async fn protocol_id(room: &Room) -> Option<String> {
    let events = room.get_state_events(StateEventType::from("m.bridge")).await.ok()?;
    events.iter().find_map(|e| {
        let RawAnySyncOrStrippedState::Sync(raw) = e else { return None };
        let content: serde_json::Value = raw.get_field("content").ok()??;
        content.pointer("/protocol/id")?.as_str().map(str::to_owned)
    })
}

/// The bridge's name for the network, the name of the chat's parent (a Discord server, a Slack
/// workspace, a Telegram forum, a WhatsApp community) when `m.bridge` has one, and the forum's id
/// when the chat is a Telegram forum topic.
async fn bridge_names(room: &Room) -> (Option<String>, Option<String>, Option<String>) {
    #[derive(Deserialize)]
    struct Section {
        id: Option<String>,
        displayname: Option<String>,
    }
    let Ok(events) = room.get_state_events(StateEventType::from("m.bridge")).await else { return (None, None, None) };
    let found = events.iter().find_map(|e| {
        let RawAnySyncOrStrippedState::Sync(raw) = e else { return None };
        let content: serde_json::Value = raw.get_field("content").ok()??;
        let section = |key: &str| content.get(key).and_then(|v| serde_json::from_value::<Section>(v.clone()).ok());
        let protocol = section("protocol")?;
        let network = section("network");
        let forum = forum::forum_id(protocol.id.as_deref(), network.as_ref().and_then(|n| n.id.as_deref()));
        let parent = network.and_then(|n| n.displayname).filter(|n| !n.trim().is_empty());
        Some((protocol.displayname.or(protocol.id), parent, forum))
    });
    found.unwrap_or((None, None, None))
}

/// Room avatar, or for a one-to-one chat the other person's, as a small thumbnail.
async fn avatar(client: &Client, room_id: &RoomId) -> Result<Option<Vec<u8>>> {
    let Some(url) = avatar_url(client, room_id).await? else { return Ok(None) };
    Ok(Some(picture(client, url).await?))
}

async fn avatar_url(client: &Client, room_id: &RoomId) -> Result<Option<matrix_sdk::ruma::OwnedMxcUri>> {
    let Some(room) = client.get_room(room_id) else { return Ok(None) };
    let own = if is_note(&room).await { client.account().get_avatar_url().await.ok().flatten() } else { None };
    let url = match own.or_else(|| room.avatar_url()) {
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
    Ok(Some(url))
}

/// A member's picture in a chat; bridges set it per chat, so the member event is asked, not the
/// profile.
async fn face(client: &Client, room_id: &RoomId, user: &UserId) -> Result<Option<Vec<u8>>> {
    let Some(url) = face_url(client, room_id, user).await? else { return Ok(None) };
    Ok(Some(picture(client, url).await?))
}

async fn face_url(client: &Client, room_id: &RoomId, user: &UserId) -> Result<Option<matrix_sdk::ruma::OwnedMxcUri>> {
    let Some(room) = client.get_room(room_id) else { return Ok(None) };
    Ok(room.get_member_no_sync(user).await?.and_then(|m| m.avatar_url().map(ToOwned::to_owned)))
}

/// A small square thumbnail of an avatar, from the media cache when it is there.
async fn picture(client: &Client, url: matrix_sdk::ruma::OwnedMxcUri) -> Result<Vec<u8>> {
    let format = MediaFormat::Thumbnail(MediaThumbnailSettings::with_method(Method::Crop, uint!(96), uint!(96)));
    let request = MediaRequestParameters { source: MediaSource::Plain(url), format };
    let bytes = client.media().get_media_content(&request, true).await?;
    Ok(tokio::task::spawn_blocking(move || square(bytes)).await?)
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
                looped: false,
                once: false,
                waveform: Vec::new(),
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
                looped: false,
                once: false,
                waveform: Vec::new(),
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
                // Bridges may fill in only the MSC1767 block.
                duration: info.and_then(|i| i.duration).or(c.audio.as_ref().map(|a| a.duration)),
                source: c.source.clone(),
                thumb: None,
                looped: false,
                once: false,
                waveform: c.audio.as_ref().map(|a| voice::bars(a.waveform.iter().map(|v| v.get().into()))).unwrap_or_default(),
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
                looped: false,
                once: false,
                waveform: Vec::new(),
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

mod location;
mod contact;
pub(crate) mod edits;
pub(crate) mod stickers;
mod gifs;
mod linkprev;
pub(crate) mod update;
pub(crate) use gifs::{Found as GifPage, Gif, enabled as gifs_enabled, wants_gif_file};
pub(crate) mod caption;
mod strip;
pub(crate) mod mp4;
pub(crate) mod forward;
mod info;
pub(crate) mod block;
pub(crate) mod group;
mod polls;
mod rich;
pub use rich::{Block, Rich, Run};
pub use location::{Support as LocationSupport, support as location_support};
pub use polls::{Place, Poll};
pub(crate) mod pollnew;
pub use pollnew::NewPoll;
pub use info::{Details, MAX_MEMBERS, MAX_SCAN, Role, links_profiles, profile_link};
pub use info::Member;

mod timeline;
pub mod union;
mod video;
mod pins;
pub use pins::Pins;
mod disappear;
pub use disappear::{Disappearing, label as disappear_label};
mod features;
pub use features::Features;
mod search;
pub(crate) mod storage;
pub use search::{Filter, Hit, Only};
pub(crate) use search::{MAX_HITS, MIN_QUERY, save as save_search};

pub(crate) mod rooms;
pub(crate) mod forum;

mod export;
pub use export::Export;
pub(crate) mod handoff;

mod mentions;
mod signin;
pub use signin::{Methods, Way};
pub(crate) mod voice;
pub mod verify;

#[cfg(test)]
mod tests;

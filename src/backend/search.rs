//! Message search over a local full-text index (SQLite FTS5).
//!
//! Most chats are end-to-end encrypted, so the server cannot search them; this device indexes the
//! text it decrypts. Sources: every timeline event a sync delivers (decrypted by then), every
//! backward page `messages` reads, and the rows of the open conversation, which carry edits and
//! deletions as the SDK resolves them.
//!
//! At rest: next to the SDK's store in the app's private directory, owner-only, and protected as
//! much as the store. A plain store (desktop, Android from before encryption) gets a plain SQLite
//! file. With a store key the index lives in memory and is written back sealed with that key
//! (`search.enc`, see `vault`) every `SAVE_EVERY` while it changes; FTS5 needs the words in clear,
//! so no part of the file can stay plain. A kill loses at most the last `SAVE_EVERY` of indexing.
//!
//! Cost: at most `MAX_MESSAGES` rows and `MAX_BYTES` of pages in use, oldest pruned first, with
//! `MAX_TEXT` indexed per message and the database never past `HARD_BYTES` (24 MiB); 16 MB at the
//! row cap for short chat lines (the `size_at_cap` test). Sealed, the database is held in memory
//! and each save seals one copy of it in place, so at most 48 MiB for a moment; a start reads and
//! opens the file in one buffer and hands SQLite one copy, also 2x. One thread, idle while nothing
//! arrives; a write is one transaction per batch, a query one FTS lookup ordered by time.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use matrix_sdk::Room;
use matrix_sdk::deserialized_responses::TimelineEvent;
use matrix_sdk::ruma::events::room::message::{MessageType, Relation};
use matrix_sdk::ruma::events::{AnySyncMessageLikeEvent, AnySyncTimelineEvent, SyncMessageLikeEvent};
use matrix_sdk::ruma::room_version_rules::RedactionRules;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use matrix_sdk::Client;
use rusqlite::{Connection, MAIN_DB, OptionalExtension as _, params};
use tokio::sync::{mpsc, oneshot};

use super::{Diff, Kind, MediaKind, Msg, content, remove_plain_reply_fallback};
use crate::vault::Seal;

/// Messages kept; the oldest go first once this is passed by `PRUNE_SLACK`.
const MAX_MESSAGES: i64 = 50_000;
const PRUNE_SLACK: i64 = 2_500;
/// Pages in use past which the oldest messages go, whatever their count: long messages fill bytes
/// long before rows. `HARD_BYTES` stops the database growing within one batch.
const MAX_BYTES: i64 = 20 << 20;
const HARD_BYTES: i64 = 24 << 20;
/// Bytes of a message's text indexed; the rest is not searched.
const MAX_TEXT: usize = 4096;
/// Edits whose message is not indexed yet, kept until it is.
const MAX_PENDING_EDITS: i64 = 2_000;
/// Batches of changes waiting for the index thread.
const QUEUE: usize = 256;
pub(crate) const MAX_HITS: usize = 50;
/// Shorter queries match too much to be useful, and run on every keystroke.
pub(crate) const MIN_QUERY: usize = 2;
const MAX_WORDS: usize = 8;
/// Words around the match in a snippet.
const SNIPPET_WORDS: i32 = 10;
const FILE: &str = "search.sqlite3";
/// How often a sealed index is written back while it changes; it is also written when the app
/// goes to the background (`save`), so this is only the backstop for an app left in front.
const SAVE_EVERY: Duration = Duration::from_secs(30 * 60);
// Mark the match in `snippet()`; stripped from indexed text so they cannot be faked.
const OPEN: char = '\u{2}';
const CLOSE: char = '\u{3}';

/// One message found.
#[derive(Clone, Debug)]
pub struct Hit {
    pub room: OwnedRoomId,
    pub id: String,
    /// The sender's display name in that room.
    pub sender: String,
    /// Sent by the user.
    pub own: bool,
    pub ts: u64,
    /// A few words around the match.
    pub snippet: String,
    /// Byte ranges of `snippet` that matched.
    pub marks: Vec<Range<usize>>,
}

#[derive(Debug, PartialEq)]
enum Change {
    /// A message; `replace` when its text is authoritative (the open timeline, edits applied).
    Add { event: String, room: String, sender: String, ts: i64, body: String, replace: bool },
    Edit { target: String, sender: String, ts: i64, body: String },
    Remove(String),
}

enum Op {
    Write(Vec<Change>),
    Query { room: Option<String>, query: String, reply: oneshot::Sender<Vec<Found>> },
    Save,
}

struct Found {
    room: String,
    event: String,
    sender: String,
    ts: i64,
    snippet: String,
}

static INDEX: Mutex<Option<mpsc::Sender<Op>>> = Mutex::new(None);
/// Moved on by `wipe`, under this lock, so a sealed index is never written back for an account that
/// signed out meanwhile.
static GENERATION: Mutex<u64> = Mutex::new(0);

fn sender() -> Option<mpsc::Sender<Op>> {
    INDEX.lock().unwrap().clone()
}

async fn write(changes: Vec<Change>) {
    if changes.is_empty() {
        return;
    }
    if let Some(tx) = sender() {
        let _ = tx.send(Op::Write(changes)).await;
    }
}

/// Starts the index under `dir` and follows `client`'s syncs. The index opens on its own thread
/// (a sealed one is read and decrypted whole), so startup does not wait for it; writes queue up.
pub(super) fn start(dir: &Path, client: &Client, seal: &Seal) {
    let (tx, rx) = mpsc::channel(QUEUE);
    let sealed = matches!(seal, Seal::Sealed(_));
    let (dir, seal) = (dir.to_owned(), seal.clone());
    let generation = *GENERATION.lock().unwrap();
    let run = move || {
        // Held while opening, so a sign-out cannot delete the files under it, nor a plain index be
        // made again for an account that has just signed out.
        let current = GENERATION.lock().unwrap();
        if *current != generation {
            return;
        }
        let opened = match &seal {
            Seal::Plain => open(&dir.join(FILE)).map(|conn| (conn, None)),
            Seal::Sealed(_) => open_sealed(&dir, &seal).map(|conn| (conn, Some(Saver { dir: dir.clone(), seal: seal.clone(), generation, dirty: false }))),
        };
        drop(current);
        match opened {
            Ok((conn, saver)) => serve(conn, rx, saver),
            Err(e) => log::warn!("search index: {e:#}"),
        }
    };
    if let Err(e) = std::thread::Builder::new().name("search".into()).spawn(run) {
        log::warn!("search thread: {e}");
        return;
    }
    if sealed {
        // Weak, so the ticks stop with the index instead of keeping it open.
        let weak = tx.downgrade();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(SAVE_EVERY);
            tick.tick().await;
            loop {
                tick.tick().await;
                let Some(tx) = weak.upgrade() else { break };
                if tx.send(Op::Save).await.is_err() {
                    break;
                }
            }
        });
    }
    *INDEX.lock().unwrap() = Some(tx);
    client.add_event_handler(|event: Raw<AnySyncTimelineEvent>, room: Room| async move {
        let rules = room.clone_info().room_version_rules_or_default().redaction;
        write(change(&event, room.room_id(), &rules).into_iter().collect()).await;
    });
}

/// Writes a sealed index back if it changed. Never waits: a full queue leaves it to the timer.
pub(crate) fn save() {
    if let Some(tx) = sender() {
        let _ = tx.try_send(Op::Save);
    }
}

/// Closes the index and deletes it: signed out, or a store left by another account.
pub(super) fn wipe(dir: &Path) {
    // Dropping the sender ends the thread once its queue is empty; the files can go before it closes.
    INDEX.lock().unwrap().take();
    let mut generation = GENERATION.lock().unwrap();
    *generation += 1;
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(PathBuf::from(format!("{}{suffix}", dir.join(FILE).display())));
    }
    crate::vault::remove(dir, FILE);
}

/// Indexes a page of events read from the server.
pub(super) async fn page(room: &Room, events: &[TimelineEvent]) {
    let rules = room.clone_info().room_version_rules_or_default().redaction;
    write(events.iter().filter_map(|e| change(e.raw(), room.room_id(), &rules)).collect()).await;
}

/// Indexes the open conversation's rows as they change.
pub(super) async fn rows(room: &RoomId, diffs: &[Diff]) {
    let mut changes = Vec::new();
    for diff in diffs {
        use matrix_sdk_ui::eyeball_im::VectorDiff::*;
        match diff {
            Append { values } | Reset { values } => changes.extend(values.iter().filter_map(|m| row(room, m))),
            PushFront { value } | PushBack { value } | Insert { value, .. } | Set { value, .. } => changes.extend(row(room, value)),
            _ => {}
        }
    }
    write(changes).await;
}

fn row(room: &RoomId, m: &Msg) -> Option<Change> {
    if m.kind != Kind::Message || !m.id.starts_with('$') {
        return None;
    }
    if m.deleted {
        return Some(Change::Remove(m.id.clone()));
    }
    if crate::backend::undecrypted(&m.body) {
        return None;
    }
    let file = m.media.as_ref().filter(|media| media.kind == MediaKind::File).map(|media| media.name.as_str());
    let body = text(&m.body, file);
    (!body.is_empty()).then(|| Change::Add {
        event: m.id.clone(),
        room: room.to_string(),
        sender: m.from.to_string(),
        ts: m.ts as i64,
        body,
        replace: true,
    })
}

/// What one decrypted event does to the index.
fn change(raw: &Raw<AnySyncTimelineEvent>, room: &RoomId, rules: &RedactionRules) -> Option<Change> {
    let AnySyncTimelineEvent::MessageLike(event) = raw.deserialize().ok()? else { return None };
    match event {
        AnySyncMessageLikeEvent::RoomMessage(SyncMessageLikeEvent::Original(e)) => {
            let (msgtype, reply) = match &e.content.relates_to {
                Some(Relation::Replacement(r)) => {
                    return Some(Change::Edit {
                        target: r.event_id.to_string(),
                        sender: e.sender.to_string(),
                        ts: i64::from(e.origin_server_ts.0),
                        body: body(&r.new_content.msgtype, false),
                    });
                }
                relation => (&e.content.msgtype, matches!(relation, Some(Relation::Reply(_)))),
            };
            let body = body(msgtype, reply);
            (!body.is_empty()).then(|| Change::Add {
                event: e.event_id.to_string(),
                room: room.to_string(),
                sender: e.sender.to_string(),
                ts: i64::from(e.origin_server_ts.0),
                body,
                replace: false,
            })
        }
        AnySyncMessageLikeEvent::RoomMessage(SyncMessageLikeEvent::Redacted(e)) => Some(Change::Remove(e.event_id.to_string())),
        AnySyncMessageLikeEvent::RoomRedaction(r) => Some(Change::Remove(r.redacts(rules)?.to_string())),
        _ => None,
    }
}

fn body(msgtype: &MessageType, reply: bool) -> String {
    let (media, body) = content(msgtype);
    // Older clients and bridges prepend the quoted message to a reply.
    let body = if reply { remove_plain_reply_fallback(&body).to_owned() } else { body };
    let file = media.as_ref().filter(|m| m.kind == MediaKind::File).map(|m| m.name.as_str());
    text(&body, file)
}

/// A file is found by its name as well as its caption.
fn text(body: &str, file: Option<&str>) -> String {
    let joined = match file {
        Some(name) if !body.contains(name) => format!("{body} {name}"),
        _ => body.to_owned(),
    };
    let clean = joined.replace([OPEN, CLOSE], " ");
    clean[..clean.floor_char_boundary(MAX_TEXT)].trim().to_owned()
}

/// Messages matching `query`, newest first, in `room` or in every room. Empty for short queries.
pub(super) async fn search(client: &Client, room: Option<&RoomId>, query: &str) -> Vec<Hit> {
    let Some(tx) = sender() else { return Vec::new() };
    let Some(query) = fts_query(query) else { return Vec::new() };
    let (reply, found) = oneshot::channel();
    if tx.send(Op::Query { room: room.map(ToString::to_string), query, reply }).await.is_err() {
        return Vec::new();
    }
    let Ok(found) = found.await else { return Vec::new() };
    let mut hits = Vec::with_capacity(found.len());
    for f in found {
        // A chat left since is not offered.
        let Some(room) = RoomId::parse(&f.room).ok().and_then(|id| client.get_room(&id)) else { continue };
        let own = client.user_id().is_some_and(|u| u.as_str() == f.sender);
        let sender = match matrix_sdk::ruma::UserId::parse(&f.sender) {
            Ok(user) => super::display_name(&room, &user).await,
            Err(_) => f.sender,
        };
        let (snippet, marks) = marked(&f.snippet);
        hits.push(Hit { room: room.room_id().to_owned(), id: f.event, sender, own, ts: f.ts.max(0) as u64, snippet, marks });
    }
    hits
}

/// Every word typed must start a word of the message, in any order.
fn fts_query(input: &str) -> Option<String> {
    if input.trim().chars().count() < MIN_QUERY {
        return None;
    }
    let words: Vec<String> = input
        .split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .take(MAX_WORDS)
        .map(|w| format!("\"{}\"*", w.replace('"', "")))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// The snippet on one line without its markers, and where they were.
fn marked(snippet: &str) -> (String, Vec<Range<usize>>) {
    let (mut out, mut marks, mut start) = (String::with_capacity(snippet.len()), Vec::new(), None);
    for c in snippet.chars() {
        match c {
            OPEN => start = Some(out.len()),
            CLOSE => marks.extend(start.take().map(|s| s..out.len())),
            c if c.is_whitespace() => out.push(' '),
            c => out.push(c),
        }
    }
    (out, marks)
}

fn open(path: &Path) -> anyhow::Result<Connection> {
    // Created owner-only before SQLite opens it; its WAL and shared-memory files take the same mode.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    let conn = Connection::open(path)?;
    schema(&conn)?;
    Ok(conn)
}

/// The index in memory, from its sealed copy when there is one. A copy that does not open is a
/// cache lost, not the account's data: the index starts again.
fn open_sealed(dir: &Path, seal: &Seal) -> anyhow::Result<Connection> {
    let mut conn = memory()?;
    // Larger than the database may grow: not this app's index, and too big to hold.
    let too_big = std::fs::metadata(seal.path(dir, FILE)).is_ok_and(|m| m.len() > HARD_BYTES as u64 + 64);
    match if too_big { Err(std::io::Error::other("too large")) } else { seal.read(dir, FILE) } {
        Ok(bytes) => {
            if let Err(e) = conn.deserialize_read_exact(MAIN_DB, &bytes[..], bytes.len(), false) {
                log::warn!("search index: {e}, starting again");
                conn = memory()?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("search index: {e}, starting again"),
    }
    schema(&conn)?;
    Ok(conn)
}

/// An empty database in memory. memdb rather than ":memory:", so `serialize` lends its pages
/// instead of copying them; the name has no leading '/', so no other connection shares it.
fn memory() -> rusqlite::Result<Connection> {
    Connection::open("file:search?vfs=memdb")
}

/// Writes a sealed index back.
struct Saver {
    dir: PathBuf,
    seal: Seal,
    generation: u64,
    dirty: bool,
}

impl Saver {
    fn save(&mut self, conn: &Connection) {
        if !self.dirty {
            return;
        }
        let generation = GENERATION.lock().unwrap();
        if *generation != self.generation {
            return;
        }
        let saved = conn.serialize(MAIN_DB).map_err(std::io::Error::other).and_then(|data| self.seal.write(&self.dir, FILE, &data));
        drop(generation);
        match saved {
            Ok(()) => self.dirty = false,
            Err(e) => log::warn!("search index: save: {e}"),
        }
    }
}

fn schema(conn: &Connection) -> rusqlite::Result<()> {
    let page: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    let _: i64 = conn.query_row(&format!("PRAGMA max_page_count = {}", HARD_BYTES / page), [], |r| r.get(0))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA secure_delete = ON;
         PRAGMA temp_store = MEMORY;
         CREATE TABLE IF NOT EXISTS msg (
             id INTEGER PRIMARY KEY,
             event TEXT NOT NULL UNIQUE,
             room TEXT NOT NULL,
             sender TEXT NOT NULL,
             ts INTEGER NOT NULL,
             edit_ts INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS msg_ts ON msg (ts);
         CREATE VIRTUAL TABLE IF NOT EXISTS body USING fts5 (text, tokenize = 'unicode61 remove_diacritics 2');
         INSERT INTO body (body, rank) VALUES ('secure-delete', 1);
         CREATE TABLE IF NOT EXISTS pending (
             target TEXT PRIMARY KEY,
             sender TEXT NOT NULL,
             text TEXT NOT NULL,
             ts INTEGER NOT NULL
         );",
    )
}

fn serve(conn: Connection, mut rx: mpsc::Receiver<Op>, mut saver: Option<Saver>) {
    let mut count: i64 = conn.query_row("SELECT count(*) FROM msg", [], |r| r.get(0)).unwrap_or(0);
    while let Some(op) = rx.blocking_recv() {
        let result = match op {
            Op::Write(changes) => {
                // A batch that changes nothing (an open chat sends its rows again on every receipt)
                // leaves the sealed copy as it is.
                let before = conn.total_changes();
                let applied = apply(&conn, &changes, &mut count);
                if let Some(saver) = &mut saver
                    && conn.total_changes() != before
                {
                    saver.dirty = true;
                }
                applied
            }
            Op::Query { room, query, reply } => query_hits(&conn, room.as_deref(), &query).map(|found| {
                let _ = reply.send(found);
            }),
            Op::Save => {
                if let Some(saver) = &mut saver {
                    saver.save(&conn);
                }
                Ok(())
            }
        };
        if let Err(e) = result {
            log::warn!("search index: {e}");
        }
    }
}

fn apply(conn: &Connection, changes: &[Change], count: &mut i64) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
    // Before as well as after: a batch must find room under `HARD_BYTES`.
    prune_bytes(&tx, count, MAX_BYTES)?;
    for change in changes {
        match change {
            Change::Add { event, room, sender, ts, body, replace } => {
                let added = tx.execute(
                    "INSERT INTO msg (event, room, sender, ts) VALUES (?1, ?2, ?3, ?4) ON CONFLICT (event) DO NOTHING",
                    params![event, room, sender, ts],
                )? == 1;
                let id: i64 = tx.query_row("SELECT id FROM msg WHERE event = ?1", [event], |r| r.get(0))?;
                if added {
                    *count += 1;
                    let edit: Option<(String, i64)> = tx
                        .query_row("SELECT text, ts FROM pending WHERE target = ?1 AND sender = ?2", [event, sender], |r| Ok((r.get(0)?, r.get(1)?)))
                        .optional()?;
                    let text = edit.as_ref().map_or(body, |(text, _)| text);
                    tx.execute("INSERT INTO body (rowid, text) VALUES (?1, ?2)", params![id, text])?;
                    if let Some((_, edit_ts)) = edit {
                        tx.execute("UPDATE msg SET edit_ts = ?2 WHERE id = ?1", params![id, edit_ts])?;
                        tx.execute("DELETE FROM pending WHERE target = ?1", [event])?;
                    }
                } else if *replace {
                    tx.execute("UPDATE body SET text = ?2 WHERE rowid = ?1 AND text IS NOT ?2", params![id, body])?;
                }
            }
            Change::Edit { target, sender, ts, body } => {
                let id: Option<i64> = tx
                    .query_row("SELECT id FROM msg WHERE event = ?1 AND sender = ?2 AND edit_ts < ?3", params![target, sender, ts], |r| r.get(0))
                    .optional()?;
                match id {
                    Some(id) => {
                        tx.execute("UPDATE body SET text = ?2 WHERE rowid = ?1", params![id, body])?;
                        tx.execute("UPDATE msg SET edit_ts = ?2 WHERE id = ?1", params![id, ts])?;
                    }
                    None => {
                        let known: bool = tx.query_row("SELECT EXISTS (SELECT 1 FROM msg WHERE event = ?1)", [target], |r| r.get(0))?;
                        if !known {
                            tx.execute(
                                "INSERT INTO pending (target, sender, text, ts) VALUES (?1, ?2, ?3, ?4)
                                 ON CONFLICT (target) DO UPDATE SET sender = ?2, text = ?3, ts = ?4 WHERE excluded.ts > pending.ts",
                                params![target, sender, body, ts],
                            )?;
                            tx.execute(
                                "DELETE FROM pending WHERE target NOT IN (SELECT target FROM pending ORDER BY ts DESC LIMIT ?1)",
                                [MAX_PENDING_EDITS],
                            )?;
                        }
                    }
                }
            }
            Change::Remove(event) => {
                if let Some(id) = tx.query_row("SELECT id FROM msg WHERE event = ?1", [event], |r| r.get::<_, i64>(0)).optional()? {
                    tx.execute("DELETE FROM body WHERE rowid = ?1", [id])?;
                    tx.execute("DELETE FROM msg WHERE id = ?1", [id])?;
                    *count -= 1;
                }
                tx.execute("DELETE FROM pending WHERE target = ?1", [event])?;
            }
        }
    }
    if *count > MAX_MESSAGES + PRUNE_SLACK {
        let over = *count - MAX_MESSAGES;
        tx.execute("DELETE FROM body WHERE rowid IN (SELECT id FROM msg ORDER BY ts, id LIMIT ?1)", [over])?;
        tx.execute("DELETE FROM msg WHERE id IN (SELECT id FROM msg ORDER BY ts, id LIMIT ?1)", [over])?;
        *count = MAX_MESSAGES;
    }
    prune_bytes(&tx, count, MAX_BYTES)?;
    tx.commit()
}

/// Deletes the oldest messages until the pages in use fit in `max` bytes.
fn prune_bytes(conn: &Connection, count: &mut i64, max: i64) -> rusqlite::Result<()> {
    let used = || conn.query_row("SELECT (SELECT page_count FROM pragma_page_count()) - (SELECT freelist_count FROM pragma_freelist_count()), (SELECT page_size FROM pragma_page_size())", [], |r| Ok(r.get::<_, i64>(0)? * r.get::<_, i64>(1)?));
    while *count > 0 && used()? > max {
        // A tenth at a time: pages do not free in proportion to rows, so measure again after each.
        let over = (*count / 10).max(1);
        conn.execute("DELETE FROM body WHERE rowid IN (SELECT id FROM msg ORDER BY ts, id LIMIT ?1)", [over])?;
        *count -= conn.execute("DELETE FROM msg WHERE id IN (SELECT id FROM msg ORDER BY ts, id LIMIT ?1)", [over])? as i64;
    }
    Ok(())
}

fn query_hits(conn: &Connection, room: Option<&str>, query: &str) -> rusqlite::Result<Vec<Found>> {
    let mut stmt = conn.prepare_cached(
        "SELECT m.room, m.event, m.sender, m.ts, snippet(body, 0, char(2), char(3), '…', ?3)
         FROM body JOIN msg m ON m.id = body.rowid
         WHERE body MATCH ?1 AND (?2 IS NULL OR m.room = ?2)
         ORDER BY m.ts DESC LIMIT ?4",
    )?;
    let rows = stmt.query_map(params![query, room, SNIPPET_WORDS, MAX_HITS as i64], |r| {
        Ok(Found { room: r.get(0)?, event: r.get(1)?, sender: r.get(2)?, ts: r.get(3)?, snippet: r.get(4)? })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(event: &str, room: &str, ts: i64, body: &str) -> Change {
        Change::Add { event: event.into(), room: room.into(), sender: "@a:x".into(), ts, body: body.into(), replace: false }
    }

    fn find(conn: &Connection, room: Option<&str>, q: &str) -> Vec<(String, String)> {
        query_hits(conn, room, &fts_query(q).unwrap()).unwrap().into_iter().map(|f| (f.event, f.snippet)).collect()
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("unisend-search-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn query_words_are_prefixes() {
        assert_eq!(fts_query("a"), None);
        assert_eq!(fts_query("  -- "), None);
        assert_eq!(fts_query("hel wor\"ld").as_deref(), Some("\"hel\"* \"world\"*"));
    }

    #[test]
    fn marks_point_into_the_clean_snippet() {
        let (text, marks) = marked("…za \u{2}żółw\u{3}\ni \u{2}kot\u{3}");
        assert_eq!(text, "…za żółw i kot");
        assert_eq!(marks.iter().map(|r| &text[r.clone()]).collect::<Vec<_>>(), ["żółw", "kot"]);
    }

    #[test]
    fn edits_redactions_and_rooms() {
        let dir = temp("edits");
        let conn = open(&dir.join(FILE)).unwrap();
        let mut n = 0;
        apply(&conn, &[add("$1", "!r", 1, "Lunch at noon?"), add("$2", "!s", 2, "lunch was great"), add("$3", "!r", 3, "Zażółć gęślą jaźń")], &mut n).unwrap();
        assert_eq!(n, 3);
        let hits = find(&conn, None, "lun");
        assert_eq!(hits.iter().map(|h| h.0.as_str()).collect::<Vec<_>>(), ["$2", "$1"], "newest first");
        assert!(hits[1].1.contains("\u{2}Lunch\u{3}"));
        assert_eq!(find(&conn, Some("!r"), "lunch").len(), 1);
        assert_eq!(find(&conn, None, "gesla").len(), 1, "diacritics folded");
        // An edit by someone else is ignored; the sender's own one replaces the text.
        let edit = |sender: &str, ts, body: &str| Change::Edit { target: "$1".into(), sender: sender.into(), ts, body: body.into() };
        apply(&conn, &[edit("@b:x", 5, "dinner")], &mut n).unwrap();
        assert!(find(&conn, None, "dinner").is_empty());
        apply(&conn, &[edit("@a:x", 6, "dinner at eight"), edit("@a:x", 4, "breakfast")], &mut n).unwrap();
        assert_eq!(find(&conn, None, "dinner").len(), 1);
        assert!(find(&conn, None, "breakfast").is_empty(), "an older edit read later does not win");
        assert_eq!(find(&conn, None, "lunch").len(), 1);
        // An edit read before its message waits for it.
        apply(&conn, &[Change::Edit { target: "$9".into(), sender: "@a:x".into(), ts: 9, body: "fixed text".into() }], &mut n).unwrap();
        apply(&conn, &[add("$9", "!r", 8, "fxied txet")], &mut n).unwrap();
        assert_eq!(find(&conn, None, "fixed").len(), 1);
        assert!(find(&conn, None, "fxied").is_empty());
        // The open timeline's text is authoritative; a sync's copy is not.
        apply(&conn, &[Change::Add { event: "$2".into(), room: "!s".into(), sender: "@a:x".into(), ts: 2, body: "brunch".into(), replace: true }], &mut n).unwrap();
        apply(&conn, &[add("$2", "!s", 2, "lunch was great")], &mut n).unwrap();
        assert_eq!(find(&conn, None, "brunch").len(), 1);
        apply(&conn, &[Change::Remove("$2".into()), Change::Remove("$unknown".into())], &mut n).unwrap();
        assert!(find(&conn, None, "brunch").is_empty());
        assert_eq!(n, 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sealed_index_saves_and_reopens() {
        let dir = temp("sealed");
        let seal = Seal::Sealed(std::sync::Arc::new(zeroize::Zeroizing::new([5; crate::vault::KEY_LEN])));
        std::fs::write(dir.join(crate::vault::SESSION), b"{}").unwrap();
        let conn = open_sealed(&dir, &seal).unwrap();
        let mut n = 0;
        apply(&conn, &[add("$1", "!r", 1, "the secret meeting place")], &mut n).unwrap();
        let mut saver = Saver { dir: dir.clone(), seal: seal.clone(), generation: *GENERATION.lock().unwrap(), dirty: true };
        saver.save(&conn);
        assert!(!saver.dirty);
        assert!(!dir.join(FILE).exists(), "nothing plain on disk");
        let sealed = std::fs::read(dir.join("search.enc")).unwrap();
        assert!(!sealed.windows(6).any(|w| w == b"secret"));
        let again = open_sealed(&dir, &seal).unwrap();
        assert_eq!(find(&again, None, "secret").len(), 1);
        // memdb lends its pages to a save instead of copying them, fresh or reopened.
        for conn in [&conn, &again] {
            assert!(matches!(conn.serialize(MAIN_DB).unwrap(), rusqlite::serialize::Data::Shared(_)));
        }
        // Another key reads nothing and starts over.
        let other = Seal::Sealed(std::sync::Arc::new(zeroize::Zeroizing::new([6; crate::vault::KEY_LEN])));
        assert!(find(&open_sealed(&dir, &other).unwrap(), None, "secret").is_empty());
        // Signed out meanwhile: the index is not written back.
        wipe(&dir);
        saver.dirty = true;
        saver.save(&conn);
        assert!(!dir.join("search.enc").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn prunes_oldest_first() {
        let dir = temp("prune");
        let conn = open(&dir.join(FILE)).unwrap();
        let mut n = MAX_MESSAGES + PRUNE_SLACK;
        // Pretend the index is full; the next batch tips it over and the oldest go.
        apply(&conn, &[add("$old", "!r", 1, "ancient words"), add("$new", "!r", 2, "fresh words")], &mut n).unwrap();
        assert_eq!(n, MAX_MESSAGES);
        assert_eq!(find(&conn, None, "words").len(), 0, "both were among the oldest rows");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_batch_sent_again_changes_nothing() {
        let dir = temp("again");
        let conn = open(&dir.join(FILE)).unwrap();
        let mut n = 0;
        let row = |body: &str| Change::Add { event: "$1".into(), room: "!r".into(), sender: "@a:x".into(), ts: 1, body: body.into(), replace: true };
        apply(&conn, &[row("first words")], &mut n).unwrap();
        let before = conn.total_changes();
        apply(&conn, &[row("first words")], &mut n).unwrap();
        assert_eq!(conn.total_changes(), before, "the same row again leaves the index as it was");
        apply(&conn, &[row("other words")], &mut n).unwrap();
        assert_ne!(conn.total_changes(), before);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn text_strips_markers_and_adds_file_names() {
        assert_eq!(text("a\u{2}b\u{3}", None), "a b");
        assert_eq!(text("", Some("report.pdf")), "report.pdf");
        assert_eq!(text("see report.pdf", Some("report.pdf")), "see report.pdf");
        // Cut to `MAX_TEXT` bytes, never inside a character.
        let long = "ż".repeat(MAX_TEXT);
        let cut = text(&long, None);
        assert_eq!(cut.len(), MAX_TEXT);
        assert!(cut.chars().all(|c| c == 'ż'));
        let odd = format!("a{long}");
        assert_eq!(text(&odd, None).len(), MAX_TEXT - 1);
    }

    #[test]
    fn prunes_by_bytes_oldest_first() {
        let conn = memory().unwrap();
        schema(&conn).unwrap();
        let mut n = 0;
        let words: Vec<String> = (0..600).map(|i| format!("w{i}x{}", i * 7919 % 1000)).collect();
        for batch in 0..10 {
            let changes: Vec<Change> = (0..100)
                .map(|i| {
                    let k = batch * 100 + i;
                    let body: String = (0..400).map(|j| words[(k * 31 + j * 17) % 600].as_str()).collect::<Vec<_>>().join(" ");
                    add(&format!("$e{k}"), "!r", k as i64, &format!("m{k} {body}"))
                })
                .collect();
            apply(&conn, &changes, &mut n).unwrap();
        }
        assert_eq!(n, 1000);
        let page: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
        let used = || conn.query_row("SELECT (SELECT page_count FROM pragma_page_count()) - (SELECT freelist_count FROM pragma_freelist_count())", [], |r| r.get::<_, i64>(0)).unwrap() * page;
        let full = used();
        // Deleting rows frees pages, though not in proportion: the FTS index keeps some structure.
        let tx = conn.unchecked_transaction().unwrap();
        prune_bytes(&tx, &mut n, full / 2).unwrap();
        tx.commit().unwrap();
        assert!(used() <= full / 2, "{} of {full} bytes in use", used());
        assert!(n > 0 && n < 1000, "{n} kept");
        let rows: i64 = conn.query_row("SELECT count(*) FROM msg", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, n);
        // The newest stay, the oldest went.
        assert_eq!(find(&conn, None, "m999").len(), 1);
        assert!(find(&conn, None, "m0").is_empty());
        // The database cannot outgrow its hard bound.
        let max: i64 = conn.query_row("PRAGMA max_page_count", [], |r| r.get(0)).unwrap();
        assert_eq!(max * page, HARD_BYTES);
    }

    /// Disk at the cap, with chat-like text. Run with `--ignored --nocapture`.
    #[test]
    #[ignore]
    fn size_at_cap() {
        let dir = temp("size");
        let path = dir.join(FILE);
        let conn = open(&path).unwrap();
        let words = ["ok", "see", "you", "tomorrow", "lunch", "meeting", "thanks", "photo", "great", "where", "are", "home", "late", "call", "me", "later"];
        let mut n = 0;
        let start = std::time::Instant::now();
        for batch in 0..(MAX_MESSAGES / 500) {
            let changes: Vec<Change> = (0..500)
                .map(|i| {
                    let k = batch * 500 + i;
                    let body: Vec<&str> = (0..(4 + k % 12)).map(|j| words[((k * 7 + j * 13) % 16) as usize]).collect();
                    Change::Add {
                        event: format!("$event{k:040}"),
                        room: format!("!room{:020}:beeper.local", k % 200),
                        sender: format!("@user{:010}:beeper.local", k % 50),
                        ts: 1_700_000_000_000 + k,
                        body: format!("{} {k}", body.join(" ")),
                        replace: false,
                    }
                })
                .collect();
            apply(&conn, &changes, &mut n).unwrap();
        }
        let took = start.elapsed();
        let q = std::time::Instant::now();
        let hits = find(&conn, None, "lunch mee");
        let query = q.elapsed();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        println!("{n} messages: {:.1} MB, indexed in {took:?}, query {query:?} ({} hits)", size as f64 / 1e6, hits.len());
        let _ = std::fs::remove_dir_all(dir);
    }
}

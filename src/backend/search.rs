//! Message search over a local full-text index (SQLite FTS5).
//!
//! Most chats are end-to-end encrypted, so the server cannot search them; this device indexes the
//! text it decrypts. Sources: every timeline event a sync delivers (decrypted by then), every
//! backward page `messages` reads, and the rows of the open conversation, which carry edits and
//! deletions as the SDK resolves them.
//!
//! At rest: the index sits next to the SDK's store in the app's private directory, owner-only. The
//! store itself is opened without a passphrase (`sqlite_store(.., None)`), so its event cache already
//! holds the same decrypted text in plain SQLite; the index is protected exactly as much. If the
//! store ever gets a passphrase, this file must be encrypted with it or dropped.
//!
//! Cost: at most `MAX_MESSAGES` rows, oldest pruned first; 16 MB on disk at the cap for short chat
//! lines (the `size_at_cap` test), more for long messages. One thread, idle while nothing arrives;
//! a write is one transaction per batch, a query one FTS lookup ordered by time.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use matrix_sdk::Room;
use matrix_sdk::deserialized_responses::TimelineEvent;
use matrix_sdk::ruma::events::room::message::{MessageType, Relation};
use matrix_sdk::ruma::events::{AnySyncMessageLikeEvent, AnySyncTimelineEvent, SyncMessageLikeEvent};
use matrix_sdk::ruma::room_version_rules::RedactionRules;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use matrix_sdk::Client;
use rusqlite::{Connection, OptionalExtension as _, params};
use tokio::sync::{mpsc, oneshot};

use super::{Diff, Kind, MediaKind, Msg, content, remove_plain_reply_fallback};

/// Messages kept; the oldest go first once this is passed by `PRUNE_SLACK`.
const MAX_MESSAGES: i64 = 50_000;
const PRUNE_SLACK: i64 = 2_500;
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
}

struct Found {
    room: String,
    event: String,
    sender: String,
    ts: i64,
    snippet: String,
}

static INDEX: Mutex<Option<mpsc::Sender<Op>>> = Mutex::new(None);

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

/// Opens the index under `dir` and starts following `client`'s syncs.
pub(super) fn start(dir: &Path, client: &Client) {
    let path = dir.join(FILE);
    let conn = match open(&path) {
        Ok(conn) => conn,
        Err(e) => {
            log::warn!("search index: {e:#}");
            return;
        }
    };
    let (tx, rx) = mpsc::channel(QUEUE);
    if let Err(e) = std::thread::Builder::new().name("search".into()).spawn(move || serve(conn, rx)) {
        log::warn!("search thread: {e}");
        return;
    }
    *INDEX.lock().unwrap() = Some(tx);
    client.add_event_handler(|event: Raw<AnySyncTimelineEvent>, room: Room| async move {
        let rules = room.clone_info().room_version_rules_or_default().redaction;
        write(change(&event, room.room_id(), &rules).into_iter().collect()).await;
    });
}

/// Closes the index and deletes it: signed out, or a store left by another account.
pub(super) fn wipe(dir: &Path) {
    // Dropping the sender ends the thread once its queue is empty; the files can go before it closes.
    INDEX.lock().unwrap().take();
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(PathBuf::from(format!("{}{suffix}", dir.join(FILE).display())));
    }
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
    if m.body == crate::backend::utd() {
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
    joined.replace([OPEN, CLOSE], " ").trim().to_owned()
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
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA secure_delete = ON;
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
    )?;
    Ok(conn)
}

fn serve(conn: Connection, mut rx: mpsc::Receiver<Op>) {
    let mut count: i64 = conn.query_row("SELECT count(*) FROM msg", [], |r| r.get(0)).unwrap_or(0);
    while let Some(op) = rx.blocking_recv() {
        let result = match op {
            Op::Write(changes) => apply(&conn, &changes, &mut count),
            Op::Query { room, query, reply } => query_hits(&conn, room.as_deref(), &query).map(|found| {
                let _ = reply.send(found);
            }),
        };
        if let Err(e) = result {
            log::warn!("search index: {e}");
        }
    }
}

fn apply(conn: &Connection, changes: &[Change], count: &mut i64) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;
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
    tx.commit()
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
        let dir = std::env::temp_dir().join(format!("murmur-search-{name}-{}", std::process::id()));
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
    fn text_strips_markers_and_adds_file_names() {
        assert_eq!(text("a\u{2}b\u{3}", None), "a b");
        assert_eq!(text("", Some("report.pdf")), "report.pdf");
        assert_eq!(text("see report.pdf", Some("report.pdf")), "see report.pdf");
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

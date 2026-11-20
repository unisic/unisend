//! Merged chats, as Beeper makes them: one Matrix room of type `com.beeper.union`, holding only the
//! user, whose `m.space.child` events name the chats that are one person on several networks.
//! The room carries no messages; the chat list shows it in place of its chats, and opening it
//! follows each chat's timeline and interleaves their rows by time. What is sent goes to one of
//! the chats, the one picked under the composer or the one a reply or reaction is about.
//! Beeper Desktop's renderer (`UnionsStore`, `UnionThread`) is the reference for every rule here.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use anyhow::{Context as _, Result, bail};
use chrono::{Local, NaiveDate, TimeZone as _};
use matrix_sdk::{
    Client, Room, RoomState,
    deserialized_responses::RawAnySyncOrStrippedState,
    ruma::{
        OwnedRoomId, RoomId,
        api::client::room::{Visibility, create_room},
        events::StateEventType,
        serde::Raw,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{AbortOnDrop, Diff, Ev, Kind, Msg, RoomSummary, timeline};

/// The `type` of the union room's `m.room.create`.
pub const KIND: &str = "com.beeper.union";
const CHILD: &str = "m.space.child";
/// Chats read from one union: Beeper's editor sets no cap, this one keeps someone else's state bounded.
pub const MAX_MEMBERS: usize = 16;
/// Beeper Desktop's default when the account names no `max-merge-chats`.
pub const DEFAULT_LIMIT: i64 = 1;
/// The last message being the user's own and older than this, the composer goes back to the default chat.
const OWN_QUIET_MS: u64 = 30 * 60 * 1000;
/// A change touching more merged rows than this is sent as a fresh list.
const RESET_PAST: usize = 500;
/// People named as typing in a merged chat, as in one chat.
const MAX_TYPING: usize = 8;

/// What a union room's state says.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Union {
    /// The member chats, in the order their `m.space.child` events were sent.
    pub rooms: Vec<OwnedRoomId>,
    /// When the union was made (ms): a limited account keeps its oldest unions.
    pub created: u64,
    /// The union has its own `m.room.name`; without one it takes its first chat's.
    pub named: bool,
    /// The union room's own `m.marked_unread`, kept apart from what its chats add to the row.
    pub marked: bool,
}

pub(super) fn is_union(room: &Room) -> bool {
    room.room_type().is_some_and(|t| t.as_str() == KIND)
}

/// The union a room's state describes; `None` for any other room. Store reads only.
pub(super) async fn read(room: &Room) -> Option<Union> {
    if !is_union(room) {
        return None;
    }
    let events = room.get_state_events(StateEventType::from(CHILD)).await.unwrap_or_default();
    let mut rooms: Vec<(u64, OwnedRoomId)> = events.iter().filter_map(|e| member(sync_json(e)?)).collect();
    rooms.sort();
    rooms.truncate(MAX_MEMBERS);
    let created = room
        .get_state_event(StateEventType::RoomCreate, "")
        .await
        .ok()
        .flatten()
        .and_then(|e| sync_json(&e)?.get("origin_server_ts")?.as_u64())
        .unwrap_or(u64::MAX);
    Some(Union { rooms: rooms.into_iter().map(|(_, id)| id).collect(), created, named: room.name().is_some(), marked: room.is_marked_unread() })
}

fn sync_json(e: &RawAnySyncOrStrippedState) -> Option<Value> {
    let RawAnySyncOrStrippedState::Sync(raw) = e else { return None };
    raw.deserialize_as_unchecked::<Value>().ok()
}

/// A `m.space.child` event that keeps its chat in the union: Beeper sends `{}` to take one out.
fn member(event: Value) -> Option<(u64, OwnedRoomId)> {
    let via = event.pointer("/content/via")?.as_array()?;
    if via.is_empty() {
        return None;
    }
    let id = RoomId::parse(event.get("state_key")?.as_str()?).ok()?;
    Some((event.get("origin_server_ts").and_then(Value::as_u64).unwrap_or(0), id))
}

/// The joined chats of a union that this device has.
pub(super) async fn members(client: &Client, union: &Room) -> Vec<Room> {
    let ids = read(union).await.map(|u| u.rooms).unwrap_or_default();
    ids.iter().filter_map(|id| client.get_room(id)).filter(|r| r.state() == RoomState::Joined).collect()
}

/// `via` of a child event: Beeper writes its own server, `beeper.com`; the user's server is that
/// for a Beeper account and the right one on any other.
fn via(client: &Client) -> Result<Value> {
    Ok(json!({ "via": [client.user_id().context("Not signed in")?.server_name().as_str()] }))
}

/// Makes a union of `rooms`, or adds them to the union among them, as Beeper's `createUnion` and
/// `updateUnion` do. Returns the union's ID once its state is on this device (or after 5 s).
pub(super) async fn merge(client: &Client, rooms: Vec<OwnedRoomId>, name: String) -> Result<OwnedRoomId> {
    let via = via(client)?;
    if let Some(union) = rooms.iter().filter_map(|id| client.get_room(id)).find(is_union) {
        for id in rooms.iter().filter(|id| *id != union.room_id()) {
            union.send_state_event_raw(CHILD, id.as_str(), via.clone()).await?;
        }
        return Ok(union.room_id().to_owned());
    }
    if rooms.len() < 2 {
        bail!("two chats are needed");
    }
    let mut state = Vec::new();
    if !name.trim().is_empty() {
        state.push(json!({ "type": "m.room.name", "state_key": "", "content": { "name": name.trim() } }));
    }
    // Beeper's editor takes the first chat's picture that has one; the union reuses its mxc.
    if let Some(url) = picture(client, &rooms).await {
        state.push(json!({ "type": "m.room.avatar", "state_key": "", "content": { "url": url } }));
    }
    state.extend(rooms.iter().take(MAX_MEMBERS).map(|id| json!({ "type": CHILD, "state_key": id, "content": via })));
    let mut request = create_room::v3::Request::new();
    request.creation_content = Some(Raw::new(&json!({ "type": KIND, KIND: true }))?.cast_unchecked());
    request.initial_state = state.iter().map(|e| Raw::new(e).map(Raw::cast_unchecked)).collect::<Result<_, _>>()?;
    request.preset = Some(create_room::v3::RoomPreset::PrivateChat);
    request.visibility = Visibility::Private;
    let room = client.create_room(request).await?;
    // The list and the timeline read the members from state, which a later sync brings.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while read(&room).await.is_none_or(|u| u.rooms.is_empty()) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Ok(room.room_id().to_owned())
}

async fn picture(client: &Client, rooms: &[OwnedRoomId]) -> Option<String> {
    for room in rooms.iter().filter_map(|id| client.get_room(id)) {
        if let Some(url) = room.avatar_url() {
            return Some(url.to_string());
        }
        if let Some(peer) = super::other_person(&room).await
            && let Ok(Some(member)) = room.get_member_no_sync(&peer).await
            && let Some(url) = member.avatar_url()
        {
            return Some(url.to_string());
        }
    }
    None
}

/// A chat the user left goes out of its unions; a union left with no chat is left too (Beeper
/// does both when a chat is deleted).
pub(super) async fn forget_member(client: &Client, left: &RoomId) {
    for union in client.joined_rooms().into_iter().filter(is_union) {
        if let Err(e) = take_out(&union, left).await {
            log::warn!("merged chat cleanup: {e:#}");
        }
    }
}

/// Takes `room` out of `union` with an empty `m.space.child`; a union left with no chat is left
/// and forgotten, as Beeper's `leaveUnionSpace` does. No-op when `room` is not in it.
pub(super) async fn take_out(union: &Room, room: &RoomId) -> Result<()> {
    let Some(u) = read(union).await.filter(|u| u.rooms.iter().any(|r| r == room)) else { return Ok(()) };
    if u.rooms.len() > 1 {
        union.send_state_event_raw(CHILD, room.as_str(), json!({})).await?;
    } else {
        union.leave().await?;
        union.forget().await?;
    }
    Ok(())
}

/// The limit on active unions in Beeper's account API (`whoami` `userInfo.features`, the `*` set
/// overlaid by `appType:desktop`, key `max-merge-chats`): a whole number from -1 (no limit) up,
/// else the default of 1.
pub fn limit(whoami: &Value) -> i64 {
    let features = whoami.pointer("/userInfo/features");
    let pick = |set: &str| features.and_then(|f| f.get(set)).and_then(|s| s.get("max-merge-chats")).cloned();
    let value = pick("appType:desktop").or_else(|| pick("*"));
    let n = match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    match n {
        Some(n) if n.fract() == 0.0 && n >= -1.0 && n <= i64::MAX as f64 => n as i64,
        _ => DEFAULT_LIMIT,
    }
}

/// The list as Beeper shows it: which rows stand in for others, and which are left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Folded {
    /// Rows not listed: the chats of a shown union, and unions over the limit or with no chat here.
    pub hidden: HashSet<OwnedRoomId>,
    /// Each shown union's chats that this device has, in the union's order.
    pub members: HashMap<OwnedRoomId, Vec<OwnedRoomId>>,
    /// The shown union a chat belongs to.
    pub union_of: HashMap<OwnedRoomId, OwnedRoomId>,
}

impl Folded {
    pub fn is_union(&self, id: &RoomId) -> bool {
        self.members.contains_key(id)
    }
}

/// Folds each shown union's chats into its row: newest message, summed unread counts, archived
/// only when every chat is (Beeper's `enrichUnionThreads`). `limit` is `max-merge-chats`, `None`
/// until the account API answered: then every union shows. Idempotent: a row it changed is
/// changed the same way again.
pub fn fold(rooms: &mut [RoomSummary], limit: Option<i64>) -> Folded {
    let index: HashMap<OwnedRoomId, usize> = rooms.iter().enumerate().map(|(ix, r)| (r.id.clone(), ix)).collect();
    let mut unions: Vec<(u64, usize)> = rooms.iter().enumerate().filter_map(|(ix, r)| Some((r.union.as_ref()?.created, ix))).collect();
    unions.sort_by_key(|&(created, ix)| (created, rooms[ix].id.clone()));
    let keep = match limit {
        Some(n) if n >= 0 => usize::try_from(n).unwrap_or(usize::MAX),
        _ => usize::MAX,
    };
    let mut out = Folded::default();
    for (rank, &(_, ix)) in unions.iter().enumerate() {
        let id = rooms[ix].id.clone();
        let wanted = rooms[ix].union.as_ref().map(|u| u.rooms.clone()).unwrap_or_default();
        let found: Vec<usize> = wanted
            .iter()
            .filter_map(|m| index.get(m).copied())
            .filter(|&m| rooms[m].pending.is_none() && rooms[m].union.is_none() && !out.union_of.contains_key(&rooms[m].id))
            .collect();
        if rank >= keep || found.is_empty() {
            out.hidden.insert(id);
            continue;
        }
        let newest = found.iter().copied().max_by_key(|&m| (rooms[m].ts, rooms[m].order)).unwrap_or(found[0]);
        let first_name = rooms[found[0]].name.clone();
        let (newest, all) = (rooms[newest].clone(), found.iter().map(|&m| rooms[m].clone()).collect::<Vec<_>>());
        let u = &mut rooms[ix];
        let Some(union) = u.union.clone() else { continue };
        if !union.named {
            u.name = first_name;
        }
        (u.preview, u.preview_id, u.own_last, u.read_by) = (newest.preview, newest.preview_id, newest.own_last, newest.read_by);
        u.ts = all.iter().map(|m| m.ts).max().unwrap_or(0);
        u.order = all.iter().map(|m| m.order).max().unwrap_or(0);
        u.unread = all.iter().map(|m| m.unread).sum();
        u.mentions = all.iter().map(|m| m.mentions).sum();
        u.marked_unread = union.marked || all.iter().any(|m| m.marked_unread);
        u.archived = all.iter().all(|m| m.archived);
        u.dm = all.iter().all(|m| m.dm);
        u.dated = all.iter().any(|m| m.dated);
        (u.network, u.parent, u.forum, u.note, u.moved_to) = (None, None, None, false, None);
        for m in &all {
            out.hidden.insert(m.id.clone());
            out.union_of.insert(m.id.clone(), id.clone());
        }
        out.members.insert(id, all.into_iter().map(|m| m.id).collect());
    }
    out
}

/// May join a union: a one-to-one chat, not a note to self, not waiting to be taken, not a union
/// and in none (Beeper's `isMergeable`, `type === "single"`).
pub fn mergeable(r: &RoomSummary, folded: &Folded) -> bool {
    r.dm && !r.note && r.pending.is_none() && r.union.is_none() && r.moved_to.is_none() && !folded.hidden.contains(&r.id)
}

/// What says two chats are one person, as Beeper keys them: `tel:` digits, `name:` a full name of
/// two words or more. Beeper reads the person's phone, email and name; this list has only the
/// chat's name, which is the person's, or their number where the network knows no name.
pub fn keys(name: &str) -> Vec<String> {
    let name = name.trim();
    let digits = name.chars().filter(char::is_ascii_digit).count();
    if digits >= 5 && name.chars().all(|c| c.is_ascii_digit() || " +-().".contains(c)) {
        return vec![format!("tel:{}", name.chars().filter(|c| c.is_ascii_digit() || *c == '+').collect::<String>())];
    }
    let words: Vec<&str> = name.split_whitespace().collect();
    if words.len() >= 2 {
        return vec![format!("name:{}", words.join(" ").to_lowercase())];
    }
    Vec::new()
}

/// Chats that look like the same person as `of`, newest first (Beeper's `getSuggestedMembersFor`).
pub fn suggested<'a>(rooms: &'a [RoomSummary], folded: &Folded, of: &[&RoomSummary]) -> Vec<&'a RoomSummary> {
    let wanted: HashSet<String> = of.iter().flat_map(|r| keys(&r.name)).collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<&RoomSummary> =
        rooms.iter().filter(|r| !of.iter().any(|o| o.id == r.id) && mergeable(r, folded) && keys(&r.name).iter().any(|k| wanted.contains(k))).collect();
    found.sort_by_key(|r| std::cmp::Reverse(r.ts));
    found
}

/// The chat the composer sends to when the user has not picked one (Beeper's `seedComposeTarget`):
/// the one with the newest message, unless that is the user's own and half an hour old, then the
/// first chat.
pub fn seed(members: &[&RoomSummary], now: u64) -> Option<OwnedRoomId> {
    let first = members.first()?;
    let newest = members.iter().filter(|m| m.ts > 0).max_by_key(|m| m.ts);
    match newest {
        Some(m) if !(m.own_last && m.ts + OWN_QUIET_MS <= now) => Some(m.id.clone()),
        _ => Some(first.id.clone()),
    }
}

/// Where a merged row comes from: a chat's row, or the day line before the first row of a day.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Key {
    Row(usize, String),
    Day(NaiveDate),
}

/// The open union's chats as their timelines have them, and the merged rows the UI was sent.
pub(super) struct Merge {
    lanes: Vec<(OwnedRoomId, Vec<Msg>)>,
    shown: Vec<Key>,
}

impl Merge {
    pub fn new(lanes: Vec<OwnedRoomId>) -> Self {
        Self { lanes: lanes.into_iter().map(|id| (id, Vec::new())).collect(), shown: Vec::new() }
    }

    /// The chat a row (event or transaction ID) is in.
    pub fn lane_of(&self, id: &str) -> Option<OwnedRoomId> {
        self.lanes.iter().find(|(_, rows)| rows.iter().any(|m| m.id == id)).map(|(room, _)| room.clone())
    }

    /// Takes one chat's changes and returns the merged rows' changes.
    pub fn take(&mut self, lane: &RoomId, diffs: Vec<Diff>) -> Vec<Diff> {
        let Some(l) = self.lanes.iter().position(|(id, _)| id == lane) else { return Vec::new() };
        let mut touched = HashSet::new();
        for diff in diffs {
            match &diff {
                Diff::Set { value, .. } | Diff::Insert { value, .. } | Diff::PushBack { value } | Diff::PushFront { value } => {
                    touched.insert(value.id.clone());
                }
                Diff::Append { values } | Diff::Reset { values } => touched.extend(values.iter().map(|m| m.id.clone())),
                _ => {}
            }
            super::apply(&mut self.lanes[l].1, diff);
        }
        let order = self.order();
        let changed = |key: &Key| matches!(key, Key::Row(at, id) if *at == l && touched.contains(id));
        let out = self.changes(&order, changed);
        self.shown = order.into_iter().map(|(key, _)| key).collect();
        out
    }

    /// The merged rows: each chat's rows in its own order, interleaved by time. A row with no time
    /// (the unread line) rides with the next row of its chat that has one. Only the first unread
    /// line stays, and one day line heads each day.
    fn order(&self) -> Vec<(Key, (usize, usize))> {
        let times: Vec<Vec<u64>> = self
            .lanes
            .iter()
            .map(|(_, rows)| {
                let mut next = 0;
                let mut t: Vec<u64> = rows.iter().rev().map(|m| {
                    if m.ts > 0 {
                        next = m.ts;
                    }
                    next
                }).collect();
                t.reverse();
                t
            })
            .collect();
        let mut heads = vec![0; self.lanes.len()];
        let mut clock = vec![0u64; self.lanes.len()];
        let mut out = Vec::with_capacity(self.lanes.iter().map(|(_, rows)| rows.len()).sum());
        let (mut day, mut unread) = (None, false);
        loop {
            let mut best: Option<(u64, usize)> = None;
            for (l, (_, rows)) in self.lanes.iter().enumerate() {
                if heads[l] < rows.len() {
                    let at = clock[l].max(times[l][heads[l]]);
                    if best.is_none_or(|(b, _)| at < b) {
                        best = Some((at, l));
                    }
                }
            }
            let Some((at, l)) = best else { break };
            let i = heads[l];
            (clock[l], heads[l]) = (at, i + 1);
            let row = &self.lanes[l].1[i];
            match row.kind {
                Kind::Day => continue,
                Kind::Unread if std::mem::replace(&mut unread, true) => continue,
                Kind::Message | Kind::Notice | Kind::Joined | Kind::Invited { .. } if row.ts > 0 => {
                    let date = local_day(row.ts);
                    if date.is_some() && date != day {
                        day = date;
                        out.extend(date.map(|d| (Key::Day(d), (l, i))));
                    }
                }
                _ => {}
            }
            out.push((Key::Row(l, row.id.clone()), (l, i)));
        }
        out
    }

    fn value(&self, key: &Key, (l, i): (usize, usize)) -> Msg {
        let (room, rows) = &self.lanes[l];
        let row = &rows[i];
        let mut m = match key {
            Key::Day(d) => Msg { ts: row.ts, ..Msg::bare(format!("day-{d}"), Kind::Day, row.from.clone()) },
            Key::Row(..) => row.clone(),
        };
        m.lane = Some(room.clone());
        m
    }

    /// The fewest changes that turn the rows sent before into `order`: the rows both share at the
    /// start and the end stay (set again where their chat changed them), the middle is replaced.
    fn changes(&self, order: &[(Key, (usize, usize))], changed: impl Fn(&Key) -> bool) -> Vec<Diff> {
        let old = &self.shown;
        if old.is_empty() || order.is_empty() {
            return if old.is_empty() && order.is_empty() { Vec::new() } else { vec![Diff::Reset { values: order.iter().map(|(k, at)| self.value(k, *at)).collect() }] };
        }
        let start = old.iter().zip(order).take_while(|(a, (b, _))| a == &b).count();
        let room = old.len().min(order.len()) - start;
        let end = old.iter().rev().zip(order.iter().rev()).take(room).take_while(|(a, (b, _))| a == &b).count();
        let (gone, came) = (old.len() - start - end, order.len() - start - end);
        if gone + came > RESET_PAST {
            return vec![Diff::Reset { values: order.iter().map(|(k, at)| self.value(k, *at)).collect() }];
        }
        let mut out = Vec::new();
        let set = |out: &mut Vec<Diff>, index: usize| {
            let (key, at) = &order[index];
            if changed(key) {
                out.push(Diff::Set { index, value: self.value(key, *at) });
            }
        };
        (0..start).for_each(|ix| set(&mut out, ix));
        out.extend((0..gone).map(|_| Diff::Remove { index: start }));
        out.extend(order[start..start + came].iter().enumerate().map(|(j, (k, at))| Diff::Insert { index: start + j, value: self.value(k, *at) }));
        (order.len() - end..order.len()).for_each(|ix| set(&mut out, ix));
        out
    }
}

fn local_day(ts: u64) -> Option<NaiveDate> {
    Local.timestamp_millis_opt(i64::try_from(ts).ok()?).single().map(|t| t.date_naive())
}

/// The open union: one timeline per chat, and the task that merges their rows.
pub(super) struct Merged {
    pub id: OwnedRoomId,
    pub lanes: Vec<timeline::Open>,
    /// Where the composer sends: the UI says, else the first chat.
    pub target: Option<OwnedRoomId>,
    merge: Arc<Mutex<Merge>>,
    _task: AbortOnDrop,
}

impl Merged {
    pub fn lane_of(&self, id: &str) -> Option<OwnedRoomId> {
        self.merge.lock().unwrap().lane_of(id)
    }
}

/// Opens every chat of the union on this device. Memory: one more copy of each chat's rows (the
/// rows as sent are only keys); CPU: the merge walks all rows once per change, a few thousand at most.
pub(super) async fn open(client: &Client, union: &Room, ev: &mpsc::Sender<Ev>) -> Result<Merged> {
    let rooms = members(client, union).await;
    let ids: Vec<OwnedRoomId> = rooms.iter().map(|r| r.room_id().to_owned()).collect();
    let merge = Arc::new(Mutex::new(Merge::new(ids.clone())));
    let (tx, rx) = mpsc::channel(super::CHANNEL_CAP);
    let task = tokio::spawn(follow(union.room_id().to_owned(), merge.clone(), rx, ev.clone()));
    let mut lanes = Vec::with_capacity(rooms.len());
    for room in &rooms {
        lanes.push(timeline::open(room, &tx).await?);
    }
    Ok(Merged { id: union.room_id().to_owned(), lanes, target: ids.first().cloned(), merge, _task: AbortOnDrop(task) })
}

/// Turns the chats' news into the union's: rows merged, typing joined; the chats' features go on
/// under their own IDs for the composer and the menus. Ends when every chat's timeline is closed.
async fn follow(id: OwnedRoomId, merge: Arc<Mutex<Merge>>, mut rx: mpsc::Receiver<Ev>, ev: mpsc::Sender<Ev>) {
    let mut typing: HashMap<OwnedRoomId, Vec<String>> = HashMap::new();
    while let Some(news) = rx.recv().await {
        let out = match news {
            Ev::Timeline { room, diffs } => {
                let diffs = merge.lock().unwrap().take(&room, diffs);
                if diffs.is_empty() {
                    continue;
                }
                Ev::Timeline { room: id.clone(), diffs }
            }
            Ev::Typing { room, names } => {
                typing.insert(room, names);
                let mut names: Vec<String> = Vec::new();
                for name in typing.values().flatten() {
                    if !names.contains(name) && names.len() < MAX_TYPING {
                        names.push(name.clone());
                    }
                }
                Ev::Typing { room: id.clone(), names }
            }
            Ev::OwnRead { .. } | Ev::Disappear { .. } | Ev::Pins { .. } => continue,
            other => other,
        };
        if ev.send(out).await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::{owned_room_id, user_id};

    // Noon UTC: a few seconds either side stay on one date in every time zone.
    const NOON: u64 = 1_704_196_800_000;
    const DAY: u64 = 86_400_000;

    fn msg(id: &str, ts: u64) -> Msg {
        Msg { ts, body: id.into(), ..Msg::bare(id.into(), Kind::Message, user_id!("@a:x").to_owned()) }
    }

    fn rows(merge: &Merge) -> Vec<String> {
        merge.order().iter().map(|(k, _)| match k {
            Key::Row(_, id) => id.clone(),
            Key::Day(_) => "day".into(),
        }).collect()
    }

    /// The UI's copy, built only from the changes sent, must match the merge.
    fn check(merge: &mut Merge, ui: &mut Vec<Msg>, lane: &RoomId, diffs: Vec<Diff>) {
        for diff in merge.take(lane, diffs) {
            crate::backend::apply(ui, diff);
        }
        let want: Vec<String> = merge.order().iter().map(|(k, at)| merge.value(k, *at).id).collect();
        assert_eq!(ui.iter().map(|m| m.id.clone()).collect::<Vec<_>>(), want);
    }

    #[test]
    fn rows_of_two_chats_interleave_by_time_with_one_day_line() {
        let (a, b) = (owned_room_id!("!a:x"), owned_room_id!("!b:x"));
        let mut merge = Merge::new(vec![a.clone(), b.clone()]);
        let mut ui = Vec::new();
        let day = |ts| Msg { ts, ..Msg::bare(format!("d{ts}"), Kind::Day, user_id!("@a:x").to_owned()) };
        check(&mut merge, &mut ui, &a, vec![Diff::Reset { values: [day(NOON), msg("a1", NOON + 1000), msg("a3", NOON + 3000)].into() }]);
        check(&mut merge, &mut ui, &b, vec![Diff::Reset { values: [day(NOON), msg("b2", NOON + 2000), msg("b4", NOON + 4000)].into() }]);
        assert_eq!(rows(&merge), ["day", "a1", "b2", "a3", "b4"]);
        assert!(ui.iter().all(|m| m.lane.is_some()));
        assert_eq!(ui[2].lane.as_ref(), Some(&b));

        // A new message, an edit, an older page.
        check(&mut merge, &mut ui, &a, vec![Diff::PushBack { value: msg("a5", NOON + 5000) }]);
        check(&mut merge, &mut ui, &b, vec![Diff::Set { index: 1, value: Msg { body: "edited".into(), ..msg("b2", NOON + 2000) } }]);
        assert_eq!(ui.iter().find(|m| m.id == "b2").map(|m| m.body.as_str()), Some("edited"));
        check(&mut merge, &mut ui, &b, vec![Diff::PushFront { value: msg("b0", NOON - DAY) }, Diff::PushFront { value: day(NOON - DAY) }]);
        assert_eq!(rows(&merge), ["day", "b0", "day", "a1", "b2", "a3", "b4", "a5"]);

        // A row without a time goes before its chat's next row; only the first unread line shows.
        let unread = |id: &str| Msg::bare(id.into(), Kind::Unread, user_id!("@a:x").to_owned());
        check(&mut merge, &mut ui, &a, vec![Diff::Insert { index: 2, value: unread("ua") }]);
        check(&mut merge, &mut ui, &b, vec![Diff::Insert { index: 3, value: unread("ub") }]);
        assert_eq!(rows(&merge), ["day", "b0", "day", "a1", "ub", "b2", "a3", "b4", "a5"]);
        assert_eq!(merge.lane_of("b4"), Some(b));
        assert_eq!(merge.lane_of("nope"), None);
    }

    fn summary(id: &str, name: &str, ts: u64) -> RoomSummary {
        RoomSummary {
            id: RoomId::parse(id).unwrap(),
            name: name.into(),
            preview: Some(format!("from {name}")),
            preview_id: None,
            ts,
            network: Some("WhatsApp".into()),
            parent: None,
            forum: None,
            unread: 1,
            mentions: 0,
            dm: true,
            pinned: false,
            low_priority: false,
            muted: false,
            marked_unread: false,
            archived: true,
            order: 0,
            dated: true,
            pending: None,
            moved_to: None,
            own_last: false,
            read_by: Vec::new(),
            note: false,
            union: None,
            net_blocked: false,
            peer: None,
            archive_mark: false,
        }
    }

    fn union(id: &str, members: &[&str], created: u64) -> RoomSummary {
        let rooms = members.iter().map(|m| RoomId::parse(*m).unwrap()).collect();
        RoomSummary { union: Some(Union { rooms, created, named: false, marked: false }), unread: 0, archived: false, network: None, ..summary(id, "Empty room", 0) }
    }

    #[test]
    fn a_union_stands_for_its_chats_and_the_limit_keeps_the_oldest() {
        let mut rooms = vec![
            summary("!wa:x", "Ann Lee", 10),
            summary("!sig:x", "Ann Lee", 20),
            summary("!tg:x", "Bob Ray", 30),
            summary("!ig:x", "Bob Ray", 40),
            union("!u1:x", &["!wa:x", "!sig:x", "!gone:x"], 100),
            union("!u2:x", &["!tg:x", "!ig:x"], 200),
            union("!u3:x", &["!gone:x"], 50),
        ];
        let folded = fold(&mut rooms, Some(2));
        let u1 = rooms.iter().find(|r| r.id == "!u1:x").unwrap();
        assert_eq!((u1.name.as_str(), u1.ts, u1.unread, u1.preview.as_deref(), u1.archived), ("Ann Lee", 20, 2, Some("from Ann Lee"), true));
        assert_eq!(folded.members[&RoomId::parse("!u1:x").unwrap()].len(), 2);
        let hidden = |id: &str| folded.hidden.contains(&RoomId::parse(id).unwrap());
        // `u3` is the oldest and counts against the limit though no chat of it is here; `u2` is past it.
        assert!(hidden("!wa:x") && hidden("!sig:x") && hidden("!u3:x") && hidden("!u2:x"));
        assert!(!hidden("!u1:x") && !hidden("!tg:x") && !hidden("!ig:x"));
        // Folding again changes nothing.
        let again = fold(&mut rooms, Some(2));
        assert_eq!(again, folded);
        assert_eq!(rooms.iter().find(|r| r.id == "!u1:x").unwrap().unread, 2);
        // Unknown limit: every union shows.
        assert!(!fold(&mut rooms, None).hidden.contains(&RoomId::parse("!u2:x").unwrap()));
        assert!(!fold(&mut rooms, Some(-1)).hidden.contains(&RoomId::parse("!u2:x").unwrap()));
    }

    #[test]
    fn chats_of_the_same_person_are_suggested() {
        assert_eq!(keys("  Ann   LEE "), ["name:ann lee"]);
        assert_eq!(keys("+48 (601) 234-567"), ["tel:+48601234567"]);
        assert!(keys("Ann").is_empty());
        let rooms = vec![summary("!a:x", "Ann Lee", 1), summary("!b:x", "ann lee", 2), summary("!c:x", "Bob Ray", 3), union("!u:x", &[], 1)];
        let folded = Folded::default();
        let found = suggested(&rooms, &folded, &[&rooms[0]]);
        assert_eq!(found.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["!b:x"]);
        assert!(!mergeable(&rooms[3], &folded));
    }

    #[test]
    fn the_composer_follows_the_newest_chat_unless_the_user_spoke_last_long_ago() {
        let (a, b) = (summary("!a:x", "A", 100), summary("!b:x", "B", 200));
        assert_eq!(seed(&[&a, &b], 300).unwrap(), "!b:x");
        let mine = RoomSummary { own_last: true, ..b.clone() };
        assert_eq!(seed(&[&a, &mine], 300).unwrap(), "!b:x");
        assert_eq!(seed(&[&a, &mine], 200 + OWN_QUIET_MS).unwrap(), "!a:x");
        assert!(seed(&[], 0).is_none());
    }

    #[test]
    fn the_limit_is_read_as_beeper_reads_it() {
        assert_eq!(limit(&json!({})), 1);
        assert_eq!(limit(&json!({ "userInfo": { "features": { "*": { "max-merge-chats": -1 } } } })), -1);
        assert_eq!(limit(&json!({ "userInfo": { "features": { "*": { "max-merge-chats": 3 }, "appType:desktop": { "max-merge-chats": "5" } } } })), 5);
        assert_eq!(limit(&json!({ "userInfo": { "features": { "*": { "max-merge-chats": 2.5 } } } })), 1);
        assert_eq!(limit(&json!({ "userInfo": { "features": { "*": { "max-merge-chats": -7 } } } })), 1);
    }
}

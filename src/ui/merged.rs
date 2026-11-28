//! Merged chats on screen (`backend/union.rs` keeps them on the server): the chat list folded as
//! Beeper folds it, the network each message of a merged chat came through, the chat the composer
//! sends to, the banner that proposes merging chats of one person, and the sheet that merges and
//! splits them.

use super::*;
use crate::backend::{
    Features,
    rooms::Ask,
    union::{self, Folded},
};
use crate::i18n::t;
use matrix_sdk::ruma::RoomId;
use std::collections::HashSet;

/// Chats offered in the merge sheet: the proposed ones first, then the newest one-to-one chats.
const MERGE_ROWS: usize = 20;
/// Proposals the user closed this run.
const MAX_DISMISSED: usize = 256;

#[derive(Default)]
pub(super) struct Ui {
    pub folded: Folded,
    /// `max-merge-chats` once Beeper's API said it; `None` shows every merged chat.
    pub limit: Option<i64>,
    /// In the open merged chat, the chat the composer sends to.
    pub target: Option<OwnedRoomId>,
    /// What each chat of the open merged chat takes, by chat.
    pub lanes: HashMap<OwnedRoomId, (Option<Features>, bool)>,
    /// The chat sheet shows the merge choices.
    pub picking: bool,
    /// The chats the open merged chat was opened with; a change opens it again.
    opened: Option<Vec<OwnedRoomId>>,
    /// shortcut: closed proposals are forgotten on restart; keep them in `Settings` if they come back too often.
    dismissed: HashSet<OwnedRoomId>,
}

impl BeeperApp {
    /// Folds merged chats over their chats in `self.rooms`; the caller sorts and filters after.
    /// A merged chat whose chats changed while open (one split off, one added, its state just
    /// synced) is opened again with them.
    pub(super) fn fold_unions(&mut self, cx: &mut Context<Self>) {
        self.merged.folded = union::fold(&mut self.rooms, self.merged.limit);
        let now = self.open.as_ref().and_then(|id| self.merged.folded.members.get(id));
        if let (Some(was), Some(now)) = (&self.merged.opened, now)
            && was != now
            && let Some(id) = self.open.take()
        {
            self.open_room(id, cx);
        }
    }

    pub(super) fn on_merge_limit(&mut self, limit: i64, cx: &mut Context<Self>) {
        self.merged.limit = Some(limit);
        self.fold_unions(cx);
        self.rooms.sort_unstable_by_key(|r| (!r.pinned, std::cmp::Reverse(r.ts)));
        self.refilter(cx);
    }

    /// The chats of the open merged chat, in the merged chat's order; empty for any other chat.
    pub(super) fn open_lanes(&self) -> &[OwnedRoomId] {
        self.open.as_ref().and_then(|id| self.merged.folded.members.get(id)).map_or(&[], Vec::as_slice)
    }

    /// A merged chat was opened: the composer starts on the chat Beeper would pick.
    pub(super) fn seed_target(&mut self, cx: &mut Context<Self>) {
        let (union, lanes) = (self.open.clone(), self.open_lanes().to_vec());
        self.merged.lanes.clear();
        self.merged.opened = union.as_ref().and_then(|id| self.merged.folded.members.get(id).cloned());
        let members: Vec<&RoomSummary> = lanes.iter().filter_map(|id| self.rooms.iter().find(|r| &r.id == id)).collect();
        self.merged.target = union::seed(&members, chrono::Utc::now().timestamp_millis().max(0) as u64);
        if let (Some(union), Some(lane)) = (union, self.merged.target.clone()) {
            self.send(Cmd::Target(union, lane), cx);
        }
    }

    fn pick_target(&mut self, lane: OwnedRoomId, cx: &mut Context<Self>) {
        let Some(union) = self.open.clone() else { return };
        if self.send(Cmd::Target(union.clone(), lane.clone()), cx) {
            // The composer takes what the picked chat takes.
            self.features = self.merged.lanes.get(&lane).map(|(f, moderate)| (union, f.clone(), *moderate));
            self.merged.target = Some(lane);
            cx.notify();
        }
    }

    /// The network a chat of the open merged chat goes through, as Beeper's `chatSourceLabel` puts
    /// it; two chats on one network are told apart by their place.
    pub(super) fn lane_label(&self, lane: &RoomId) -> String {
        let lanes = self.open_lanes();
        let network = |id: &RoomId| self.rooms.iter().find(|r| r.id == id).and_then(|r| r.network.clone()).unwrap_or_else(|| "Matrix".to_owned());
        let own = network(lane);
        // shortcut: Beeper adds the account's phone or handle; this list knows neither, so a number.
        let same: Vec<&OwnedRoomId> = lanes.iter().filter(|id| network(id) == own).collect();
        match same.iter().position(|id| *id == lane) {
            Some(n) if same.len() > 1 => format!("{own} {}", n + 1),
            _ => own,
        }
    }

    /// The row's network over a message of a merged chat, at the start of each run.
    pub(super) fn lane_tag(&self, m: &Msg, cx: &App) -> Option<Div> {
        let lane = m.lane.as_ref().filter(|_| self.open_lanes().len() > 1)?;
        let p = Palette::of(cx);
        Some(div().pb(px(4.)).text_size(fs(11.)).text_color(p.faint).truncate().child(self.lane_label(lane)))
    }

    /// Under the messages of a merged chat: which chat the composer sends to; a tap picks another.
    pub(super) fn render_lane_picker(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let lanes = self.open_lanes();
        if lanes.len() < 2 {
            return None;
        }
        let p = Palette::of(cx);
        // A reply or an edit goes to its message's chat, whatever is picked.
        let about = match &self.compose {
            Compose::Reply(id) | Compose::Edit(id) => self.msgs.iter().find(|m| &m.id == id).and_then(|m| m.lane.as_ref()),
            Compose::New => None,
        };
        let chips = lanes.iter().enumerate().map(|(ix, lane)| {
            let on = about.or(self.merged.target.as_ref()) == Some(lane);
            let id = lane.clone();
            div()
                .id(("lane", ix))
                .h(px(28.))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded_full()
                .text_size(fs(13.))
                .font_weight(FontWeight::MEDIUM)
                .map(|el| if on { el.bg(p.text).text_color(p.ground) } else { el.bg(p.surface).text_color(p.muted) })
                .cursor_pointer()
                .role(Role::RadioButton)
                .aria_toggled(on.into())
                .child(self.lane_label(lane))
                .on_click(cx.listener(move |this, _, _, cx| this.pick_target(id.clone(), cx)))
        });
        Some(
            h_flex()
                .mx(px(GUTTER))
                .mt(px(8.))
                .gap(px(6.))
                .flex_wrap()
                .child(div().text_size(fs(13.)).text_color(p.faint).child(t!("Send via")))
                .children(chips)
                .into_any_element(),
        )
    }

    /// A one-to-one chat may start a merged chat when the plan has room for one more.
    fn may_merge(&self, room: &RoomSummary) -> bool {
        if self.merged.folded.is_union(&room.id) {
            return self.merged.folded.members.get(&room.id).is_some_and(|m| m.len() < union::MAX_MEMBERS);
        }
        let room_left = match self.merged.limit {
            Some(n) if n >= 0 => usize::try_from(n).is_ok_and(|n| self.merged.folded.members.len() < n),
            _ => true,
        };
        room_left && union::mergeable(room, &self.merged.folded)
    }

    /// The chat sheet's way to the merge choices.
    pub(super) fn merge_entry(&self, room: &RoomSummary, live: bool, window: &mut Window, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        if self.elsewhere.contains_key(&room.id) || !self.may_merge(room) && !self.merged.folded.is_union(&room.id) {
            return None;
        }
        let label = if self.merged.folded.is_union(&room.id) { t!("Merged chats") } else { t!("Merge with another chat") };
        let row = sheet_row("merge-chats", IconName::Merge, label, Palette::of(cx).text, window, cx);
        Some(if live {
            row.on_click(cx.listener(|this, _, _, cx| {
                this.merged.picking = true;
                cx.notify();
            }))
        } else {
            row
        })
    }

    /// Chats `room` could be merged with: those that look like the same person, then the newest.
    fn merge_candidates(&self, room: &RoomSummary) -> Vec<&RoomSummary> {
        let of: Vec<&RoomSummary> = match self.merged.folded.members.get(&room.id) {
            Some(members) => members.iter().filter_map(|id| self.rooms.iter().find(|r| &r.id == id)).collect(),
            None => vec![room],
        };
        let mut found = union::suggested(&self.rooms, &self.merged.folded, &of);
        let mut rest: Vec<&RoomSummary> = self.rooms.iter().filter(|r| r.id != room.id && union::mergeable(r, &self.merged.folded) && !found.iter().any(|f| f.id == r.id)).collect();
        rest.sort_by_key(|r| std::cmp::Reverse(r.ts));
        found.extend(rest);
        found.truncate(MERGE_ROWS);
        found
    }

    /// The merge sheet: a merged chat's chats, each taken out by a tap, then chats to merge in.
    pub(super) fn merge_sheet(&self, room: &RoomSummary, live: bool, window: &mut Window, cx: &mut Context<Self>) -> (String, Vec<Stateful<Div>>) {
        let p = Palette::of(cx);
        let mut rows = Vec::new();
        let union = self.merged.folded.is_union(&room.id).then(|| room.id.clone());
        if let Some(union) = &union {
            let members = self.merged.folded.members.get(union).cloned().unwrap_or_default();
            for (ix, id) in members.into_iter().enumerate() {
                let name = self.rooms.iter().find(|r| r.id == id).map(|r| r.name.clone()).unwrap_or_default();
                let label = t!("Split off {name} ({network})", name = name, network = self.lane_label(&id));
                let row = super::disappear::plain_row(("merge-out", ix), label, cx);
                rows.push(if live {
                    let union = union.clone();
                    row.on_click(cx.listener(move |this, _, _, cx| {
                        this.send(Cmd::People(Ask::Unmerge { union: union.clone(), room: id.clone() }), cx);
                        (this.chat_sheet, this.merged.picking) = (None, false);
                        cx.notify();
                    }))
                } else {
                    row
                });
            }
        }
        if self.may_merge(room) {
            for (ix, other) in self.merge_candidates(room).into_iter().enumerate() {
                let mut label = match &other.network {
                    Some(network) => format!("{} ({network})", other.name),
                    None => other.name.clone(),
                };
                if union.is_some() {
                    label = t!("Add {name}", name = label);
                }
                let row = super::disappear::plain_row(("merge-in", ix), label, cx);
                rows.push(if live {
                    let rooms = vec![room.id.clone(), other.id.clone()];
                    row.on_click(cx.listener(move |this, _, _, cx| this.merge(rooms.clone(), cx)))
                } else {
                    row
                });
            }
        }
        if rows.is_empty() {
            rows.push(
                h_flex()
                    .id("merge-none")
                    .h(px(SHEET_ROW_H))
                    .px(px(GUTTER + 4.))
                    .text_size(fs(13.))
                    .text_color(p.faint)
                    .child(t!("No one-to-one chats to merge with")),
            );
        }
        let _ = window;
        (if union.is_some() { t!("Merged chats") } else { t!("Merge with…") }.to_owned(), rows)
    }

    /// Merges chats; the merged chat opens once the server made it.
    fn merge(&mut self, rooms: Vec<OwnedRoomId>, cx: &mut Context<Self>) {
        if self.send(Cmd::People(Ask::Merge { rooms, name: String::new() }), cx) {
            (self.chat_sheet, self.merged.picking) = (None, false);
            cx.notify();
        }
    }

    /// Over the composer of a one-to-one chat: the other chats that look like the same person, as
    /// Beeper proposes them, with Merge and a close button.
    pub(super) fn render_merge_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let room = self.open.as_ref().and_then(|id| self.rooms.iter().find(|r| &r.id == id))?;
        if self.merged.dismissed.contains(&room.id) || self.elsewhere.contains_key(&room.id) || !self.may_merge(room) {
            return None;
        }
        let others = union::suggested(&self.rooms, &self.merged.folded, &[room]);
        let first = others.first()?;
        let p = Palette::of(cx);
        let networks: Vec<String> = others.iter().take(3).map(|r| r.network.clone().unwrap_or_else(|| "Matrix".to_owned())).collect();
        let text = t!("{name} is also on {networks}. Merge the chats into one?", name = first.name.clone(), networks = networks.join(", "));
        let rooms: Vec<OwnedRoomId> = std::iter::once(room.id.clone()).chain(others.iter().take(union::MAX_MEMBERS - 1).map(|r| r.id.clone())).collect();
        let id = room.id.clone();
        Some(
            h_flex()
                .mx(px(GUTTER))
                .mt(px(8.))
                .p(px(8.))
                .gap(px(8.))
                .rounded(px(RADIUS))
                .bg(p.surface)
                .child(div().flex_1().min_w_0().text_size(fs(13.)).line_height(fs(18.)).child(text))
                .child(
                    div()
                        .id("merge-suggested")
                        .h(px(32.))
                        .px(px(14.))
                        .flex()
                        .flex_none()
                        .items_center()
                        .rounded_full()
                        .bg(p.text)
                        .text_color(p.ground)
                        .text_size(fs(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .child(t!("Merge"))
                        .on_click(cx.listener(move |this, _, _, cx| this.merge(rooms.clone(), cx))),
                )
                .child(round_button("merge-dismiss", IconName::X, t!("Close"), cx).on_click(cx.listener(move |this, _, _, cx| {
                    if this.merged.dismissed.len() < MAX_DISMISSED {
                        this.merged.dismissed.insert(id.clone());
                    }
                    cx.notify();
                })))
                .into_any_element(),
        )
    }
}

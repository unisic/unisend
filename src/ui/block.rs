//! Blocking a person: the blocked list the backend keeps current, the bar that replaces the
//! composer in a blocked person's chat, and Settings' list of blocked people. Asking before a
//! block is the dialog in `rooms.rs`. A one-to-one chat whose bridge blocks on the network does
//! that instead of the Matrix ignore list, and its chat row says whether the person is blocked.

use super::panels::{card, footnote, row};
use super::*;
use crate::backend::block::Blocked;
use crate::i18n::t;

const FACE: f32 = 44.;

#[derive(Default)]
pub(super) struct Ui {
    list: Vec<Blocked>,
    /// The other person of the open one-to-one chat.
    peer: Option<(OwnedRoomId, OwnedUserId)>,
}

impl BeeperApp {
    pub(super) fn blocked_arrived(&mut self, list: Vec<Blocked>, cx: &mut Context<Self>) {
        self.blocked.list = list;
        cx.notify();
    }

    pub(super) fn peer_arrived(&mut self, room: OwnedRoomId, user: OwnedUserId, cx: &mut Context<Self>) {
        if self.open.as_ref() == Some(&room) {
            self.blocked.peer = Some((room, user));
            cx.notify();
        }
    }

    pub(super) fn blocked_count(&self) -> usize {
        self.blocked.list.len() + self.rooms.iter().filter(|r| r.net_blocked).count()
    }

    /// The open one-to-one chat `room` blocks on its network: the bridge's features, when it does.
    fn network_block(&self, room: &OwnedRoomId) -> Option<&crate::backend::Features> {
        self.published().filter(|f| f.block_user && self.open.as_ref() == Some(room))
    }

    fn net_blocked(&self, room: &OwnedRoomId) -> bool {
        self.rooms.iter().any(|r| &r.id == room && r.net_blocked)
    }

    /// Asks the bridge, and shows the answer at once; the chat's next update says what it did.
    pub(super) fn block_on_network(&mut self, room: OwnedRoomId, on: bool, cx: &mut Context<Self>) {
        if self.send(Cmd::BlockOnNetwork(room.clone(), on), cx)
            && let Some(r) = self.rooms.iter_mut().find(|r| r.id == room)
        {
            r.net_blocked = on;
            cx.notify();
        }
    }

    pub(super) fn is_blocked(&self, user: &OwnedUserId) -> bool {
        self.blocked.list.iter().any(|b| b.id == *user)
    }

    pub(super) fn unblock(&mut self, user: OwnedUserId, cx: &mut Context<Self>) {
        self.send(Cmd::SetBlocked(user, false), cx);
    }

    /// The Block or Unblock row of a person's sheet or panel; Block asks first. `dm`: the
    /// one-to-one chat with them, which may block on its network.
    pub(super) fn block_row(&self, user: OwnedUserId, name: String, network: Option<String>, dm: Option<OwnedRoomId>, live: bool, window: &mut Window, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let p = Palette::of(cx);
        let on_network = dm.filter(|room| self.network_block(room).is_some());
        let blocked = match &on_network {
            Some(room) => self.net_blocked(room),
            None => self.is_blocked(&user),
        };
        if blocked && on_network.as_ref().and_then(|room| self.network_block(room)).is_some_and(|f| f.no_unblock) {
            return None;
        }
        let row = if blocked {
            sheet_row("person-unblock", IconName::UserCheck, t!("Unblock"), p.text, window, cx)
        } else {
            sheet_row("person-block", IconName::Ban, t!("Block"), p.negative, window, cx)
        };
        if !live {
            return Some(row);
        }
        Some(row.on_click(cx.listener(move |this, _, _, cx| {
            this.chat_sheet = None;
            match (&on_network, blocked) {
                (Some(room), true) => this.block_on_network(room.clone(), false, cx),
                (None, true) => this.unblock(user.clone(), cx),
                _ => this.ask_block_person(user.clone(), name.clone(), network.clone(), on_network.clone(), cx),
            }
        })))
    }

    /// In place of the composer while the open chat's person is blocked.
    pub(super) fn render_blocked_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.open.clone()?;
        // On the network the bridge says it; unblocking there may be impossible (`no_unblock`).
        let unblock: Option<Result<OwnedRoomId, OwnedUserId>> = if self.network_block(&open).is_some() {
            if !self.net_blocked(&open) {
                return None;
            }
            self.network_block(&open).filter(|f| !f.no_unblock).map(|_| Ok(open.clone()))
        } else {
            let user = self.blocked.peer.as_ref().filter(|(room, user)| *room == open && self.is_blocked(user)).map(|(_, user)| user.clone())?;
            Some(Err(user))
        };
        let p = Palette::of(cx);
        Some(
            v_flex()
                .px(px(GUTTER))
                .pt(px(12.))
                .pb(px(GUTTER))
                .gap(px(8.))
                .border_t_1()
                .border_color(p.raised)
                .bg(p.ground)
                .items_center()
                .child(div().text_size(fs(15.)).line_height(fs(20.)).text_color(p.muted).child(t!("You blocked this person")))
                .children(unblock.map(|who| {
                    div()
                        .id("blocked-unblock")
                        .w_full()
                        .h(px(TAP))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(p.text)
                        .text_color(p.ground)
                        .text_size(fs(15.))
                        .font_weight(FontWeight::MEDIUM)
                        .cursor_pointer()
                        .active(|el| el.opacity(0.8))
                        .role(Role::Button)
                        .child(t!("Unblock"))
                        .on_click(cx.listener(move |this, _, _, cx| match &who {
                            Ok(room) => this.block_on_network(room.clone(), false, cx),
                            Err(user) => this.unblock(user.clone(), cx),
                        }))
                }))
                .into_any_element(),
        )
    }

    pub(super) fn render_blocked_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let on_networks: Vec<RoomSummary> = self.rooms.iter().filter(|r| r.net_blocked).cloned().collect();
        if self.blocked.list.is_empty() && on_networks.is_empty() {
            return v_flex().gap(px(8.)).child(card(cx).child(row(t!("No one is blocked"), "", cx))).child(footnote(t!("People you block cannot send you messages on Matrix."), cx)).into_any_element();
        }
        let line = |face: AnyElement, name: String, second: String, button: Button| {
            h_flex()
                .min_h(px(FACE + 16.))
                .py(px(8.))
                .gap(px(GAP))
                .child(face)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_size(fs(16.)).line_height(fs(22.)).truncate().child(name))
                        .child(div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.faint).truncate().child(second)),
                )
                .child(button)
                .into_any_element()
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        for (ix, b) in self.blocked.list.clone().into_iter().enumerate() {
            let user = b.id.clone();
            let button = Button::new(("unblock", ix)).label(t!("Unblock")).on_click(cx.listener(move |this, _, _, cx| this.unblock(user.clone(), cx)));
            rows.push(line(avatar(&b.name, self.faces.get(&b.id), FACE, cx).into_any_element(), b.name, b.id.to_string(), button));
        }
        for (ix, r) in on_networks.into_iter().enumerate() {
            let room = r.id.clone();
            let button = Button::new(("unblock-net", ix)).label(t!("Unblock")).on_click(cx.listener(move |this, _, _, cx| this.block_on_network(room.clone(), false, cx)));
            let face = room_avatar(&r, &self.avatars, FACE, cx).into_any_element();
            rows.push(line(face, r.name.clone(), t!("Blocked on {network}", network = r.network.clone().unwrap_or_default()), button));
        }
        v_flex().gap(px(8.)).child(card(cx).children(rows)).child(footnote(t!("People you block cannot send you messages on Matrix. On a chat network they are blocked only where it says so."), cx)).into_any_element()
    }
}

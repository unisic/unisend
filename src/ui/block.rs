//! Blocking a person: the blocked list the backend keeps current, the bar that replaces the
//! composer in a blocked person's chat, and Settings' list of blocked people. Asking before a
//! block is the dialog in `rooms.rs`.

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
        self.blocked.list.len()
    }

    pub(super) fn is_blocked(&self, user: &OwnedUserId) -> bool {
        self.blocked.list.iter().any(|b| b.id == *user)
    }

    pub(super) fn unblock(&mut self, user: OwnedUserId, cx: &mut Context<Self>) {
        self.send(Cmd::SetBlocked(user, false), cx);
    }

    /// The Block or Unblock row of a person's sheet or panel; Block asks first.
    pub(super) fn block_row(&self, user: OwnedUserId, name: String, network: Option<String>, live: bool, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = Palette::of(cx);
        let blocked = self.is_blocked(&user);
        let row = if blocked {
            sheet_row("person-unblock", IconName::UserCheck, t!("Unblock"), p.text, window, cx)
        } else {
            sheet_row("person-block", IconName::Ban, t!("Block"), p.negative, window, cx)
        };
        if !live {
            return row;
        }
        row.on_click(cx.listener(move |this, _, _, cx| {
            this.chat_sheet = None;
            if blocked {
                this.unblock(user.clone(), cx);
            } else {
                this.ask_block_person(user.clone(), name.clone(), network.clone(), cx);
            }
        }))
    }

    /// In place of the composer while the open chat's person is blocked.
    pub(super) fn render_blocked_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.open.as_ref()?;
        let user = self.blocked.peer.as_ref().filter(|(room, user)| room == open && self.is_blocked(user)).map(|(_, user)| user.clone())?;
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
                .child(
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
                        .on_click(cx.listener(move |this, _, _, cx| this.unblock(user.clone(), cx))),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_blocked_list(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        if self.blocked.list.is_empty() {
            return v_flex().gap(px(8.)).child(card(cx).child(row(t!("No one is blocked"), "", cx))).child(footnote(t!("People you block cannot send you messages on Matrix."), cx)).into_any_element();
        }
        let rows: Vec<AnyElement> = self
            .blocked
            .list
            .clone()
            .into_iter()
            .enumerate()
            .map(|(ix, b)| {
                let user = b.id.clone();
                h_flex()
                    .min_h(px(FACE + 16.))
                    .py(px(8.))
                    .gap(px(GAP))
                    .child(avatar(&b.name, self.faces.get(&b.id), FACE, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(fs(16.)).line_height(fs(22.)).truncate().child(b.name))
                            .child(div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.faint).truncate().child(b.id.to_string())),
                    )
                    .child(Button::new(("unblock", ix)).label(t!("Unblock")).on_click(cx.listener(move |this, _, _, cx| this.unblock(user.clone(), cx))))
                    .into_any_element()
            })
            .collect();
        v_flex().gap(px(8.)).child(card(cx).children(rows)).child(footnote(t!("People you block cannot send you messages on Matrix. On a chat network they are not blocked."), cx)).into_any_element()
    }
}

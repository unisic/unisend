//! Chats that wait for the user's answer, Signal style: an invitation or a bridged network's
//! message request shows who it is and Accept, Delete or Decline, and Block in place of the
//! composer; an upgraded room points to the room that replaced it.

use super::*;
use crate::backend::rooms::Ask;
use crate::i18n::t;

impl BeeperApp {
    /// What stands in for the composer while the open chat waits for the user, or has moved.
    pub(super) fn render_pending_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let room = self.open.as_ref().and_then(|id| self.rooms.iter().find(|r| &r.id == id))?;
        let p = Palette::of(cx);
        let id = room.id.clone();
        let button = |key: &'static str, label: &'static str, ink: Hsla, bg: Hsla| {
            div()
                .id(key)
                .flex_1()
                .min_w_0()
                .h(px(TAP))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(bg)
                .text_color(ink)
                .text_size(fs(15.))
                .font_weight(FontWeight::MEDIUM)
                .truncate()
                .cursor_pointer()
                .active(|el| el.opacity(0.8))
                .child(label)
        };
        let (title, who, buttons) = match (&room.pending, &room.moved_to) {
            (Some(Pending::Invite { by, name }), _) => {
                let from = if name.is_empty() { room.name.clone() } else { name.clone() };
                let title = if room.dm { t!("{name} invited you to chat", name = from) } else { t!("{name} invited you to this group", name = from) };
                let mut row = Vec::new();
                if room.dm && by.is_some() {
                    let id = id.clone();
                    row.push(button("pending-block", t!("Block"), p.negative, p.raised).on_click(cx.listener(move |this, _, _, cx| this.ask_block(id.clone(), cx))));
                }
                let decline = id.clone();
                row.push(button("pending-decline", t!("Decline"), p.text, p.raised).on_click(cx.listener(move |this, _, _, cx| this.decline(decline.clone(), cx))));
                row.push(button("pending-accept", t!("Accept"), p.ground, p.text).on_click(cx.listener(move |this, _, _, cx| {
                    this.send(Cmd::People(Ask::Accept(id.clone())), cx);
                })));
                (title, by.as_ref().map(|u| u.to_string()), row)
            }
            (Some(Pending::Request), _) => {
                let (block, delete) = (id.clone(), id.clone());
                let row = vec![
                    button("pending-block", t!("Block"), p.negative, p.raised).on_click(cx.listener(move |this, _, _, cx| this.ask_block(block.clone(), cx))),
                    button("pending-delete", t!("Delete"), p.text, p.raised).on_click(cx.listener(move |this, _, _, cx| this.ask_leave(delete.clone(), cx))),
                    button("pending-accept", t!("Accept"), p.ground, p.text).on_click(cx.listener(move |this, _, _, cx| {
                        this.send(Cmd::People(Ask::Accept(id.clone())), cx);
                    })),
                ];
                (t!("Message request from {name}", name = room.name), None, row)
            }
            (None, Some(_)) => {
                let row = vec![button("pending-moved", t!("Open the new chat"), p.ground, p.text).on_click(cx.listener(move |this, _, _, cx| {
                    this.send(Cmd::People(Ask::Upgrade(id.clone())), cx);
                }))];
                (t!("This chat has moved").to_owned(), None, row)
            }
            (None, None) => return None,
        };
        let quiet = room.pending.is_some().then(|| t!("They won't know you've seen this until you accept."));
        Some(
            v_flex()
                .px(px(GUTTER))
                .pt(px(12.))
                .pb(px(GUTTER))
                .gap(px(4.))
                .border_t_1()
                .border_color(p.raised)
                .bg(p.ground)
                .text_center()
                .child(div().text_size(fs(15.)).line_height(fs(20.)).font_weight(FontWeight::SEMIBOLD).child(title))
                .children(who.map(|who| div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.muted).truncate().child(who)))
                .children(quiet.map(|quiet| div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.muted).child(quiet)))
                .child(h_flex().pt(px(8.)).gap(px(8.)).children(buttons))
                .into_any_element(),
        )
    }
}

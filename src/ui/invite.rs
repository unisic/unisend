//! A group's link, as Signal's group link with "Approve new members" on: the admin turns it on or
//! off, copies the `matrix.to` link, and lets in or turns away those who asked to join with it. On
//! is the `knock` join rule; the backend (`backend/group.rs`) says who may change it and where.
//! Letting in is an invite and turning away a kick, as Element does with knocks. A rule set
//! elsewhere (anyone joins, a space's members) is shown, and the switch sets it back to asking.

use super::info::{MEMBER_AVATAR, ROW_H};
use super::panels::card;
use super::*;
use gpui_kit::component::switch::Switch;
use crate::backend::group::{Change, Join};
use crate::i18n::{t, tn};

impl BeeperApp {
    pub(super) fn open_group_link(&mut self, cx: &mut Context<Self>) {
        self.info.page = Some(group::Page::Link);
        cx.notify();
    }

    fn set_group_link(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(room) = self.info.room.clone() else { return };
        self.send(Cmd::Group { room, change: Change::Link(on) }, cx);
    }

    fn answer_knock(&mut self, user: OwnedUserId, admit: bool, cx: &mut Context<Self>) {
        let Some(room) = self.info.room.clone() else { return };
        self.send(Cmd::Group { room, change: if admit { Change::Invite(user) } else { Change::Remove(user) } }, cx);
    }

    /// The info screen's row to the page: whether the link is on and how many ask to join.
    pub(super) fn render_group_link_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let Some(d) = self.info.details.as_ref() else { return div().into_any_element() };
        let state = match (d.knocks.len(), d.join) {
            (0, Join::Invite) => t!("Off").to_owned(),
            (0, _) => t!("On").to_owned(),
            (n, _) => tn!(n as u64, "{n} request to join", "{n} requests to join"),
        };
        h_flex()
            .id("info-group-link")
            .w_full()
            .h(px(ROW_H))
            .px(px(GUTTER))
            .gap(px(12.))
            .cursor_pointer()
            .active(|el| el.bg(p.surface))
            .child(div().size(px(MEMBER_AVATAR)).flex().flex_none().items_center().justify_center().rounded_full().bg(p.surface).child(Icon::new(IconName::Link).size_5().text_color(p.text)))
            .child(div().flex_1().min_w_0().text_size(fs(16.)).truncate().child(t!("Group link")))
            .child(div().flex_none().text_size(fs(14.)).text_color(if d.knocks.is_empty() { p.faint } else { p.accent }).child(state))
            .on_click(cx.listener(|this, _, _, cx| this.open_group_link(cx)))
            .into_any_element()
    }

    pub(super) fn render_group_link_page(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(group::Page::Link) = &self.info.page else { return None };
        let d = self.info.details.as_ref()?;
        let p = Palette::of(cx);
        let note = |text: &'static str| div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.muted).child(text);
        let on = d.join != Join::Invite;
        let explain = match d.join {
            Join::Public => t!("Anyone with the link joins without asking. Turning the link off and on again makes them ask first."),
            Join::Other => t!("Another app set who may join without an invite. Turning the link off and on again makes people ask with the link."),
            _ => t!("Whoever has the link can ask to join; a member who may add people lets them in. The link holds the group's address and its server, not its name or messages."),
        };
        let link = d.link.clone().filter(|_| on);
        let requests = d.knocks.iter().map(|m| {
            let (yes, no) = (m.id.clone(), m.id.clone());
            h_flex()
                .id(SharedString::from(format!("knock-{}", m.id)))
                .w_full()
                .h(px(ROW_H))
                .gap(px(12.))
                .child(avatar(&m.name, self.faces.get(&m.id), MEMBER_AVATAR, cx))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_size(fs(16.)).truncate().child(self.shown_name(&m.id, &m.name)))
                        .child(div().text_size(fs(13.)).text_color(p.faint).truncate().child(m.id.to_string())),
                )
                .when(d.can.remove, |el| el.child(Button::new(SharedString::from(format!("knock-no-{}", m.id))).label(t!("Decline")).on_click(cx.listener(move |this, _, _, cx| this.answer_knock(no.clone(), false, cx)))))
                .child(Button::new(SharedString::from(format!("knock-yes-{}", m.id))).primary().label(t!("Approve")).on_click(cx.listener(move |this, _, _, cx| this.answer_knock(yes.clone(), true, cx))))
        });
        Some(
            v_flex()
                .id("group-link-page")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(GUTTER))
                .pt(px(8.))
                .pb(px(GAP * 2.))
                .gap(px(GAP))
                .when(d.can.link, |el| {
                    el.child(
                        h_flex()
                            .w_full()
                            .gap(px(12.))
                            .child(div().flex_1().text_size(fs(16.)).child(t!("Group link")))
                            .child(Switch::new("group-link-switch").checked(on).on_click(cx.listener(|this, on: &bool, _, cx| this.set_group_link(*on, cx)))),
                    )
                    .child(note(explain))
                })
                .when_some(link, |el, link| {
                    let copied = link.clone();
                    el.child(card(cx).p(px(12.)).child(div().w_full().text_size(fs(14.)).text_color(p.muted).child(link)))
                        .child(h_flex().child(Button::new("group-link-copy").label(t!("Copy link")).on_click(move |_, _, cx| crate::copy(cx, copied.clone()))))
                })
                .when(!d.knocks.is_empty(), |el| {
                    el.child(div().pt(px(8.)).text_size(fs(12.)).text_color(p.faint).child(t!("Requests to join"))).children(requests)
                })
                .into_any_element(),
        )
    }
}

/// What a change does to the shown details, before the info screen is read again.
pub(super) fn apply(d: &mut crate::backend::Details, change: &Change) {
    match change {
        Change::Link(on) => d.join = if *on { Join::Knock } else { Join::Invite },
        Change::Invite(user) | Change::Remove(user) => d.knocks.retain(|m| &m.id != user),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use crate::backend::group::{Change, Join};
    use crate::backend::{Details, Member, Role};
    use matrix_sdk::ruma::OwnedUserId;

    #[test]
    fn answering_a_request_takes_it_off_and_the_switch_sets_the_rule() {
        let ann = OwnedUserId::try_from("@ann:x").unwrap();
        let mut d = Details { join: Join::Public, ..Details::default() };
        d.knocks.push(Member { id: ann.clone(), name: "Ann".into(), role: Role::Member, removable: true });
        super::apply(&mut d, &Change::Invite(ann));
        assert!(d.knocks.is_empty());
        super::apply(&mut d, &Change::Link(false));
        assert_eq!(d.join, Join::Invite);
        super::apply(&mut d, &Change::Link(true));
        assert_eq!(d.join, Join::Knock);
    }
}

//! What a poll and a shared place look like inside a message bubble.

use super::*;
use crate::backend::{Place, Poll};

impl BeeperApp {
    /// A touch on an answer: votes for it, or takes the vote back.
    fn vote(&mut self, target: &str, answer: &str, cx: &mut Context<Self>) {
        let (Some(room), Some(me)) = (self.open.clone(), self.user.clone()) else { return };
        let Some(poll) = self.msgs.iter().rfind(|m| m.id == target).and_then(|m| m.poll.as_ref()) else { return };
        if poll.ended.is_some() {
            return;
        }
        if let Some(answers) = poll.touch(me.as_str(), answer) {
            let cmd = Cmd::Vote { room, target: target.to_owned(), unstable: poll.unstable, answers };
            self.send(cmd, cx);
        }
    }
}

/// `width` is what a bubble has for its content.
pub(super) fn poll(m: &Msg, poll: &Poll, me: Option<&OwnedUserId>, width: f32, p: Palette, window: &mut Window, cx: &mut Context<BeeperApp>) -> AnyElement {
    let (counts, voters) = poll.tally();
    let mine = me.map_or(&[][..], |me| poll.choice(me.as_str()));
    let open = poll.ended.is_none();
    let rows = poll.answers.iter().zip(counts).enumerate().map(|(n, (answer, count))| {
        let chosen = mine.contains(&answer.id);
        let (target, id) = (m.id.clone(), answer.id.clone());
        // A vote moves the bars to their new shares, so the change the touch made can be seen.
        let share = transition((SharedString::from(format!("{}-share-{n}", m.id)), "share"), count as f32 / voters.max(1) as f32, motion::enter(), window, cx);
        div()
            .id(SharedString::from(format!("{}-answer-{n}", m.id)))
            .relative()
            .h(px(TAP))
            .rounded(px(12.))
            .overflow_hidden()
            .bg(p.text.opacity(0.07))
            .when(open, |el| {
                el.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.vote(&target, &id, cx);
                }))
            })
            .child(div().absolute().top_0().left_0().h_full().w(relative(share)).bg(if chosen { p.text.opacity(0.3) } else { p.lifted }))
            .child(
                h_flex()
                    .relative()
                    .size_full()
                    .px(px(10.))
                    .gap(px(8.))
                    .items_center()
                    .text_size(fs(14.))
                    .when(chosen, |el| el.child(Icon::new(IconName::Check).size_4()))
                    .child(div().flex_1().min_w_0().line_clamp(2).text_ellipsis().child(answer.text.clone()))
                    .child(div().text_color(p.muted).font_features(tabular()).child(count.to_string())),
            )
    });
    let rows: Vec<_> = rows.collect();
    let total = match voters {
        0 => "No votes".to_owned(),
        1 => "1 vote".to_owned(),
        n => format!("{n} votes"),
    };
    v_flex()
        .w(px(width.min(BUBBLE_W)))
        .gap(px(6.))
        .child(div().text_size(fs(16.)).line_height(fs(22.)).font_weight(FontWeight::SEMIBOLD).child(poll.question.clone()))
        .children(rows)
        .child(div().text_size(fs(12.)).text_color(p.faint).child(total))
        .when(!open, |el| el.child(div().text_size(fs(12.)).text_color(p.faint).child("Poll ended")))
        .into_any_element()
}

pub(super) fn place(m: &Msg, place: &Place, width: f32, p: Palette, cx: &mut Context<BeeperApp>) -> AnyElement {
    let place_for_tap = place.clone();
    h_flex()
        .id(SharedString::from(format!("{}-place", m.id)))
        .w(px(width.min(LINK_W)))
        .min_h(px(TAP))
        .px(px(10.))
        .py(px(8.))
        .gap(px(10.))
        .rounded(px(12.))
        .bg(p.text.opacity(0.07))
        .cursor_pointer()
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.stop_propagation();
            crate::open_place(cx, &place_for_tap);
        }))
        .child(Icon::new(IconName::MapPin).size_5())
        .child(
            v_flex()
                .min_w_0()
                .when(!place.description.is_empty(), |el| el.child(div().text_size(fs(14.)).line_height(fs(19.)).line_clamp(2).text_ellipsis().child(place.description.clone())))
                .child(div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.muted).child(place.coordinates())),
        )
        .into_any_element()
}

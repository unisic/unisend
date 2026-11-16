//! What a poll and a shared place look like inside a message bubble.

use super::*;
use crate::backend::{Place, Poll};
use crate::i18n::{t, tn};

impl BeeperApp {
    /// A touch on an answer: votes for it, or takes the vote back.
    fn vote(&mut self, target: &str, answer: &str, cx: &mut Context<Self>) {
        let (Some(room), Some(me)) = (self.open.clone(), self.user.clone()) else { return };
        let Some(poll) = self.msgs.iter().rfind(|m| m.id == target).and_then(|m| m.poll.as_ref()) else { return };
        if poll.ended.is_some() || !poll.unstable {
            return;
        }
        if let Some(answers) = poll.touch(me.as_str(), answer) {
            let cmd = Cmd::Vote { room, target: target.to_owned(), unstable: poll.unstable, answers };
            self.send(cmd, cx);
        }
    }
}

/// Tells the bubble's press which answer the finger landed on.
fn answer_touch(id: String, n: usize, cx: &mut Context<BeeperApp>) -> impl IntoElement + use<> {
    let weak = cx.entity().downgrade();
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let visible = window.content_mask().bounds;
            window.on_mouse_event(move |e: &TouchDragEvent, phase, _, cx| {
                let at = e.start_position;
                if phase.bubble() && e.phase == TouchPhase::Started && bounds.contains(&at) && visible.contains(&at) {
                    let _ = weak.update(cx, |this, _| this.part_pressed(&id, n));
                }
            });
        },
    )
    .absolute()
    .inset_0()
}

/// `width` is what a bubble has for its content; `pressed` is the answer a finger rests on, and how
/// strongly it shows.
#[allow(clippy::too_many_arguments, reason = "the parts of one card, read once")]
pub(super) fn poll(m: &Msg, poll: &Poll, me: Option<&OwnedUserId>, width: f32, p: Palette, pressed: Option<(usize, f32)>, window: &mut Window, cx: &mut Context<BeeperApp>) -> AnyElement {
    let (counts, voters) = poll.tally();
    let mine = me.map_or(&[][..], |me| poll.choice(me.as_str()));
    // The app counts and casts only the unstable poll's votes; a stable poll is shown, not voted in.
    let counted = poll.unstable;
    let open = poll.ended.is_none() && counted;
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
                .child(answer_touch(m.id.clone(), n, cx))
            })
            .child(div().absolute().top_0().left_0().h_full().w(relative(share)).bg(if chosen { p.text.opacity(0.3) } else { p.lifted }))
            // `.active` never shows for a touch, which reaches the app only as it lifts.
            .when_some(pressed.filter(|(on, _)| *on == n), |el, (_, level)| el.child(div().absolute().inset_0().rounded(px(12.)).bg(p.text.opacity(0.08 * level))))
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
                    .when(counted, |el| el.child(div().text_color(p.muted).font_features(tabular()).child(count.to_string()))),
            )
    });
    let rows: Vec<_> = rows.collect();
    let total = match voters {
        _ if !counted => t!("Vote in another app").to_owned(),
        0 => t!("No votes").to_owned(),
        n => tn!(n, "{n} vote", "{n} votes"),
    };
    v_flex()
        .w(px(width.min(BUBBLE_W)))
        .gap(px(6.))
        .child(div().text_size(fs(16.)).line_height(fs(22.)).font_weight(FontWeight::SEMIBOLD).child(poll.question.clone()))
        .children(rows)
        .child(div().text_size(fs(12.)).text_color(p.faint).child(total))
        .when(poll.ended.is_some(), |el| el.child(div().text_size(fs(12.)).text_color(p.faint).child(t!("Poll ended"))))
        .into_any_element()
}

pub(super) fn place(m: &Msg, place: &Place, width: f32, p: Palette, cx: &mut Context<BeeperApp>) -> AnyElement {
    place_card(&m.id, place, None, width, p, cx)
}

/// The card of a place; `note` is a line under the coordinates, the accuracy of a fresh fix.
pub(super) fn place_card(id: &str, place: &Place, note: Option<String>, width: f32, p: Palette, cx: &mut Context<BeeperApp>) -> AnyElement {
    let place_for_tap = place.clone();
    h_flex()
        .id(SharedString::from(format!("{id}-place")))
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
                .child(div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.muted).child(place.coordinates()))
                .children(note.map(|note| div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.faint).child(note))),
        )
        .into_any_element()
}

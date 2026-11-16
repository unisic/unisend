//! The pinned-message bar under the chat header, after Signal: the newest pin, one line, with its
//! place among the pins; a tap goes to that message and shows the next pin the next time.

use super::*;
use crate::backend::{Pins, Quote};
use crate::i18n::t;

/// Index in `list` (newest first) of the pin the bar shows: the one last shown while it is still
/// pinned, so a change made on another device leaves the bar in place, else the newest.
fn shown(list: &[Quote], id: Option<&str>) -> usize {
    id.and_then(|id| list.iter().position(|q| q.id == id)).unwrap_or(0)
}

/// The pin after index `ix`, going back in time and round to the newest again.
fn after(list: &[Quote], ix: usize) -> usize {
    (ix + 1) % list.len().max(1)
}

impl BeeperApp {
    /// The open chat's pins, once read.
    pub(super) fn pins(&self) -> Option<&Pins> {
        self.pins.as_ref().filter(|(room, _)| self.open.as_ref() == Some(room)).map(|(_, pins)| pins)
    }

    /// Whether the user may pin in the open chat and whether `id` is pinned there; `None` when the
    /// menu has no Pin to offer. A bridge may not carry pins to its network: only power levels decide.
    pub(super) fn pin_offer(&self, id: &str) -> Option<bool> {
        let pins = self.pins().filter(|p| p.can)?;
        Some(pins.list.iter().any(|q| q.id == id))
    }

    pub(super) fn on_pins(&mut self, room: OwnedRoomId, pins: Pins) {
        if self.open.as_ref() == Some(&room) {
            self.pins = Some((room, pins));
        }
    }

    fn pin_tap(&mut self, cx: &mut Context<Self>) {
        let Some(pins) = self.pins().filter(|p| !p.list.is_empty()) else { return };
        let ix = shown(&pins.list, self.pin_shown.as_deref());
        let (id, next) = (pins.list[ix].id.clone(), pins.list[after(&pins.list, ix)].id.clone());
        self.pin_shown = Some(next);
        self.reveal_within(&id, super::search::JUMP_PAGES, cx);
        cx.notify();
    }

    pub(super) fn pin_bar(&self, end: f32, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let pins = self.pins().filter(|p| !p.list.is_empty() && !self.find.open)?;
        let p = Palette::of(cx);
        let ix = shown(&pins.list, self.pin_shown.as_deref());
        let q = &pins.list[ix];
        let (icon, label) = super::quote_label(q);
        let who = if q.own { t!("You").to_owned() } else { q.sender.clone() };
        let label = label.replace('\n', " ");
        Some(
            h_flex()
                .id("pin-bar")
                .flex_none()
                .h(px(TAP))
                .pl(px(GUTTER))
                .pr(px(GUTTER + end))
                .gap(px(8.))
                .bg(p.surface)
                .cursor_pointer()
                .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised)))
                .active(|el| el.bg(p.raised))
                .on_click(cx.listener(|this, _, _, cx| this.pin_tap(cx)))
                .child(Icon::new(IconName::Pin).size_4().flex_none().text_color(p.faint))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(6.))
                        .text_size(fs(14.))
                        .child(div().flex_none().max_w(px(140.)).truncate().font_weight(FontWeight::MEDIUM).text_color(p.text).child(who))
                        .children(icon.map(|icon| Icon::new(icon).size_4().flex_none().text_color(p.muted)))
                        .child(div().flex_1().min_w_0().truncate().text_color(p.muted).child(crate::emoji::styled(label, Vec::new()))),
                )
                .when(pins.list.len() > 1, |el| {
                    el.child(div().flex_none().text_size(fs(12.)).font_features(tabular()).text_color(p.faint).child(format!("{}/{}", ix + 1, pins.list.len())))
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Quote, after, shown};
    use crate::backend::QuoteState;

    fn pins(ids: &[&str]) -> Vec<Quote> {
        ids.iter().map(|id| Quote::unknown((*id).to_owned(), QuoteState::Ready)).collect()
    }

    #[test]
    fn the_bar_starts_at_the_newest_and_goes_round() {
        let list = pins(&["$c", "$b", "$a"]);
        assert_eq!(shown(&list, None), 0);
        assert_eq!(after(&list, 0), 1);
        assert_eq!(after(&list, 2), 0);
        assert_eq!(after(&pins(&["$a"]), 0), 0);
    }

    #[test]
    fn the_bar_stays_on_its_pin_when_others_change() {
        assert_eq!(shown(&pins(&["$d", "$c", "$b", "$a"]), Some("$b")), 2);
        assert_eq!(shown(&pins(&["$d", "$a"]), Some("$b")), 0, "an unpinned message gives way to the newest");
        assert_eq!(shown(&[], Some("$b")), 0);
    }
}

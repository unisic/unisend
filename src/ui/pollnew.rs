//! The form that starts a poll: a question, 2 to 20 options (fewer where the network takes fewer), one answer or several.

use gpui_kit::component::switch::Switch;

use super::*;
use crate::backend::NewPoll;
use crate::backend::pollnew::{MAX_ANSWERS, MIN_ANSWERS};
use crate::i18n::t;

type Field = (Entity<InputState>, Subscription);

struct Fields {
    question: Field,
    options: Vec<Field>,
    multiple: bool,
    error: Option<&'static str>,
}

/// Kept after closing until the next opening, so the sheet can finish falling.
#[derive(Default)]
pub(super) struct Form {
    open: bool,
    fields: Option<Fields>,
}

fn field(placeholder: &'static str, window: &mut Window, cx: &mut Context<BeeperApp>) -> Field {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
    crate::ime::follow(&input, window, cx);
    let sub = cx.subscribe_in(&input, window, |_, _, ev: &InputEvent, _, cx| keyboard_follows(ev, Keyboard::Text, cx));
    (input, sub)
}

impl BeeperApp {
    pub(super) fn open_poll_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_gallery(cx);
        let question = field(t!("Question"), window, cx);
        let options = vec![field(t!("Option 1"), window, cx), field(t!("Option 2"), window, cx)];
        let focus = question.0.read(cx).focus_handle(cx);
        self.poll_form = Form { open: true, fields: Some(Fields { question, options, multiple: false, error: None }) };
        window.focus(&focus, cx);
        cx.notify();
    }

    pub(super) fn close_poll_form(&mut self, cx: &mut Context<Self>) -> bool {
        let was = std::mem::replace(&mut self.poll_form.open, false);
        if was {
            crate::hide_keyboard();
            cx.notify();
        }
        was
    }

    /// The most options the open chat's network takes in a poll.
    fn poll_limit(&self) -> usize {
        self.published().map_or(MAX_ANSWERS, |f| if f.poll_max == 0 { MAX_ANSWERS } else { f.poll_max.clamp(MIN_ANSWERS, MAX_ANSWERS) })
    }

    fn add_poll_option(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.poll_form.fields.as_ref().is_none_or(|f| f.options.len() >= self.poll_limit()) {
            return;
        }
        let new = field(t!("Option"), window, cx);
        let focus = new.0.read(cx).focus_handle(cx);
        if let Some(f) = self.poll_form.fields.as_mut() {
            f.options.push(new);
            f.error = None;
        }
        window.focus(&focus, cx);
        cx.notify();
    }

    fn remove_poll_option(&mut self, at: usize, cx: &mut Context<Self>) {
        if let Some(f) = self.poll_form.fields.as_mut().filter(|f| f.options.len() > MIN_ANSWERS && at < f.options.len()) {
            drop(f.options.remove(at));
            f.error = None;
            cx.notify();
        }
    }

    fn send_poll(&mut self, cx: &mut Context<Self>) {
        let (Some(room), Some(f)) = (self.open.clone(), self.poll_form.fields.as_ref()) else { return };
        let question = f.question.0.read(cx).value();
        let answers: Vec<String> = f.options.iter().map(|(o, _)| o.read(cx).value().to_string()).collect();
        match NewPoll::check(&question, &answers, f.multiple) {
            Ok(poll) => {
                if self.send(Cmd::SendPoll { room, question: poll.question, answers: poll.answers, multiple: poll.multiple }, cx) {
                    self.close_poll_form(cx);
                }
            }
            Err(e) => {
                if let Some(f) = self.poll_form.fields.as_mut() {
                    f.error = Some(e);
                }
                cx.notify();
            }
        }
    }

    pub(super) fn render_poll_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let live = self.poll_form.open;
        let motion = if live {
            Transition::new(motion::SHEET_IN).easing(motion::EASE_SHEET)
        } else {
            Transition::new(motion::SHEET_OUT).easing(motion::EASE_OUT)
        };
        let shown = Presence::new("poll-form", live).transition(motion).sample(window, cx);
        if !shown.should_render() {
            return None;
        }
        let f = self.poll_form.fields.as_ref()?;
        let p = Palette::of(cx);
        let (top, bottom) = crate::insets();
        let y = f32::from(frame::viewport(window).height) * (1. - shown.progress);
        let label = |text: &'static str| div().px(px(GUTTER)).pt(px(14.)).pb(px(6.)).text_size(fs(14.)).text_color(p.faint).child(text);
        let n = f.options.len();
        let rows = f.options.iter().enumerate().map(|(i, (input, _))| {
            h_flex()
                .gap(px(4.))
                .pl(px(GUTTER))
                .pr(px(GUTTER - 6.))
                .pb(px(8.))
                .items_center()
                .child(div().flex_1().min_w_0().child(Input::new(input).large()).child(crate::ime::marks(input)))
                .child(
                    div()
                        .id(("poll-remove", i))
                        .size(px(TAP))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .text_color(p.muted)
                        .when(n > MIN_ANSWERS, |el| {
                            el.cursor_pointer().active(|el| el.bg(p.raised)).child(Icon::new(IconName::Minus).size_5()).on_click(cx.listener(move |this, _, _, cx| this.remove_poll_option(i, cx)))
                        }),
                )
        });
        let add = (n < self.poll_limit()).then(|| {
            h_flex()
                .id("poll-add")
                .h(px(TAP))
                .mx(px(GUTTER - 6.))
                .px(px(6.))
                .gap(px(10.))
                .rounded_full()
                .text_size(fs(15.))
                .text_color(p.text)
                .cursor_pointer()
                .active(|el| el.bg(p.raised))
                .child(Icon::new(IconName::Plus).size_5())
                .child(t!("Add option"))
                .when(live, |el| el.on_click(cx.listener(|this, _, window, cx| this.add_poll_option(window, cx))))
        });
        let multiple = f.multiple;
        let weak = cx.entity().downgrade();
        let toggle = h_flex()
            .id("poll-multiple")
            .h(px(TAP + 8.))
            .mx(px(GUTTER))
            .mt(px(8.))
            .justify_between()
            .items_center()
            .child(div().text_size(fs(16.)).child(t!("Allow multiple answers")))
            .child(Switch::new("poll-multiple-switch").checked(multiple).on_click(move |on, _, cx| {
                if live {
                    let _ = weak.update(cx, |this, cx| {
                        if let Some(f) = this.poll_form.fields.as_mut() {
                            f.multiple = *on;
                        }
                        cx.notify();
                    });
                }
            }));
        let send = h_flex()
            .id("poll-send")
            .h(px(TAP))
            .px(px(18.))
            .flex_none()
            .rounded_full()
            .bg(p.text)
            .text_color(p.ground)
            .text_size(fs(15.))
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .active(|el| el.bg(p.muted))
            .child(t!("Send"))
            .when(live, |el| el.on_click(cx.listener(|this, _, _, cx| this.send_poll(cx))));
        let head = h_flex()
            .flex_none()
            .h(px(TAP + 8.))
            .pl(px(GUTTER - 6.))
            .pr(px(GUTTER))
            .gap(px(6.))
            .child(div().id("poll-close").size(px(TAP)).flex().flex_none().items_center().justify_center().rounded_full().cursor_pointer().active(|el| el.bg(p.raised)).child(Icon::new(IconName::X).size_5()).when(live, |el| {
                el.on_click(cx.listener(|this, _, _, cx| {
                    this.close_poll_form(cx);
                }))
            }))
            .child(div().flex_1().min_w_0().text_size(fs(18.)).font_weight(FontWeight::SEMIBOLD).child(t!("New poll")))
            .child(send);
        Some(
            v_flex()
                .id("poll-form")
                .absolute()
                .left_0()
                .top(px(y))
                .w_full()
                .h_full()
                .pt(px(top))
                .pb(px(bottom))
                .bg(p.ground)
                .occlude()
                .child(head)
                .child(
                    v_flex()
                        .id("poll-body")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .pb(px(24.))
                        .child(label(t!("Question")))
                        .child(div().px(px(GUTTER)).child(Input::new(&f.question.0).large()).child(crate::ime::marks(&f.question.0)))
                        .child(label(t!("Options")))
                        .children(rows)
                        .children(add)
                        .child(toggle)
                        .when_some(f.error, |el, e| el.child(div().px(px(GUTTER)).pt(px(14.)).text_size(fs(14.)).text_color(p.negative).child(e))),
                )
                .into_any_element(),
        )
    }
}

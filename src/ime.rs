//! Keeps the Android keyboard's view of the focused text field in step with the field, so the IME
//! has the words around the caret for autocorrect, sentence capitals and prediction.
//!
//! The Java proxy (`GpuiInputActivity.InputProxy`) holds a window of the field's text around the
//! caret. Its edits arrive here as exact UTF-16 replacements (`nativeImeEdit`) and are applied to
//! the field on the GPUI thread. Every change of the field, made by the IME or by the app (sending,
//! a tap moving the caret, a draft, an emoji or a mention), is published back (`gpuiImeSync`) with
//! the sequence number of the last applied edit, so Java tells its own echo from the app's change.
//! Only fields passed to `follow` take part: secrets never reach the proxy as context.

use std::ops::Range;

/// UTF-16 units of context the keyboard gets before and after the selection.
/// `GpuiInputActivity` trims and refills its copy relative to `BEFORE`.
const BEFORE: usize = 512;
const AFTER: usize = 128;

/// The field's text the keyboard is shown, around a UTF-16 selection.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn around(sel: &Range<usize>) -> Range<usize> {
    sel.start.saturating_sub(BEFORE)..sel.end.saturating_add(AFTER)
}

/// One edit of the proxy, in UTF-16 offsets of the whole field.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
#[derive(Debug)]
struct Edit {
    field: u64,
    seq: i64,
    /// Old range and the text now in its place; `None` when only the caret or composition moved.
    replace: Option<(Range<usize>, String)>,
    /// `None` keeps the field's selection.
    sel: Option<Range<usize>>,
    /// The IME's composing (marked) text after the edit.
    comp: Option<Range<usize>>,
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
#[derive(Debug, PartialEq)]
enum Op {
    Replace(Range<usize>, String),
    /// Replace and mark the new text, selection relative to it.
    ReplaceAndMark(Range<usize>, String, Range<usize>),
    /// Mark text already there, selection relative to it.
    Mark(Range<usize>, Range<usize>),
    Unmark,
    Select(Range<usize>),
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn plan(edit: &Edit) -> Vec<Op> {
    let relative = |c: &Range<usize>| {
        let sel = edit.sel.clone().unwrap_or(c.end..c.end);
        let at = |x: usize| x.clamp(c.start, c.end) - c.start;
        at(sel.start)..at(sel.end)
    };
    match (&edit.replace, &edit.comp) {
        // Typing into a composing word: one replacement, as a platform IME would send it.
        (Some((range, text)), Some(comp))
            if comp.start == range.start && comp.end == range.start + text.encode_utf16().count() =>
        {
            vec![Op::ReplaceAndMark(range.clone(), text.clone(), relative(comp))]
        }
        (replace, comp) => {
            let mut ops: Vec<Op> = replace.iter().map(|(range, text)| Op::Replace(range.clone(), text.clone())).collect();
            match comp {
                Some(comp) => ops.push(Op::Mark(comp.clone(), relative(comp))),
                None => ops.extend([Op::Unmark].into_iter().chain(edit.sel.clone().map(Op::Select))),
            }
            ops
        }
    }
}

#[cfg(target_os = "android")]
pub(crate) use android::follow;

#[cfg(not(target_os = "android"))]
pub(crate) fn follow<T: 'static>(_: &gpui_kit::Entity<gpui_kit::component::input::InputState>, _: &mut gpui_kit::Window, _: &mut gpui_kit::Context<T>) {}

#[cfg(target_os = "android")]
mod android {
    use std::cell::{Cell, RefCell};
    use std::ffi::c_void;
    use std::ops::Range;
    use std::sync::{Mutex, OnceLock};

    use gpui_kit::component::input::{InputEvent, InputState};
    use gpui_kit::{App, Context, Entity, EntityInputHandler as _, Focusable as _, WeakEntity, Window};
    use gpui_mobile::android::jni::{activity, get_string, with_env};
    use jni::objects::{JObject, JValue};
    use tokio::sync::mpsc;

    use super::{Edit, Op, around, plan};

    enum Msg {
        Edit(Edit),
        Resync,
    }

    /// Edits waiting for the GPUI thread; Java resyncs if one does not fit.
    const QUEUE: usize = 256;
    static TX: OnceLock<mpsc::Sender<Msg>> = OnceLock::new();
    static RX: Mutex<Option<mpsc::Receiver<Msg>>> = Mutex::new(None);

    fn sender() -> &'static mpsc::Sender<Msg> {
        TX.get_or_init(|| {
            let (tx, rx) = mpsc::channel(QUEUE);
            *RX.lock().unwrap_or_else(|e| e.into_inner()) = Some(rx);
            tx
        })
    }

    #[derive(PartialEq)]
    struct State {
        field: u64,
        applied: i64,
        start: usize,
        text: String,
        sel: Range<usize>,
        /// The field's whole length, so Java knows where it ends.
        len: usize,
    }

    thread_local! {
        /// Followed fields still alive, pruned on every `follow`.
        static FIELDS: RefCell<Vec<WeakEntity<InputState>>> = const { RefCell::new(Vec::new()) };
        static APPLIED: Cell<i64> = const { Cell::new(0) };
        /// What Java was last told: `None` before the first push, `Some(None)` for no field.
        static PUSHED: RefCell<Option<Option<State>>> = const { RefCell::new(None) };
    }

    /// Mirror this field to the keyboard while it is focused. The subscriptions end with the field.
    pub(crate) fn follow<T: 'static>(input: &Entity<InputState>, window: &mut Window, cx: &mut Context<T>) {
        FIELDS.with_borrow_mut(|fields| {
            fields.retain(|f| f.upgrade().is_some());
            fields.push(input.downgrade());
        });
        sender();
        if let Some(mut rx) = RX.lock().unwrap_or_else(|e| e.into_inner()).take() {
            window
                .spawn(cx, async move |cx| {
                    while let Some(msg) = rx.recv().await {
                        if cx.update(|window, cx| handle(msg, window, cx)).is_err() {
                            break;
                        }
                    }
                })
                .detach();
        }
        cx.observe_in(input, window, |_, input, window, cx| publish(&input, false, window, cx)).detach();
        cx.subscribe_in(input, window, |_, input, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::Focus | InputEvent::Blur) {
                publish(input, false, window, cx);
            }
        })
        .detach();
    }

    fn focused(window: &Window, cx: &App) -> Option<Entity<InputState>> {
        FIELDS.with_borrow(|fields| {
            fields.iter().filter_map(WeakEntity::upgrade).find(|f| f.read(cx).focus_handle(cx).is_focused(window))
        })
    }

    fn handle(msg: Msg, window: &mut Window, cx: &mut App) {
        if let Msg::Edit(edit) = &msg {
            APPLIED.set(edit.seq);
            let target = FIELDS.with_borrow(|fields| {
                fields.iter().filter_map(WeakEntity::upgrade).find(|f| f.entity_id().as_u64() == edit.field)
            });
            if let Some(target) = target {
                target.update(cx, |state, cx| apply(state, edit, window, cx));
            }
        }
        match focused(window, cx) {
            Some(input) => publish(&input, matches!(msg, Msg::Resync), window, cx),
            None => push(None, matches!(msg, Msg::Resync)),
        }
    }

    fn apply(s: &mut InputState, edit: &Edit, window: &mut Window, cx: &mut Context<InputState>) {
        for op in plan(edit) {
            match op {
                Op::Replace(range, text) => s.replace_text_in_range(Some(range), &text, window, cx),
                Op::ReplaceAndMark(range, text, sel) => {
                    s.replace_and_mark_text_in_range(Some(range), &text, Some(sel), window, cx)
                }
                Op::Mark(range, sel) => {
                    let mut adjusted = None;
                    if let Some(text) = s.text_for_range(range.clone(), &mut adjusted, window, cx) {
                        s.replace_and_mark_text_in_range(Some(adjusted.unwrap_or(range)), &text, Some(sel), window, cx);
                    }
                }
                Op::Unmark => {
                    if s.marked_text_range(window, cx).is_some() {
                        s.unmark_text(window, cx);
                    }
                }
                Op::Select(range) => {
                    if s.selected_text_range(false, window, cx).is_some_and(|now| now.range == range) {
                        continue;
                    }
                    // Marking empty text only moves the caret; marking the text and unmarking selects it.
                    let mut adjusted = None;
                    let text = s.text_for_range(range.clone(), &mut adjusted, window, cx).unwrap_or_default();
                    let range = adjusted.unwrap_or(range);
                    if text.is_empty() {
                        s.replace_and_mark_text_in_range(Some(range.start..range.start), "", None, window, cx);
                    } else {
                        let len = text.encode_utf16().count();
                        s.replace_and_mark_text_in_range(Some(range), &text, Some(0..len), window, cx);
                        s.unmark_text(window, cx);
                    }
                }
            }
        }
    }

    fn publish(input: &Entity<InputState>, force: bool, window: &mut Window, cx: &mut App) {
        let field = input.entity_id().as_u64();
        if !input.read(cx).focus_handle(cx).is_focused(window) {
            let ours = PUSHED.with_borrow(|p| matches!(p, Some(Some(s)) if s.field == field));
            if ours {
                push(None, force);
            }
            return;
        }
        let state = input.update(cx, |s, cx| {
            let sel = s.selected_text_range(false, window, cx)?.range;
            let want = around(&sel);
            let mut adjusted = None;
            let text = s.text_for_range(want.clone(), &mut adjusted, window, cx)?;
            let start = adjusted.map_or(want.start, |r| r.start);
            let mut end = None;
            s.text_for_range(usize::MAX..usize::MAX, &mut end, window, cx);
            let len = end.map_or(0, |r| r.end);
            Some(State { field, applied: APPLIED.get(), start, text, sel, len })
        });
        push(state, force);
    }

    fn push(state: Option<State>, force: bool) {
        if !force && PUSHED.with_borrow(|p| p.as_ref() == Some(&state)) {
            return;
        }
        let (field, applied, start, sel, len, text) = match &state {
            Some(s) => (s.field, s.applied, s.start, s.sel.clone(), s.len, s.text.as_str()),
            None => (0, APPLIED.get(), 0, 0..0, 0, ""),
        };
        let int = |x: usize| i32::try_from(x).unwrap_or(i32::MAX);
        let sent = with_env(|env| {
            let activity = activity(env)?;
            let text = env.new_string(text).map_err(|e| e.to_string())?;
            let result = env.call_method(
                &activity,
                jni::jni_str!("gpuiImeSync"),
                jni::jni_sig!("(JJIIIILjava/lang/String;)V"),
                &[
                    JValue::Long(field as i64),
                    JValue::Long(applied),
                    JValue::Int(int(start)),
                    JValue::Int(int(sel.start)),
                    JValue::Int(int(sel.end)),
                    JValue::Int(int(len)),
                    JValue::Object(&text),
                ],
            );
            env.exception_clear();
            result.map(|_| ()).map_err(|e| e.to_string())
        });
        match sent {
            Ok(()) => PUSHED.set(Some(state)),
            Err(e) => log::warn!("ime sync: {e}"),
        }
    }

    fn offset(x: i32) -> usize {
        usize::try_from(x).unwrap_or(0)
    }

    fn range(a: i32, b: i32) -> Range<usize> {
        offset(a.min(b))..offset(a.max(b))
    }

    /// # Safety
    /// Called by the JVM with a valid (or null) string reference.
    #[unsafe(no_mangle)]
    #[allow(clippy::too_many_arguments)]
    pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeImeEdit(
        _env: *mut c_void,
        _class: *mut c_void,
        field: i64,
        seq: i64,
        start: i32,
        end: i32,
        text: *mut c_void,
        sel_start: i32,
        sel_end: i32,
        comp_start: i32,
        comp_end: i32,
    ) -> u8 {
        let replace = if start < 0 {
            None
        } else {
            let Ok(text) = with_env(|env| {
                let text = unsafe { JObject::from_raw(env, text as jni::sys::jobject) };
                Ok(get_string(env, &text))
            }) else {
                return 0;
            };
            Some((range(start, end), text))
        };
        let edit = Edit {
            field: field as u64,
            seq,
            replace,
            sel: (sel_start >= 0).then(|| range(sel_start, sel_end)),
            comp: (comp_start >= 0 && comp_end > comp_start).then(|| range(comp_start, comp_end)),
        };
        u8::from(sender().try_send(Msg::Edit(edit)).is_ok())
    }

    /// # Safety
    /// Called by the JVM.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeImeResync(_env: *mut c_void, _class: *mut c_void) {
        let _ = sender().try_send(Msg::Resync);
    }
}

#[cfg(test)]
mod tests {
    use super::{AFTER, BEFORE, Edit, Op, around, plan};

    fn edit(replace: Option<(std::ops::Range<usize>, &str)>, sel: std::ops::Range<usize>, comp: Option<std::ops::Range<usize>>) -> Edit {
        Edit { field: 1, seq: 1, replace: replace.map(|(r, t)| (r, t.to_owned())), sel: Some(sel), comp }
    }

    #[test]
    fn the_window_is_bounded_around_the_selection() {
        assert_eq!(around(&(3..3)), 0..3 + AFTER);
        assert_eq!(around(&(2000..2010)), 2000 - BEFORE..2010 + AFTER);
        assert_eq!(around(&(usize::MAX..usize::MAX)).end, usize::MAX);
    }

    #[test]
    fn typing_into_a_composing_word_is_one_marked_replacement() {
        // "te" composing at 10, Gboard sends "teh".
        let ops = plan(&edit(Some((10..12, "teh")), 13..13, Some(10..13)));
        assert_eq!(ops, vec![Op::ReplaceAndMark(10..12, "teh".into(), 3..3)]);
        // A surrogate pair counts as two units of the composing range.
        let ops = plan(&edit(Some((4..4, "😀")), 6..6, Some(4..6)));
        assert_eq!(ops, vec![Op::ReplaceAndMark(4..4, "😀".into(), 2..2)]);
    }

    #[test]
    fn an_autocorrected_commit_replaces_and_unmarks() {
        // "teh" composing at 10..13 becomes "the " committed.
        let ops = plan(&edit(Some((11..13, "he ")), 14..14, None));
        assert_eq!(ops, vec![Op::Replace(11..13, "he ".into()), Op::Unmark, Op::Select(14..14)]);
    }

    #[test]
    fn recomposing_an_existing_word_only_marks_it() {
        let ops = plan(&edit(None, 7..7, Some(4..7)));
        assert_eq!(ops, vec![Op::Mark(4..7, 3..3)]);
        // A caret outside the composing text is clamped into it.
        let ops = plan(&edit(None, 9..9, Some(4..7)));
        assert_eq!(ops, vec![Op::Mark(4..7, 3..3)]);
    }

    #[test]
    fn a_change_outside_the_composing_word_is_replaced_then_marked() {
        let ops = plan(&edit(Some((0..1, "H")), 7..7, Some(4..7)));
        assert_eq!(ops, vec![Op::Replace(0..1, "H".into()), Op::Mark(4..7, 3..3)]);
    }

    #[test]
    fn a_caret_move_selects_without_text() {
        let ops = plan(&edit(None, 2..5, None));
        assert_eq!(ops, vec![Op::Unmark, Op::Select(2..5)]);
        // Ending a composition when the keyboard goes leaves the caret where the field has it.
        let ops = plan(&Edit { sel: None, ..edit(None, 0..0, None) });
        assert_eq!(ops, vec![Op::Unmark]);
    }
}

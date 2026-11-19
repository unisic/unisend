//! Keeps the Android keyboard's view of the focused text field in step with the field, so the IME
//! has the words around the caret for autocorrect, sentence capitals and prediction.
//!
//! The Java proxy (`GpuiInputActivity.InputProxy`) holds a window of the field's text around the
//! caret. Its edits arrive here as exact UTF-16 replacements (`nativeImeEdit`) and are applied to
//! the field on the GPUI thread. Every change of the field, made by the IME or by the app (sending,
//! a tap moving the caret, a draft, an emoji or a mention), is published back (`gpuiImeSync`) with
//! the sequence number of the last applied edit, so Java tells its own echo from the app's change.
//! Only fields passed to `follow` take part: secrets never reach the proxy as context.
//!
//! The text is drawn by GPUI, so what a native EditText underlines (the word being composed, words
//! the system spell checker marked in the proxy) is kept here and drawn over the field by `marks`.

use std::cell::RefCell;
use std::ops::Range;

use gpui_kit::component::RopeExt as _;
use gpui_kit::component::input::{InputState, TextareaState};
use gpui_kit::{App, ContentMask, Entity, Focusable as _, Hsla, IntoElement, MouseDownEvent, Pixels, Styled as _, Window, canvas, fill, point, px, size};

use crate::Palette;

/// A field the keyboard follows: a one-line one, or the composer, which grows with its text.
#[derive(Clone)]
pub(crate) enum Field {
    Line(Entity<InputState>),
    Area(Entity<TextareaState>),
}

impl From<&Entity<InputState>> for Field {
    fn from(e: &Entity<InputState>) -> Self {
        Self::Line(e.clone())
    }
}

impl From<&Entity<TextareaState>> for Field {
    fn from(e: &Entity<TextareaState>) -> Self {
        Self::Area(e.clone())
    }
}

/// `$body` once per kind of field, with `$e` its entity: the two states share no nameable type.
macro_rules! each {
    ($field:expr, $e:ident => $body:expr) => {
        match $field {
            Field::Line($e) => $body,
            Field::Area($e) => $body,
        }
    };
}

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

/// Misspelled ranges kept per field; the proxy's window holds about 100 words.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
const MAX_SPELL: usize = 64;
/// Underline height above the line box's bottom, in line heights: about where GPUI draws one.
const LIFT: f32 = 0.15;

/// What the keyboard marks in the focused followed field, in UTF-16 offsets of the whole field.
#[derive(Default)]
struct Marks {
    field: u64,
    /// The word the keyboard is composing.
    comp: Option<Range<usize>>,
    /// Misspelled words with the text each covered when reported: a range gone stale is not drawn.
    spell: Vec<(Range<usize>, String)>,
}

thread_local! {
    static MARKS: RefCell<Marks> = RefCell::default();
}

/// Moves spell ranges past an edit that replaced `old` with `new_len` units; ranges it touched go.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn shift(spell: &mut Vec<(Range<usize>, String)>, old: &Range<usize>, new_len: usize) {
    spell.retain_mut(|(r, _)| {
        if r.end <= old.start {
            return true;
        }
        if r.start < old.end {
            return false;
        }
        *r = r.start - old.end + old.start + new_len..r.end - old.end + old.start + new_len;
        true
    });
}

/// Draws the composing underline and the misspelled-word dots over `input` while it has focus.
/// Place it next to the field's `Input`; it takes no space.
pub(crate) fn marks<F: Into<Field>>(input: F) -> impl IntoElement + use<F> {
    let input = input.into();
    canvas(|_, _, _| (), move |_, _, window, cx| paint_marks(&input, window, cx)).absolute().inset_0()
}

/// Brings back the Android keyboard the user hid (Back, swipe down) when a press leaves the focused
/// input focused, as a tap on that input does. Place it once in the window's root; it takes no space.
/// shortcut: any press that moves no focus counts, also one on a disabled button (it stops the root
/// from taking focus); check the press hit the input once gpui-kit reports taps on a focused input.
pub(crate) fn taps() -> impl IntoElement {
    canvas(
        |_, _, _| (),
        |_, _, window, _| {
            window.on_mouse_event(|_: &MouseDownEvent, phase, window, cx| {
                let Some(before) = window.focused(cx).filter(|_| phase.capture()) else {
                    return;
                };
                // The press moves focus in its bubble phase, so the check runs after it.
                window.defer(cx, move |window, cx| {
                    if window.focused(cx).as_ref() == Some(&before) {
                        tapped();
                    }
                });
            });
        },
    )
    .absolute()
}

fn paint_marks(input: &Field, window: &mut Window, cx: &mut App) {
    each!(input, input => {
        let field = input.entity_id().as_u64();
        let (comp, spell) = MARKS.with_borrow(|m| if m.field == field { (m.comp.clone(), m.spell.clone()) } else { (None, Vec::new()) });
        if comp.is_none() && spell.is_empty() {
            return;
        }
        let state = input.read(cx);
        if !state.focus_handle(cx).is_focused(window) {
            return;
        }
        let Some(text_bounds) = state.text_bounds() else {
            return;
        };
        let rope = state.text();
        let bytes = |r: &Range<usize>| rope.offset_utf16_to_offset(r.start)..rope.offset_utf16_to_offset(r.end);
        // One stretch per line the range covers (a word wrapped at the edge has two): left x, right x, underline y.
        let lines = |r: &Range<usize>| -> Vec<(Pixels, Pixels, Pixels)> {
            let (Some(a), Some(b)) = (state.range_to_bounds(&(r.start..r.start)), state.range_to_bounds(&(r.end..r.end))) else {
                return Vec::new();
            };
            let lift = a.size.height * LIFT;
            let stretches = if a.top() == b.top() {
                vec![(a.left(), b.left(), a.bottom() - lift)]
            } else {
                vec![(a.left(), text_bounds.right(), a.bottom() - lift), (text_bounds.left(), b.left(), b.bottom() - lift)]
            };
            stretches.into_iter().filter(|(x0, x1, _)| x1 > x0).collect()
        };
        let p = Palette::of(cx);
        let mut quads = Vec::new();
        if let Some(comp) = comp.as_ref().map(bytes).filter(|r| !r.is_empty()) {
            for (x0, x1, y) in lines(&comp) {
                quads.push(fill(gpui_kit::Bounds::new(point(x0, y), size(x1 - x0, px(1.))), p.text));
            }
        }
        for (range, word) in &spell {
            let range = bytes(range);
            if range.is_empty() || rope.slice(range.clone()) != word.as_str() {
                continue;
            }
            for (x0, x1, y) in lines(&range) {
                quads.extend(dots(x0, x1, y, p.negative));
            }
        }
        window.with_content_mask(Some(ContentMask { bounds: state.input_bounds() }), |window| {
            for quad in quads {
                window.paint_quad(quad);
            }
        });
    })
}

/// A dotted line from `x0` to `x1`: round 2 px dots, 2 px apart, as a spell checker marks a word.
fn dots(x0: Pixels, x1: Pixels, y: Pixels, colour: Hsla) -> impl Iterator<Item = gpui_kit::PaintQuad> {
    const DOT: f32 = 2.;
    let count = (f32::from(x1 - x0) / (DOT * 2.)).ceil() as usize;
    (0..count).map(move |i| fill(gpui_kit::Bounds::new(point(x0 + px(i as f32 * DOT * 2.), y - px(DOT / 2.)), size(px(DOT), px(DOT))), colour).corner_radii(px(DOT / 2.)))
}

#[cfg(target_os = "android")]
pub(crate) use android::{follow, on_picture, set_incognito, tapped};

#[cfg(not(target_os = "android"))]
pub(crate) fn follow<T: 'static>(_: impl Into<Field>, _: &mut gpui_kit::Window, _: &mut gpui_kit::Context<T>) {}

/// Settings > Privacy > Incognito keyboard; only the Android keyboard has the flag.
#[cfg(not(target_os = "android"))]
pub(crate) fn set_incognito(_: bool) {}

/// A press that left the focused input focused; only the Android keyboard can have been hidden by the user.
#[cfg(not(target_os = "android"))]
pub(crate) fn tapped() {}

/// Where a picture the Android keyboard inserts goes (its clipboard, stickers); desktop pastes on its own.
#[cfg(not(target_os = "android"))]
pub(crate) fn on_picture(_: impl Fn(std::path::PathBuf, &mut gpui_kit::App) + 'static) {}

#[cfg(target_os = "android")]
mod android {
    use std::cell::{Cell, RefCell};
    use std::ffi::c_void;
    use std::ops::Range;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
    use gpui_kit::{AnyEntity, AnyWeakEntity, App, Context, EntityInputHandler, Focusable as _, Window};
    use gpui_mobile::android::jni::{activity, get_string, with_env};
    use jni::objects::{JIntArray, JObject, JValue};
    use tokio::sync::mpsc;

    use super::{Edit, Field, MARKS, MAX_SPELL, Op, around, plan, shift};

    impl Field {
        fn id(&self) -> u64 {
            each!(self, e => e.entity_id().as_u64())
        }

        fn of(any: AnyEntity) -> Option<Self> {
            match any.downcast::<InputState>() {
                Ok(e) => Some(Self::Line(e)),
                Err(any) => any.downcast::<TextareaState>().ok().map(Self::Area),
            }
        }
    }

    enum Msg {
        Edit(Edit),
        Resync,
        /// Misspelled ranges of a field, all of them, in field UTF-16 offsets.
        Spell(u64, Vec<Range<usize>>),
        /// A picture the keyboard inserted, already copied into the cache.
        Picture(PathBuf),
        /// A hardware keyboard's Enter without Shift in this field: a submit, as on a desktop.
        Enter(u64),
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
        static FIELDS: RefCell<Vec<AnyWeakEntity>> = const { RefCell::new(Vec::new()) };
        static APPLIED: Cell<i64> = const { Cell::new(0) };
        /// What Java was last told: `None` before the first push, `Some(None)` for no field.
        static PUSHED: RefCell<Option<Option<State>>> = const { RefCell::new(None) };
        static ON_PICTURE: RefCell<Option<Box<dyn Fn(PathBuf, &mut App)>>> = RefCell::new(None);
    }

    pub(crate) fn on_picture(f: impl Fn(PathBuf, &mut App) + 'static) {
        ON_PICTURE.set(Some(Box::new(f)));
    }

    /// Mirror this field to the keyboard while it is focused. The subscriptions end with the field.
    pub(crate) fn follow<T: 'static>(input: impl Into<Field>, window: &mut Window, cx: &mut Context<T>) {
        let input = input.into();
        FIELDS.with_borrow_mut(|fields| {
            fields.retain(|f| f.upgrade().is_some());
            fields.push(each!(&input, e => e.downgrade().into()));
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
        each!(&input, input => {
            cx.observe_in(input, window, |_, input, window, cx| publish(&Field::from(&input), false, window, cx)).detach();
            cx.subscribe_in(input, window, |_, input, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::Focus | InputEvent::Blur) {
                    publish(&Field::from(input), false, window, cx);
                }
            })
            .detach();
        });
    }

    fn focused(window: &Window, cx: &App) -> Option<Field> {
        FIELDS.with_borrow(|fields| {
            fields.iter().filter_map(|f| Field::of(f.upgrade()?)).find(|f| each!(f, e => e.read(cx).focus_handle(cx).is_focused(window)))
        })
    }

    fn find(field: u64) -> Option<Field> {
        FIELDS.with_borrow(|fields| fields.iter().filter_map(|f| Field::of(f.upgrade()?)).find(|f| f.id() == field))
    }

    fn handle(msg: Msg, window: &mut Window, cx: &mut App) {
        if let Msg::Enter(field) = msg {
            if let Some(target) = find(field) {
                each!(target, e => e.update(cx, |_, cx| cx.emit(InputEvent::PressEnter { secondary: false, shift: false })));
            }
            return;
        }
        if let Msg::Picture(path) = msg {
            match ON_PICTURE.take() {
                Some(f) => {
                    f(path, cx);
                    ON_PICTURE.set(Some(f));
                }
                None => crate::share::discard(&path),
            }
            return;
        }
        if let Msg::Edit(edit) = &msg {
            APPLIED.set(edit.seq);
            MARKS.with_borrow_mut(|m| {
                if m.field != edit.field {
                    *m = super::Marks { field: edit.field, ..Default::default() };
                }
                if let Some((old, text)) = &edit.replace {
                    shift(&mut m.spell, old, text.encode_utf16().count());
                }
                m.comp = edit.comp.clone();
            });
            if let Some(target) = find(edit.field) {
                // The keyboard's Enter arrives as a typed newline, which a one-line field drops.
                let enter = matches!(target, Field::Line(_)) && edit.replace.as_ref().is_some_and(|(_, text)| text == "\n");
                each!(target, e => e.update(cx, |state, cx| {
                    apply(state, edit, window, cx);
                    if enter {
                        cx.emit(InputEvent::PressEnter { secondary: false, shift: false });
                    }
                    // A composition that only ended changes no text, yet its underline must go.
                    cx.notify();
                }));
            }
        }
        if let Msg::Spell(field, ranges) = &msg {
            if let Some(target) = find(*field) {
                each!(target, e => e.update(cx, |state, cx| {
                    let spell = ranges
                        .iter()
                        .filter_map(|r| {
                            let mut adjusted = None;
                            let word = state.text_for_range(r.clone(), &mut adjusted, window, cx)?;
                            (adjusted.as_ref() == Some(r)).then(|| (r.clone(), word))
                        })
                        .collect();
                    MARKS.with_borrow_mut(|m| {
                        if m.field != *field {
                            *m = super::Marks { field: *field, ..Default::default() };
                        }
                        m.spell = spell;
                    });
                    cx.notify();
                }));
            }
            return;
        }
        match focused(window, cx) {
            Some(input) => publish(&input, matches!(msg, Msg::Resync), window, cx),
            None => push(None, matches!(msg, Msg::Resync)),
        }
    }

    fn apply<T: EntityInputHandler>(s: &mut T, edit: &Edit, window: &mut Window, cx: &mut Context<T>) {
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

    fn publish(input: &Field, force: bool, window: &mut Window, cx: &mut App) {
        let field = input.id();
        if !each!(input, e => e.read(cx).focus_handle(cx).is_focused(window)) {
            let ours = PUSHED.with_borrow(|p| matches!(p, Some(Some(s)) if s.field == field));
            if ours {
                push(None, force);
            }
            return;
        }
        let state = each!(input, e => e.update(cx, |s, cx| {
            let sel = s.selected_text_range(false, window, cx)?.range;
            let want = around(&sel);
            let mut adjusted = None;
            let text = s.text_for_range(want.clone(), &mut adjusted, window, cx)?;
            let start = adjusted.map_or(want.start, |r| r.start);
            let mut end = None;
            s.text_for_range(usize::MAX..usize::MAX, &mut end, window, cx);
            let len = end.map_or(0, |r| r.end);
            Some(State { field, applied: APPLIED.get(), start, text, sel, len })
        }));
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
    pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeImeEnter(_env: *mut c_void, _class: *mut c_void, field: i64) {
        let _ = sender().try_send(Msg::Enter(field as u64));
    }

    /// # Safety
    /// Called by the JVM.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeImeResync(_env: *mut c_void, _class: *mut c_void) {
        let _ = sender().try_send(Msg::Resync);
    }

    /// # Safety
    /// Called by the JVM with a valid (or null) `int[]` of start, end pairs.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeImeSpell(_env: *mut c_void, _class: *mut c_void, field: i64, ranges: *mut c_void) -> u8 {
        let Ok(flat) = with_env(|env| {
            let array = unsafe { JIntArray::from_raw(env, ranges as jni::sys::jarray) };
            let len = array.len(env).map_err(|e| e.to_string())?.min(MAX_SPELL * 2);
            let mut flat = vec![0; len];
            array.get_region(env, 0, &mut flat).map_err(|e| e.to_string())?;
            Ok(flat)
        }) else {
            return 0;
        };
        let ranges = flat.chunks_exact(2).filter(|p| 0 <= p[0] && p[0] < p[1]).map(|p| range(p[0], p[1])).collect();
        u8::from(sender().try_send(Msg::Spell(field as u64, ranges)).is_ok())
    }

    /// # Safety
    /// Called by the JVM with a valid (or null) string reference.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_dev_gpui_mobile_GpuiInputActivity_nativeImePicture(_env: *mut c_void, _class: *mut c_void, path: *mut c_void) {
        let Ok(path) = with_env(|env| Ok(get_string(env, &unsafe { JObject::from_raw(env, path as jni::sys::jobject) }))) else { return };
        let path = PathBuf::from(path);
        if let Err(e) = sender().try_send(Msg::Picture(path.clone())) {
            log::warn!("keyboard picture dropped: {e}");
            crate::share::discard(&path);
        }
    }

    /// Settings > Privacy > Incognito keyboard: whether text fields ask the keyboard not to learn.
    pub(crate) fn set_incognito(on: bool) {
        let sent = with_env(|env| {
            let activity = activity(env)?;
            let result = env.call_method(&activity, jni::jni_str!("gpuiImeIncognito"), jni::jni_sig!("(Z)V"), &[JValue::Bool(on)]);
            env.exception_clear();
            result.map(|_| ()).map_err(|e| e.to_string())
        });
        if let Err(e) = sent {
            log::warn!("ime incognito: {e}");
        }
    }

    /// A press that left the focused input focused: brings back a keyboard the user hid (Back, swipe down).
    pub(crate) fn tapped() {
        let sent = with_env(|env| {
            let activity = activity(env)?;
            let result = env.call_method(&activity, jni::jni_str!("gpuiTapField"), jni::jni_sig!("()V"), &[]);
            env.exception_clear();
            result.map(|_| ()).map_err(|e| e.to_string())
        });
        if let Err(e) = sent {
            log::warn!("ime tap: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AFTER, BEFORE, Edit, Op, around, plan, shift};

    fn edit(replace: Option<(std::ops::Range<usize>, &str)>, sel: std::ops::Range<usize>, comp: Option<std::ops::Range<usize>>) -> Edit {
        Edit { field: 1, seq: 1, replace: replace.map(|(r, t)| (r, t.to_owned())), sel: Some(sel), comp }
    }

    #[test]
    fn spell_ranges_follow_edits_before_them_and_go_with_edits_inside() {
        let words = |v: &[(std::ops::Range<usize>, &str)]| v.iter().map(|(r, w)| (r.clone(), (*w).to_owned())).collect::<Vec<_>>();
        // "teh qick fox": misspelled "teh" 0..3 and "qick" 4..8.
        let mut spell = words(&[(0..3, "teh"), (4..8, "qick")]);
        // Typing "x" at the end moves nothing.
        shift(&mut spell, &(12..12), 1);
        assert_eq!(spell, words(&[(0..3, "teh"), (4..8, "qick")]));
        // "teh" corrected to "the " (4 units over 3, with the space): it goes, "qick" moves by one.
        shift(&mut spell, &(0..3), 4);
        assert_eq!(spell, words(&[(5..9, "qick")]));
        // An emoji (two units) inserted right before the word moves it; touching its end removes it.
        shift(&mut spell, &(5..5), 2);
        assert_eq!(spell, words(&[(7..11, "qick")]));
        shift(&mut spell, &(10..11), 0);
        assert!(spell.is_empty());
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

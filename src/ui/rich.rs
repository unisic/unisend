//! A formatted message body drawn inside a bubble: styled runs, quotes, code, lists. A spoiler is
//! covered by a block of its color until a tap on it uncovers the message's spoilers, as Signal
//! does; uncovered messages are remembered for the session, at most `MAX_SHOWN`.

use std::collections::VecDeque;
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;

use super::appearance::fs;
use super::taps;
use crate::Palette;
use crate::backend::{Block, Msg, Rich, Run};

/// Monospace face for code. GPUI resolves a family by name only, so the name is the platform's.
#[cfg(not(target_os = "android"))]
pub(super) const MONO: &str = "Noto Sans Mono";
#[cfg(target_os = "android")]
pub(super) const MONO: &str = "Droid Sans Mono";

/// Messages whose spoilers were uncovered, newest last.
const MAX_SHOWN: usize = 256;

#[derive(Default)]
struct Shown(VecDeque<String>);

impl Global for Shown {}

/// What the drawing needs besides the message: its palette, how a mention is drawn, whether its
/// spoilers are uncovered and the view to redraw when they are.
#[derive(Clone, Copy)]
struct Look {
    p: Palette,
    mention: HighlightStyle,
    shown: bool,
    view: EntityId,
}

pub(super) fn body(m: &Msg, rich: &Rich, p: Palette, mention: HighlightStyle, view: EntityId, cx: &App) -> AnyElement {
    let shown = cx.try_global::<Shown>().is_some_and(|s| s.0.contains(&m.id));
    let look = Look { p, mention, shown, view };
    let mut n = 0;
    blocks(m, &rich.blocks, look, &mut n).into_any_element()
}

fn blocks(m: &Msg, list: &[Block], look: Look, n: &mut usize) -> Div {
    v_flex().min_w_0().gap(px(4.)).children(list.iter().map(|block| one(m, block, look, n)))
}

fn one(m: &Msg, block: &Block, look: Look, n: &mut usize) -> AnyElement {
    *n += 1;
    match block {
        Block::Para(runs) => para(m, runs, look, *n),
        Block::Quote(inside) => h_flex()
            .items_stretch()
            .gap(px(10.))
            .child(div().w(px(3.)).flex_none().rounded_full().bg(look.p.faint))
            .child(blocks(m, inside, look, n).flex_1().text_color(look.p.muted))
            .into_any_element(),
        Block::Code(code, _) => div()
            .min_w_0()
            .px(px(10.))
            .py(px(6.))
            .rounded(px(10.))
            .bg(look.p.text.opacity(0.07))
            .font_family(MONO)
            .text_size(fs(14.))
            .line_height(fs(20.))
            .child(StyledText::new(code.clone()))
            .into_any_element(),
        Block::List { start, items } => v_flex()
            .min_w_0()
            .gap(px(2.))
            .children(items.iter().enumerate().map(|(i, item)| {
                let mark = match start {
                    Some(first) => format!("{}.", first.saturating_add(i as u64)),
                    None => "\u{2022}".to_owned(),
                };
                h_flex()
                    .items_start()
                    .gap(px(6.))
                    .child(div().flex_none().min_w(px(if start.is_some() { 20. } else { 12. })).text_color(look.p.muted).child(mark))
                    .child(blocks(m, item, look, n).flex_1())
            }))
            .into_any_element(),
    }
}

fn para(m: &Msg, runs: &[Run], look: Look, n: usize) -> AnyElement {
    let text: String = runs.iter().map(|r| r.text.as_str()).collect();
    let Some(base) = runs.first().map(|r| r.at) else { return div().into_any_element() };
    let whole = base..base + text.len();
    // Mentions, bare addresses and codes are ranges of the whole message; here they are shifted to the paragraph.
    let inside = |r: &Range<usize>| r.start >= whole.start && r.end <= whole.end;
    let shift = |r: &Range<usize>| r.start - base..r.end - base;
    let mentions: Vec<_> = m.mentions.iter().filter(|r| inside(r)).map(shift).collect();
    let to_me: Vec<_> = m.to_me.iter().filter(|r| inside(r)).map(shift).collect();
    let apart = |u: &Range<usize>| mentions.iter().all(|r| r.end <= u.start || u.end <= r.start);
    let bare: Vec<_> = taps(&m.body).into_iter().filter(|(r, _)| inside(r)).map(|(r, to)| (shift(&r), to)).filter(|(r, _)| apart(r)).collect();

    let mut cuts: Vec<usize> = vec![0, text.len()];
    cuts.extend(runs.iter().map(|r| r.at - base));
    cuts.extend(mentions.iter().chain(bare.iter().map(|(r, _)| r)).flat_map(|r| [r.start, r.end]));
    cuts.sort_unstable();
    cuts.dedup();

    let (mut marks, mut links, mut targets, mut hidden) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for pair in cuts.windows(2) {
        let seg = pair[0]..pair[1];
        let Some(run) = runs.iter().rev().find(|r| r.at - base <= seg.start) else { continue };
        if run.spoiler && !look.shown {
            // Text and fill in one color: the words take their room but cannot be read.
            let cover = look.p.muted;
            marks.push((seg.clone(), HighlightStyle { color: Some(cover), background_color: Some(cover), ..Default::default() }));
            hidden.push(seg);
            continue;
        }
        let mut style = HighlightStyle::default();
        if run.bold {
            style.font_weight = Some(FontWeight::SEMIBOLD);
        }
        if run.italic {
            style.font_style = Some(FontStyle::Italic);
        }
        if run.strike {
            style.strikethrough = Some(StrikethroughStyle { thickness: px(1.), ..Default::default() });
        }
        if run.code {
            style.background_color = Some(look.p.text.opacity(0.12));
        }
        let linked = run.href.is_some() || bare.iter().any(|(r, _)| r.start <= seg.start && seg.end <= r.end);
        if run.underline || linked {
            style.underline = Some(UnderlineStyle { thickness: px(1.), ..Default::default() });
        }
        if mentions.iter().any(|r| r.start <= seg.start && seg.end <= r.end) {
            // The accent is for what addresses the user; a mention of someone else is only bold.
            let color = if m.mentioned && to_me.iter().any(|r| r.start <= seg.start && seg.end <= r.end) { look.mention.color } else { None };
            style = HighlightStyle { color, font_weight: look.mention.font_weight, ..style };
        }
        if let Some(href) = &run.href {
            let shown = link_text(runs, run);
            links.push(seg.clone());
            // A check travels in the target: after a newline, which no link this app opens holds.
            targets.push(if super::misleading(&shown, href) { format!("\n{href}\n{shown}") } else { href.clone() });
        } else if let Some((_, to)) = bare.iter().find(|(r, _)| r.start <= seg.start && seg.end <= r.end) {
            links.push(seg.clone());
            targets.push(to.clone());
        }
        if style != HighlightStyle::default() {
            marks.push((seg, style));
        }
    }

    let text = SharedString::from(text);
    let styled = crate::emoji::styled(text.clone(), marks);
    let layout = styled.layout().clone();
    let id = format!("{}-rich-{n}", m.id);
    let text = div().min_w_0().child(linked(id.clone(), text, styled, links, targets));
    if hidden.is_empty() {
        return text.into_any_element();
    }
    let (msg, view) = (m.id.clone(), look.view);
    text.id(SharedString::from(format!("{id}-spoiler")))
        .on_click(move |e, _, cx| {
            if let Ok(ix) = layout.index_for_position(e.position())
                && hidden.iter().any(|r| r.contains(&ix))
            {
                // The bubble under the text opens the message's menu on a click; a spoiler does not.
                cx.stop_propagation();
                let shown = &mut cx.default_global::<Shown>().0;
                if !shown.contains(&msg) {
                    if shown.len() == MAX_SHOWN {
                        shown.pop_front();
                    }
                    shown.push_back(msg.clone());
                }
                cx.notify(view);
            }
        })
        .into_any_element()
}

/// The whole text of the link `run` is part of: the runs beside it with the same address, as a link
/// with bold words in it is cut into.
fn link_text(runs: &[Run], run: &Run) -> String {
    let Some(at) = runs.iter().position(|r| std::ptr::eq(r, run)) else { return run.text.clone() };
    let same = |r: &&Run| r.href == run.href;
    let first = at - runs[..at].iter().rev().take_while(same).count();
    let last = at + runs[at + 1..].iter().take_while(same).count();
    runs[first..=last].iter().map(|r| r.text.as_str()).collect()
}

/// `text`, drawn as `styled`, whose `links` ranges open the matching `targets` on a click.
pub(super) fn linked(id: String, text: SharedString, styled: StyledText, links: Vec<Range<usize>>, targets: Vec<String>) -> AnyElement {
    let layout = styled.layout().clone();
    let links: Rc<[Range<usize>]> = links.into();
    let body = Body { id: SharedString::from(id.clone()).into(), text, styled, links: links.clone() };
    if links.is_empty() {
        return body.into_any_element();
    }
    div()
        .id(SharedString::from(format!("{id}-tap")))
        // InteractiveText hears a release only a frame after the press; a tap on Android brings both at once.
        .on_click(move |e, window, cx| {
            if let Ok(ix) = layout.index_for_position(e.position())
                && let Some(at) = links.iter().position(|r| r.contains(&ix))
                && !base::TextSelection::has_selection(window, cx)
            {
                // The bubble under the text opens the message's menu on a click; a link does not. The
                // selection the press began would miss this release and follow the pointer.
                base::TextSelection::end(window, cx);
                cx.stop_propagation();
                follow(cx, &targets[at]);
            }
        })
        .child(body)
        .into_any_element()
}

/// A message's text. On a computer the mouse selects it as in a browser: a drag, a double click for
/// a word, a triple for a line, and Ctrl+C copies; gpui-kit's `TextSelectionLayer` in the window's
/// `Root` does that work. Over a link the pointer is a hand.
struct Body {
    id: ElementId,
    text: SharedString,
    styled: StyledText,
    links: Rc<[Range<usize>]>,
}

impl Body {
    fn on_link(&self, at: Point<Pixels>) -> bool {
        self.styled.layout().index_for_position(at).is_ok_and(|ix| self.links.iter().any(|r| r.contains(&ix)))
    }
}

impl IntoElement for Body {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Body {
    /// `None` on a phone, where a long press on a bubble is its menu and "Select text" selects.
    type RequestLayoutState = Option<base::TextSelectionHandle>;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, id: Option<&GlobalElementId>, inspector: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        let handle = (!gpui_kit::is_mobile()).then(|| {
            // A drag changes the selection between frames; without the refresh the window draws it
            // only when something else redraws, so the highlight trails the pointer in jumps.
            window.with_element_state(id.expect("Body has an id"), |kept: Option<(base::TextSelectionHandle, Subscription)>, window| {
                let kept = kept.unwrap_or_else(|| {
                    let handle = base::TextSelectionHandle::new(self.text.to_string(), cx);
                    let refresh = handle.refresh_window_on_change(window, cx);
                    (handle, refresh)
                });
                (kept.0.clone(), kept)
            })
        });
        (self.styled.request_layout(id, inspector, window, cx).0, handle)
    }

    fn prepaint(&mut self, id: Option<&GlobalElementId>, inspector: Option<&InspectorElementId>, bounds: Bounds<Pixels>, handle: &mut Self::RequestLayoutState, window: &mut Window, cx: &mut App) -> Hitbox {
        self.styled.prepaint(id, inspector, bounds, &mut (), window, cx);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        if let Some(handle) = handle {
            handle.register(base::TextSelectionRegistration::new(hitbox.clone(), bounds).with_text_bounds(vec![bounds]), window, cx);
        }
        hitbox
    }

    fn paint(&mut self, id: Option<&GlobalElementId>, inspector: Option<&InspectorElementId>, bounds: Bounds<Pixels>, handle: &mut Self::RequestLayoutState, hitbox: &mut Hitbox, window: &mut Window, cx: &mut App) {
        let layout = self.styled.layout().clone();
        if let Some(handle) = handle {
            let before = base::TextSelection::selected_text(window, cx);
            let shown = handle.update_runs(&[base::TextSelectionRun::new(self.text.clone(), layout.clone(), bounds)], cx);
            if before != base::TextSelection::selected_text(window, cx) {
                window.refresh();
            }
            let color = window.text_style().color.opacity(0.28);
            for range in shown.ranges().iter().flatten() {
                let (Some(start), Some(end)) = (layout.position_for_index(range.start), layout.position_for_index(range.end)) else { continue };
                for quad in selection_quads(start, end, bounds, layout.line_height()) {
                    window.paint_quad(fill(quad, color));
                }
            }
        }
        let on_link = !self.links.is_empty() && self.on_link(window.mouse_position());
        if on_link {
            window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        } else if handle.is_some() {
            window.set_cursor_style(CursorStyle::IBeam, hitbox);
        }
        if !self.links.is_empty() {
            // The hand comes and goes inside one hitbox, so a move across a link's edge draws again.
            let ranges = self.links.clone();
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, _| {
                if phase.bubble() && on_link != layout.index_for_position(e.position).is_ok_and(|ix| ranges.iter().any(|r| r.contains(&ix))) {
                    window.refresh();
                }
            });
        }
        self.styled.paint(id, inspector, bounds, &mut (), &mut (), window, cx);
    }
}

/// The rectangles a selection from `start` to `end` covers: the rest of the first line, the whole
/// lines between, the start of the last.
fn selection_quads(start: Point<Pixels>, end: Point<Pixels>, bounds: Bounds<Pixels>, line: Pixels) -> Vec<Bounds<Pixels>> {
    if start.y == end.y {
        return vec![Bounds::from_corners(start, point(end.x, end.y + line))];
    }
    let mut quads = vec![Bounds::from_corners(start, point(bounds.right(), start.y + line))];
    if end.y > start.y + line {
        quads.push(Bounds::from_corners(point(bounds.left(), start.y + line), point(bounds.right(), end.y)));
    }
    quads.push(Bounds::from_corners(point(bounds.left(), end.y), point(end.x, end.y + line)));
    quads
}

/// Follows a target of `taps` or a link's own: a misleading link goes to the view to be confirmed, a
/// Matrix ID, alias or link to the view to offer a chat, another address with a scheme opens in the
/// app for it, and digits are copied (Android shows that they were). A computer has no dialer, so a
/// phone number is copied there.
fn follow(cx: &mut App, target: &str) {
    if let Some((url, shown)) = target.strip_prefix('\n').and_then(|t| t.split_once('\n')) {
        cx.set_global(super::Tapped::Check { url: url.to_owned(), shown: shown.to_owned() });
    } else if target.starts_with(['@', '#']) || target.starts_with("matrix:") || target.contains("://matrix.to/#/") {
        cx.set_global(super::Tapped::Chat(target.to_owned()));
    } else if let Some(number) = target.strip_prefix("tel:").filter(|_| !cfg!(target_os = "android")) {
        crate::copy(cx, number.to_owned());
    } else if target.contains(':') {
        crate::open_link(cx, target);
    } else {
        crate::copy(cx, target.to_owned());
    }
}

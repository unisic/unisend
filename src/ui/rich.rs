//! A formatted message body drawn inside a bubble: styled runs, quotes, code, lists. A spoiler is
//! covered by a block of its colour until a tap on it uncovers the message's spoilers, as Signal
//! does; uncovered messages are remembered for the session, at most `MAX_SHOWN`.

use std::collections::VecDeque;
use std::ops::Range;

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
            // Text and fill in one colour: the words take their room but cannot be read.
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
            links.push(seg.clone());
            targets.push(href.clone());
        } else if let Some((_, to)) = bare.iter().find(|(r, _)| r.start <= seg.start && seg.end <= r.end) {
            links.push(seg.clone());
            targets.push(to.clone());
        }
        if style != HighlightStyle::default() {
            marks.push((seg, style));
        }
    }

    let styled = crate::emoji::styled(text, marks);
    let layout = styled.layout().clone();
    let id = format!("{}-rich-{n}", m.id);
    let text = div().min_w_0().child(linked(id.clone(), styled, links, targets));
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

/// Text whose `links` ranges open the matching `targets` on a click.
pub(super) fn linked(id: String, styled: StyledText, links: Vec<Range<usize>>, targets: Vec<String>) -> AnyElement {
    if links.is_empty() {
        return styled.into_any_element();
    }
    let layout = styled.layout().clone();
    let (ranges, urls) = (links.clone(), targets.clone());
    div()
        .id(SharedString::from(format!("{id}-tap")))
        // InteractiveText hears a release only a frame after the press; a tap on Android brings both at once.
        .on_click(move |e, _, cx| {
            if let Ok(ix) = layout.index_for_position(e.position())
                && let Some(at) = ranges.iter().position(|r| r.contains(&ix))
            {
                cx.stop_propagation();
                follow(cx, &urls[at]);
            }
        })
        .child(InteractiveText::new(SharedString::from(id), styled).on_click(links, move |at, _, cx| {
            // The bubble under the text opens the message's menu on a click; a link does not.
            cx.stop_propagation();
            follow(cx, &targets[at]);
        }))
        .into_any_element()
}

/// Follows a target of `taps` or a link's own: an address with a scheme opens in the app for it, a
/// Matrix ID or room alias goes to the view to offer a chat, and digits are copied (Android shows
/// that they were). A computer has no dialer, so a phone number is copied there.
fn follow(cx: &mut App, target: &str) {
    if target.starts_with(['@', '#']) {
        cx.set_global(super::Tapped(target.to_owned()));
    } else if let Some(number) = target.strip_prefix("tel:").filter(|_| !cfg!(target_os = "android")) {
        crate::copy(cx, number.to_owned());
    } else if target.contains(':') {
        crate::open_link(cx, target);
    } else {
        crate::copy(cx, target.to_owned());
    }
}

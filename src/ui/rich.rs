//! A formatted message body drawn inside a bubble: styled runs, quotes, code, lists.

use std::ops::Range;

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;

use super::appearance::fs;
use super::urls;
use crate::Palette;
use crate::backend::{Block, Msg, Rich, Run};

/// Monospace face for code. GPUI resolves a family by name only, so the name is the platform's.
#[cfg(not(target_os = "android"))]
pub(super) const MONO: &str = "Noto Sans Mono";
#[cfg(target_os = "android")]
pub(super) const MONO: &str = "Droid Sans Mono";

/// What the drawing needs besides the message: its palette and how a mention is drawn.
#[derive(Clone, Copy)]
struct Look {
    p: Palette,
    mention: HighlightStyle,
}

pub(super) fn body(m: &Msg, rich: &Rich, p: Palette, mention: HighlightStyle) -> AnyElement {
    let look = Look { p, mention };
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
    // Mentions and bare addresses are ranges of the whole message; here they are shifted to the paragraph.
    let inside = |r: &Range<usize>| r.start >= whole.start && r.end <= whole.end;
    let shift = |r: &Range<usize>| r.start - base..r.end - base;
    let mentions: Vec<_> = m.mentions.iter().filter(|r| inside(r)).map(shift).collect();
    let to_me: Vec<_> = m.to_me.iter().filter(|r| inside(r)).map(shift).collect();
    let apart = |u: &Range<usize>| mentions.iter().all(|r| r.end <= u.start || u.end <= r.start);
    let bare: Vec<_> = urls(&m.body).iter().filter(|r| inside(r)).map(shift).filter(apart).collect();

    let mut cuts: Vec<usize> = vec![0, text.len()];
    cuts.extend(runs.iter().map(|r| r.at - base));
    cuts.extend(mentions.iter().chain(&bare).flat_map(|r| [r.start, r.end]));
    cuts.sort_unstable();
    cuts.dedup();

    let (mut marks, mut links, mut targets) = (Vec::new(), Vec::new(), Vec::new());
    for pair in cuts.windows(2) {
        let seg = pair[0]..pair[1];
        let Some(run) = runs.iter().rev().find(|r| r.at - base <= seg.start) else { continue };
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
        let linked = run.href.is_some() || bare.iter().any(|r| r.start <= seg.start && seg.end <= r.end);
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
        } else if let Some(url) = bare.iter().find(|r| r.start <= seg.start && seg.end <= r.end) {
            links.push(seg.clone());
            targets.push(text[url.clone()].to_owned());
        }
        if style != HighlightStyle::default() {
            marks.push((seg, style));
        }
    }

    let styled = crate::emoji::styled(text, marks);
    div().min_w_0().child(linked(format!("{}-rich-{n}", m.id), styled, links, targets)).into_any_element()
}

/// Text whose `links` ranges open the matching `targets` on a click.
pub(super) fn linked(id: String, styled: StyledText, links: Vec<Range<usize>>, targets: Vec<String>) -> AnyElement {
    if links.is_empty() {
        return styled.into_any_element();
    }
    InteractiveText::new(SharedString::from(id), styled)
        .on_click(links, move |at, _, cx| {
            // The bubble under the text opens the message's menu on a click; a link does not.
            cx.stop_propagation();
            crate::open_link(cx, &targets[at]);
        })
        .into_any_element()
}

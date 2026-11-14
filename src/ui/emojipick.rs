//! The emoji picker: a panel that stands in for the keyboard under the composer, and a sheet
//! opened by the + of the reaction bar. Data is the `emojis` crate (Unicode CLDR names and
//! GitHub shortcodes), drawn in the app's emoji font. The grid is a `uniform_list` of rows, so
//! only the rows on screen are built, whatever the 1900 emoji.

use std::path::Path;

use emojis::{Group, SkinTone};
use super::*;
use crate::emoji;

/// Recents kept, newest first.
const RECENTS: usize = 32;
const FILE: &str = "emoji-recents.json";
/// One cell of the grid, and the height of a row; a category's name takes a row of its own.
const CELL: f32 = 44.;
const GLYPH: f32 = 28.;
/// The panel's height until the keyboard has been seen, and with it up for the search field.
const PANEL_H: f32 = 300.;
const PANEL_SEARCH_H: f32 = 220.;
/// Taller than this at the bottom is the keyboard, not the navigation bar.
const KEYBOARD_MIN: f32 = 120.;
const SHEET_W: f32 = 480.;
const TONES: [SkinTone; 6] =
    [SkinTone::Default, SkinTone::Light, SkinTone::MediumLight, SkinTone::Medium, SkinTone::MediumDark, SkinTone::Dark];

/// What a chosen emoji goes to.
#[derive(Clone, PartialEq, Debug)]
pub(super) enum Target {
    /// The composer's cursor; the panel stays open for the next one.
    Composer,
    /// A reaction to the message with this ID; the sheet closes.
    React(String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Cat {
    Recent,
    Group(Group),
}

impl Cat {
    const ALL: [Cat; 10] = [
        Cat::Recent,
        Cat::Group(Group::SmileysAndEmotion),
        Cat::Group(Group::PeopleAndBody),
        Cat::Group(Group::AnimalsAndNature),
        Cat::Group(Group::FoodAndDrink),
        Cat::Group(Group::TravelAndPlaces),
        Cat::Group(Group::Activities),
        Cat::Group(Group::Objects),
        Cat::Group(Group::Symbols),
        Cat::Group(Group::Flags),
    ];

    fn label(self) -> &'static str {
        match self {
            Cat::Recent => "Recent",
            Cat::Group(Group::SmileysAndEmotion) => "Smileys",
            Cat::Group(Group::PeopleAndBody) => "People",
            Cat::Group(Group::AnimalsAndNature) => "Animals",
            Cat::Group(Group::FoodAndDrink) => "Food",
            Cat::Group(Group::TravelAndPlaces) => "Travel",
            Cat::Group(Group::Activities) => "Activities",
            Cat::Group(Group::Objects) => "Objects",
            Cat::Group(Group::Symbols) => "Symbols",
            Cat::Group(Group::Flags) => "Flags",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Cat::Recent => IconName::Clock,
            Cat::Group(Group::SmileysAndEmotion) => IconName::FaceSlightlySmiling,
            Cat::Group(Group::PeopleAndBody) => IconName::Hand,
            Cat::Group(Group::AnimalsAndNature) => IconName::PawPrint,
            Cat::Group(Group::FoodAndDrink) => IconName::Pizza,
            Cat::Group(Group::TravelAndPlaces) => IconName::Plane,
            Cat::Group(Group::Activities) => IconName::Trophy,
            Cat::Group(Group::Objects) => IconName::Lightbulb,
            Cat::Group(Group::Symbols) => IconName::Hash,
            Cat::Group(Group::Flags) => IconName::Flag,
        }
    }
}

/// Puts `emoji` first, once.
fn remember(recents: &mut Vec<&'static str>, emoji: &'static str) {
    recents.retain(|r| *r != emoji);
    recents.insert(0, emoji);
    recents.truncate(RECENTS);
}

enum Row {
    Head(Cat),
    Cells(Vec<&'static str>),
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Saved {
    recents: Vec<String>,
    tone: usize,
}

/// The emoji of `group` (or all, for a search) in the chosen tone: the neutral variant of each,
/// swapped for the tone's when it has one.
fn shown(tone: usize) -> impl Iterator<Item = &'static emojis::Emoji> {
    let tone = TONES.get(tone).copied().unwrap_or(SkinTone::Default);
    emojis::iter().filter(|e| e.skin_tone().is_none_or(|t| t == SkinTone::Default)).map(move |e| {
        if tone == SkinTone::Default { e } else { e.with_skin_tone(tone).unwrap_or(e) }
    })
}

fn matches(e: &emojis::Emoji, query: &str) -> bool {
    e.name().to_lowercase().contains(query) || e.shortcodes().any(|s| s.contains(query))
}

/// The grid for `cols` columns, plus the row each category starts at.
fn build(cols: usize, query: &str, tone: usize, recents: &[&'static str]) -> (Vec<Row>, Vec<(Cat, usize)>) {
    let cols = cols.max(1);
    let (mut rows, mut starts) = (Vec::new(), Vec::new());
    let query = query.trim().to_lowercase();
    let mut push = |cat: Option<Cat>, items: Vec<&'static str>| {
        if items.is_empty() {
            return;
        }
        if let Some(cat) = cat {
            starts.push((cat, rows.len()));
            rows.push(Row::Head(cat));
        }
        rows.extend(items.chunks(cols).map(|c| Row::Cells(c.to_vec())));
    };
    if query.is_empty() {
        push(Some(Cat::Recent), recents.to_vec());
        for cat in Cat::ALL.into_iter().skip(1) {
            let Cat::Group(g) = cat else { continue };
            push(Some(cat), shown(tone).filter(|e| e.group() == g).map(|e| e.as_str()).collect());
        }
    } else {
        push(None, shown(tone).filter(|e| matches(e, &query)).map(|e| e.as_str()).collect());
    }
    (rows, starts)
}

pub(super) struct Picker {
    pub search: Entity<InputState>,
    scroll: UniformListScrollHandle,
    recents: Vec<&'static str>,
    tone: usize,
    keyboard: f32,
    active: Cat,
    rows: Vec<Row>,
    starts: Vec<(Cat, usize)>,
    /// What `rows` was built for: columns, query, tone, recents.
    built: Option<(usize, String, usize, Vec<&'static str>)>,
}

impl Picker {
    pub fn new(search: Entity<InputState>, dir: &Path) -> Self {
        let saved: Saved = std::fs::read(dir.join(FILE)).ok().and_then(|raw| serde_json::from_slice(&raw).ok()).unwrap_or_default();
        let recents = saved.recents.iter().filter_map(|s| emojis::get(s)).map(|e| e.as_str()).take(RECENTS).collect();
        Self {
            search,
            scroll: UniformListScrollHandle::new(),
            recents,
            tone: saved.tone.min(TONES.len() - 1),
            keyboard: PANEL_H,
            active: Cat::Group(Group::SmileysAndEmotion),
            rows: Vec::new(),
            starts: Vec::new(),
            built: None,
        }
    }

    fn save(&self, dir: &Path) {
        let saved = Saved { recents: self.recents.iter().map(|s| (*s).to_owned()).collect(), tone: self.tone };
        if let Err(e) = serde_json::to_vec(&saved).map_err(std::io::Error::other).and_then(|raw| std::fs::write(dir.join(FILE), raw)) {
            log::warn!("{FILE}: {e}");
        }
    }

    fn refresh(&mut self, cols: usize, query: &str) {
        let key = (cols, query.to_owned(), self.tone, self.recents.clone());
        if self.built.as_ref() != Some(&key) {
            (self.rows, self.starts) = build(cols, query, self.tone, &self.recents);
            self.built = Some(key);
        }
    }
}

impl BeeperApp {
    pub(super) fn toggle_emoji_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.emoji_open == Some(Target::Composer) {
            self.close_emoji(cx);
            self.composer.update(cx, |c, cx| c.focus(window, cx));
            crate::show_keyboard(Keyboard::Text);
        } else {
            self.open_emoji(Target::Composer, window, cx);
            crate::hide_keyboard();
        }
        cx.notify();
    }

    pub(super) fn open_emoji(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        self.search_emoji("", window, cx);
        if target == Target::Composer {
            self.composer.update(cx, |c, cx| c.focus(window, cx));
        }
        self.emoji_open = Some(target);
        cx.notify();
    }

    pub(super) fn close_emoji(&mut self, cx: &mut Context<Self>) {
        self.emoji_open = None;
        cx.notify();
    }

    fn search_emoji(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.emoji_pick.search.update(cx, |s, cx| s.set_value(text, window, cx));
    }

    fn pick_emoji(&mut self, emoji: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        remember(&mut self.emoji_pick.recents, emoji);
        self.emoji_pick.save(&self.dir);
        match self.emoji_open.clone() {
            Some(Target::React(id)) => {
                self.emoji_open = None;
                self.toggle_reaction(&id, emoji, cx);
            }
            Some(Target::Composer) => self.composer.update(cx, |c, cx| c.insert(emoji, window, cx)),
            None => {}
        }
        cx.notify();
    }

    /// The panel under the composer, in the keyboard's place.
    pub(super) fn render_emoji_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.emoji_open != Some(Target::Composer) {
            return None;
        }
        let (_, bottom) = crate::insets();
        let typing = bottom > KEYBOARD_MIN;
        if typing {
            self.emoji_pick.keyboard = bottom.clamp(PANEL_SEARCH_H, 480.);
        }
        let height = if typing { PANEL_SEARCH_H } else { self.emoji_pick.keyboard };
        let width = f32::from(window.viewport_size().width);
        Some(div().id("emoji-panel").w_full().h(px(height)).flex_none().child(self.emoji_body(width, cx)).into_any_element())
    }

    /// The sheet for a reaction: rises from the bottom over a dimmed screen.
    pub(super) fn render_emoji_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !matches!(self.emoji_open, Some(Target::React(_))) {
            return None;
        }
        let p = Palette::of(cx);
        let view = window.viewport_size();
        let (_, bottom) = crate::insets();
        let height = (f32::from(view.height) * 0.6).min(460.);
        let width = f32::from(view.width).min(SHEET_W);
        let card = v_flex()
            .id("emoji-sheet")
            .relative()
            .w(px(width))
            .h(px(height))
            .pb(px(bottom))
            .rounded_t(px(RADIUS))
            .bg(p.surface)
            .occlude()
            .child(self.emoji_body(width, cx));
        let card = if cx.reduce_motion() {
            card.into_any_element()
        } else {
            card.with_animation("emoji-sheet-in", Animation::new(motion::SHEET_IN).with_easing(motion::ease_sheet), move |el, t| {
                el.top(px(height * (1. - t)))
            })
            .into_any_element()
        };
        Some(
            div()
                .id("emoji-sheet-barrier")
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .justify_end()
                .items_center()
                .bg(gpui_kit::black().opacity(0.6))
                .occlude()
                .on_click(cx.listener(|this, _, _, cx| this.close_emoji(cx)))
                .child(card)
                .into_any_element(),
        )
    }

    /// Search and skin tone on top, the grid, the category tabs under it.
    fn emoji_body(&mut self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let cols = (((width - 2. * 8.) / CELL) as usize).max(1);
        let query = self.emoji_pick.search.read(cx).value().to_string();
        self.emoji_pick.refresh(cols, &query);
        let tone = self.emoji_pick.tone;
        let searching = !query.trim().is_empty();
        let rows = self.emoji_pick.rows.len();
        let grid = if rows == 0 {
            div().flex_1().flex().items_center().justify_center().text_size(fs(14.)).text_color(p.faint).child("No emoji found").into_any_element()
        } else {
            uniform_list(
                "emoji-grid",
                rows,
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    let pick = &mut this.emoji_pick;
                    if let Some(&(cat, _)) = pick.starts.iter().rev().find(|(_, at)| *at <= range.start)
                        && cat != pick.active
                    {
                        pick.active = cat;
                        cx.notify();
                    }
                    let p = Palette::of(cx);
                    range
                        .map(|ix| match &this.emoji_pick.rows[ix] {
                            Row::Head(cat) => div()
                                .h(px(CELL))
                                .px(px(12.))
                                .flex()
                                .items_end()
                                .pb(px(6.))
                                .text_size(fs(13.))
                                .text_color(p.faint)
                                .child(cat.label())
                                .into_any_element(),
                            Row::Cells(cells) => h_flex()
                                .h(px(CELL))
                                .px(px(8.))
                                .children(cells.iter().enumerate().map(|(n, &emoji)| {
                                    div()
                                        .id(("emoji", ix * 64 + n))
                                        .size(px(CELL))
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(10.))
                                        .cursor_pointer()
                                        .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised)))
                                        .active(|el| el.bg(p.raised))
                                        .text_size(px(GLYPH))
                                        .line_height(px(CELL))
                                        .on_click(cx.listener(move |this, _, window, cx| this.pick_emoji(emoji, window, cx)))
                                        .child(emoji::styled(emoji, Vec::new()))
                                }))
                                .into_any_element(),
                        })
                        .collect()
                }),
            )
            .track_scroll(&self.emoji_pick.scroll)
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        let tabs = h_flex().h(px(40.)).px(px(4.)).justify_between().children(Cat::ALL.into_iter().filter(|c| *c != Cat::Recent || !self.emoji_pick.recents.is_empty()).map(|cat| {
            let on = !searching && self.emoji_pick.active == cat;
            let at = self.emoji_pick.starts.iter().find(|(c, _)| *c == cat).map(|(_, at)| *at);
            div()
                .id(("emoji-tab", Cat::ALL.iter().position(|c| *c == cat).unwrap_or(0)))
                .h(px(40.))
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_color(if on { p.text } else { p.faint })
                .child(Icon::new(cat.icon()).size_5())
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(at) = at {
                        this.search_emoji("", window, cx);
                        this.emoji_pick.active = cat;
                        this.emoji_pick.scroll.scroll_to_item_strict(at, ScrollStrategy::Top);
                        cx.notify();
                    }
                }))
        }));
        let swatch = TONES[tone];
        let hand = emojis::get("\u{1F44B}").and_then(|e| if swatch == SkinTone::Default { Some(e) } else { e.with_skin_tone(swatch) }).map_or("\u{1F44B}", |e| e.as_str());
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .px(px(8.))
                    .pt(px(8.))
                    .pb(px(4.))
                    .gap(px(8.))
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(40.))
                            .px(px(12.))
                            .flex()
                            .items_center()
                            .rounded(px(20.))
                            .bg(p.raised)
                            .child(Input::new(&self.emoji_pick.search).appearance(false)),
                    )
                    .child(
                        div()
                            .id("emoji-tone")
                            .size(px(40.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .cursor_pointer()
                            .active(|el| el.bg(p.raised))
                            .text_size(px(GLYPH - 4.))
                            .line_height(px(40.))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.emoji_pick.tone = (this.emoji_pick.tone + 1) % TONES.len();
                                this.emoji_pick.save(&this.dir);
                                cx.notify();
                            }))
                            .child(emoji::styled(hand, Vec::new())),
                    ),
            )
            .child(grid)
            .child(tabs)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Cat, RECENTS, Row, build, remember};

    fn cells(rows: &[Row]) -> Vec<&'static str> {
        rows.iter().flat_map(|r| if let Row::Cells(c) = r { c.clone() } else { Vec::new() }).collect()
    }

    #[test]
    fn grid_has_every_group_and_skips_tone_variants() {
        let (rows, starts) = build(8, "", 0, &[]);
        assert_eq!(starts.len(), 9, "no recents, nine groups");
        let all = cells(&rows);
        assert!(all.len() > 1500 && all.len() < 2200, "{}", all.len());
        assert!(!all.contains(&"\u{1F44B}\u{1F3FD}"));
        assert!(rows.iter().all(|r| match r {
            Row::Cells(c) => c.len() <= 8,
            Row::Head(_) => true,
        }));
    }

    #[test]
    fn recents_are_unique_newest_first_and_capped() {
        let mut r = Vec::new();
        for e in emojis::iter().take(RECENTS + 5) {
            remember(&mut r, e.as_str());
        }
        let first = emojis::iter().next().unwrap().as_str();
        remember(&mut r, first);
        assert_eq!(r.len(), RECENTS);
        assert_eq!(r[0], first);
        assert_eq!(r.iter().filter(|e| **e == first).count(), 1);
    }

    #[test]
    fn tone_swaps_the_variant() {
        let (rows, _) = build(8, "", 3, &[]);
        let all = cells(&rows);
        assert!(all.contains(&"\u{1F44B}\u{1F3FD}"));
        assert!(!all.contains(&"\u{1F44B}"));
    }

    #[test]
    fn search_finds_by_name_and_shortcode() {
        let (rows, starts) = build(8, " Thumbs ", 0, &[]);
        assert!(starts.is_empty());
        assert!(cells(&rows).contains(&"\u{1F44D}"));
        assert!(cells(&build(8, "joy", 0, &[]).0).contains(&"\u{1F602}"));
        assert!(build(8, "zzzzqq", 0, &[]).0.is_empty());
    }

    #[test]
    fn recents_lead_when_there_are_some() {
        let (rows, starts) = build(8, "", 0, &["\u{1F602}"]);
        assert_eq!(starts[0], (Cat::Recent, 0));
        assert!(matches!(&rows[1], Row::Cells(c) if c == &["\u{1F602}"]));
    }
}

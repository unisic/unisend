//! The emoji picker: a panel that stands in for the keyboard under the composer, and a sheet
//! opened by the + of the reaction bar. Data is the `emojis` crate (Unicode CLDR names and
//! GitHub shortcodes), drawn in the app's emoji font. The grid is a `uniform_list` of rows, so
//! only the rows on screen are built, whatever the 1900 emoji.

use std::path::Path;

use emojis::{Group, SkinTone};
use super::sheet::{self, SheetMotion};
use super::*;
use crate::emoji;
use crate::i18n::t;

/// Recents kept, newest first.
const RECENTS: usize = 32;
const FILE: &str = "emoji-recents.json";
/// One cell of the grid, and the height of a row; a category's name takes a row of its own.
const CELL: f32 = 44.;
/// Room at each end of the grid, the tabs and the search bar.
const PAD: f32 = 8.;
const GLYPH: f32 = 28.;
/// The panel's height until the keyboard has been seen, and with it up for the search field.
const PANEL_H: f32 = 300.;
const PANEL_SEARCH_H: f32 = 220.;
/// Taller than this at the bottom is the keyboard, not the navigation bar.
const KEYBOARD_MIN: f32 = 120.;
const SHEET_W: f32 = 480.;
/// The skin tone menu's cells, and its gap under the swatch.
const TONE_CELL: f32 = 40.;
const TONE_GAP: f32 = 4.;
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
            Cat::Recent => t!("Recent"),
            Cat::Group(Group::SmileysAndEmotion) => t!("Smileys"),
            Cat::Group(Group::PeopleAndBody) => t!("People"),
            Cat::Group(Group::AnimalsAndNature) => t!("Animals"),
            Cat::Group(Group::FoodAndDrink) => t!("Food"),
            Cat::Group(Group::TravelAndPlaces) => t!("Travel"),
            Cat::Group(Group::Activities) => t!("Activities"),
            Cat::Group(Group::Objects) => t!("Objects"),
            Cat::Group(Group::Symbols) => t!("Symbols"),
            Cat::Group(Group::Flags) => t!("Flags"),
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
/// swapped for the tone's when it has one, each only where `keep` takes it (the neutral one when
/// it takes only that).
fn shown(tone: usize, keep: &dyn Fn(&str) -> bool) -> impl Iterator<Item = &'static emojis::Emoji> {
    let tone = TONES.get(tone).copied().unwrap_or(SkinTone::Default);
    emojis::iter().filter(|e| e.skin_tone().is_none_or(|t| t == SkinTone::Default)).filter_map(move |e| {
        let toned = if tone == SkinTone::Default { None } else { e.with_skin_tone(tone) };
        toned.filter(|t| keep(t.as_str())).or_else(|| keep(e.as_str()).then_some(e))
    })
}

/// What the grid offers: what the emoji face draws and, for a reaction in a chat that takes only
/// some, those.
fn offered(allowed: Option<&[String]>, emoji: &str) -> bool {
    emoji::drawable(emoji) && allowed.is_none_or(|list| list.iter().any(|a| super::menu::same_emoji(a, emoji)))
}

fn matches(e: &emojis::Emoji, query: &str) -> bool {
    e.name().to_lowercase().contains(query) || e.shortcodes().any(|s| s.contains(query))
}

/// The waving hand in `tone`, the skin tone control's face.
fn waving(tone: SkinTone) -> &'static str {
    emojis::get("\u{1F44B}").and_then(|e| if tone == SkinTone::Default { Some(e) } else { e.with_skin_tone(tone) }).map_or("\u{1F44B}", |e| e.as_str())
}

/// How many columns of at least `CELL` fit across `width` inside `PAD` at each end, and how wide
/// each is: the remainder widens every column, so the grid fills the row evenly.
fn columns(width: f32) -> (usize, f32) {
    let room = (width - 2. * PAD).max(CELL);
    let cols = ((room / CELL) as usize).max(1);
    (cols, room / cols as f32)
}

/// Records the width it is laid out at and redraws `view` when it changed.
fn width_probe(cell: Rc<Cell<f32>>, view: EntityId) -> impl IntoElement {
    canvas(
        move |bounds, window, _| {
            let got = f32::from(bounds.size.width);
            if (got - cell.get()).abs() > 0.5 {
                cell.set(got);
                // During prepaint, so not `request_animation_frame`, which needs a view being rendered.
                window.on_next_frame(move |_, cx| cx.notify(view));
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

/// The grid for `cols` columns, plus the row each category starts at.
fn build(cols: usize, query: &str, tone: usize, recents: &[&'static str], keep: &dyn Fn(&str) -> bool) -> (Vec<Row>, Vec<(Cat, usize)>) {
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
        push(Some(Cat::Recent), recents.iter().copied().filter(|e| keep(e)).collect());
        for cat in Cat::ALL.into_iter().skip(1) {
            let Cat::Group(g) = cat else { continue };
            push(Some(cat), shown(tone, keep).filter(|e| e.group() == g).map(|e| e.as_str()).collect());
        }
    } else {
        push(None, shown(tone, keep).filter(|e| matches(e, &query)).map(|e| e.as_str()).collect());
    }
    (rows, starts)
}

type Built = (usize, String, usize, Vec<&'static str>, Option<Vec<String>>);

pub(super) struct Picker {
    pub search: Entity<InputState>,
    scroll: UniformListScrollHandle,
    recents: Vec<&'static str>,
    tone: usize,
    keyboard: f32,
    active: Cat,
    rows: Vec<Row>,
    starts: Vec<(Cat, usize)>,
    /// What `rows` was built for: columns, query, tone, recents, the reactions the chat takes.
    built: Option<Built>,
    pub sheet: SheetMotion,
    /// The panel's width as last laid out: beside a wide window's chat list it is not the window's.
    width: Rc<Cell<f32>>,
    /// The skin tone menu is open.
    tones: bool,
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
            sheet: SheetMotion::default(),
            width: Rc::default(),
            tones: false,
        }
    }

    fn save(&self, dir: &Path) {
        let saved = Saved { recents: self.recents.iter().map(|s| (*s).to_owned()).collect(), tone: self.tone };
        if let Err(e) = serde_json::to_vec(&saved).map_err(std::io::Error::other).and_then(|raw| std::fs::write(dir.join(FILE), raw)) {
            log::warn!("{FILE}: {e}");
        }
    }

    fn fresh(&self, cols: usize, query: &str, allowed: Option<&[String]>) -> bool {
        self.built.as_ref().is_some_and(|(c, q, t, r, a)| (*c, q.as_str(), *t, r, a.as_deref()) == (cols, query, self.tone, &self.recents, allowed))
    }

    fn rebuild(&mut self, cols: usize, query: &str, allowed: Option<Vec<String>>) {
        (self.rows, self.starts) = build(cols, query, self.tone, &self.recents, &|e| offered(allowed.as_deref(), e));
        self.built = Some((cols, query.to_owned(), self.tone, self.recents.clone(), allowed));
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
        // The composer is not focused for it: on Android a text input gaining focus raises the
        // keyboard from the platform layer (gpui-mobile `text_input_state_changed`), past any hide.
        // `insert` works at the kept caret without focus.
        self.search_emoji("", window, cx);
        self.emoji_open = Some(target);
        cx.notify();
    }

    pub(super) fn close_emoji(&mut self, cx: &mut Context<Self>) {
        self.emoji_open = None;
        self.emoji_pick.tones = false;
        cx.notify();
    }

    /// Escape or Back with the skin tone menu open closes only the menu.
    pub(super) fn close_tone_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let open = std::mem::take(&mut self.emoji_pick.tones);
        if open {
            cx.notify();
        }
        open
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
        let width = Some(self.emoji_pick.width.get()).filter(|w| *w > 0.).unwrap_or_else(|| f32::from(frame::viewport(window).width));
        let probe = width_probe(self.emoji_pick.width.clone(), cx.entity_id());
        Some(div().id("emoji-panel").relative().w_full().h(px(height)).flex_none().child(probe).child(self.emoji_body(width, false, window, cx)).into_any_element())
    }

    /// The sheet for a reaction: rises from the bottom over a dimmed screen, falls when an emoji is
    /// picked. Opened from the long-press menu it rises over the menu's fading dim, its own scrim
    /// already full, so the screen never brightens between the two.
    pub(super) fn render_emoji_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let live = matches!(self.emoji_open, Some(Target::React(_)));
        if live && !self.emoji_pick.sheet.is_live() {
            self.emoji_pick.sheet.handoff = self.menu.is_some();
        }
        let frame = self.emoji_pick.sheet.sample(live, window, cx)?;
        let live = frame.live;
        let p = Palette::of(cx);
        let view = frame::viewport(window);
        let (_, bottom) = crate::insets();
        let height = (f32::from(view.height) * 0.6).min(460.);
        let width = f32::from(view.width).min(SHEET_W);
        let card = v_flex()
            .id("emoji-sheet")
            .relative()
            .top(px(frame.y))
            .w(px(width))
            .h(px(height + sheet::HANDLE_H))
            .pb(px(bottom))
            .rounded_t(px(RADIUS))
            .bg(p.surface)
            .when(live, |el| el.occlude())
            .child(self.emoji_pick.sheet.probe(cx.entity_id()))
            .child(sheet::handle(p.faint).id("emoji-sheet-handle").when(live, |el| {
                el.on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| {
                    if this.emoji_pick.sheet.drag(e) {
                        this.close_emoji(cx);
                    }
                    cx.notify();
                }))
            }))
            .child(self.emoji_body(width, true, window, cx));
        Some(
            sheet::scrim("emoji-sheet-barrier", frame)
                .when(live, |el| el.on_click(cx.listener(|this, _, _, cx| this.close_emoji(cx))))
                .child(card)
                .into_any_element(),
        )
    }

    /// Search and skin tone on top, the grid, the category tabs under it. `react`: for a reaction,
    /// limited to what the chat takes.
    fn emoji_body(&mut self, width: f32, react: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let (cols, cell_w) = columns(width);
        let query = self.emoji_pick.search.read(cx).value().to_string();
        let allowed = if react { self.caps().allowed_reactions.as_deref() } else { None };
        if !self.emoji_pick.fresh(cols, &query, allowed) {
            let allowed = allowed.map(<[String]>::to_vec);
            self.emoji_pick.rebuild(cols, &query, allowed);
        }
        let tone = self.emoji_pick.tone;
        let searching = !query.trim().is_empty();
        let rows = self.emoji_pick.rows.len();
        let grid = if rows == 0 {
            div().flex_1().flex().items_center().justify_center().text_size(fs(14.)).text_color(p.faint).child(t!("No emoji found")).into_any_element()
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
                                .px(px(PAD))
                                .children(cells.iter().enumerate().map(|(n, &emoji)| {
                                    div()
                                        .id(("emoji", ix * 64 + n))
                                        .w(px(cell_w))
                                        .h(px(CELL))
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
        let tabs = h_flex().h(px(40.)).px(px(PAD)).justify_between().children(Cat::ALL.into_iter().filter(|c| (searching && *c != Cat::Recent) || self.emoji_pick.starts.iter().any(|(s, _)| s == c)).map(|cat| {
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
        let open = self.emoji_pick.tones;
        let shown = Presence::new("emoji-tones", open).transition(if open { motion::enter() } else { motion::exit() }).sample(window, cx);
        // Opened from the swatch at the top right, it drops from under it as the app's menus do.
        let menu = shown.should_render().then(|| {
            let t = shown.progress;
            h_flex()
                .id("emoji-tones")
                .absolute()
                .top(px(PAD + 40. + TONE_GAP - 4. * (1. - t)))
                .right(px(PAD))
                .p(px(4.))
                .gap(px(2.))
                .rounded_full()
                .bg(p.lifted)
                .opacity(t)
                .when(open, |el| el.occlude())
                .children(TONES.iter().enumerate().map(|(n, &choice)| {
                    div()
                        .id(("emoji-tone-choice", n))
                        .size(px(TONE_CELL))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .cursor_pointer()
                        .when(n == tone, |el| el.border_2().border_color(p.text))
                        .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised)))
                        .active(|el| el.bg(p.raised))
                        .text_size(px(GLYPH - 4.))
                        .line_height(px(TONE_CELL))
                        .when(open, |el| {
                            el.on_click(cx.listener(move |this, _, _, cx| {
                                this.emoji_pick.tones = false;
                                if this.emoji_pick.tone != n {
                                    this.emoji_pick.tone = n;
                                    this.emoji_pick.save(&this.dir);
                                }
                                cx.notify();
                            }))
                        })
                        .child(emoji::styled(waving(choice), Vec::new()))
                }))
        });
        // A tap anywhere else in the picker only closes the menu.
        let barrier = open.then(|| div().id("emoji-tones-barrier").absolute().inset_0().occlude().on_click(cx.listener(|this, _, _, cx| {
            this.close_tone_menu(cx);
        })));
        v_flex()
            .size_full()
            .relative()
            .child(
                h_flex()
                    .px(px(PAD))
                    .pt(px(PAD))
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
                            .child(Input::new(&self.emoji_pick.search).appearance(false))
                            .child(crate::ime::marks(&self.emoji_pick.search)),
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
                            .when(open, |el| el.bg(p.raised))
                            .active(|el| el.bg(p.raised))
                            .text_size(px(GLYPH - 4.))
                            .line_height(px(40.))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.emoji_pick.tones = !this.emoji_pick.tones;
                                cx.notify();
                            }))
                            .child(emoji::styled(waving(TONES[tone]), Vec::new())),
                    ),
            )
            .child(grid)
            .child(tabs)
            .children(barrier)
            .children(menu)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{CELL, Cat, PAD, RECENTS, Row, build, columns, offered, remember};

    fn cells(rows: &[Row]) -> Vec<&'static str> {
        rows.iter().flat_map(|r| if let Row::Cells(c) = r { c.clone() } else { Vec::new() }).collect()
    }

    #[test]
    fn grid_has_every_group_and_skips_tone_variants() {
        let (rows, starts) = build(8, "", 0, &[], &|_| true);
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
        let (rows, _) = build(8, "", 3, &[], &|_| true);
        let all = cells(&rows);
        assert!(all.contains(&"\u{1F44B}\u{1F3FD}"));
        assert!(!all.contains(&"\u{1F44B}"));
    }

    #[test]
    fn search_finds_by_name_and_shortcode() {
        let (rows, starts) = build(8, " Thumbs ", 0, &[], &|_| true);
        assert!(starts.is_empty());
        assert!(cells(&rows).contains(&"\u{1F44D}"));
        assert!(cells(&build(8, "joy", 0, &[], &|_| true).0).contains(&"\u{1F602}"));
        assert!(build(8, "zzzzqq", 0, &[], &|_| true).0.is_empty());
    }

    #[test]
    fn a_chat_that_takes_some_reactions_offers_only_those() {
        let allowed = ["\u{2764}".to_owned(), "\u{1F44D}".to_owned()];
        let keep = |e: &str| offered(Some(&allowed), e);
        let (rows, starts) = build(8, "", 3, &["\u{1F602}", "\u{1F44D}"], &keep);
        assert_eq!(cells(&rows), ["\u{1F44D}", "\u{2764}\u{FE0F}", "\u{1F44D}"], "recents, then each group: the neutral thumb, as the toned one is not taken");
        assert_eq!(starts.iter().map(|(c, _)| *c).collect::<Vec<_>>(), [Cat::Recent, Cat::Group(emojis::Group::SmileysAndEmotion), Cat::Group(emojis::Group::PeopleAndBody)]);
        assert!(cells(&build(8, "", 0, &[], &|e| offered(None, e)).0).len() > 1500, "unrestricted with no face set");
    }

    #[test]
    fn recents_lead_when_there_are_some() {
        let (rows, starts) = build(8, "", 0, &["\u{1F602}"], &|_| true);
        assert_eq!(starts[0], (Cat::Recent, 0));
        assert!(matches!(&rows[1], Row::Cells(c) if c == &["\u{1F602}"]));
    }

    #[test]
    fn columns_fill_the_width_evenly() {
        for width in [320., 360., 384., 412., 480., 600., 1000.] {
            let (cols, w) = columns(width);
            assert!(w >= CELL && w < CELL * (1. + 1. / cols as f32), "{width}: {cols} x {w}");
            assert!((cols as f32 * w + 2. * PAD - width).abs() < 0.01, "{width}: no remainder left over");
        }
        // A 360 dp phone: seven columns where 44 px cells left 36 px on the right.
        assert_eq!(columns(360.).0, 7);
        // Too narrow for one cell still gives one.
        assert_eq!(columns(10.), (1, CELL));
    }
}

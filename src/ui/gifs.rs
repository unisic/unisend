//! The GIF picker: KLIPY's trending GIFs, a search field and a grid of previews; a tap sends one.
//! Shown only when the build carries a KLIPY key. Previews are KLIPY's small JPEGs, not the
//! animations: GPUI decodes every frame of a GIF up front (a small KLIPY GIF is 1 to 4 MB of pixels),
//! which a grid of up to 96 cannot afford on a phone.

use std::ops::Range;

use super::sheet::{self, SheetMotion};
use super::*;
use crate::backend::{Gif, GifPage, gifs_enabled};
use crate::i18n::t;

/// A search goes out once typing has paused this long; KLIPY's testing keys allow 100 requests an
/// hour.
const DEBOUNCE: Duration = Duration::from_millis(400);
/// Rows from the end that ask for the next page.
const AHEAD_ROWS: usize = 2;
const GRID_GAP: f32 = 2.;

#[derive(Default)]
pub(super) struct Picker {
    open: bool,
    input: Option<Entity<InputState>>,
    _input_events: Option<Subscription>,
    /// What the shown results are for; empty is trending.
    query: String,
    /// Bumped by every keystroke; a debounce timer that finds it moved on does nothing.
    typed: u64,
    gifs: Vec<Gif>,
    /// Pages loaded, and whether the last one asked for has arrived.
    pages: u32,
    loading: bool,
    more: bool,
    failed: bool,
    /// One per loaded GIF at most (KLIPY's small JPEGs, about 200 KB of pixels each), so at most
    /// `MAX_PAGES` × 24 of them: under 20 MB. Dropped with the results.
    previews: HashMap<String, Arc<Image>>,
    pub sheet: SheetMotion,
}

impl BeeperApp {
    pub(super) fn gifs_open(&self) -> bool {
        self.gifs.open
    }

    pub(super) fn open_gifs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !gifs_enabled() || self.open.is_none() {
            return;
        }
        if self.gifs.input.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Search KLIPY")));
            crate::ime::follow(&input, window, cx);
            self.gifs._input_events = Some(cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| match ev {
                InputEvent::Change => this.gif_typed(cx),
                other => keyboard_follows(other, Keyboard::Text, cx),
            }));
            self.gifs.input = Some(input);
        }
        if let Some(input) = self.gifs.input.clone() {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.gifs.open = true;
        self.gif_search(String::new(), cx);
    }

    pub(super) fn close_gifs(&mut self, cx: &mut Context<Self>) {
        let g = &mut self.gifs;
        if !g.open {
            return;
        }
        g.open = false;
        g.typed += 1;
        let _ = self.cmd.try_send(Cmd::GifStop);
        self.forget_gifs(cx);
        cx.notify();
    }

    fn forget_gifs(&mut self, cx: &mut Context<Self>) {
        let g = &mut self.gifs;
        g.gifs.clear();
        (g.pages, g.loading, g.more, g.failed) = (0, false, false, false);
        for (_, image) in g.previews.drain() {
            image.remove_asset(cx);
        }
    }

    fn gif_typed(&mut self, cx: &mut Context<Self>) {
        let Some(input) = &self.gifs.input else { return };
        let text = input.read(cx).value().trim().to_owned();
        self.gifs.typed += 1;
        let typed = self.gifs.typed;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                if this.gifs.open && this.gifs.typed == typed && this.gifs.query != text {
                    this.gif_search(text, cx);
                }
            });
        })
        .detach();
    }

    fn gif_search(&mut self, query: String, cx: &mut Context<Self>) {
        self.forget_gifs(cx);
        self.gifs.query = query.clone();
        self.gifs.pages = 1;
        self.gifs.loading = self.send(Cmd::GifSearch { query, page: 1 }, cx);
    }

    fn gif_more(&mut self) {
        let g = &mut self.gifs;
        if !g.open || g.loading || !g.more || g.failed {
            return;
        }
        g.pages += 1;
        let cmd = Cmd::GifSearch { query: g.query.clone(), page: g.pages };
        self.gifs.loading = self.cmd.try_send(cmd).is_ok();
    }

    pub(super) fn on_gifs(&mut self, query: String, page: u32, found: Option<GifPage>) {
        let g = &mut self.gifs;
        if !g.open || query != g.query || page != g.pages {
            return;
        }
        g.loading = false;
        match found {
            Some(found) => {
                g.gifs.extend(found.gifs);
                g.more = found.more;
            }
            None => g.failed = true,
        }
    }

    pub(super) fn on_gif_preview(&mut self, url: String, bytes: Vec<u8>) {
        let g = &mut self.gifs;
        if g.open
            && !g.previews.contains_key(&url)
            && g.gifs.iter().any(|gif| gif.preview.url == url)
            && let Some(format) = image_format(&bytes)
        {
            g.previews.insert(url, Arc::new(Image::from_bytes(format, bytes)));
        }
    }

    fn send_gif(&mut self, ix: usize, cx: &mut Context<Self>) {
        let (Some(room), Some(gif)) = (self.open.clone(), self.gifs.gifs.get(ix).cloned()) else { return };
        let reply_to = match std::mem::replace(&mut self.compose, Compose::New) {
            Compose::Reply(id) => Some(id),
            _ => None,
        };
        let query = self.gifs.query.clone();
        if self.send(Cmd::SendGif { room, gif, query, reply_to }, cx) {
            self.uploads += 1;
        }
        self.close_gifs(cx);
    }

    pub(super) fn render_gifs(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let frame = self.gifs.sheet.sample(self.gifs.open, window, cx)?;
        let live = frame.live;
        let p = Palette::of(cx);
        let view = frame::viewport(window);
        let (view_w, view_h) = (f32::from(view.width), f32::from(view.height));
        let (top, bottom) = crate::insets();
        let width = view_w.min(SHEET_W);
        let height = ((view_h - top) * 0.72).round();

        let cols = if width >= 480. { 4 } else { 3 };
        let cell = ((width - GRID_GAP * (cols - 1) as f32) / cols as f32).floor();

        let search = self.gifs.input.as_ref().map(|input| {
            h_flex()
                .flex_none()
                .mx(px(GUTTER))
                .mb(px(8.))
                .h(px(TAP))
                .pl(px(GAP))
                .pr(px(4.))
                .gap(px(4.))
                .rounded_full()
                .bg(p.raised)
                .child(Icon::new(IconName::Search).size_4().flex_none().text_color(p.faint))
                .child(div().flex_1().min_w_0().child(Input::new(input).appearance(false)).child(crate::ime::marks(input)))
        });

        let g = &self.gifs;
        let status = if g.failed {
            Some(t!("Could not load GIFs. Check the connection and search again."))
        } else if g.gifs.is_empty() && !g.loading {
            Some(t!("No GIFs found"))
        } else {
            None
        };
        let grid = if g.gifs.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px(px(GUTTER))
                .text_size(fs(14.))
                .text_color(p.faint)
                .children(status)
                .into_any_element()
        } else {
            let rows = g.gifs.len().div_ceil(cols);
            uniform_list(
                "gifs-grid",
                rows,
                cx.processor(move |this, range: Range<usize>, _, cx| {
                    if live && range.end + AHEAD_ROWS >= rows {
                        this.gif_more();
                    }
                    range
                        .map(|row| {
                            let cells = row * cols..((row + 1) * cols).min(this.gifs.gifs.len());
                            h_flex()
                                .h(px(cell + GRID_GAP))
                                .pb(px(GRID_GAP))
                                .gap(px(GRID_GAP))
                                .children(cells.map(|ix| this.render_gif(ix, cell, live, cx)))
                        })
                        .collect()
                }),
            )
            .flex_1()
            .into_any_element()
        };

        Some(
            sheet::scrim("gifs-scrim", frame)
                .when(live, |el| el.on_click(cx.listener(|this, _, _, cx| this.close_gifs(cx))))
                .child(
                    v_flex()
                        .id("gifs-sheet")
                        .relative()
                        .top(px(frame.y))
                        .w(px(width))
                        .h(px(height + bottom))
                        .pb(px(bottom + 8.))
                        .rounded_t(px(RADIUS))
                        .overflow_hidden()
                        .bg(p.surface)
                        .when(live, |el| el.on_click(|_, _, cx| cx.stop_propagation()))
                        .child(self.gifs.sheet.probe(cx.entity_id()))
                        .child(sheet::handle(p.faint).id("gifs-handle").when(live, |el| {
                            el.child(sheet::grip(cx, |this: &mut Self| &mut this.gifs.sheet, |this, cx| this.close_gifs(cx)))
                        }))
                        .children(search)
                        .child(grid)
                        .child(div().flex_none().pt(px(8.)).px(px(GUTTER)).text_size(fs(12.)).text_color(p.faint).child(t!("Powered by KLIPY"))),
                )
                .into_any_element(),
        )
    }

    fn render_gif(&mut self, ix: usize, cell: f32, live: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = Palette::of(cx);
        let gif = &self.gifs.gifs[ix];
        let preview = self.gifs.previews.get(&gif.preview.url).cloned();
        div()
            .id(("gif", ix))
            .size(px(cell))
            .flex_none()
            .overflow_hidden()
            .bg(p.raised)
            .cursor_pointer()
            .active(|el| el.opacity(0.7))
            .children(preview.map(|image| img(image).size_full().object_fit(ObjectFit::Cover)))
            .when(live, |el| el.on_click(cx.listener(move |this, _, _, cx| this.send_gif(ix, cx))))
    }
}

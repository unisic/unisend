//! The attach sheet: the newest photos and videos in a grid, as in Signal, so sending a picture
//! skips the system's picker. Android reads MediaStore through `dev.gpui.beeper.Gallery`. Desktop
//! keeps its file dialog; a debug build shows the sheet for the directory named by
//! `BEEPER_GPUI_GALLERY`, only so the sheet can be checked there.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;

use super::camera::TILES;
use super::*;

/// Items asked of the store at a time.
const PAGE: usize = 120;
pub(super) const MAX_SELECTED: usize = 10;
/// Decoded thumbnails kept, also while the sheet is closed: each at most `THUMB_MAX`² × 4 B, so
/// at most 33 MB of pixels in all.
const MAX_CACHED: usize = 80;
const THUMB_MAX: f32 = 320.;
/// Thumbnails being read at once.
const MAX_LOADS: usize = 4;
/// Rows below the visible ones whose thumbnails load ahead of the scroll.
const AHEAD_ROWS: usize = 2;
/// A downward flick this fast (px/s) dismisses the sheet however short it was.
const DISMISS_SPEED: f32 = 800.;
const GRID_GAP: f32 = 2.;
/// Attachments waiting their turn to upload, all batches together.
const MAX_QUEUED: usize = 30;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Item {
    pub id: i64,
    /// A content address on Android, a path on desktop.
    pub uri: String,
    pub mime: String,
    /// When it was added, in seconds.
    pub date: i64,
    pub duration_ms: u64,
    /// Pixels as stored, 0 when unknown; Android asks for thumbnails in this shape.
    pub width: u32,
    pub height: u32,
}

impl Item {
    fn video(&self) -> bool {
        self.mime.starts_with("video/")
    }

    /// Where the next page starts: items sort newest first by (date, id).
    fn key(&self) -> (i64, i64) {
        (self.date, self.id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Access {
    Full,
    /// Android 14's "Allow limited access": only the photos the user chose.
    Partial,
    None,
}

/// The Java side's page: id, uri, mime type, date, duration, width, height, one item a line,
/// tab-separated.
#[cfg_attr(all(not(target_os = "android"), not(test)), expect(dead_code, reason = "desktop lists a directory"))]
fn parse_page(text: &str) -> Vec<Item> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            Some(Item {
                id: f.next()?.parse().ok()?,
                uri: f.next()?.to_owned(),
                mime: f.next()?.to_owned(),
                date: f.next()?.parse().ok()?,
                duration_ms: f.next()?.parse().ok()?,
                width: f.next()?.parse().ok()?,
                height: f.next()?.parse().ok()?,
            })
        })
        .collect()
}

/// Up to `n` items older than `after`, from a list sorted newest first. The same rule as the SQL
/// in `Gallery.page`.
#[cfg_attr(target_os = "android", expect(dead_code, reason = "Android pages in SQL"))]
fn page_after(all: &[Item], after: Option<(i64, i64)>, n: usize) -> Vec<Item> {
    all.iter().filter(|item| after.is_none_or(|key| item.key() < key)).take(n).cloned().collect()
}

/// The chosen items in the order they were chosen.
#[derive(Default)]
pub(super) struct Selection(Vec<i64>);

impl Selection {
    /// Adds or removes an item; false when it is full and the item is not in it.
    fn toggle(&mut self, id: i64) -> bool {
        if let Some(at) = self.0.iter().position(|&chosen| chosen == id) {
            self.0.remove(at);
        } else if self.0.len() < MAX_SELECTED {
            self.0.push(id);
        } else {
            return false;
        }
        true
    }

    /// The item's place in the order, from 1.
    fn number(&self, id: i64) -> Option<usize> {
        self.0.iter().position(|&chosen| chosen == id).map(|at| at + 1)
    }

    fn is_full(&self) -> bool {
        self.0.len() >= MAX_SELECTED
    }
}

/// The least recently drawn thumbnails go first.
#[derive(Default)]
struct Thumbs {
    images: HashMap<i64, Arc<Image>>,
    order: VecDeque<i64>,
}

impl Thumbs {
    fn get(&mut self, id: i64) -> Option<Arc<Image>> {
        let image = self.images.get(&id)?.clone();
        if let Some(at) = self.order.iter().position(|&old| old == id) {
            self.order.remove(at);
        }
        self.order.push_back(id);
        Some(image)
    }

    /// Returns what no longer fits, for the caller to free.
    fn insert(&mut self, id: i64, image: Arc<Image>) -> Vec<Arc<Image>> {
        if self.images.insert(id, image).is_none() {
            self.order.push_back(id);
        }
        let mut evicted = Vec::new();
        while self.order.len() > MAX_CACHED {
            let Some(old) = self.order.pop_front() else { break };
            evicted.extend(self.images.remove(&old));
        }
        evicted
    }
}

/// A finger dragging the sheet by its top: how far it has gone down, and its last positions.
struct Drag {
    offset: f32,
    samples: VecDeque<(Instant, f32)>,
}

/// Speed in px/s over the last 100 ms of a drag.
fn speed(samples: &VecDeque<(Instant, f32)>) -> f32 {
    let Some(&(end, at)) = samples.back() else { return 0. };
    let Some(&(start, from)) = samples.iter().find(|(t, _)| end.duration_since(*t) <= Duration::from_millis(100)) else { return 0. };
    let dt = end.duration_since(start).as_secs_f32();
    if dt < 0.001 { 0. } else { (at - from) / dt }
}

fn dismisses(offset: f32, speed: f32, height: f32) -> bool {
    offset > height * 0.3 || (offset > 0. && speed > DISMISS_SPEED)
}

/// Up past the open position the sheet follows a fifth of the way, so it meets resistance.
fn follow(offset: f32) -> f32 {
    if offset >= 0. { offset } else { offset * 0.2 }
}

#[derive(Default)]
pub(super) struct Gallery {
    pub open: bool,
    access: Option<Access>,
    /// Newest first. Trimmed to one page when the sheet closes, so it opens with them at once.
    items: Vec<Item>,
    /// No older items than these.
    done: bool,
    paging: bool,
    /// Bumped on every opening and closing; answers meant for another one are dropped.
    epoch: u64,
    selection: Selection,
    thumbs: Thumbs,
    loading: HashSet<i64>,
    /// Items without a thumbnail, not asked again while the sheet is open.
    failed: HashSet<i64>,
    /// Items whose thumbnails are wanted now: the visible rows and a few below.
    visible: Range<usize>,
    thumb_px: u32,
    drag: Option<Drag>,
    /// How far down the sheet was drawn last, where it falls from when it closes.
    offset: f32,
    fall_from: f32,
    /// Asked for access and watching for the answer.
    waiting: bool,
    asked: bool,
}

/// Whether attaching opens the sheet rather than the system's picker.
pub(super) fn enabled() -> bool {
    #[cfg(target_os = "android")]
    return true;
    #[cfg(not(target_os = "android"))]
    store::dir().is_some()
}

impl BeeperApp {
    pub(super) fn open_gallery(&mut self, cx: &mut Context<Self>) {
        let g = &mut self.gallery;
        (g.open, g.fall_from, g.offset) = (true, 0., 0.);
        g.epoch += 1;
        let access = store::access();
        g.access = Some(access);
        if access == Access::None && !g.asked {
            g.asked = true;
            self.ask_gallery(cx);
        } else if access != Access::None {
            self.load_page(true, cx);
        }
        cx.notify();
    }

    /// Only the thumbnail cache and one page of items outlive the sheet.
    pub(super) fn close_gallery(&mut self, cx: &mut Context<Self>) {
        let g = &mut self.gallery;
        if !g.open {
            return;
        }
        g.open = false;
        g.epoch += 1;
        g.fall_from = g.offset;
        (g.drag, g.waiting, g.paging) = (None, false, false);
        g.selection = Selection::default();
        g.loading.clear();
        g.failed.clear();
        g.visible = 0..0;
        if g.items.len() > PAGE {
            g.items.truncate(PAGE);
            g.done = false;
        }
        cx.notify();
    }

    /// Asks for access (Android shows its dialog, or the app's settings once it stops asking), then
    /// watches until the answer changes or the user is back from the dialog.
    fn ask_gallery(&mut self, cx: &mut Context<Self>) {
        store::ask();
        if std::mem::replace(&mut self.gallery.waiting, true) {
            return;
        }
        let epoch = self.gallery.epoch;
        cx.spawn(async move |this, cx| {
            let mut left = false;
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let watching = this.update(cx, |this, cx| {
                    let g = &mut this.gallery;
                    if !g.open || g.epoch != epoch || !g.waiting {
                        return false;
                    }
                    let now = store::access();
                    let front = crate::FOREGROUND.load(std::sync::atomic::Ordering::Relaxed);
                    let back = left && front;
                    left |= !front;
                    if Some(now) == g.access && !back {
                        return true;
                    }
                    (g.access, g.waiting) = (Some(now), false);
                    if now != Access::None {
                        this.load_page(true, cx);
                    }
                    cx.notify();
                    false
                });
                if !matches!(watching, Ok(true)) {
                    return;
                }
            }
        })
        .detach();
    }

    fn load_page(&mut self, first: bool, cx: &mut Context<Self>) {
        let g = &mut self.gallery;
        if g.paging {
            return;
        }
        let after = if first { None } else { g.items.last().map(Item::key) };
        g.paging = true;
        let epoch = g.epoch;
        cx.spawn(async move |this, cx| {
            let page = cx.background_spawn(async move { store::page(after) }).await;
            let _ = this.update(cx, |this, cx| {
                let g = &mut this.gallery;
                if g.epoch != epoch {
                    return;
                }
                g.paging = false;
                let Some(page) = page else {
                    g.done = true;
                    return cx.notify();
                };
                g.done = page.len() < PAGE;
                if first {
                    g.items = page;
                } else {
                    g.items.extend(page);
                }
                this.pump_thumbs(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn pump_thumbs(&mut self, cx: &mut Context<Self>) {
        let g = &mut self.gallery;
        if !g.open {
            return;
        }
        let wanted = g.visible.start.min(g.items.len())..g.visible.end.min(g.items.len());
        for item in &g.items[wanted] {
            if g.loading.len() >= MAX_LOADS {
                break;
            }
            if g.thumbs.images.contains_key(&item.id) || g.loading.contains(&item.id) || g.failed.contains(&item.id) {
                continue;
            }
            g.loading.insert(item.id);
            let (item, px, epoch) = (item.clone(), g.thumb_px, g.epoch);
            cx.spawn(async move |this, cx| {
                let id = item.id;
                let bytes = cx.background_spawn(async move { store::thumb(&item, px) }).await;
                let _ = this.update(cx, |this, cx| {
                    let g = &mut this.gallery;
                    if g.epoch != epoch {
                        return;
                    }
                    g.loading.remove(&id);
                    match bytes.and_then(|bytes| Some((image_format(&bytes)?, bytes))) {
                        Some((format, bytes)) => {
                            for old in g.thumbs.insert(id, Arc::new(Image::from_bytes(format, bytes))) {
                                old.remove_asset(cx);
                            }
                        }
                        None => {
                            g.failed.insert(id);
                        }
                    }
                    this.pump_thumbs(cx);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Learns which rows the grid draws: their thumbnails load first, and the next page when they
    /// near the end.
    fn grid_drawn(&mut self, rows: Range<usize>, cols: usize, cx: &mut Context<Self>) {
        let g = &mut self.gallery;
        let wanted = (rows.start * cols).saturating_sub(TILES)..((rows.end + AHEAD_ROWS) * cols).saturating_sub(TILES);
        if !g.done && wanted.end >= g.items.len().saturating_sub(PAGE / 4) {
            self.load_page(false, cx);
        }
        if self.gallery.visible != wanted {
            self.gallery.visible = wanted;
            self.pump_thumbs(cx);
        }
    }

    fn toggle_item(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.gallery.selection.toggle(id) {
            #[cfg(target_os = "android")]
            motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Selection);
            cx.notify();
        }
    }

    /// Copies the chosen items in order, off the UI thread, then sends them one after another.
    /// Only the first replies to the message the composer was replying to.
    fn send_chosen(&mut self, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone() else { return };
        let g = &self.gallery;
        let chosen: Vec<Item> = g.selection.0.iter().filter_map(|id| g.items.iter().find(|item| item.id == *id).cloned()).collect();
        if chosen.is_empty() {
            return;
        }
        let reply_to = match std::mem::replace(&mut self.compose, Compose::New) {
            Compose::Reply(id) => Some(id),
            _ => None,
        };
        let wanted = chosen.len();
        self.uploads += wanted;
        self.close_gallery(cx);
        cx.spawn(async move |this, cx| {
            let paths: Vec<PathBuf> = cx.background_spawn(async move { chosen.iter().filter_map(store::local_copy).collect() }).await;
            let _ = this.update(cx, |this, cx| {
                let lost = wanted - paths.len();
                if lost > 0 {
                    this.uploads = this.uploads.saturating_sub(lost);
                    this.error = Some(format!("{lost} of {wanted} could not be read").into());
                }
                this.queue_attach(room, paths, reply_to, cx);
            });
        })
        .detach();
    }

    /// Attachments upload one at a time, so a batch arrives in the order it was chosen.
    pub(super) fn queue_attach(&mut self, room: OwnedRoomId, paths: Vec<PathBuf>, mut reply_to: Option<String>, cx: &mut Context<Self>) {
        if self.attach_queue.len() + paths.len() > MAX_QUEUED {
            self.uploads = self.uploads.saturating_sub(paths.len());
            self.error = Some("Too many attachments waiting, try again when these are sent".into());
            return cx.notify();
        }
        for path in paths {
            self.attach_queue.push_back(Cmd::Attach { room: room.clone(), path, reply_to: reply_to.take() });
        }
        self.next_attach(cx);
    }

    pub(super) fn next_attach(&mut self, cx: &mut Context<Self>) {
        if self.attaching {
            return;
        }
        let Some(cmd) = self.attach_queue.pop_front() else { return };
        if self.send(cmd, cx) {
            self.attaching = true;
        } else {
            self.uploads = self.uploads.saturating_sub(1 + self.attach_queue.len());
            self.attach_queue.clear();
        }
    }

    /// Follows a finger on the sheet's top; on release it closes or springs back.
    fn drag_gallery(&mut self, e: &ScrollWheelEvent, height: f32, cx: &mut Context<Self>) {
        let dy = f32::from(e.delta.pixel_delta(px(20.)).y);
        let now = Instant::now();
        match e.touch_phase {
            TouchPhase::Started => self.gallery.drag = Some(Drag { offset: dy, samples: VecDeque::from([(now, dy)]) }),
            TouchPhase::Moved => {
                // A mouse wheel has no start, so it never drags.
                let Some(drag) = &mut self.gallery.drag else { return };
                drag.offset += dy;
                drag.samples.push_back((now, drag.offset));
                if drag.samples.len() > 6 {
                    drag.samples.pop_front();
                }
            }
            TouchPhase::Ended => {
                let Some(mut drag) = self.gallery.drag.take() else { return };
                // A finger that rested before lifting has no speed left.
                drag.samples.push_back((now, drag.offset));
                if dismisses(drag.offset, speed(&drag.samples), height) {
                    self.gallery.offset = follow(drag.offset);
                    self.close_gallery(cx);
                }
            }
            TouchPhase::Cancelled => self.gallery.drag = None,
        }
        cx.notify();
    }

    pub(super) fn render_gallery(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let live = self.gallery.open;
        let dragged = self.gallery.drag.as_ref().map(|d| follow(d.offset));
        // Pinned to the finger while it drags, springing back after; at rest while closed, so
        // the next opening starts from the bottom.
        let held = spring("gallery-drag", if live { dragged.unwrap_or(0.) } else { 0. }, motion::RELEASE.with_travel(live && dragged.is_none()), window, cx);
        let motion = if live {
            Transition::new(motion::SHEET_IN).easing(motion::EASE_SHEET)
        } else {
            Transition::new(motion::SHEET_OUT).easing(motion::EASE_OUT)
        };
        let shown = Presence::new("gallery", live).transition(motion).sample(window, cx);
        if !shown.should_render() {
            return None;
        }
        let p = Palette::of(cx);
        let view = window.viewport_size();
        let (view_w, view_h) = (f32::from(view.width), f32::from(view.height));
        let (top, bottom) = crate::insets();
        let width = view_w.min(SHEET_W);
        let height = ((view_h - top) * 0.72).round();
        let offset = if live { held } else { self.gallery.fall_from };
        self.gallery.offset = offset;
        let y = offset + (height - offset) * (1. - shown.progress);
        let present = (1. - y / height).clamp(0., 1.);

        let cols = if width >= 480. { 4 } else { 3 };
        let cell = ((width - GRID_GAP * (cols - 1) as f32) / cols as f32).floor();
        self.gallery.thumb_px = (cell * window.scale_factor()).min(THUMB_MAX) as u32;

        let g = &self.gallery;
        let count = g.selection.0.len();
        let title: SharedString = match count {
            0 => "Photos and videos".into(),
            MAX_SELECTED => format!("{MAX_SELECTED} selected, the most at once").into(),
            n => format!("{n} selected").into(),
        };
        let head = v_flex()
            .id("gallery-head")
            .flex_none()
            .when(live, |el| el.on_scroll_wheel(cx.listener(move |this, e: &ScrollWheelEvent, _, cx| this.drag_gallery(e, height, cx))))
            .child(div().h(px(20.)).flex().items_center().justify_center().child(div().w(px(36.)).h(px(4.)).rounded_full().bg(p.faint.opacity(0.5))))
            .child(
                h_flex()
                    .h(px(TAP))
                    .pl(px(GUTTER + 4.))
                    .pr(px(GUTTER - 4.))
                    .justify_between()
                    .child(div().min_w_0().truncate().text_size(px(14.)).text_color(p.faint).child(title))
                    .child(
                        h_flex()
                            .flex_none()
                            .children(super::contacts::sheet_entry(live, cx))
                            .child(
                        h_flex()
                            .id("gallery-files")
                            .h(px(TAP))
                            .px(px(12.))
                            .gap(px(8.))
                            .flex_none()
                            .rounded_full()
                            .text_size(px(14.))
                            .text_color(p.text)
                            .cursor_pointer()
                            .active(|el| el.bg(p.raised))
                            .child(Icon::new(IconName::File).size_4())
                            .child("Files")
                            .when(live, |el| {
                                el.on_click(cx.listener(|this, _, _, cx| {
                                    this.close_gallery(cx);
                                    this.attach_file(cx);
                                }))
                            }),
                            ),
                    ),
            );

        let notice = match g.access {
            Some(Access::None) => Some(("Allow access to choose photos here, or send them as files.", "Allow")),
            Some(Access::Partial) => Some(("Showing only the photos you allowed.", "Choose more")),
            _ => None,
        };
        let notice = notice.map(|(text, action)| {
            h_flex()
                .flex_none()
                .gap(px(12.))
                .px(px(GUTTER + 4.))
                .pb(px(8.))
                .child(div().flex_1().min_w_0().text_size(px(13.)).text_color(p.muted).child(text))
                .child(
                    div()
                        .id("gallery-ask")
                        .flex_none()
                        .h(px(36.))
                        .px(px(14.))
                        .flex()
                        .items_center()
                        .rounded_full()
                        .bg(p.raised)
                        .text_size(px(14.))
                        .cursor_pointer()
                        .active(|el| el.bg(p.lifted))
                        .child(action)
                        .when(live, |el| el.on_click(cx.listener(|this, _, _, cx| this.ask_gallery(cx)))),
                )
        });

        let grid = if g.items.is_empty() || g.access == Some(Access::None) {
            let empty = g.access != Some(Access::None) && !g.paging && !g.waiting;
            v_flex()
                .flex_1()
                .child(h_flex().gap(px(GRID_GAP)).children((0..TILES).map(|ix| self.render_camera_tile(ix, cell, live, cx))))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(14.))
                        .text_color(p.faint)
                        .when(empty, |el| el.child("No photos or videos")),
                )
                .into_any_element()
        } else {
            let rows = (TILES + g.items.len()).div_ceil(cols);
            uniform_list(
                "gallery-grid",
                rows,
                cx.processor(move |this, range: Range<usize>, _, cx| {
                    if live {
                        this.grid_drawn(range.clone(), cols, cx);
                    }
                    range
                        .map(|row| {
                            let cells = row * cols..((row + 1) * cols).min(TILES + this.gallery.items.len());
                            h_flex()
                                .h(px(cell + GRID_GAP))
                                .pb(px(GRID_GAP))
                                .gap(px(GRID_GAP))
                                .children(cells.map(|ix| match ix.checked_sub(TILES) {
                                    Some(item) => this.render_cell(item, cell, live, cx),
                                    None => this.render_camera_tile(ix, cell, live, cx),
                                }))
                        })
                        .collect()
                }),
            )
            .flex_1()
            .into_any_element()
        };

        let send = (count > 0).then(|| {
            h_flex().flex_none().justify_end().px(px(GUTTER)).pt(px(8.)).child(
                h_flex()
                    .id("gallery-send")
                    .h(px(TAP))
                    .pl(px(18.))
                    .pr(px(14.))
                    .gap(px(8.))
                    .rounded_full()
                    .bg(p.text)
                    .text_color(p.ground)
                    .text_size(px(15.))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .active(|el| el.bg(p.muted))
                    .child(div().font_features(tabular()).child(format!("Send {count}")))
                    .child(Icon::new(IconName::ArrowUp).size_5())
                    .when(live, |el| el.on_click(cx.listener(|this, _, _, cx| this.send_chosen(cx)))),
            )
        });

        Some(
            div()
                .id("gallery")
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .justify_end()
                .items_center()
                .bg(gpui_kit::black().opacity(0.6 * present))
                .when(live, |el| el.occlude().on_click(cx.listener(|this, _, _, cx| this.close_gallery(cx))))
                .child(
                    v_flex()
                        .id("gallery-sheet")
                        .relative()
                        .top(px(y))
                        .w(px(width))
                        .h(px(height + bottom))
                        .pb(px(bottom + 8.))
                        .rounded_t(px(RADIUS))
                        .overflow_hidden()
                        .bg(p.surface)
                        // A tap inside is not a tap outside.
                        .when(live, |el| el.on_click(|_, _, cx| cx.stop_propagation()))
                        .child(head)
                        .children(notice)
                        .child(grid)
                        .children(send),
                )
                .into_any_element(),
        )
    }

    fn render_cell(&mut self, ix: usize, cell: f32, live: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let g = &mut self.gallery;
        let item = &g.items[ix];
        let (id, video, duration) = (item.id, item.video(), item.duration_ms);
        let number = g.selection.number(id);
        let blocked = number.is_none() && g.selection.is_full();
        let thumb = g.thumbs.get(id);
        let p = Palette::of(cx);
        let marker = div()
            .absolute()
            .top(px(6.))
            .right(px(6.))
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .border_2()
            .border_color(gpui_kit::white())
            .text_size(px(13.))
            .font_weight(FontWeight::SEMIBOLD)
            .font_features(tabular())
            .map(|el| match number {
                Some(n) => el.bg(p.text).text_color(p.ground).child(n.to_string()),
                None => el.bg(gpui_kit::black().opacity(0.2)),
            });
        div()
            .id(("gallery-cell", ix))
            .relative()
            .size(px(cell))
            .flex_none()
            .overflow_hidden()
            .bg(p.raised)
            .cursor_pointer()
            .when(blocked, |el| el.opacity(0.4))
            .map(|el| match thumb {
                Some(image) => el.child(img(image).size_full().object_fit(ObjectFit::Cover)),
                None => el.child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(p.faint)
                        .child(Icon::new(if video { IconName::Video } else { IconName::Image }).size_5()),
                ),
            })
            .when(number.is_some(), |el| el.child(div().absolute().inset_0().bg(gpui_kit::black().opacity(0.25))))
            .when(video, |el| {
                el.child(
                    div()
                        .absolute()
                        .left(px(6.))
                        .bottom(px(6.))
                        .px_1p5()
                        .rounded_full()
                        .bg(gpui_kit::black().opacity(0.55))
                        .text_color(gpui_kit::white())
                        .text_size(px(11.))
                        .font_features(tabular())
                        .child(length(Duration::from_millis(duration))),
                )
            })
            .child(marker)
            .when(live, |el| el.on_click(cx.listener(move |this, _, _, cx| this.toggle_item(id, cx))))
    }
}

#[cfg(target_os = "android")]
mod store {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::{JByteArray, JValue};

    use super::{Access, Item, PAGE, parse_page};

    const CLASS: &str = "dev.gpui.beeper.Gallery";

    pub fn access() -> Access {
        let found = helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let access = env
                .call_static_method(&class, jni::jni_str!("access"), jni::jni_sig!("(Landroid/app/Activity;)Ljava/lang/String;"), &[JValue::Object(&activity)])
                .and_then(|v| v.l())
                .map_err(|e| {
                    env.exception_clear();
                    e.to_string()
                })?;
            Ok(helpers::get_string(env, &access))
        });
        match found.map_err(|e| log::warn!("gallery access: {e}")).as_deref() {
            Ok("full") => Access::Full,
            Ok("partial") => Access::Partial,
            _ => Access::None,
        }
    }

    pub fn ask() {
        let asked = helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            env.call_static_method(&class, jni::jni_str!("ask"), jni::jni_sig!("(Landroid/app/Activity;)V"), &[JValue::Object(&activity)])
                .map(|_| ())
                .map_err(|e| {
                    env.exception_clear();
                    e.to_string()
                })
        });
        if let Err(e) = asked {
            log::warn!("gallery permission: {e}");
        }
    }

    pub fn page(after: Option<(i64, i64)>) -> Option<Vec<Item>> {
        let (date, id) = after.unwrap_or((-1, 0));
        let text = helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let text = env
                .call_static_method(
                    &class,
                    jni::jni_str!("page"),
                    jni::jni_sig!("(Landroid/app/Activity;JJI)Ljava/lang/String;"),
                    &[JValue::Object(&activity), JValue::Long(date), JValue::Long(id), JValue::Int(PAGE as i32)],
                )
                .and_then(|v| v.l())
                .map_err(|e| {
                    env.exception_clear();
                    e.to_string()
                })?;
            Ok((!text.is_null()).then(|| helpers::get_string(env, &text)))
        });
        Some(parse_page(&text.map_err(|e| log::warn!("gallery page: {e}")).ok()??))
    }

    pub fn thumb(item: &Item, px: u32) -> Option<Vec<u8>> {
        let bytes = helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let uri = env.new_string(&item.uri).map_err(|e| e.to_string())?;
            let bytes = env
                .call_static_method(
                    &class,
                    jni::jni_str!("thumb"),
                    jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;ZIII)[B"),
                    &[
                        JValue::Object(&activity),
                        JValue::Object(&uri),
                        JValue::Bool(item.video()),
                        JValue::Int(px as i32),
                        JValue::Int(item.width as i32),
                        JValue::Int(item.height as i32),
                    ],
                )
                .and_then(|v| v.l())
                .map_err(|e| {
                    env.exception_clear();
                    e.to_string()
                })?;
            if bytes.is_null() {
                return Ok(None);
            }
            let bytes = env.cast_local::<JByteArray>(bytes).map_err(|e| e.to_string())?;
            env.convert_byte_array(&bytes).map(Some).map_err(|e| e.to_string())
        });
        bytes.map_err(|e| log::warn!("gallery thumbnail: {e}")).ok()?
    }

    pub fn local_copy(item: &Item) -> Option<std::path::PathBuf> {
        crate::android_files::copy_to_cache(&item.uri).map_err(|e| log::warn!("gallery item: {e}")).ok()?.map(Into::into)
    }
}

/// The debug stand-in for MediaStore: the pictures in one directory, newest first. A name ending
/// in `_<seconds>s` (`beach_75s.jpg`) poses as a video of that length.
#[cfg(not(target_os = "android"))]
mod store {
    use std::path::PathBuf;

    use super::{Access, Item, PAGE, page_after};

    pub fn dir() -> Option<PathBuf> {
        std::env::var_os("BEEPER_GPUI_GALLERY").filter(|_| cfg!(debug_assertions)).map(PathBuf::from)
    }

    /// `BEEPER_GPUI_GALLERY_ACCESS` (`none`, `partial`) shows the sheet without full access.
    pub fn access() -> Access {
        match std::env::var("BEEPER_GPUI_GALLERY_ACCESS").ok().filter(|_| cfg!(debug_assertions)).as_deref() {
            Some("none") => Access::None,
            Some("partial") => Access::Partial,
            _ => Access::Full,
        }
    }

    pub fn ask() {}

    pub fn page(after: Option<(i64, i64)>) -> Option<Vec<Item>> {
        let mut files: Vec<(i64, String, PathBuf)> = std::fs::read_dir(dir()?)
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let ext = path.extension()?.to_str()?.to_ascii_lowercase();
                if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif") {
                    return None;
                }
                let modified = entry.metadata().ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
                Some((modified as i64, path.file_name()?.to_string_lossy().into_owned(), path))
            })
            .collect();
        files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let n = files.len() as i64;
        let all: Vec<Item> = files
            .into_iter()
            .zip(0..)
            .map(|((date, name, path), ix)| {
                let stem = name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem);
                let seconds = stem.rsplit_once('_').and_then(|(_, s)| s.strip_suffix('s')?.parse::<u64>().ok());
                Item {
                    id: n - ix,
                    uri: path.to_string_lossy().into_owned(),
                    mime: if seconds.is_some() { "video/mp4".into() } else { mime_guess::from_path(&path).first_or_octet_stream().to_string() },
                    date,
                    duration_ms: seconds.unwrap_or(0) * 1000,
                    width: 0,
                    height: 0,
                }
            })
            .collect();
        Some(page_after(&all, after, PAGE))
    }

    pub fn thumb(item: &Item, px: u32) -> Option<Vec<u8>> {
        let picture = image::ImageReader::open(&item.uri).ok()?.with_guessed_format().ok()?.decode().ok()?;
        let mut out = std::io::Cursor::new(Vec::new());
        picture.resize_to_fill(px, px, image::imageops::FilterType::Triangle).to_rgb8().write_to(&mut out, image::ImageFormat::Jpeg).ok()?;
        Some(out.into_inner())
    }

    pub fn local_copy(item: &Item) -> Option<PathBuf> {
        Some(PathBuf::from(&item.uri))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use gpui_kit::{Image, ImageFormat};

    use super::{Item, MAX_CACHED, MAX_SELECTED, Selection, Thumbs, dismisses, page_after, parse_page, speed};

    fn item(id: i64, date: i64) -> Item {
        Item { id, uri: format!("content://media/external/images/media/{id}"), mime: "image/jpeg".into(), date, duration_ms: 0, width: 4, height: 3 }
    }

    #[test]
    fn chosen_items_keep_their_order_up_to_the_limit() {
        let mut chosen = Selection::default();
        for id in 1..=MAX_SELECTED as i64 {
            assert!(chosen.toggle(id));
        }
        assert!(!chosen.toggle(99), "an eleventh is refused");
        assert_eq!(chosen.number(99), None);
        assert!(chosen.toggle(3), "taking one out");
        assert_eq!((chosen.number(2), chosen.number(4), chosen.number(10)), (Some(2), Some(3), Some(9)));
        assert!(chosen.toggle(99), "room again");
        assert_eq!(chosen.number(99), Some(10));
        assert!(chosen.is_full());
    }

    #[test]
    fn the_thumbnail_cache_keeps_the_last_drawn() {
        let mut thumbs = Thumbs::default();
        let image = || Arc::new(Image::from_bytes(ImageFormat::Png, Vec::new()));
        let mut evicted = 0;
        for id in 0..MAX_CACHED as i64 {
            evicted += thumbs.insert(id, image()).len();
        }
        assert!(thumbs.get(0).is_some(), "drawn again, so newest");
        for id in 1000..1005 {
            evicted += thumbs.insert(id, image()).len();
        }
        assert_eq!((evicted, thumbs.images.len(), thumbs.order.len()), (5, MAX_CACHED, MAX_CACHED));
        assert!(thumbs.get(0).is_some());
        assert!(thumbs.get(1).is_none() && thumbs.get(5).is_none() && thumbs.get(6).is_some());
        assert_eq!(thumbs.insert(6, image()).len(), 0, "replacing is not growing");
    }

    #[test]
    fn pages_cover_every_item_once_even_with_equal_dates() {
        // Newest first by (date, id), as MediaStore returns them.
        let all: Vec<Item> = [(9, 50), (9, 40), (9, 30), (7, 60), (7, 20), (5, 10), (5, 5)].map(|(date, id)| item(id, date)).into();
        let mut seen = Vec::new();
        let mut after = None;
        loop {
            let page = page_after(&all, after, 3);
            seen.extend(page.iter().map(|i| i.id));
            if page.len() < 3 {
                break;
            }
            after = page.last().map(|i| (i.date, i.id));
        }
        assert_eq!(seen, [50, 40, 30, 60, 20, 10, 5]);
    }

    #[test]
    fn a_page_from_java_is_read_and_bad_lines_skipped() {
        let page = parse_page("12\tcontent://media/external/video/media/12\tvideo/mp4\t1700000000\t42000\t1920\t1080\nbroken\n13\tcontent://x/13\timage/png\t1699999999\t0\t0\t0\n");
        assert_eq!(page.len(), 2);
        assert!(page[0].video() && page[0].duration_ms == 42000 && (page[0].width, page[0].height) == (1920, 1080));
        assert_eq!((page[1].id, page[1].date, page[1].video()), (13, 1_699_999_999, false));
    }

    #[test]
    fn a_long_drag_or_a_quick_flick_dismisses() {
        let start = Instant::now();
        let at = |ms: u64, y: f32| (start + Duration::from_millis(ms), y);
        let flick = VecDeque::from([at(0, 0.), at(16, 20.), at(32, 40.)]);
        assert!(speed(&flick) > 1000.);
        assert!(dismisses(40., speed(&flick), 600.));
        let slow = VecDeque::from([at(0, 0.), at(500, 30.), at(1000, 60.)]);
        assert!(!dismisses(60., speed(&slow), 600.), "a short slow drag springs back");
        assert!(dismisses(200., 0., 600.), "past 30 %");
        assert!(!dismisses(-20., 2000., 600.), "flicked up");
    }
}

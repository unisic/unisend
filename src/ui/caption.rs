//! The preview before photos and videos leave: the items large with paging, a caption for each,
//! a way to drop one, the editor for a photo (`edit`), the trim bar for a video (`trim`), an HD
//! switch and Send. Everything bound for a chat as a photo or video comes through
//! `queue_attach`, which hands it here; documents skip it.
//!
//! Desktop has the tray instead, as Discord does: every file attached, documents too, waits as a
//! thumbnail over the composer, and the composer's Enter sends them with what it holds as the
//! first one's caption.
//!
//! Memory: one full picture is decoded at a time, on a background thread. A 160 px thumbnail is
//! kept per item (at most `MAX_SELECTED`, 0.1 MB each); the 1280 px picture only for the item
//! shown and its neighbors (at most 3, 6 MB each decoded).

use std::path::{Path, PathBuf};

mod edit;
mod paint;
mod trim;

use super::gallery::{MAX_QUEUED, MAX_SELECTED};
use super::*;
use crate::backend::caption as prepare;
use crate::i18n::t;

/// A sideways pan this long (px), or one let go this fast (px/s), turns the page.
const PAN_PAGE: f32 = 48.;
const PAN_SPEED: f32 = 500.;
/// Between neighboring pages while they slide.
const PAGE_GAP: f32 = 16.;
/// How far the stage follows a pan past the first or the last item.
const EDGE_GIVE: f32 = 0.3;
const STRIP_CELL: f32 = 48.;

struct Entry {
    id: u64,
    path: PathBuf,
    video: bool,
    caption: String,
    strip: Option<Arc<Image>>,
    big: Option<Arc<Image>>,
    /// Nothing could be drawn for it; the file still sends.
    failed: bool,
    /// `path` is an edited copy: the original and what was done to it.
    edited: Option<(PathBuf, paint::Edits)>,
    /// `path` is a trimmed copy: the original and the stretch kept, in seconds.
    cut: Option<(PathBuf, [f64; 2])>,
}

impl Entry {
    /// `path` is a copy made here, which goes when the item does without being sent.
    fn copied(&self) -> bool {
        self.edited.is_some() || self.cut.is_some()
    }
}

pub(super) struct Preview {
    open: bool,
    entries: Vec<Entry>,
    cur: usize,
    hd: bool,
    next_id: u64,
    room: Option<OwnedRoomId>,
    reply_to: Option<String>,
    loading: bool,
    pan: Option<Pan>,
    /// Where the stage is, sideways, once no finger holds it: it glides home from a let-go pan.
    slide: motion::Glide,
    field: Entity<TextareaState>,
    editor: Option<edit::Editor>,
    trimmer: Option<trim::Trimmer>,
    /// When a finger last touched the editor or the trim bar: the mouse events a tap makes after
    /// it are not a second stroke.
    touched: Option<Instant>,
    _subs: Vec<Subscription>,
}

impl Preview {
    pub fn new(window: &mut Window, cx: &mut Context<BeeperApp>) -> Self {
        let field = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 4).submit_on_enter(true).placeholder(t!("Add a caption")));
        crate::ime::follow(&field, window, cx);
        let subs = vec![cx.subscribe_in(&field, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { shift: false, .. } => {
                this.send_preview(window, cx);
            }
            InputEvent::Change => {
                let text = this.preview.field.read(cx).value().to_string();
                if let Some(entry) = this.preview.entries.get_mut(this.preview.cur) {
                    entry.caption = text;
                }
            }
            other => keyboard_follows(other, Keyboard::Text, cx),
        })];
        Self {
            open: false,
            entries: Vec::new(),
            cur: 0,
            hd: false,
            next_id: 0,
            room: None,
            reply_to: None,
            loading: false,
            pan: None,
            slide: motion::Glide::new(motion::SETTLE.0, motion::SETTLE.1, 0.5, 0.),
            field,
            editor: None,
            trimmer: None,
            touched: None,
            _subs: subs,
        }
    }

    pub fn open(&self) -> bool {
        self.open
    }
}

/// Files wait in the tray over the composer instead of the full preview: a pointer and a
/// keyboard, as on desktop.
pub(super) fn tray() -> bool {
    !gpui_kit::is_mobile()
}

const TRAY_CELL: f32 = 88.;

fn is_media(path: &Path) -> Option<bool> {
    let mime = mime_guess::from_path(path).first()?;
    match mime.type_() {
        mime_guess::mime::IMAGE => Some(false),
        mime_guess::mime::VIDEO => Some(true),
        _ => None,
    }
}

/// A sideways pan over the stage: how far it went, and its last few (time, total) for the speed.
struct Pan {
    total: f32,
    samples: VecDeque<(Instant, f32)>,
}

/// Advances a sideways pan by one event, starting from `from` (a stage still gliding is caught where
/// it is); gives the total and its speed in px/s when it ends. A mouse wheel has no start, so it never
/// moves the stage.
fn pan_step(pan: &mut Option<Pan>, from: f32, dx: f32, dy: f32, phase: TouchPhase, now: Instant) -> Option<(f32, f32)> {
    let sideways = if dx.abs() > dy.abs() { dx } else { 0. };
    match phase {
        TouchPhase::Started => *pan = Some(Pan { total: from + sideways, samples: VecDeque::from([(now, from + sideways)]) }),
        TouchPhase::Moved => {
            if let Some(pan) = pan {
                pan.total += sideways;
                pan.samples.push_back((now, pan.total));
                if pan.samples.len() > 4 {
                    pan.samples.pop_front();
                }
            }
        }
        TouchPhase::Ended => {
            let mut pan = pan.take()?;
            pan.samples.push_back((now, pan.total));
            return Some((pan.total, gallery::speed(&pan.samples)));
        }
        TouchPhase::Cancelled => *pan = None,
    }
    None
}

/// The page a pan let go at `total` (px, positive rightward) and `speed` turns to: -1 the previous,
/// 1 the next, 0 none.
fn page_turn(total: f32, speed: f32) -> isize {
    let flung = speed.abs() >= PAN_SPEED && speed.signum() == total.signum();
    if total == 0. || !(total.abs() >= PAN_PAGE || flung) {
        0
    } else if total > 0. {
        -1
    } else {
        1
    }
}

/// How far the stage follows a pan: 1:1 toward a neighbor, with resistance past the ends.
fn give(total: f32, previous: bool, next: bool) -> f32 {
    let open = if total > 0. { previous } else { next };
    if open { total } else { total * EDGE_GIVE }
}

impl BeeperApp {
    /// Photos and videos among `paths` wait in the preview; the rest is left for the caller to queue.
    /// The callers counted each as an upload already, so those are given back until Send.
    pub(super) fn preview_media(&mut self, room: &OwnedRoomId, paths: Vec<PathBuf>, reply_to: &mut Option<String>, cx: &mut Context<Self>) -> Vec<PathBuf> {
        let (media, rest): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| tray() || is_media(p).is_some());
        if media.is_empty() {
            return rest;
        }
        self.uploads = self.uploads.saturating_sub(media.len());
        let p = &mut self.preview;
        if p.open && p.room.as_ref() != Some(room) {
            self.error = Some(t!("Finish the photos waiting to be sent first").into());
            cx.notify();
            return rest;
        }
        if p.entries.len() + media.len() > MAX_SELECTED {
            self.error = Some(t!("At most {max} photos and videos at once", max = MAX_SELECTED).into());
            cx.notify();
            return rest;
        }
        if !p.open {
            (p.room, p.cur, p.hd) = (Some(room.clone()), 0, false);
            p.open = true;
            // The tray leaves the reply in the composer, which sends it.
            match reply_to.take() {
                Some(id) if tray() => self.compose = Compose::Reply(id),
                reply => p.reply_to = reply,
            }
        }
        for path in media {
            p.next_id += 1;
            let video = is_media(&path) == Some(true);
            // A document has no picture to draw; its name stands in.
            let failed = is_media(&path).is_none();
            p.entries.push(Entry { id: p.next_id, path, video, caption: String::new(), strip: None, big: None, failed, edited: None, cut: None });
        }
        self.pump_preview(cx);
        cx.notify();
        rest
    }

    /// Loads what is missing, one picture at a time: the one shown, its neighbors, then the rest.
    fn pump_preview(&mut self, cx: &mut Context<Self>) {
        let p = &mut self.preview;
        if p.loading || !p.open {
            return;
        }
        let near = |ix: usize| !tray() && ix.abs_diff(p.cur) <= 1;
        let order = std::iter::once(p.cur).chain(p.cur.checked_add(1)).chain(p.cur.checked_sub(1)).chain(0..p.entries.len());
        let Some(ix) = order.into_iter().find(|&ix| p.entries.get(ix).is_some_and(|e| !e.failed && (e.strip.is_none() || near(ix) && e.big.is_none()))) else {
            return;
        };
        let want_big = near(ix);
        let (id, path, video) = (p.entries[ix].id, p.entries[ix].path.clone(), p.entries[ix].video);
        p.loading = true;
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async move {
                    if video {
                        let strip = video_frame(&path, prepare::STRIP_PX)?;
                        Some((strip, if want_big { video_frame(&path, prepare::PREVIEW_PX) } else { None }))
                    } else {
                        prepare::preview(&path, want_big).map_err(|e| log::warn!("preview: {e:#}")).ok()
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.preview.loading = false;
                if let Some(entry) = this.preview.entries.iter_mut().find(|e| e.id == id) {
                    match loaded {
                        Some((strip, big)) => {
                            let image = |bytes: Vec<u8>| image_format(&bytes).map(|format| Arc::new(Image::from_bytes(format, bytes)));
                            if let Some(old) = entry.strip.take() {
                                this.released.push(old, cx);
                            }
                            entry.strip = image(strip);
                            if let Some(big) = big.and_then(image) {
                                entry.big = Some(big);
                            }
                            entry.failed = entry.strip.is_none();
                        }
                        None => entry.failed = true,
                    }
                }
                this.pump_preview(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Frees the large pictures of items out of reach of the one shown.
    fn trim_preview(&mut self, cx: &mut Context<Self>) {
        let cur = self.preview.cur;
        for (ix, entry) in self.preview.entries.iter_mut().enumerate() {
            if ix.abs_diff(cur) > 1
                && let Some(big) = entry.big.take()
            {
                self.released.push(big, cx);
            }
        }
    }

    fn show_item(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let p = &mut self.preview;
        if ix >= p.entries.len() || ix == p.cur {
            return;
        }
        p.cur = ix;
        p.slide.set(0.);
        let text = p.entries[ix].caption.clone();
        p.field.update(cx, |f, cx| f.set_value(text, window, cx));
        self.trim_preview(cx);
        self.pump_preview(cx);
        cx.notify();
    }

    /// The stage follows a sideways pan 1:1; let go, it turns the page or not, and glides home from
    /// the finger's speed in the frame of the page it lands on.
    fn pan_preview(&mut self, e: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let d = e.delta.pixel_delta(px(20.));
        let p = &mut self.preview;
        let (previous, next) = (p.cur > 0, p.cur + 1 < p.entries.len());
        let held = p.pan.is_some();
        let Some((total, speed)) = pan_step(&mut p.pan, p.slide.pos, f32::from(d.x), f32::from(d.y), e.touch_phase, Instant::now()) else {
            if let Some(pan) = &p.pan {
                p.slide.hold(give(pan.total, previous, next));
                cx.notify();
            } else if held {
                // Canceled: back where it was, without a fling.
                p.slide.release(0., 0., Instant::now());
                cx.notify();
            }
            return;
        };
        let at = give(total, previous, next);
        let turn = match page_turn(total, speed) {
            -1 if previous => -1,
            1 if next => 1,
            _ => 0,
        };
        let page = f32::from(frame::viewport(window).width) + PAGE_GAP;
        if turn != 0 {
            let ix = self.preview.cur.saturating_add_signed(turn);
            self.show_item(ix, window, cx);
        }
        let slide = &mut self.preview.slide;
        slide.hold(at + turn as f32 * page);
        if cx.reduce_motion() {
            slide.set(0.);
        } else {
            slide.release(0., speed, Instant::now());
        }
        cx.notify();
    }

    fn remove_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let p = &mut self.preview;
        if p.cur >= p.entries.len() {
            return;
        }
        let gone = p.entries.remove(p.cur);
        if gone.copied() {
            self.drop_copy(&gone.path, cx);
        }
        for image in [gone.strip, gone.big].into_iter().flatten() {
            self.released.push(image, cx);
        }
        let p = &mut self.preview;
        if p.entries.is_empty() {
            return self.close_preview(true, window, cx);
        }
        p.cur = p.cur.min(p.entries.len() - 1);
        let text = p.entries[p.cur].caption.clone();
        p.field.update(cx, |f, cx| f.set_value(text, window, cx));
        self.trim_preview(cx);
        self.pump_preview(cx);
        cx.notify();
    }

    /// Closes without sending; the reply the user was writing comes back to the composer.
    pub(super) fn close_preview(&mut self, restore_reply: bool, window: &mut Window, cx: &mut Context<Self>) {
        let p = &mut self.preview;
        p.open = false;
        p.loading = false;
        p.pan = None;
        p.slide.set(0.);
        let reply = p.reply_to.take();
        let entries = std::mem::take(&mut p.entries);
        let field = p.field.clone();
        for entry in entries {
            if entry.copied() {
                self.drop_copy(&entry.path, cx);
            }
            for image in [entry.strip, entry.big].into_iter().flatten() {
                self.released.push(image, cx);
            }
        }
        self.close_editor(cx);
        self.preview.trimmer = None;
        field.update(cx, |f, cx| f.set_value("", window, cx));
        if restore_reply && let Some(id) = reply {
            self.compose = Compose::Reply(id);
        }
        cx.notify();
    }

    /// The tray's room when it holds files.
    pub(super) fn tray_room(&self) -> Option<&OwnedRoomId> {
        self.preview.room.as_ref().filter(|_| tray() && self.preview.open && !self.preview.entries.is_empty())
    }

    /// Sends the tray, `caption` with its first file; false when nothing left.
    pub(super) fn send_tray(&mut self, caption: String, reply_to: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let p = &mut self.preview;
        for (ix, entry) in p.entries.iter_mut().enumerate() {
            entry.caption = if ix == 0 { caption.clone() } else { String::new() };
        }
        p.reply_to = reply_to;
        let sent = self.send_preview(window, cx);
        if !sent {
            self.preview.reply_to = None;
        }
        sent
    }

    fn tray_remove(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.preview.cur = ix;
        self.remove_item(window, cx);
        self.composer.update(cx, |c, cx| c.focus(window, cx));
    }

    fn send_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let p = &mut self.preview;
        let Some(room) = p.room.clone().filter(|_| p.open && !p.entries.is_empty()) else { return false };
        if self.attach_queue.len() + p.entries.len() > MAX_QUEUED {
            self.error = Some(t!("Too many attachments waiting, try again when these are sent").into());
            cx.notify();
            return false;
        }
        let (hd, mut reply_to) = (p.hd, p.reply_to.take());
        let cmds: Vec<Cmd> = p
            .entries
            .iter()
            .map(|e| Cmd::AttachMedia { room: room.clone(), path: e.path.clone(), reply_to: reply_to.take(), caption: e.caption.clone(), hd })
            .collect();
        // The edited copies go with the send, which deletes them.
        for e in &mut p.entries {
            (e.edited, e.cut) = (None, None);
        }
        self.uploads += cmds.len();
        self.attach_queue.extend(cmds);
        self.close_preview(false, window, cx);
        self.next_attach(cx);
        true
    }

    pub(super) fn render_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.preview.editor.is_some() {
            return self.render_editor(window, cx);
        }
        if self.preview.trimmer.is_some() {
            return self.render_trim(cx);
        }
        let live = self.preview.open && !tray();
        let shown = Presence::new("preview", live).transition(motion::enter()).sample(window, cx);
        if !live {
            return None;
        }
        let p = Palette::of(cx);
        let (top, bottom) = crate::insets();
        let shift = if self.preview.slide.running() {
            window.request_animation_frame();
            self.preview.slide.step(Instant::now())
        } else {
            self.preview.slide.pos
        };
        let width = f32::from(frame::viewport(window).width);
        let pv = &self.preview;
        let count = pv.entries.len();
        let entry = pv.entries.get(pv.cur)?;
        let any_photo = pv.entries.iter().any(|e| !e.video);
        let hd = pv.hd;
        let edit_cur = can_edit(entry);
        let trim_cur = can_trim(entry);

        let bar = h_flex()
            .h(px(TAP))
            .flex_none()
            .px(px(GUTTER - 10.))
            .justify_between()
            .child(round_button("preview-close", IconName::X, cx).on_click(cx.listener(|this, _, window, cx| this.close_preview(true, window, cx))))
            .child(div().text_size(fs(14.)).text_color(p.muted).when(count > 1, |el| el.child(t!("{index} of {count}", index = pv.cur + 1, count = count))))
            .child(
                h_flex().h(px(TAP)).min_w(px(TAP)).flex_none().items_center().justify_end().when(any_photo, |el| {
                    el.child(
                        div()
                            .id("preview-hd")
                            .h(px(36.))
                            .px(px(14.))
                            .flex()
                            .items_center()
                            .rounded_full()
                            .text_size(fs(14.))
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .when(hd, |el| el.bg(p.text).text_color(p.ground))
                            .when(!hd, |el| el.bg(p.raised).text_color(p.muted))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.preview.hd = !this.preview.hd;
                                cx.notify();
                            }))
                            .child("HD"),
                    )
                }),
            );

        let stage = div()
            .id("preview-stage")
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, window, cx| this.pan_preview(e, window, cx)))
            // The neighbor a pan brings in waits a gap beside the page.
            .when_some(pv.cur.checked_sub(1).and_then(|ix| pv.entries.get(ix)).filter(|_| shift > 0.), |el, e| el.child(page(e, shift - width - PAGE_GAP, width, p)))
            .child(page(entry, shift, width, p))
            .when_some(pv.entries.get(pv.cur + 1).filter(|_| shift < 0.), |el, e| el.child(page(e, shift + width + PAGE_GAP, width, p)))
            .child(
                h_flex()
                    .absolute()
                    .top(px(4.))
                    .right(px(GUTTER - 10.))
                    .gap(px(8.))
                    .when(trim_cur, |el| {
                        el.child(
                            round_button("preview-trim", IconName::Scissors, cx)
                                .bg(gpui_kit::black().opacity(0.5))
                                .text_color(gpui_kit::white())
                                .on_click(cx.listener(|this, _, _, cx| this.open_trim(this.preview.cur, cx))),
                        )
                    })
                    .when(edit_cur, |el| {
                        el.child(
                            round_button("preview-edit", IconName::Pencil, cx)
                                .bg(gpui_kit::black().opacity(0.5))
                                .text_color(gpui_kit::white())
                                .on_click(cx.listener(|this, _, window, cx| this.open_editor(this.preview.cur, window, cx))),
                        )
                    })
                    .child(
                        round_button("preview-remove", IconName::Trash, cx)
                            .bg(gpui_kit::black().opacity(0.5))
                            .text_color(gpui_kit::white())
                            .on_click(cx.listener(|this, _, window, cx| this.remove_item(window, cx))),
                    ),
            );

        let strip = (count > 1).then(|| {
            h_flex()
                .id("preview-strip")
                .flex_none()
                .h(px(STRIP_CELL + 12.))
                .px(px(GUTTER))
                .gap(px(6.))
                .items_center()
                .overflow_x_scroll()
                .children(pv.entries.iter().enumerate().map(|(ix, e)| {
                    let here = ix == pv.cur;
                    div()
                        .id(("preview-cell", ix))
                        .size(px(STRIP_CELL))
                        .flex_none()
                        .rounded(px(10.))
                        .overflow_hidden()
                        .bg(p.surface)
                        .cursor_pointer()
                        .opacity(if here { 1. } else { 0.5 })
                        .on_click(cx.listener(move |this, _, window, cx| this.show_item(ix, window, cx)))
                        .when_some(e.strip.clone(), |el, image| el.child(img(image).size_full().object_fit(ObjectFit::Cover)))
                }))
        });

        let compose = h_flex()
            .flex_none()
            .items_end()
            .gap(px(6.))
            .px(px(GUTTER))
            .py(px(8.))
            .child(div().flex_1().min_w_0().min_h(px(TAP)).px(px(6.)).flex().items_center().rounded(px(TAP / 2.)).overflow_hidden().bg(p.surface).child(Textarea::new(&pv.field).appearance(false).text_size(fs(16.))).child(crate::ime::marks(&pv.field)))
            .child(
                div()
                    .id("preview-send")
                    .size(px(TAP))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(p.text)
                    .text_color(p.ground)
                    .cursor_pointer()
                    .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.muted)))
                    .active(|el| el.bg(p.muted))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.send_preview(window, cx);
                    }))
                    .child(Icon::new(IconName::ArrowUp).size_5()),
            );

        Some(
            v_flex()
                .id("preview")
                .absolute()
                .inset_0()
                .pt(px(top))
                .pb(px(bottom))
                .bg(gpui_kit::black())
                .opacity(shown.progress)
                .occlude()
                .child(bar)
                .child(stage)
                .children(strip)
                .child(compose)
                .into_any_element(),
        )
    }

    /// The tray over the composer: a thumbnail per file with its X, and HD while a photo waits.
    pub(super) fn render_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let room = self.tray_room().filter(|r| self.open.as_ref() == Some(*r))?;
        let v = Presence::new(format!("tray-{room}"), true).transition(motion::enter()).sample(window, cx).progress;
        let p = Palette::of(cx);
        let pv = &self.preview;
        let photo = pv.entries.iter().any(|e| is_media(&e.path) == Some(false));
        let hd = pv.hd;
        let cells = pv.entries.iter().enumerate().map(|(ix, e)| {
            let name = e.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            div()
                .id(("tray-cell", e.id))
                .relative()
                .size(px(TRAY_CELL))
                .flex_none()
                .rounded(px(14.))
                .overflow_hidden()
                .bg(p.text.opacity(0.07))
                .map(|el| match e.strip.clone() {
                    // The cell's overflow clips square; the picture rounds its own corners.
                    Some(image) => el.child(img(image).size_full().rounded(px(14.)).object_fit(ObjectFit::Cover)),
                    None => el.child(
                        v_flex()
                            .size_full()
                            .p(px(8.))
                            .gap(px(4.))
                            .items_center()
                            .justify_center()
                            .text_color(p.faint)
                            .child(Icon::new(if e.video { IconName::Video } else { IconName::File }).size_6())
                            .when(e.failed, |el| el.child(div().w_full().text_size(fs(12.)).line_height(fs(16.)).text_center().truncate().child(name))),
                    ),
                })
                .when(e.video && e.strip.is_some(), |el| {
                    el.child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(div().size(px(32.)).flex().items_center().justify_center().rounded_full().bg(gpui_kit::black().opacity(0.5)).text_color(gpui_kit::white()).child(Icon::new(IconName::Play).size_4())),
                    )
                })
                .child(
                    // A 36 target round a 24 disc, in the corner.
                    div()
                        .id(("tray-remove", e.id))
                        .absolute()
                        .top_0()
                        .right_0()
                        .size(px(36.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| this.tray_remove(ix, window, cx)))
                        .child(
                            div()
                                .size(px(24.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .bg(gpui_kit::black().opacity(0.6))
                                .text_color(gpui_kit::white())
                                .hover(|el| el.bg(gpui_kit::black().opacity(0.8)))
                                .child(Icon::new(IconName::X).size_3p5()),
                        ),
                )
                .when(can_edit(e) || can_trim(e), |el| {
                    let video = e.video;
                    el.child(
                        div()
                            .id(("tray-edit", e.id))
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .size(px(36.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| if video { this.open_trim(ix, cx) } else { this.open_editor(ix, window, cx) }))
                            .child(
                                div()
                                    .size(px(24.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .bg(gpui_kit::black().opacity(0.6))
                                    .text_color(gpui_kit::white())
                                    .hover(|el| el.bg(gpui_kit::black().opacity(0.8)))
                                    .child(Icon::new(if video { IconName::Scissors } else { IconName::Pencil }).size_3p5()),
                            ),
                    )
                })
        });
        Some(
            div()
                .flex_none()
                .overflow_hidden()
                .max_h(px((TRAY_CELL + 24.) * v))
                .opacity(v)
                .child(
                    h_flex()
                        .id("tray")
                        .mx(px(GUTTER))
                        .mt(px(8.))
                        .p(px(8.))
                        .gap(px(8.))
                        .items_center()
                        .rounded(px(RADIUS))
                        .bg(p.surface)
                        .overflow_x_scroll()
                        .children(cells)
                        .when(photo, |el| {
                            el.child(
                                div()
                                    .id("tray-hd")
                                    .flex_none()
                                    .h(px(36.))
                                    .px(px(14.))
                                    .flex()
                                    .items_center()
                                    .rounded_full()
                                    .text_size(fs(14.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .cursor_pointer()
                                    .when(hd, |el| el.bg(p.text).text_color(p.ground))
                                    .when(!hd, |el| el.bg(p.raised).text_color(p.muted).hover(|el| el.text_color(p.text)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.preview.hd = !this.preview.hd;
                                        cx.notify();
                                    }))
                                    .child("HD"),
                            )
                        }),
                )
                .into_any_element(),
        )
    }
}

/// A photo the editor can take, once it has been drawn.
fn can_edit(entry: &Entry) -> bool {
    !entry.video && !entry.failed && entry.strip.is_some() && edit::editable(entry.edited.as_ref().map_or(&entry.path, |(source, _)| source))
}

fn can_trim(entry: &Entry) -> bool {
    entry.video && trim::trimmable(entry.cut.as_ref().map_or(&entry.path, |(source, _)| source))
}

/// One item on the stage, `x` from its left edge.
fn page(entry: &Entry, x: f32, width: f32, p: Palette) -> Div {
    let picture = entry.big.clone().or_else(|| entry.strip.clone());
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(x))
        .w(px(width))
        .flex()
        .items_center()
        .justify_center()
        .map(|el| match picture {
            Some(image) => el.child(img(image).size_full().object_fit(ObjectFit::Contain)),
            None if entry.failed => el.child(
                div().text_size(fs(14.)).text_color(p.faint).child(entry.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            ),
            None => el,
        })
        .when(entry.video, |el| {
            el.child(
                div()
                    .absolute()
                    .size(px(56.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(gpui_kit::black().opacity(0.5))
                    .text_color(gpui_kit::white())
                    .child(Icon::new(IconName::Play).size_6()),
            )
        })
}

/// A still from a video, as JPEG with its long side at most `max`. Only Android can read one.
#[cfg(not(target_os = "android"))]
pub(crate) fn video_frame(_: &Path, _: u32) -> Option<Vec<u8>> {
    None
}

#[cfg(target_os = "android")]
pub(crate) fn video_frame(path: &Path, max: u32) -> Option<Vec<u8>> {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::{JByteArray, JValue};

    let bytes = helpers::with_env(|env| {
        let class = helpers::find_app_class(env, "dev.gpui.beeper.VideoFrame")?;
        let path = env.new_string(path.to_string_lossy()).map_err(|e| e.to_string())?;
        let bytes = env
            .call_static_method(&class, jni::jni_str!("frame"), jni::jni_sig!("(Ljava/lang/String;I)[B"), &[JValue::Object(&path), JValue::Int(max as i32)])
            .and_then(|v| v.l())
            .map_err(|e| crate::java_error(env, e))?;
        if bytes.is_null() {
            return Ok(None);
        }
        let bytes = env.cast_local::<JByteArray>(bytes).map_err(|e| e.to_string())?;
        env.convert_byte_array(&bytes).map(Some).map_err(|e| e.to_string())
    });
    bytes.map_err(|e| log::warn!("video frame: {e}")).ok()?
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use std::time::{Duration, Instant};

    use super::{EDGE_GIVE, PAN_PAGE, Path, give, is_media, page_turn, pan_step};
    use gpui_kit::TouchPhase;
    use gpui_kit::TouchPhase::{Cancelled, Ended, Moved, Started};

    /// The page turn for `(dx, dy, phase)` events `gap` apart.
    fn run(events: &[(f32, f32, TouchPhase)], gap: Duration) -> Option<isize> {
        let (t0, mut pan) = (Instant::now(), None);
        events.iter().zip(1..).find_map(|(&(dx, dy, phase), n)| pan_step(&mut pan, 0., dx, dy, phase, t0 + gap * n)).map(|(total, speed)| page_turn(total, speed))
    }

    const SLOW: Duration = Duration::from_millis(200);

    #[test]
    fn a_long_sideways_pan_turns_the_page_toward_where_it_came_from() {
        let far = PAN_PAGE / 2. + 1.;
        assert_eq!(run(&[(0., 0., Started), (-far, 0., Moved), (-far, 0., Moved), (0., 0., Ended)], SLOW), Some(1));
        assert_eq!(run(&[(0., 0., Started), (far, 0., Moved), (far, 0., Moved), (0., 0., Ended)], SLOW), Some(-1));
    }

    #[test]
    fn a_short_flick_turns_the_page_and_a_short_slow_pan_does_not() {
        let quick = Duration::from_millis(8);
        assert_eq!(run(&[(0., 0., Started), (-10., 0., Moved), (-10., 0., Moved), (0., 0., Ended)], quick), Some(1));
        assert_eq!(run(&[(0., 0., Started), (-10., 0., Moved), (-10., 0., Moved), (0., 0., Ended)], SLOW), Some(0));
        // Flung back the other way at the end: the speed must agree with where the pan went.
        assert_eq!(page_turn(-30., 900.), 0);
    }

    #[test]
    fn short_vertical_wheel_and_canceled_pans_stay_put() {
        let far = PAN_PAGE / 2. + 1.;
        assert_eq!(run(&[(0., 0., Started), (far, 0., Moved), (0., 0., Ended)], SLOW), Some(0));
        assert_eq!(run(&[(0., 0., Started), (2., 300., Moved), (2., 300., Moved), (0., 0., Ended)], SLOW), Some(0));
        assert_eq!(run(&[(far, 0., Moved), (far, 0., Moved), (0., 0., Ended)], SLOW), None);
        assert_eq!(run(&[(0., 0., Started), (far, 0., Moved), (far, 0., Moved), (0., 0., Cancelled), (0., 0., Ended)], SLOW), None);
    }

    #[test]
    fn the_stage_resists_past_the_ends() {
        assert_eq!(give(40., true, false), 40.);
        assert_eq!(give(40., false, true), 40. * EDGE_GIVE);
        assert_eq!(give(-40., false, true), -40.);
    }

    #[test]
    fn photos_and_videos_are_media_and_documents_are_not() {
        assert_eq!(is_media(Path::new("a.JPG")), Some(false));
        assert_eq!(is_media(Path::new("a.mp4")), Some(true));
        assert_eq!(is_media(Path::new("a.pdf")), None);
        assert_eq!(is_media(Path::new("a")), None);
    }
}

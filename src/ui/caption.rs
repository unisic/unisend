//! The preview before photos and videos leave: the items large with paging, a caption for each,
//! a way to drop one, an HD switch and Send. Everything bound for a chat as a photo or video comes
//! through `queue_attach`, which hands it here; documents skip it.
//!
//! Memory: one full picture is decoded at a time, on a background thread. A 160 px thumbnail is
//! kept per item (at most `MAX_SELECTED`, 0.1 MB each); the 1280 px picture only for the item
//! shown and its neighbours (at most 3, 6 MB each decoded).

use std::path::{Path, PathBuf};

use super::gallery::{MAX_QUEUED, MAX_SELECTED};
use super::*;
use crate::backend::caption as prepare;
use crate::i18n::t;

/// A sideways pan this long (px), or one let go this fast (px/s), turns the page.
const PAN_PAGE: f32 = 48.;
const PAN_SPEED: f32 = 500.;
/// Between neighbouring pages while they slide.
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
    field: Entity<InputState>,
    _subs: Vec<Subscription>,
}

impl Preview {
    pub fn new(window: &mut Window, cx: &mut Context<BeeperApp>) -> Self {
        let field = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Add a caption")));
        crate::ime::follow(&field, window, cx);
        let subs = vec![cx.subscribe_in(&field, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { shift: false, .. } => this.send_preview(window, cx),
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
            _subs: subs,
        }
    }

    pub fn open(&self) -> bool {
        self.open
    }
}

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

/// How far the stage follows a pan: 1:1 toward a neighbour, with resistance past the ends.
fn give(total: f32, previous: bool, next: bool) -> f32 {
    let open = if total > 0. { previous } else { next };
    if open { total } else { total * EDGE_GIVE }
}

impl BeeperApp {
    /// Photos and videos among `paths` wait in the preview; the rest is left for the caller to queue.
    /// The callers counted each as an upload already, so those are given back until Send.
    pub(super) fn preview_media(&mut self, room: &OwnedRoomId, paths: Vec<PathBuf>, reply_to: &mut Option<String>, cx: &mut Context<Self>) -> Vec<PathBuf> {
        let (media, rest): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| is_media(p).is_some());
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
            (p.room, p.reply_to, p.cur, p.hd) = (Some(room.clone()), reply_to.take(), 0, false);
            p.open = true;
        }
        for path in media {
            p.next_id += 1;
            let video = is_media(&path) == Some(true);
            p.entries.push(Entry { id: p.next_id, path, video, caption: String::new(), strip: None, big: None, failed: false });
        }
        self.pump_preview(cx);
        cx.notify();
        rest
    }

    /// Loads what is missing, one picture at a time: the one shown, its neighbours, then the rest.
    fn pump_preview(&mut self, cx: &mut Context<Self>) {
        let p = &mut self.preview;
        if p.loading || !p.open {
            return;
        }
        let near = |ix: usize| ix.abs_diff(p.cur) <= 1;
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
                                old.remove_asset(cx);
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
                big.remove_asset(cx);
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
                // Cancelled: back where it was, without a fling.
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
        let page = f32::from(window.viewport_size().width) + PAGE_GAP;
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
        for image in [gone.strip, gone.big].into_iter().flatten() {
            image.remove_asset(cx);
        }
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
        for entry in p.entries.drain(..) {
            for image in [entry.strip, entry.big].into_iter().flatten() {
                image.remove_asset(cx);
            }
        }
        p.field.update(cx, |f, cx| f.set_value("", window, cx));
        if restore_reply && let Some(id) = reply {
            self.compose = Compose::Reply(id);
        }
        cx.notify();
    }

    fn send_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let p = &mut self.preview;
        let Some(room) = p.room.clone().filter(|_| p.open && !p.entries.is_empty()) else { return };
        if self.attach_queue.len() + p.entries.len() > MAX_QUEUED {
            self.error = Some(t!("Too many attachments waiting, try again when these are sent").into());
            return cx.notify();
        }
        let (hd, mut reply_to) = (p.hd, p.reply_to.take());
        let cmds: Vec<Cmd> = p
            .entries
            .iter()
            .map(|e| Cmd::AttachMedia { room: room.clone(), path: e.path.clone(), reply_to: reply_to.take(), caption: e.caption.clone(), hd })
            .collect();
        self.uploads += cmds.len();
        self.attach_queue.extend(cmds);
        self.close_preview(false, window, cx);
        self.next_attach(cx);
    }

    pub(super) fn render_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let live = self.preview.open;
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
        let width = f32::from(window.viewport_size().width);
        let pv = &self.preview;
        let count = pv.entries.len();
        let entry = pv.entries.get(pv.cur)?;
        let any_photo = pv.entries.iter().any(|e| !e.video);
        let hd = pv.hd;

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
            // The neighbour a pan brings in waits a gap beside the page.
            .when_some(pv.cur.checked_sub(1).and_then(|ix| pv.entries.get(ix)).filter(|_| shift > 0.), |el, e| el.child(page(e, shift - width - PAGE_GAP, width, p)))
            .child(page(entry, shift, width, p))
            .when_some(pv.entries.get(pv.cur + 1).filter(|_| shift < 0.), |el, e| el.child(page(e, shift + width + PAGE_GAP, width, p)))
            .child(
                div().absolute().top(px(4.)).right(px(GUTTER - 10.)).child(
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
            .child(div().flex_1().min_w_0().min_h(px(TAP)).px(px(6.)).flex().items_center().rounded(px(TAP / 2.)).bg(p.surface).child(Input::new(&pv.field).appearance(false).large()))
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
                    .on_click(cx.listener(|this, _, window, cx| this.send_preview(window, cx)))
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
fn video_frame(_: &Path, _: u32) -> Option<Vec<u8>> {
    None
}

#[cfg(target_os = "android")]
fn video_frame(path: &Path, max: u32) -> Option<Vec<u8>> {
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
    fn short_vertical_wheel_and_cancelled_pans_stay_put() {
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

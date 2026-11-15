//! The attach sheet's Location entry: asks for permission, takes one fix and shows it as a card
//! before it is sent as an `m.location` message. Android only (the platform's LocationManager through
//! `dev.gpui.beeper.Where`); desktop hides the entry. One fix, no live location.

use std::time::Duration;

use super::cards;
use super::sheet::{self, SheetMotion};
use super::*;
use crate::backend::{LocationSupport as Support, Place, location_support};
use crate::i18n::t;

/// How long to wait for a fix before giving up.
const TIMEOUT: Duration = Duration::from_secs(20);
/// Accuracy worse than this (m) is called approximate.
const APPROXIMATE: f64 = 500.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Fix {
    pub lat: f64,
    pub lon: f64,
    /// Metres, when the source says.
    pub accuracy: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
enum State {
    /// The system's question is on screen.
    Asking,
    Denied,
    /// No location source is switched on.
    Off,
    Locating,
    Failed,
    Found(Fix),
}

pub(super) struct Locate {
    pub open: bool,
    state: State,
    epoch: u64,
    asked: bool,
    pub sheet: SheetMotion,
}

impl Default for Locate {
    fn default() -> Self {
        Self { open: false, state: State::Locating, epoch: 0, asked: false, sheet: SheetMotion::default() }
    }
}

/// What the sheet's one line says about an accuracy in metres.
fn accuracy_text(metres: Option<f64>) -> Option<String> {
    let m = metres.filter(|m| *m > 0.)?;
    let size = if m >= 1000. { format!("{:.1} km", m / 1000.) } else { format!("{} m", m.round() as u32) };
    Some(if m > APPROXIMATE { t!("Approximate, within about {size}", size = size) } else { t!("Accurate to about {size}", size = size) })
}

/// `"lat\tlon\taccuracy"` from the Java side, or the word for why there is none.
fn parse_fix(text: &str) -> Result<Fix, State> {
    let mut f = text.trim().split('\t');
    let (Some(lat), Some(lon), Some(acc)) = (f.next(), f.next(), f.next()) else {
        return Err(match text.trim() {
            "off" => State::Off,
            "none" => State::Denied,
            _ => State::Failed,
        });
    };
    let (lat, lon, acc): (f64, f64, f64) = match (lat.parse(), lon.parse(), acc.parse()) {
        (Ok(a), Ok(b), Ok(c)) => (a, b, c),
        _ => return Err(State::Failed),
    };
    if Place::parse(&format!("geo:{lat},{lon}"), "").is_none() {
        return Err(State::Failed);
    }
    Ok(Fix { lat, lon, accuracy: (acc > 0.).then_some(acc) })
}

/// Whether the entry exists: only where there is a phone's location service.
pub(super) fn enabled() -> bool {
    cfg!(target_os = "android")
}

impl BeeperApp {
    pub(super) fn open_location(&mut self, cx: &mut Context<Self>) {
        let l = &mut self.location;
        (l.open, l.epoch) = (true, l.epoch + 1);
        if store::access() == store::Access::None {
            if l.asked {
                l.state = State::Denied;
            } else {
                l.asked = true;
                l.state = State::Asking;
                store::ask();
                self.watch_location(cx);
            }
        } else {
            self.locate(cx);
        }
        cx.notify();
    }

    pub(super) fn close_location(&mut self, cx: &mut Context<Self>) {
        if self.location.open {
            self.location.open = false;
            self.location.epoch += 1;
            cx.notify();
        }
    }

    /// Asks again (or opens the app's settings once Android stops asking) and watches for the answer.
    fn ask_location(&mut self, cx: &mut Context<Self>) {
        self.location.state = State::Asking;
        store::ask();
        self.watch_location(cx);
        cx.notify();
    }

    /// Polls the permission while the sheet waits on it, every 0.5 s and only then: one cheap JNI call.
    fn watch_location(&mut self, cx: &mut Context<Self>) {
        let epoch = self.location.epoch;
        cx.spawn(async move |this, cx| {
            let (mut left, mut ticks) = (false, 0u32);
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let front = crate::FOREGROUND.load(std::sync::atomic::Ordering::Relaxed);
                left |= !front;
                ticks += 1;
                let keep = this.update(cx, |this, cx| {
                    let l = &this.location;
                    if !l.open || l.epoch != epoch || !matches!(l.state, State::Asking | State::Denied) {
                        return false;
                    }
                    if store::access() != store::Access::None {
                        this.locate(cx);
                        return false;
                    }
                    // Back from the dialog with nothing granted, or no dialog came up at all.
                    if this.location.state == State::Asking && ((left && front) || ticks >= 6) {
                        this.location.state = State::Denied;
                        cx.notify();
                    }
                    true
                });
                if !matches!(keep, Ok(true)) {
                    return;
                }
            }
        })
        .detach();
    }

    fn locate(&mut self, cx: &mut Context<Self>) {
        let l = &mut self.location;
        (l.state, l.epoch) = (State::Locating, l.epoch + 1);
        let epoch = l.epoch;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let found = cx.background_spawn(async move { store::fix(TIMEOUT) }).await;
            let _ = this.update(cx, |this, cx| {
                if !this.location.open || this.location.epoch != epoch {
                    return;
                }
                this.location.state = match found.map(|text| parse_fix(&text)) {
                    Some(Ok(fix)) => State::Found(fix),
                    Some(Err(why)) => why,
                    None => State::Failed,
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn send_location(&mut self, fix: Fix, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone() else {
            return;
        };
        let reply_to = match std::mem::replace(&mut self.compose, Compose::New) {
            Compose::Reply(id) => Some(id),
            _ => None,
        };
        self.close_location(cx);
        self.send(Cmd::SendLocation { room, lat: fix.lat, lon: fix.lon, accuracy: fix.accuracy, reply_to }, cx);
    }

    pub(super) fn render_location(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let frame = self.location.sheet.sample(self.location.open, window, cx)?;
        let live = frame.live;
        let p = Palette::of(cx);
        let width = f32::from(window.viewport_size().width).min(SHEET_W);
        let (_, bottom) = crate::insets();
        let state = self.location.state.clone();
        let network = self.open.as_ref().and_then(|id| self.rooms.iter().find(|r| &r.id == id)).and_then(|r| r.network.clone());
        // The bridge's word decides; the guess by name is only for a bridge that publishes none.
        let support = network.as_deref().and_then(location_support).and_then(|s| match self.published() {
            Some(caps) => (caps.location == 1).then_some(Support { block: false, ..s }),
            None => Some(s),
        });
        let blocked = support.as_ref().is_some_and(|s| s.block);

        let button = |id: &'static str, label: &'static str, strong: bool| {
            h_flex()
                .id(id)
                .flex_none()
                .h(px(TAP))
                .px(px(18.))
                .justify_center()
                .rounded_full()
                .text_size(fs(15.))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .map(|el| if strong { el.bg(p.text).text_color(p.ground).active(|el| el.bg(p.muted)) } else { el.bg(p.raised).text_color(p.text).active(|el| el.bg(p.lifted)) })
                .child(label)
        };
        let line = |text: &'static str| div().text_size(fs(14.)).line_height(fs(20.)).text_color(p.muted).child(text);

        let body = match &state {
            State::Asking => v_flex().gap(px(12.)).child(line(t!("Waiting for your answer to the location question."))),
            State::Denied => v_flex()
                .gap(px(12.))
                .child(line(t!("Murmur has no access to your location. Allow it to share where you are.")))
                .child(button("location-allow", t!("Allow"), true).when(live, |el| el.on_click(cx.listener(|this, _, _, cx| this.ask_location(cx))))),
            State::Off => v_flex()
                .gap(px(12.))
                .child(line(t!("Location is switched off on this phone. Turn it on in the quick settings, then try again.")))
                .child(button("location-retry", t!("Try again"), true).when(live, |el| el.on_click(cx.listener(|this, _, _, cx| this.open_location(cx))))),
            State::Locating => v_flex().gap(px(12.)).child(line(t!("Finding your location. This can take up to 20 seconds."))),
            State::Failed => v_flex()
                .gap(px(12.))
                .child(line(t!("No GPS fix in time. Murmur uses only GPS, so try again outdoors or by a window.")))
                .child(button("location-retry", t!("Try again"), true).when(live, |el| el.on_click(cx.listener(|this, _, _, cx| this.open_location(cx))))),
            State::Found(fix) => {
                let fix = *fix;
                let place = Place { lat: fix.lat, lon: fix.lon, description: String::new() };
                v_flex()
                    .gap(px(12.))
                    .child(cards::place_card("location-preview", &place, accuracy_text(fix.accuracy), width - 2. * GUTTER, p, cx))
                    .children(support.as_ref().map(|s| div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.muted).child(s.note)))
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .justify_end()
                            .child(button("location-refresh", t!("Update"), false).when(live, |el| el.on_click(cx.listener(|this, _, _, cx| this.open_location(cx)))))
                            .when(!blocked, |el| {
                                el.child(button("location-send", t!("Send"), true).when(live, |el| el.on_click(cx.listener(move |this, _, _, cx| this.send_location(fix, cx)))))
                            }),
                    )
            }
        };
        let note_only = !matches!(state, State::Found(_));
        let body = body.when(note_only, |el| el.children(support.as_ref().map(|s| div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.muted).child(s.note))));

        let key = match state {
            State::Asking => 0,
            State::Denied => 1,
            State::Off => 2,
            State::Locating => 3,
            State::Failed => 4,
            State::Found(_) => 5,
        };
        let view = cx.entity_id();
        let body = self.location.sheet.fit("location-body", key, body, view, window, cx);
        let head = v_flex()
            .id("location-head")
            .gap(px(12.))
            .when(live, |el| {
                el.on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, _, cx| {
                    if this.location.sheet.drag(e) {
                        this.close_location(cx);
                    }
                    cx.notify();
                }))
            })
            .child(sheet::handle(p.faint))
            .child(div().text_size(fs(16.)).font_weight(FontWeight::SEMIBOLD).child(t!("Share location")));
        Some(
            sheet::scrim("location", frame)
                .when(live, |el| el.on_click(cx.listener(|this, _, _, cx| this.close_location(cx))))
                .child(
                    v_flex()
                        .id("location-sheet")
                        .relative()
                        .top(px(frame.y))
                        .w(px(width))
                        .gap(px(12.))
                        .px(px(GUTTER))
                        .pb(px(bottom + 16.))
                        .rounded_t(px(RADIUS))
                        .bg(p.surface)
                        .when(live, |el| el.on_click(|_, _, cx| cx.stop_propagation()))
                        .child(self.location.sheet.probe(view))
                        .child(head)
                        .child(body),
                )
                .into_any_element(),
        )
    }
}

#[cfg(target_os = "android")]
mod store {
    use std::time::Duration;

    use gpui_mobile::android::jni as helpers;
    use jni::objects::JValue;

    const CLASS: &str = "dev.gpui.beeper.Where";

    #[derive(Clone, Copy, PartialEq)]
    pub enum Access {
        Allowed,
        None,
    }

    pub fn access() -> Access {
        let found = helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let word = env
                .call_static_method(&class, jni::jni_str!("access"), jni::jni_sig!("(Landroid/app/Activity;)Ljava/lang/String;"), &[JValue::Object(&activity)])
                .and_then(|v| v.l())
                .map_err(|e| crate::java_error(env, e))?;
            Ok(helpers::get_string(env, &word))
        });
        match found.map_err(|e| log::warn!("location access: {e}")).as_deref() {
            Ok("fine" | "coarse") => Access::Allowed,
            _ => Access::None,
        }
    }

    pub fn ask() {
        let asked = helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            env.call_static_method(&class, jni::jni_str!("ask"), jni::jni_sig!("(Landroid/app/Activity;)V"), &[JValue::Object(&activity)]).map(|_| ()).map_err(|e| crate::java_error(env, e))
        });
        if let Err(e) = asked {
            log::warn!("location permission: {e}");
        }
    }

    /// Blocks up to `timeout` plus a moment; always off the UI thread.
    pub fn fix(timeout: Duration) -> Option<String> {
        let found = helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let word = env
                .call_static_method(
                    &class,
                    jni::jni_str!("fix"),
                    jni::jni_sig!("(Landroid/app/Activity;J)Ljava/lang/String;"),
                    &[JValue::Object(&activity), JValue::Long(timeout.as_millis() as i64)],
                )
                .and_then(|v| v.l())
                .map_err(|e| crate::java_error(env, e))?;
            Ok((!word.is_null()).then(|| helpers::get_string(env, &word)))
        });
        found.map_err(|e| log::warn!("location fix: {e}")).ok()?
    }
}

#[cfg(not(target_os = "android"))]
mod store {
    use std::time::Duration;

    #[derive(Clone, Copy, PartialEq)]
    pub enum Access {
        #[expect(dead_code, reason = "desktop has no location entry")]
        Allowed,
        None,
    }

    pub fn access() -> Access {
        Access::None
    }

    pub fn ask() {}

    pub fn fix(_: Duration) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{State, accuracy_text, parse_fix};

    #[test]
    fn reads_a_fix() {
        let fix = parse_fix("52.520080\t13.404950\t12").unwrap();
        assert_eq!((fix.lat, fix.lon, fix.accuracy), (52.52008, 13.40495, Some(12.)));
        assert_eq!(parse_fix("1.0\t2.0\t0").unwrap().accuracy, None);
    }

    #[test]
    fn reads_the_reasons() {
        assert_eq!(parse_fix("off"), Err(State::Off));
        assert_eq!(parse_fix("none"), Err(State::Denied));
        assert_eq!(parse_fix("timeout"), Err(State::Failed));
        assert_eq!(parse_fix("95\t2\t3"), Err(State::Failed));
        assert_eq!(parse_fix("x\t2\t3"), Err(State::Failed));
    }

    #[test]
    fn accuracy_wording() {
        assert_eq!(accuracy_text(Some(12.4)).unwrap(), "Accurate to about 12 m");
        assert_eq!(accuracy_text(Some(1500.)).unwrap(), "Approximate, within about 1.5 km");
        assert_eq!(accuracy_text(None), None);
    }
}

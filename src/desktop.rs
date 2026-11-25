//! Desktop: Unisend keeps running with its window closed, as Signal Desktop does, so messages and
//! notifications keep coming. The app's view and the sync outlive the window; a tray icon (Linux),
//! the Dock (macOS), a notification or a second launch opens a new window on the same view.

use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::*;

use crate::ui::BeeperApp;

#[cfg(target_os = "macos")]
mod dock;
#[cfg(target_os = "windows")]
mod taskbar;
#[cfg(target_os = "linux")]
mod tray;

/// Settings > Notifications: closing the window leaves the app running. Only where there is a
/// way back: Windows has no tray icon here and a second launch cannot reach the first.
pub(crate) const CAN_KEEP_RUNNING: bool = cfg!(any(target_os = "linux", target_os = "macos"));

static KEEP_RUNNING: AtomicBool = AtomicBool::new(true);

pub(crate) fn keep_running(on: bool) {
    KEEP_RUNNING.store(on && CAN_KEEP_RUNNING, Ordering::Relaxed);
}

/// How many chats wait: the count on the app's icon (the launcher on Linux, the Dock, the taskbar
/// button on Windows) and the tray icon's dot (Linux). Only this machine's own desktop hears of it.
pub(crate) fn waiting(count: usize, cx: &mut App) {
    #[cfg(target_os = "linux")]
    tray::waiting(count);
    #[cfg(target_os = "macos")]
    dock::count(count);
    #[cfg(target_os = "windows")]
    taskbar::count(count, cx);
    #[cfg(not(target_os = "windows"))]
    let _ = cx;
}

/// The app's view while the process lives, and where its window was when it closed.
struct Main {
    app: Entity<BeeperApp>,
    bounds: Option<WindowBounds>,
}

impl Global for Main {}

/// Keeps `app` for later windows and decides, as `window` closes, between quitting and running on.
pub(crate) fn keep(window: AnyWindowHandle, app: Entity<BeeperApp>, cx: &mut App) {
    cx.set_global(Main { app, bounds: None });
    cx.set_quit_mode(QuitMode::Explicit);
    watch(window, cx);
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() && !KEEP_RUNNING.load(Ordering::Relaxed) {
            cx.quit();
        }
    })
    .detach();
    #[cfg(target_os = "linux")]
    tray::start(cx);
}

fn watch(window: AnyWindowHandle, cx: &mut App) {
    let _ = window.update(cx, |_, window, cx| {
        window.on_window_should_close(cx, |window, cx| {
            closing(window, cx);
            true
        });
    });
}

/// The title bar's close button: what the system's close does.
pub(crate) fn close(window: &mut Window, cx: &mut App) {
    closing(window, cx);
    window.remove_window();
}

fn closing(window: &mut Window, cx: &mut App) {
    if !KEEP_RUNNING.load(Ordering::Relaxed) || !cx.has_global::<Main>() {
        return;
    }
    let bounds = window.window_bounds();
    let main = cx.global_mut::<Main>();
    main.bounds = Some(bounds);
    let app = main.app.clone();
    // The view's text fields and its own observers were made with this window; the next one takes them.
    window.hand_listeners_on(cx);
    app.update(cx, |app, cx| app.hidden(cx));
}

/// Brings the window to front, opening a new one on the kept view when it was closed.
pub(crate) fn show(cx: &mut App) {
    if let Some(window) = cx.windows().into_iter().next() {
        let _ = window.update(cx, |_, window, _| window.activate_window());
        return;
    }
    let Some(Main { app, bounds }) = cx.try_global::<Main>() else { return };
    let (app, bounds) = (app.clone(), *bounds);
    match gpui_kit::open_window(crate::window_options(bounds), cx, |_, _| app) {
        Ok((window, _)) => {
            watch(window, cx);
            let _ = window.update(cx, |_, window, _| window.activate_window());
        }
        Err(e) => log::error!("open window: {e:#}"),
    }
}

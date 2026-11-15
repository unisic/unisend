mod dispatcher;
mod headless;
mod keyboard;
mod platform;
mod system_notifications;
#[cfg(any(feature = "wayland", feature = "x11"))]
mod text_system;
#[cfg(feature = "wayland")]
mod wayland;
#[cfg(feature = "x11")]
mod x11;

#[cfg(any(feature = "wayland", feature = "x11"))]
mod xdg_desktop_portal;

pub use dispatcher::*;
pub(crate) use headless::*;
pub(crate) use keyboard::*;
pub(crate) use platform::*;
#[cfg(any(feature = "wayland", feature = "x11"))]
pub(crate) use text_system::*;
#[cfg(feature = "wayland")]
pub(crate) use wayland::*;
#[cfg(feature = "x11")]
pub(crate) use x11::*;

use std::rc::Rc;
use std::sync::Mutex;

// murmur patch: an xdg-activation token from elsewhere (a notification server's
// ActivationToken signal) for the next `activate()`; latest wins, used once.
static NEXT_ACTIVATION_TOKEN: Mutex<Option<String>> = Mutex::new(None);

/// murmur patch: the next window `activate()` on Wayland uses this token instead of
/// requesting its own, so the compositor treats the raise as user initiated. X11 ignores it.
pub fn set_next_activation_token(token: String) {
    *NEXT_ACTIVATION_TOKEN.lock().unwrap_or_else(|e| e.into_inner()) = Some(token);
}

#[cfg(feature = "wayland")]
pub(crate) fn take_next_activation_token() -> Option<String> {
    NEXT_ACTIVATION_TOKEN.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Returns the default platform implementation for the current OS.
pub fn current_platform(headless: bool) -> Rc<dyn gpui::Platform> {
    #[cfg(feature = "x11")]
    use anyhow::Context as _;

    if headless {
        return Rc::new(LinuxPlatform {
            inner: HeadlessClient::new(),
        });
    }

    match gpui::guess_compositor() {
        #[cfg(feature = "wayland")]
        "Wayland" => Rc::new(LinuxPlatform {
            inner: WaylandClient::new(),
        }),

        #[cfg(feature = "x11")]
        "X11" => Rc::new(LinuxPlatform {
            inner: X11Client::new()
                .context("Failed to initialize X11 client.")
                .unwrap(),
        }),

        "Headless" => Rc::new(LinuxPlatform {
            inner: HeadlessClient::new(),
        }),
        _ => unreachable!(
            r#"At least one of the "wayland" or "x11" features must be enabled on gpui_linux or gpui_platform."#
        ),
    }
}

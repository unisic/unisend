//! The browser for a chat network's sign-in on Linux, started by Unisend (`src/weblogin.rs` there)
//! with one request line on stdin. A program of its own so that only it links WebKitGTK and GTK 3:
//! Unisend starts without them. It shows the page in a WebKitGTK window with an ephemeral profile
//! and prints one answer line: the cookies by name, local storage by key and the bridge script's
//! result by field ID, as one JSON object, or nothing when the window closed before the sign-in
//! was complete. Exit code: 0 once answered, 1 for a bad request, 2 when no window can open.

#[cfg(target_os = "linux")]
mod browser;

#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(browser::run());
}

/// WebKitGTK is a Linux library: elsewhere Unisend opens the system's browser.
#[cfg(not(target_os = "linux"))]
fn main() {}

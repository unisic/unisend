//! The count of waiting chats on the app's icon in the Dock: `NSDockTile`'s badge, which the
//! system draws and which System Settings > Notifications can turn off.

use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;
use objc2_foundation::NSString;

/// Shows `count` on the Dock icon, or nothing for 0. GPUI calls this on the main thread.
pub(super) fn count(count: usize) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
    NSApplication::sharedApplication(mtm).dockTile().setBadgeLabel(label.as_deref());
}

#[cfg(not(target_os = "android"))]
fn main() {
    beeper_gpui::run_desktop();
}

// Android loads the cdylib and calls `android_main`; this binary is unused there.
#[cfg(target_os = "android")]
fn main() {}

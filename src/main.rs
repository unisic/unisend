// Release builds open no console window beside the GUI on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(target_os = "android"))]
fn main() {
    // The app starts itself again as the browser for a chat network's sign-in.
    #[cfg(target_os = "linux")]
    if std::env::args_os()
        .nth(1)
        .is_some_and(|a| a == beeper_gpui::weblogin::ARG)
    {
        std::process::exit(beeper_gpui::weblogin::browser());
    }
    beeper_gpui::run_desktop();
}

// Android loads the cdylib and calls `android_main`; this binary is unused there.
#[cfg(target_os = "android")]
fn main() {}

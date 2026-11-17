// Release builds open no console window beside the GUI on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(target_os = "android"))]
fn main() {
    beeper_gpui::run_desktop();
}

// Android loads the cdylib and calls `android_main`; this binary is unused there.
#[cfg(target_os = "android")]
fn main() {}

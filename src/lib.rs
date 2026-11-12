//! Beeper client: Matrix (matrix-sdk) against Beeper's homeserver, UI in GPUI.
//! Desktop enters through `run_desktop`, Android through `android_main`.

mod backend;
mod beeper;
mod ui;

use std::path::PathBuf;

use gpui_kit::component::Theme;
use gpui_kit::*;

// Icons beyond the 101 the component library embeds; each costs its SVG, about 400 bytes.
gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [Archive, ArchiveRestore, BellOff, ChevronsDown, ClipboardPaste, Mail, MailOpen, Pin, PinOff, SendHorizontal]
);

struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<std::borrow::Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        Ok(paths)
    }
}

fn open(cx: &mut App, dir: PathBuf, window_bounds: Option<WindowBounds>) {
    gpui_kit::init(cx);
    Theme::sync_system_appearance(None, cx);
    let (cmd, events) = backend::start(dir);
    let options = WindowOptions {
        window_bounds,
        titlebar: Some(TitlebarOptions { title: Some("Beeper".into()), ..Default::default() }),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| ui::BeeperApp::new(cmd, events, window, cx)))
        .expect("open main window");
    cx.activate(true);
}

fn private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(target_os = "android"))]
pub fn run_desktop() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,beeper_gpui=info")).init();
    let dir = std::env::var_os("BEEPER_GPUI_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|d| d.join("beeper-gpui")))
        .expect("no data directory");
    private_dir(&dir).expect("create data directory");
    gpui_kit::application().with_assets(Assets).run(move |cx| {
        let bounds = WindowBounds::Windowed(Bounds::centered(None, size(px(1000.), px(700.)), cx));
        open(cx, dir, Some(bounds));
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    use gpui_mobile::android::jni;
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("beeper-gpui"),
    );
    jni::install_panic_hook();
    let dir = app.internal_data_path().expect("app files dir").join("beeper");
    if let Err(e) = private_dir(&dir) {
        log::error!("data dir {}: {e}", dir.display());
        return;
    }
    jni::init_platform(&app);
    let Some(platform) = jni::shared_platform() else {
        log::error!("gpui-mobile platform unavailable");
        return;
    };
    // Blocks on the Android event loop; the closure runs once the native window exists.
    Application::with_platform(platform.into_rc()).with_assets(Assets).run(move |cx| open(cx, dir, None));
}

#[derive(Clone, Copy)]
pub(crate) enum Keyboard {
    Text,
    Email,
    /// Codes and recovery keys: no auto-capitalisation or autocorrect, which would corrupt base58.
    Key,
}

pub(crate) fn show_keyboard(kind: Keyboard) {
    #[cfg(target_os = "android")]
    gpui_mobile::show_keyboard_with_type(match kind {
        Keyboard::Text => gpui_mobile::KeyboardType::Default,
        Keyboard::Email => gpui_mobile::KeyboardType::EmailAddress,
        Keyboard::Key => gpui_mobile::KeyboardType::URL,
    });
    #[cfg(not(target_os = "android"))]
    let _ = kind;
}

/// System clipboard text. gpui-mobile's platform clipboard is an in-process stub, so Android goes through JNI.
pub(crate) fn paste(cx: &gpui_kit::App) -> Option<String> {
    #[cfg(target_os = "android")]
    {
        let _ = cx;
        gpui_mobile::packages::clipboard::get_text().unwrap_or_else(|e| {
            log::warn!("clipboard: {e}");
            None
        })
    }
    #[cfg(not(target_os = "android"))]
    cx.read_from_clipboard().and_then(|c| c.text())
}

pub(crate) fn hide_keyboard() {
    #[cfg(target_os = "android")]
    gpui_mobile::hide_keyboard();
}

/// Top and bottom space taken by system bars and the soft keyboard, in logical px.
pub(crate) fn insets() -> (f32, f32) {
    #[cfg(target_os = "android")]
    {
        let safe = gpui_mobile::android::jni::platform()
            .and_then(|p| p.primary_window())
            .map(|w| w.safe_area_insets_logical());
        let (top, bottom) = safe.map_or((0., 0.), |s| (s.top, s.bottom));
        let keyboard = gpui_mobile::keyboard_height();
        return (top, if keyboard > 0. { keyboard } else { bottom });
    }
    #[allow(unreachable_code)]
    (0., 0.)
}

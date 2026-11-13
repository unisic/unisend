//! Beeper client: Matrix (matrix-sdk) against Beeper's homeserver, UI in GPUI.
//! Desktop enters through `run_desktop`, Android through `android_main`.

// The matrix-sdk futures nested in the command handler exceed the default layout query depth.
#![recursion_limit = "256"]

mod backend;
mod beeper;
mod bridges;
mod ui;

use std::path::PathBuf;

use gpui_kit::component::Theme;
use gpui_kit::*;

// Icons beyond the 101 the component library embeds; each costs its SVG, about 400 bytes.
gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        Archive,
        ArchiveRestore,
        BellOff,
        ChevronsDown,
        ClipboardPaste,
        Image,
        ListFilter,
        Mail,
        MailOpen,
        MessageCircle,
        Mic,
        Music,
        Paperclip,
        Pencil,
        Pin,
        PinOff,
        Reply,
        SendHorizontal,
        Trash,
        Video,
        X
    ]
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
    #[cfg(not(target_os = "android"))]
    bundled_emoji(cx);
    #[cfg(target_os = "android")]
    extra_fonts(cx);
    let settings = Settings::load(&dir);
    cx.set_global(settings.theme);
    sync_theme(None, cx);
    let (cmd, events) = backend::start(dir.clone());
    let options = WindowOptions {
        window_bounds,
        titlebar: Some(TitlebarOptions { title: Some("Beeper".into()), ..Default::default() }),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| ui::BeeperApp::new(cmd, events, dir, settings, window, cx)))
        .expect("open main window");
    cx.activate(true);
}

/// Makes emoji draw on desktops whose "Noto Color Emoji" is the COLRv1 build (Fedora, others):
/// GPUI's shaper draws bitmap emoji only, and it picks the emoji font by that family name.
/// shortcut: leans on GPUI dropping faces without a Latin 'm' when a family is requested, which
/// removes the system face before ours is added; replace once GPUI can draw COLRv1.
#[cfg(not(target_os = "android"))]
fn bundled_emoji(cx: &mut App) {
    let emoji: &[u8] = include_bytes!("../android/app/src/main/assets/fonts/NotoColorEmoji.ttf");
    let _ = cx.text_system().resolve_font(&font("Noto Color Emoji"));
    if let Err(e) = cx.text_system().add_fonts(vec![emoji.into()]) {
        log::warn!("emoji font: {e}");
    }
}

/// Follows the system's light or dark mode and puts Beeper's colors over the library's.
pub(crate) fn sync_theme(window: Option<&mut Window>, cx: &mut App) {
    use gpui_kit::component::ThemeMode;
    match cx.try_global::<ThemeChoice>().copied().unwrap_or_default() {
        ThemeChoice::Dark => Theme::change(ThemeMode::Dark, window, cx),
        ThemeChoice::Light => Theme::change(ThemeMode::Light, window, cx),
        ThemeChoice::System => Theme::sync_system_appearance(window, cx),
    }
    apply_palette(cx);
}

/// Dark unless the user chooses otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum ThemeChoice {
    #[default]
    Dark,
    Light,
    System,
}

impl Global for ThemeChoice {}

/// What the settings page changes, kept in `settings.json` in the data directory.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Settings {
    #[serde(default)]
    pub theme: ThemeChoice,
    /// Read receipts stay private: others do not see what has been read.
    #[serde(default = "Settings::private_by_default")]
    pub incognito: bool,
}

impl Settings {
    fn private_by_default() -> bool {
        true
    }

    fn path(dir: &std::path::Path) -> PathBuf {
        dir.join("settings.json")
    }

    fn load(dir: &std::path::Path) -> Self {
        let stored = std::fs::read(Self::path(dir)).ok().and_then(|raw| serde_json::from_slice(&raw).ok());
        stored.unwrap_or(Self { theme: ThemeChoice::default(), incognito: true })
    }

    pub fn save(&self, dir: &std::path::Path) {
        let written = serde_json::to_vec_pretty(self).map_err(std::io::Error::other).and_then(|raw| std::fs::write(Self::path(dir), raw));
        if let Err(e) = written {
            log::warn!("settings.json: {e}");
        }
    }
}

/// Beeper's colors. The dark set is sampled from Beeper Android 4.64; the light set mirrors it in
/// the same hue. The accent is this project's choice: Beeper's could not be read from the apps.
fn apply_palette(cx: &mut App) {
    let c = |hex: u32| -> Hsla { rgb(hex).into() };
    let (bg, fg, dim, raised, raised_2, bar) = if Theme::global(cx).is_dark() {
        (0x110b1a, 0xefe0ff, 0xb4a5c8, 0x231c2c, 0x2d2539, 0x171022)
    } else {
        (0xffffff, 0x1b1326, 0x6f637f, 0xf3eef9, 0xe6ddf1, 0xf7f3fb)
    };
    let (accent, accent_hover, on_accent) = (0x6d5ef5, 0x7b6df7, 0xffffff);
    Theme::update(cx, |theme| {
        let k = &mut theme.colors;
        (k.background, k.foreground, k.muted_foreground) = (c(bg), c(fg), c(dim));
        (k.muted, k.secondary, k.secondary_hover, k.secondary_active) = (c(raised), c(raised), c(raised_2), c(raised_2));
        (k.secondary_foreground, k.accent, k.accent_foreground) = (c(fg), c(raised), c(fg));
        (k.list, k.list_hover, k.list_active, k.list_even) = (c(bg), c(raised), c(raised_2), c(bg));
        (k.border, k.input, k.title_bar, k.title_bar_border) = (c(raised_2), c(raised_2), c(bar), c(raised_2));
        (k.popover, k.popover_foreground, k.sidebar, k.sidebar_foreground) = (c(bar), c(fg), c(bg), c(fg));
        (k.primary, k.primary_hover, k.primary_active, k.primary_foreground) = (c(accent), c(accent_hover), c(accent), c(on_accent));
        (k.button_primary, k.button_primary_hover, k.button_primary_active) = (c(accent), c(accent_hover), c(accent));
        (k.button_primary_foreground, k.ring, k.caret) = (c(on_accent), c(accent), c(fg));
    });
}

/// Fonts gpui-mobile does not load: a bold Roboto (the system's is one variable font, which the
/// shaper does not embolden), and the system symbols font for mathematical alphanumerics (names
/// typed in "fancy" letters), circled letters and arrows. About 1.3 MB of font data.
#[cfg(target_os = "android")]
fn extra_fonts(cx: &mut App) {
    let bold: &[u8] = include_bytes!("../assets/fonts/Roboto-Bold.ttf");
    let files = ["NotoSansSymbols-Regular-Subsetted.ttf", "NotoSansSymbols-Regular-Subsetted2.ttf", "SECFallback.ttf"];
    let system = files.iter().filter_map(|f| std::fs::read(format!("/system/fonts/{f}")).ok()).map(Into::into);
    if let Err(e) = cx.text_system().add_fonts(std::iter::once(bold.into()).chain(system).collect()) {
        log::warn!("extra fonts: {e}");
    }
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
    // Android can create a second instance of the activity in a process that already runs the
    // app: relaunching the task (from recents, say) starts the new one before destroying the old.
    // GPUI cannot be built twice in one process, so end it; Android then starts a fresh process
    // for the instance it just asked for.
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        std::process::exit(0);
    }
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
    // The loop ends when the activity is destroyed, but Android keeps the process, and a GPUI app
    // cannot be built a second time in one process: reopening would panic and leave the app
    // unable to start. Ending the process makes the next launch a clean one.
    std::process::exit(0);
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

pub(crate) fn copy(cx: &mut gpui_kit::App, text: String) {
    #[cfg(target_os = "android")]
    {
        let _ = cx;
        if let Err(e) = gpui_mobile::packages::clipboard::set_text(&text) {
            log::warn!("clipboard: {e}");
        }
    }
    #[cfg(not(target_os = "android"))]
    cx.write_to_clipboard(ClipboardItem::new_string(text));
}

/// Hands a downloaded file to whatever the system opens it with.
pub(crate) fn open_file(cx: &mut gpui_kit::App, path: &std::path::Path, mime: Option<&str>) {
    #[cfg(not(target_os = "android"))]
    {
        let _ = mime;
        cx.open_with_system(path);
    }
    #[cfg(target_os = "android")]
    {
        let _ = cx;
        match android_files::view(&path.to_string_lossy(), mime.unwrap_or("*/*")) {
            Ok(true) => {}
            Ok(false) => log::warn!("no app opens {}", path.display()),
            Err(e) => log::warn!("open {}: {e}", path.display()),
        }
    }
}

/// Lets the user choose a file to send. `None` when cancelled or unreadable.
pub(crate) fn pick_file(cx: &mut gpui_kit::App) -> gpui_kit::Task<Option<PathBuf>> {
    #[cfg(not(target_os = "android"))]
    {
        let options = PathPromptOptions { files: true, directories: false, multiple: false, prompt: None };
        let picked = cx.prompt_for_paths(options);
        cx.background_spawn(async move { picked.await.ok()?.ok()??.into_iter().next() })
    }
    // The picker blocks its caller until the user is done, so it runs off the UI thread.
    #[cfg(target_os = "android")]
    cx.background_spawn(async {
        let picked = gpui_mobile::packages::file_selector::open_file(&Default::default());
        let uri = picked.map_err(|e| log::warn!("file picker: {e}")).ok()??.path;
        let copied = android_files::copy_to_cache(&uri);
        copied.map_err(|e| log::warn!("picked file: {e}")).ok()?.map(PathBuf::from)
    })
}

/// Whether the window is in front. Notifications are for when it is not.
pub(crate) static FOREGROUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Starts or stops staying connected in the background (Android: a foreground service, which
/// also asks for the notification permission). Desktop stays connected while it runs.
pub(crate) fn background(on: bool) {
    #[cfg(target_os = "android")]
    if let Err(e) = android_files::notify(if on { "start" } else { "stop" }, &[]) {
        log::warn!("background connection: {e}");
    }
    #[cfg(not(target_os = "android"))]
    let _ = on;
}

/// Shows a message notification for a chat; a newer one for the same chat replaces it.
/// shortcut: Android only, desktop has none yet; add them through the platform's notification
/// service when the desktop app is used day to day.
pub(crate) fn notify(chat: &str, title: &str, text: &str) {
    #[cfg(target_os = "android")]
    if let Err(e) = android_files::notify("message", &[chat, title, text]) {
        log::warn!("notification: {e}");
    }
    #[cfg(not(target_os = "android"))]
    let _ = (chat, title, text);
}

pub(crate) fn clear_notification(chat: &str) {
    #[cfg(target_os = "android")]
    if let Err(e) = android_files::notify("clear", &[chat]) {
        log::warn!("notification: {e}");
    }
    #[cfg(not(target_os = "android"))]
    let _ = chat;
}

/// Signs in to a chat network in a browser and returns its cookies as `name=value; ...`.
/// Desktop has no embedded browser: it opens the page in the system's, and the user pastes the
/// cookies; so it returns nothing.
pub(crate) fn web_login(cx: &mut gpui_kit::App, wanted: &bridges::Cookies) -> gpui_kit::Task<Option<String>> {
    #[cfg(not(target_os = "android"))]
    {
        cx.open_url(&wanted.url);
        gpui_kit::Task::ready(None)
    }
    #[cfg(target_os = "android")]
    {
        let (url, agent, pattern) = (wanted.url.clone(), wanted.user_agent.clone(), wanted.wait_for.clone());
        let mut domains: Vec<&str> = wanted.fields.iter().flat_map(|f| &f.cookies).map(|c| c.1.as_str()).filter(|d| !d.is_empty()).collect();
        domains.sort_unstable();
        domains.dedup();
        let domains = domains.join(",");
        let required: Vec<String> = wanted.fields.iter().filter(|f| f.required).flat_map(|f| &f.cookies).map(|c| format!("{}@{}", c.0, c.1)).collect();
        let required = required.join(",");
        // Blocks until the browser closes, so off the UI thread.
        cx.background_spawn(async move {
            android_files::web_login(&url, &agent, &pattern, &domains, &required).map_err(|e| log::warn!("web login: {e}")).ok()?
        })
    }
}

/// Calls into `dev.gpui.beeper.Files`, this app's Java helper.
#[cfg(target_os = "android")]
mod android_files {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::JValue;

    const CLASS: &str = "dev.gpui.beeper.Files";

    pub fn view(path: &str, mime: &str) -> Result<bool, String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let path = env.new_string(path).map_err(|e| e.to_string())?;
            let mime = env.new_string(mime).map_err(|e| e.to_string())?;
            env.call_static_method(
                &class,
                jni::jni_str!("view"),
                jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;Ljava/lang/String;)Z"),
                &[JValue::Object(&activity), JValue::Object(&path), JValue::Object(&mime)],
            )
            .and_then(|v| v.z())
            .map_err(|e| {
                env.exception_clear();
                e.to_string()
            })
        })
    }

    pub fn web_login(url: &str, agent: &str, pattern: &str, domains: &str, required: &str) -> Result<Option<String>, String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, "dev.gpui.beeper.WebLogin")?;
            let mut text = |s: &str| env.new_string(s).map_err(|e| e.to_string());
            let (url, agent, pattern, domains, required) = (text(url)?, text(agent)?, text(pattern)?, text(domains)?, text(required)?);
            let cookies = env
                .call_static_method(
                    &class,
                    jni::jni_str!("run"),
                    jni::jni_sig!(
                        "(Landroid/app/Activity;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;"
                    ),
                    &[
                        JValue::Object(&activity),
                        JValue::Object(&url),
                        JValue::Object(&agent),
                        JValue::Object(&pattern),
                        JValue::Object(&domains),
                        JValue::Object(&required),
                    ],
                )
                .and_then(|v| v.l())
                .map_err(|e| {
                    env.exception_clear();
                    e.to_string()
                })?;
            Ok((!cookies.is_null()).then(|| helpers::get_string(env, &cookies)))
        })
    }

    /// Calls a `void` static method of `dev.gpui.beeper.Notify` taking the activity and up to
    /// three strings.
    pub fn notify(method: &str, args: &[&str]) -> Result<(), String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, "dev.gpui.beeper.Notify")?;
            let mut strings = Vec::with_capacity(args.len());
            for arg in args {
                strings.push(env.new_string(arg).map_err(|e| e.to_string())?);
            }
            let mut values = vec![JValue::Object(&activity)];
            values.extend(strings.iter().map(|s| JValue::Object(s)));
            let name = jni::strings::JNIString::new(method);
            let called = match args.len() {
                0 => env.call_static_method(&class, &name, jni::jni_sig!("(Landroid/app/Activity;)V"), &values),
                1 => env.call_static_method(&class, &name, jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;)V"), &values),
                _ => env.call_static_method(
                    &class,
                    &name,
                    jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V"),
                    &values,
                ),
            };
            called.map(|_| ()).map_err(|e| {
                env.exception_clear();
                e.to_string()
            })
        })
    }

    pub fn copy_to_cache(uri: &str) -> Result<Option<String>, String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let uri = env.new_string(uri).map_err(|e| e.to_string())?;
            let path = env
                .call_static_method(
                    &class,
                    jni::jni_str!("copyToCache"),
                    jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;)Ljava/lang/String;"),
                    &[JValue::Object(&activity), JValue::Object(&uri)],
                )
                .and_then(|v| v.l())
                .map_err(|e| {
                    env.exception_clear();
                    e.to_string()
                })?;
            Ok((!path.is_null()).then(|| helpers::get_string(env, &path)))
        })
    }
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

#[cfg(test)]
mod tests {
    use super::Assets;
    use gpui_kit::AssetSource as _;

    /// An icon the UI names but the build does not embed draws as nothing, with only a log line.
    #[test]
    fn every_icon_the_ui_uses_is_embedded() {
        for source in [include_str!("ui.rs"), include_str!("ui/panels.rs")] {
            for name in source.split("IconName::").skip(1) {
                let name: String = name.chars().take_while(char::is_ascii_alphanumeric).collect();
                let mut file = String::new();
                for (ix, c) in name.chars().enumerate() {
                    if c.is_ascii_uppercase() && ix > 0 {
                        file.push('-');
                    }
                    file.push(c.to_ascii_lowercase());
                }
                let path = format!("icons/{file}.svg");
                assert!(matches!(Assets.load(&path), Ok(Some(_))), "{path} is used but not embedded");
            }
        }
    }
}

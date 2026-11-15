//! Beeper client: Matrix (matrix-sdk) against Beeper's homeserver, UI in GPUI.
//! Desktop enters through `run_desktop`, Android through `android_main`.

// The matrix-sdk futures nested in the command handler exceed the default layout query depth.
#![recursion_limit = "256"]

mod applock;
mod backend;
mod beeper;
mod bridges;
mod emoji;
mod i18n;
mod ime;
#[cfg(not(target_os = "android"))]
mod notify;
mod share;
mod ui;
mod vault;
mod vcard;

use std::path::PathBuf;

use gpui_kit::component::Theme;
use gpui_kit::*;
use crate::i18n::t;

// Icons beyond the 101 the component library embeds; each costs its SVG, about 400 bytes.
gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        Archive,
        ArchiveRestore,
        ArrowUp,
        AtSign,
        BellOff,
        Camera,
        ChartBar,
        ChevronsDown,
        ClipboardPaste,
        Download,
        ExternalLink,
        Forward,
        Image,
        Link,
        ListChecks,
        ListFilter,
        Lock,
        LockOpen,
        MonitorSmartphone,
        Shield,
        MapPin,
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
        RotateCw,
        SendHorizontal,
        Share2,
        Square,
        Sticker,
        Timer,
        Trash,
        Unlink,
        Video,
        Waypoints,
        X,
        Clock,
        FaceSlightlySmiling,
        Hand,
        PawPrint,
        Pizza,
        Plane,
        Trophy,
        Lightbulb,
        Hash,
        Flag,
        LogOut,
        Users,
        CheckCheck,
        Circle,
        CircleCheck,
        CircleAlert
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
    ui::motion::follow_system(cx);
    let mut settings = Settings::load(&dir);
    apply_language(&mut settings, &dir);
    let own = emoji::apply(&mut settings, &dir, cx);
    if !own {
        emoji::use_bundled(&dir, cx);
    }
    fonts(cx);
    cx.set_global(settings.theme);
    cx.set_global(settings.chat_colour);
    sync_theme(None, cx);
    let (cmd, events) = backend::start(dir.clone());
    let options = WindowOptions {
        window_bounds,
        titlebar: Some(TitlebarOptions { title: Some("Unisend".into()), ..Default::default() }),
        // Wayland app id; matches packaging/linux/dev.gpui.beeper.desktop so the shell finds the icon.
        app_id: Some("dev.gpui.beeper".into()),
        // The app draws its own title bar (`ui/frame.rs`); a compositor that insists on drawing one wins.
        #[cfg(target_os = "linux")]
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };
    let (window, app) = gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| ui::BeeperApp::new(cmd, events, dir, settings, window, cx)))
        .expect("open main window");
    #[cfg(not(target_os = "android"))]
    notify::attach(window, app.downgrade(), cx);
    #[cfg(target_os = "android")]
    let _ = (window, app);
    cx.activate(true);
}

/// Takes the language Android 13 and later keep for the app (Settings, Apps, Unisend, Language)
/// into `settings`, so the two never disagree, and applies it. True when the interface language changed.
pub(crate) fn apply_language(settings: &mut Settings, dir: &std::path::Path) -> bool {
    let before = i18n::polish();
    if let Some((app, system_polish)) = platform_language() {
        i18n::set_system(system_polish);
        if let Some(app) = app.filter(|app| *app != settings.language) {
            settings.language = app;
            settings.save(dir);
        }
    }
    i18n::set(settings.language);
    i18n::polish() != before
}

/// Android: the app's language (`None` before 13, which keeps none) and whether the phone's is Polish.
fn platform_language() -> Option<(Option<i18n::LanguageChoice>, bool)> {
    #[cfg(target_os = "android")]
    {
        android_lang::read()
            .map(|(app, system)| (app.as_deref().map(i18n::from_tag), system == "pl"))
            .inspect_err(|e| log::warn!("app language: {e}"))
            .ok()
    }
    #[cfg(not(target_os = "android"))]
    None
}

/// Android 13 and later: keeps the choice as the app's language, so the texts Android draws for the
/// app (notifications, the lock prompt, the player) follow it and the system settings show it.
pub(crate) fn set_platform_language(choice: i18n::LanguageChoice) {
    #[cfg(target_os = "android")]
    if let Err(e) = android_lang::set(i18n::tag(choice)) {
        log::warn!("app language: {e}");
    }
    #[cfg(not(target_os = "android"))]
    let _ = choice;
}

/// Follows the chosen or the system's light or dark mode and puts the app's colours over the library's.
pub(crate) fn sync_theme(window: Option<&mut Window>, cx: &mut App) {
    use gpui_kit::component::ThemeMode;
    match cx.try_global::<ThemeChoice>().copied().unwrap_or_default() {
        ThemeChoice::Dark => Theme::change(ThemeMode::Dark, window, cx),
        ThemeChoice::Light => Theme::change(ThemeMode::Light, window, cx),
        // gpui-mobile answers Dark for the app and Light for a window until the first configuration
        // change, so Android's night mode is asked for directly.
        #[cfg(target_os = "android")]
        ThemeChoice::System => {
            let night = gpui_mobile::android::jni::query_night_mode_via_jni();
            Theme::change(if night { ThemeMode::Dark } else { ThemeMode::Light }, window, cx)
        }
        #[cfg(not(target_os = "android"))]
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

/// The colour of the user's own bubbles: Beeper's nine, or the Night design's `raised` grey.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum ChatColour {
    #[default]
    Default,
    Blue,
    Indigo,
    Purple,
    Pink,
    Red,
    Orange,
    Green,
    Teal,
    Slate,
}

impl Global for ChatColour {}

impl ChatColour {
    pub const ALL: [Self; 10] = [
        Self::Default,
        Self::Blue,
        Self::Indigo,
        Self::Purple,
        Self::Pink,
        Self::Red,
        Self::Orange,
        Self::Green,
        Self::Teal,
        Self::Slate,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => t!("Default"),
            Self::Blue => t!("Blue"),
            Self::Indigo => t!("Indigo"),
            Self::Purple => t!("Purple"),
            Self::Pink => t!("Pink"),
            Self::Red => t!("Red"),
            Self::Orange => t!("Orange"),
            Self::Green => t!("Green"),
            Self::Teal => t!("Teal"),
            Self::Slate => t!("Slate"),
        }
    }

    /// Beeper's swatch, the same in both modes; `None` for the design's own grey. Blue is Beeper's
    /// #3478F6 darkened a little: neither white nor dark text reaches 4.5:1 on the original.
    pub fn fill(self) -> Option<u32> {
        Some(match self {
            Self::Default => return None,
            Self::Blue => 0x2e6ef0,
            Self::Indigo => 0x4f5bd5,
            Self::Purple => 0x734ef3,
            Self::Pink => 0xc2185b,
            Self::Red => 0xd32f2f,
            Self::Orange => 0xe8710a,
            Self::Green => 0x2e7d32,
            Self::Teal => 0x00838f,
            Self::Slate => 0x37474f,
        })
    }
}

/// WCAG relative luminance of an sRGB colour.
fn luminance(hex: u32) -> f32 {
    let lin = |shift: u32| {
        let c = ((hex >> shift) & 0xff) as f32 / 255.;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(16) + 0.7152 * lin(8) + 0.0722 * lin(0)
}

/// WCAG contrast ratio of two sRGB colours.
fn contrast(a: u32, b: u32) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The text on a bubble of this fill: white, or Beeper's near-black where that reads better.
fn ink_on(fill: u32) -> u32 {
    const WHITE: u32 = 0xffffff;
    const DARK: u32 = 0x1a1a1a;
    if contrast(fill, WHITE) >= contrast(fill, DARK) { WHITE } else { DARK }
}

/// What the settings page changes, kept in `settings.json` in the data directory.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Settings {
    #[serde(default)]
    pub theme: ThemeChoice,
    /// Interface language; `System` follows the device.
    #[serde(default)]
    pub language: i18n::LanguageChoice,
    /// Read receipts stay private: others do not see what has been read.
    #[serde(default = "Settings::private_by_default")]
    pub incognito: bool,
    /// Own bubbles' colour, kept on this device like the rest.
    #[serde(default)]
    pub chat_colour: ChatColour,
    /// Family name of the user's own emoji font, kept as `emoji::FILE`; `None` for the system's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji_font: Option<String>,
    #[serde(default)]
    pub lock: applock::LockSettings,
    /// Text size step, for messages and the rest of the interface.
    #[serde(default)]
    pub text_size: ui::appearance::TextSize,
    /// What is drawn behind the messages, for all chats.
    #[serde(default)]
    pub wallpaper: ui::appearance::Wallpaper,
    /// Percent of black over a photo wallpaper.
    #[serde(default = "ui::appearance::default_dim")]
    pub wallpaper_dim: u8,
    /// Android: a foreground service keeps the connection while the app is closed.
    #[serde(default = "Settings::private_by_default")]
    pub stay_connected: bool,
    /// What a chat row swiped right to left does.
    #[serde(default = "ui::rowswipe::SwipeAction::left_default")]
    pub swipe_left: ui::rowswipe::SwipeAction,
    /// What a chat row swiped left to right does.
    #[serde(default = "ui::rowswipe::SwipeAction::right_default")]
    pub swipe_right: ui::rowswipe::SwipeAction,
    /// Android: text fields ask the keyboard not to learn from what is typed. Secrets always do.
    #[serde(default = "Settings::private_by_default")]
    pub incognito_keyboard: bool,
    /// How the delivery of the user's own messages is shown.
    #[serde(default)]
    pub receipts: ui::receipt::ReceiptStyle,
    /// Whether others' messages show the user's reading always or after a tap.
    #[serde(default)]
    pub incoming: ui::receipt::IncomingStatus,
}

impl Settings {
    fn private_by_default() -> bool {
        true
    }

    fn path(dir: &std::path::Path) -> PathBuf {
        dir.join("settings.json")
    }

    fn load(dir: &std::path::Path) -> Self {
        let stored = std::fs::read(Self::path(dir)).ok().and_then(|raw| Self::parse(&raw));
        stored.unwrap_or(Self { theme: ThemeChoice::default(), language: Default::default(), incognito: true, chat_colour: ChatColour::default(), emoji_font: None, text_size: Default::default(), wallpaper: Default::default(), wallpaper_dim: ui::appearance::default_dim(), lock: Default::default(), stay_connected: true, swipe_left: ui::rowswipe::SwipeAction::left_default(), swipe_right: ui::rowswipe::SwipeAction::right_default(), incognito_keyboard: true, receipts: Default::default(), incoming: Default::default() })
    }

    fn parse(raw: &[u8]) -> Option<Self> {
        serde_json::from_slice(raw).ok()
    }

    pub fn save(&self, dir: &std::path::Path) {
        let written = serde_json::to_vec_pretty(self).map_err(std::io::Error::other).and_then(|raw| std::fs::write(Self::path(dir), raw));
        if let Err(e) = written {
            log::warn!("settings.json: {e}");
        }
    }
}

/// The app's colours, by role; `docs/design.md` says what each is for.
#[derive(Clone, Copy)]
pub(crate) struct Palette {
    pub ground: Hsla,
    pub surface: Hsla,
    pub raised: Hsla,
    /// One step above `raised`: a reaction badge on a message.
    pub lifted: Hsla,
    pub rule: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    /// Fill behind what is addressed to the user; `on_accent` is the text on it.
    pub accent: Hsla,
    pub on_accent: Hsla,
    /// The accent as text: the fill itself is unreadable on a light ground.
    pub accent_ink: Hsla,
    /// The owner's violet, on the icons of the settings' rows only.
    pub brand: Hsla,
    pub negative: Hsla,
    pub warning: Hsla,
    pub warning_bg: Hsla,
    /// An own bubble's fill and the text on it.
    pub own: Hsla,
    pub on_own: Hsla,
    /// Whether the user chose a colour, so what is drawn inside the bubble must follow it.
    pub own_coloured: bool,
    /// Behind a chat row swiped to archive, mark read or unread, pin, mute, set low priority.
    /// The ground is the icon on each, as on `negative`.
    pub swipe_archive: Hsla,
    pub swipe_read: Hsla,
    pub swipe_pin: Hsla,
    pub swipe_mute: Hsla,
    pub swipe_later: Hsla,
}

/// The `swipe_*` fills, dark then light: muted, so a row being moved does not shout.
const SWIPE: [[u32; 5]; 2] = [[0x4fae79, 0x6290dc, 0xd29a45, 0xa87fe2, 0x8796ab], [0x237a4b, 0x2d5cbf, 0x9a5c12, 0x6c34b8, 0x4e5c73]];

/// The `brand` icon colour, dark then light: #8006da is 2.7:1 on the dark `surface`, so dark takes a
/// lighter step of the same hue.
const BRAND: [u32; 2] = [0x9b3cf0, 0x8006da];

impl Global for Palette {}

impl Palette {
    pub fn of(cx: &App) -> Self {
        *cx.global()
    }

    /// The palette for what is drawn inside an own bubble: on a chosen colour every text role is
    /// the bubble's ink (a lighter grey would miss 4.5:1 on blue and teal) and insets are tints of it.
    pub fn in_own_bubble(self) -> Self {
        if !self.own_coloured {
            return self;
        }
        Self { text: self.on_own, muted: self.on_own, faint: self.on_own, accent_ink: self.on_own, lifted: self.on_own.opacity(0.28), ..self }
    }
}

/// Sets `Palette` for the current mode and lays it over the component library's theme.
fn apply_palette(cx: &mut App) {
    let c = |hex: u32| -> Hsla { rgb(hex).into() };
    let chosen = cx.try_global::<ChatColour>().copied().unwrap_or_default().fill();
    let dark = Theme::global(cx).is_dark();
    let [swipe_archive, swipe_read, swipe_pin, swipe_mute, swipe_later] = SWIPE[usize::from(!dark)].map(c);
    let mut p = if dark {
        Palette {
            ground: c(0x000000),
            surface: c(0x111111),
            raised: c(0x2a2a2a),
            lifted: c(0x3d3d3d),
            rule: c(0x1a1a1a),
            text: c(0xffffff),
            muted: c(0xb4b4b4),
            faint: c(0x949494),
            accent: c(0xc6f24e),
            on_accent: c(0x000000),
            accent_ink: c(0xc6f24e),
            brand: c(BRAND[0]),
            negative: c(0xe5484d),
            warning: c(0xe5a93b),
            warning_bg: c(0x2b2414),
            own: c(0x2a2a2a),
            on_own: c(0xffffff),
            own_coloured: false,
            swipe_archive,
            swipe_read,
            swipe_pin,
            swipe_mute,
            swipe_later,
        }
    } else {
        Palette {
            ground: c(0xffffff),
            surface: c(0xf2f2f2),
            raised: c(0xe2e2e2),
            lifted: c(0xcfcfcf),
            rule: c(0xe8e8e8),
            text: c(0x000000),
            muted: c(0x4f4f4f),
            faint: c(0x6e6e6e),
            accent: c(0xc6f24e),
            on_accent: c(0x000000),
            accent_ink: c(0x486b00),
            brand: c(BRAND[1]),
            negative: c(0xc62f35),
            warning: c(0x7a5200),
            warning_bg: c(0xfbefd2),
            own: c(0xe2e2e2),
            on_own: c(0x000000),
            own_coloured: false,
            swipe_archive,
            swipe_read,
            swipe_pin,
            swipe_mute,
            swipe_later,
        }
    };
    if let Some(fill) = chosen {
        (p.own, p.on_own, p.own_coloured) = (c(fill), c(ink_on(fill)), true);
    }
    cx.set_global(p);
    // The system bars share the ground, with their icons in the ink that shows on it.
    #[cfg(target_os = "android")]
    {
        let dark = Theme::global(cx).is_dark();
        let ground = if dark { 0x000000 } else { 0xffffff };
        gpui_mobile::set_system_chrome(&gpui_mobile::SystemChromeStyle {
            status_bar_color: Some(ground),
            status_bar_style: if dark { gpui_mobile::StatusBarContentStyle::Light } else { gpui_mobile::StatusBarContentStyle::Dark },
            navigation_bar_color: Some(ground),
        });
    }
    Theme::update(cx, |theme| {
        // Depth is a lightness step: nothing casts a shadow.
        theme.shadow = false;
        theme.font_family = FACE.into();
        // Controls are fully round: no single-line control is taller than twice this.
        (theme.radius, theme.radius_lg) = (px(22.), px(20.));
        let k = &mut theme.colors;
        (k.background, k.foreground, k.muted_foreground) = (p.ground, p.text, p.muted);
        (k.muted, k.secondary, k.secondary_hover, k.secondary_active) = (p.surface, p.raised, p.raised, p.raised);
        (k.secondary_foreground, k.accent, k.accent_foreground) = (p.text, p.raised, p.text);
        (k.list, k.list_hover, k.list_active, k.list_even) = (p.ground, p.surface, p.raised, p.ground);
        (k.border, k.input, k.title_bar, k.title_bar_border) = (p.rule, p.raised, p.ground, p.rule);
        (k.popover, k.popover_foreground, k.sidebar, k.sidebar_foreground) = (p.surface, p.text, p.ground, p.text);
        // The primary control is the inverted one.
        (k.primary, k.primary_hover, k.primary_active, k.primary_foreground) = (p.text, p.muted, p.muted, p.ground);
        (k.button_primary, k.button_primary_hover, k.button_primary_active) = (p.text, p.muted, p.muted);
        (k.button_primary_foreground, k.ring, k.caret) = (p.ground, p.faint, p.text);
        (k.danger, k.danger_hover, k.danger_active) = (p.negative, p.negative, p.negative);
        (k.switch, k.switch_thumb, k.selection) = (p.faint.opacity(0.5), p.ground, p.text.opacity(0.28));
        // Desktop only: a thumb with no track, one and two steps above the ground.
        if !gpui_kit::is_mobile() {
            (k.scrollbar, k.scrollbar_thumb, k.scrollbar_thumb_hover) = (p.ground.opacity(0.), p.raised, p.faint);
        }
    });
}

/// The app's face, in the three weights the design uses.
pub(crate) const FACE: &str = "Inter";

/// Loads the app's face (about 1.25 MB) and, on Android, the system symbols fonts gpui-mobile
/// leaves out: mathematical alphanumerics (names typed in "fancy" letters), circled letters, arrows.
fn fonts(cx: &mut App) {
    let faces: [&[u8]; 3] = [
        include_bytes!("../assets/fonts/Inter-Regular.ttf"),
        include_bytes!("../assets/fonts/Inter-Medium.ttf"),
        include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    ];
    #[cfg_attr(not(target_os = "android"), expect(unused_mut))]
    let mut fonts: Vec<std::borrow::Cow<'static, [u8]>> = faces.into_iter().map(Into::into).collect();
    #[cfg(target_os = "android")]
    {
        // Not NotoSansSymbols-Regular-Subsetted2: it holds only plain versions of emoji.
        let files = ["NotoSansSymbols-Regular-Subsetted.ttf", "SECFallback.ttf"];
        fonts.extend(files.iter().filter_map(|f| std::fs::read(format!("/system/fonts/{f}")).ok()).map(Into::into));
    }
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        log::warn!("fonts: {e}");
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

/// Libraries log only warnings: matrix-sdk's info lines name rooms, users and servers.
const LOG_FILTER: &str = "warn,beeper_gpui=info";

#[cfg(not(target_os = "android"))]
pub fn run_desktop() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(LOG_FILTER)).init();
    i18n::init(None);
    let dir = std::env::var_os("BEEPER_GPUI_DATA_DIR")
        .map(PathBuf::from)
        // Local, not roaming, on Windows: the store is large and belongs to one device.
        .or_else(|| dirs::data_local_dir().map(|d| d.join("beeper-gpui")))
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
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_filter(android_logger::FilterBuilder::new().parse(LOG_FILTER).build())
            .with_tag("beeper-gpui"),
    );
    jni::install_panic_hook();
    i18n::init(Some(app.config().language().unwrap_or_default()));
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
            Ok(false) => log::warn!("no app opens this file"),
            Err(e) => log::warn!("open file: {e}"),
        }
    }
}

/// Whether a picture or video can be saved to the gallery: Android 10 and later, and desktops.
pub(crate) fn can_save() -> bool {
    #[cfg(target_os = "android")]
    return android_files::can_save().map_err(|e| log::warn!("can save: {e}")).unwrap_or(false);
    #[cfg(not(target_os = "android"))]
    true
}

/// Whether there is a share sheet. Desktops have none.
pub(crate) fn can_share() -> bool {
    cfg!(target_os = "android")
}

/// Offers a downloaded file to other apps through the system's share sheet.
pub(crate) fn share_file(path: &std::path::Path, mime: Option<&str>) {
    #[cfg(target_os = "android")]
    match android_files::share(&path.to_string_lossy(), mime.unwrap_or("*/*")) {
        Ok(true) => {}
        Ok(false) => log::warn!("no app takes this file"),
        Err(e) => log::warn!("share file: {e}"),
    }
    #[cfg(not(target_os = "android"))]
    let _ = (path, mime);
}

/// Copies a downloaded file into the user's pictures: on Android `Pictures/Unisend` (`Movies/Unisend`
/// for a video) in the gallery, on desktops `Pictures/Unisend` in the home folder. Returns where it
/// went, for the user to read. Blocking.
pub(crate) fn save_file(path: &std::path::Path, name: &str, mime: Option<&str>) -> Result<String, String> {
    #[cfg(target_os = "android")]
    {
        let mime = mime.unwrap_or("application/octet-stream");
        android_files::save(&path.to_string_lossy(), name, mime)?.ok_or_else(|| "the gallery refused the file".to_owned())
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = mime;
        let folder = dirs::picture_dir().or_else(|| dirs::home_dir().map(|h| h.join("Pictures"))).ok_or("no pictures folder")?.join("Unisend");
        std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let target = free_name(&folder, name);
        std::fs::copy(path, &target).map_err(|e| e.to_string())?;
        Ok(target.display().to_string())
    }
}

/// `name` in `folder`, or `name (2).ext`, `name (3).ext` when taken. Gives up after 999.
#[cfg(any(test, not(target_os = "android")))]
fn free_name(folder: &std::path::Path, name: &str) -> PathBuf {
    let first = folder.join(name);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    (2..1000).map(|n| folder.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).filter(|_| first.exists()).unwrap_or(first)
}

/// Opens a web address in the browser. Addresses come out of other people's messages, so only
/// http and https ones go any further.
pub(crate) fn open_link(cx: &mut gpui_kit::App, url: &str) {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return log::warn!("not a web address, not opened");
    }
    #[cfg(not(target_os = "android"))]
    cx.open_url(url);
    // gpui-mobile's `open_url` only logs.
    #[cfg(target_os = "android")]
    {
        let _ = cx;
        match android_files::browse(url) {
            Ok(true) => {}
            Ok(false) => log::warn!("no app opens web addresses"),
            Err(e) => log::warn!("open link: {e}"),
        }
    }
}

/// Opens a place: on Android in the user's maps app, elsewhere in the browser. Built from parsed
/// coordinates only.
pub(crate) fn open_place(cx: &mut gpui_kit::App, place: &crate::backend::Place) {
    #[cfg(target_os = "android")]
    if android_files::browse(&place.geo_url()).map_err(|e| log::warn!("open place: {e}")).unwrap_or(false) {
        return;
    }
    open_link(cx, &place.web_url());
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

/// On: asks for the notification permission (Android). Off, when signing out: ends the
/// background connection and takes the notifications away.
pub(crate) fn background(on: bool) {
    #[cfg(target_os = "android")]
    if let Err(e) = android_files::notify(if on { "start" } else { "stop" }, &[]) {
        log::warn!("background connection: {e}");
    }
    #[cfg(not(target_os = "android"))]
    if !on {
        notify::close_all();
    }
}

/// Starts or ends staying connected while the app is closed (Android: a foreground service with
/// a quiet notice, started only while the app is in front and only if it is not running).
/// Desktop stays connected while it runs.
pub(crate) fn stay_connected(on: bool) {
    #[cfg(target_os = "android")]
    if let Err(e) = android_files::notify(if on { "connect" } else { "disconnect" }, &[]) {
        log::warn!("background connection: {e}");
    }
    #[cfg(not(target_os = "android"))]
    let _ = on;
}

/// A message for a chat's notification.
pub(crate) struct Notice<'a> {
    pub chat: &'a str,
    pub name: &'a str,
    #[cfg_attr(not(target_os = "android"), expect(dead_code, reason = "desktop notifications name the sender in every chat"))]
    pub group: bool,
    /// Path of the chat's picture, or empty.
    pub icon: &'a str,
    pub note: &'a backend::Note,
    /// False for all but the last of messages that arrive together: one sound for them.
    pub alert: bool,
}

/// Adds a message to the chat's notification, which shows the last few messages of the chat.
pub(crate) fn notify(notice: &Notice) {
    #[cfg(target_os = "android")]
    if let Err(e) = android_files::message(notice) {
        log::warn!("notification: {e}");
    }
    #[cfg(not(target_os = "android"))]
    notify::show(notice);
}

/// The chat a tapped notification asked to open, once.
pub(crate) fn pending_chat() -> Option<String> {
    #[cfg(target_os = "android")]
    return android_files::pending_chat().unwrap_or_else(|e| {
        log::warn!("notification chat: {e}");
        None
    });
    #[cfg(not(target_os = "android"))]
    notify::take_pending()
}

/// Opens the system's page for the app's notification sounds and importance, where there is one.
pub(crate) fn notification_settings() {
    #[cfg(target_os = "android")]
    if let Err(e) = android_files::notify("settings", &[]) {
        log::warn!("notification settings: {e}");
    }
}

/// Opens the system's page for the background connection's notice alone, so it can be hidden.
pub(crate) fn connection_notice_settings() {
    #[cfg(target_os = "android")]
    if let Err(e) = android_files::notify("connectionSettings", &[]) {
        log::warn!("connection notice settings: {e}");
    }
}

pub(crate) fn clear_notification(chat: &str) {
    #[cfg(target_os = "android")]
    if let Err(e) = android_files::notify("clear", &[chat]) {
        log::warn!("notification: {e}");
    }
    #[cfg(not(target_os = "android"))]
    notify::close(chat);
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

/// The text of a failed JNI call. A thrown Java exception is caught here, so it is no longer
/// pending, and named with its message and where it was thrown; `e.to_string()` alone only says
/// that one was thrown.
#[cfg(target_os = "android")]
pub(crate) fn java_error(env: &jni::Env<'_>, e: jni::errors::Error) -> String {
    match env.exception_catch() {
        Err(jni::errors::Error::CaughtJavaException { name, msg, stack, .. }) => {
            format!("{name}: {msg} ({})", stack.lines().next().unwrap_or_default())
        }
        _ => e.to_string(),
    }
}

/// Calls into `dev.gpui.beeper.Lang`: the app's language as Android keeps it.
#[cfg(target_os = "android")]
mod android_lang {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::JValue;

    const CLASS: &str = "dev.gpui.beeper.Lang";

    /// The tag set for the app ("" for none, `None` before Android 13) and the phone's language.
    pub fn read() -> Result<(Option<String>, String), String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let app = env
                .call_static_method(&class, jni::jni_str!("app"), jni::jni_sig!("(Landroid/content/Context;)Ljava/lang/String;"), &[JValue::Object(&activity)])
                .and_then(|v| v.l())
                .map_err(|e| crate::java_error(env, e))?;
            let app = (!app.is_null()).then(|| helpers::get_string(env, &app));
            let system = env
                .call_static_method(&class, jni::jni_str!("system"), jni::jni_sig!("(Landroid/content/Context;)Ljava/lang/String;"), &[JValue::Object(&activity)])
                .and_then(|v| v.l())
                .map_err(|e| crate::java_error(env, e))?;
            Ok((app, helpers::get_string(env, &system)))
        })
    }

    pub fn set(tag: &str) -> Result<(), String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let tag = env.new_string(tag).map_err(|e| e.to_string())?;
            env.call_static_method(&class, jni::jni_str!("set"), jni::jni_sig!("(Landroid/content/Context;Ljava/lang/String;)V"), &[JValue::Object(&activity), JValue::Object(&tag)])
                .map(|_| ())
                .map_err(|e| crate::java_error(env, e))
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
            .map_err(|e| crate::java_error(env, e))
        })
    }

    pub fn browse(url: &str) -> Result<bool, String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let url = env.new_string(url).map_err(|e| e.to_string())?;
            env.call_static_method(
                &class,
                jni::jni_str!("browse"),
                jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;)Z"),
                &[JValue::Object(&activity), JValue::Object(&url)],
            )
            .and_then(|v| v.z())
            .map_err(|e| crate::java_error(env, e))
        })
    }

    pub fn can_save() -> Result<bool, String> {
        helpers::with_env(|env| {
            let class = helpers::find_app_class(env, CLASS)?;
            env.call_static_method(&class, jni::jni_str!("canSave"), jni::jni_sig!("()Z"), &[]).and_then(|v| v.z()).map_err(|e| crate::java_error(env, e))
        })
    }

    pub fn share(path: &str, mime: &str) -> Result<bool, String> {
        view_like("share", path, mime)
    }

    fn view_like(method: &str, path: &str, mime: &str) -> Result<bool, String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let path = env.new_string(path).map_err(|e| e.to_string())?;
            let mime = env.new_string(mime).map_err(|e| e.to_string())?;
            env.call_static_method(
                &class,
                jni::strings::JNIString::new(method),
                jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;Ljava/lang/String;)Z"),
                &[JValue::Object(&activity), JValue::Object(&path), JValue::Object(&mime)],
            )
            .and_then(|v| v.z())
            .map_err(|e| crate::java_error(env, e))
        })
    }

    pub fn save(path: &str, name: &str, mime: &str) -> Result<Option<String>, String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let mut text = |s: &str| env.new_string(s).map_err(|e| e.to_string());
            let (path, name, mime) = (text(path)?, text(name)?, text(mime)?);
            let folder = env
                .call_static_method(
                    &class,
                    jni::jni_str!("save"),
                    jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;"),
                    &[JValue::Object(&activity), JValue::Object(&path), JValue::Object(&name), JValue::Object(&mime)],
                )
                .and_then(|v| v.l())
                .map_err(|e| crate::java_error(env, e))?;
            Ok((!folder.is_null()).then(|| helpers::get_string(env, &folder)))
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
                .map_err(|e| crate::java_error(env, e))?;
            Ok((!cookies.is_null()).then(|| helpers::get_string(env, &cookies)))
        })
    }

    /// Calls a `void` static method of `dev.gpui.beeper.Notify` taking the activity and up to
    /// three strings.
    /// Adds a message to a chat's conversation notification.
    pub fn message(notice: &crate::Notice) -> Result<(), String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, "dev.gpui.beeper.Notify")?;
            let mut text = |s: &str| env.new_string(s).map_err(|e| e.to_string());
            let (chat, name, sender, body, icon) =
                (text(notice.chat)?, text(notice.name)?, text(&notice.note.sender)?, text(&notice.note.text)?, text(notice.icon)?);
            env.call_static_method(
                &class,
                jni::jni_str!("message"),
                jni::jni_sig!(
                    "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;ZLjava/lang/String;Ljava/lang/String;JZLjava/lang/String;Z)V"
                ),
                &[
                    JValue::Object(&activity),
                    JValue::Object(&chat),
                    JValue::Object(&name),
                    JValue::Bool(notice.group),
                    JValue::Object(&sender),
                    JValue::Object(&body),
                    JValue::Long(i64::try_from(notice.note.ts).unwrap_or(i64::MAX)),
                    JValue::Bool(notice.note.mention),
                    JValue::Object(&icon),
                    JValue::Bool(notice.alert),
                ],
            )
            .map(|_| ())
            .map_err(|e| crate::java_error(env, e))
        })
    }

    /// The chat the notification that opened the app asked for, taken from the activity's intent.
    pub fn pending_chat() -> Result<Option<String>, String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, "dev.gpui.beeper.Notify")?;
            let chat = env
                .call_static_method(
                    &class,
                    jni::jni_str!("takeChat"),
                    jni::jni_sig!("(Landroid/app/Activity;)Ljava/lang/String;"),
                    &[JValue::Object(&activity)],
                )
                .and_then(|v| v.l())
                .map_err(|e| crate::java_error(env, e))?;
            Ok((!chat.is_null()).then(|| helpers::get_string(env, &chat)))
        })
    }

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
            called.map(|_| ()).map_err(|e| crate::java_error(env, e))
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
                .map_err(|e| crate::java_error(env, e))?;
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

/// Entry points the Java side of notifications calls: the reply box and the mark-as-read and mute buttons.
#[cfg(target_os = "android")]
mod from_java {
    use std::ffi::c_void;

    use gpui_mobile::android::jni::{get_string, with_env};
    use jni::objects::JObject;

    fn text(env: &mut jni::Env<'_>, raw: *mut c_void) -> String {
        let obj = unsafe { JObject::from_raw(env, raw as jni::sys::jobject) };
        get_string(env, &obj)
    }

    /// # Safety
    /// Called by the JVM with valid string references.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_dev_gpui_beeper_Notify_nativeReply(
        _env: *mut c_void,
        _class: *mut c_void,
        chat: *mut c_void,
        body: *mut c_void,
    ) -> u8 {
        let Ok((chat, body)) = with_env(|env| Ok((text(env, chat), text(env, body)))) else { return 0 };
        u8::from(crate::backend::reply(&chat, &body))
    }

    /// # Safety
    /// Called by the JVM with a valid string reference.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_dev_gpui_beeper_Notify_nativeRead(_env: *mut c_void, _class: *mut c_void, chat: *mut c_void) {
        if let Ok(chat) = with_env(|env| Ok(text(env, chat))) {
            crate::backend::read_from_notification(&chat);
        }
    }

    /// # Safety
    /// Called by the JVM with a valid string reference.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Java_dev_gpui_beeper_Notify_nativeMute(_env: *mut c_void, _class: *mut c_void, chat: *mut c_void) -> u8 {
        let Ok(chat) = with_env(|env| Ok(text(env, chat))) else { return 0 };
        u8::from(crate::backend::mute_from_notification(&chat))
    }
}

#[cfg(test)]
mod tests {
    use super::Assets;
    use gpui_kit::AssetSource as _;

    #[test]
    fn a_taken_name_in_the_pictures_folder_gets_a_number() {
        let dir = std::env::temp_dir().join(format!("unisend-free-name-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(super::free_name(&dir, "a.jpg"), dir.join("a.jpg"));
        std::fs::write(dir.join("a.jpg"), b"x").unwrap();
        assert_eq!(super::free_name(&dir, "a.jpg"), dir.join("a (2).jpg"));
        std::fs::write(dir.join("a (2).jpg"), b"x").unwrap();
        std::fs::write(dir.join("b"), b"x").unwrap();
        assert_eq!(super::free_name(&dir, "a.jpg"), dir.join("a (3).jpg"));
        assert_eq!(super::free_name(&dir, "b"), dir.join("b (2)"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn logs_keep_libraries_at_warnings_and_this_app_at_info() {
        use log::{Level, Log as _, Metadata};
        let logger = env_logger::Builder::new().parse_filters(super::LOG_FILTER).build();
        let on = |target: &str, level: Level| logger.enabled(&Metadata::builder().target(target).level(level).build());
        assert!(!on("matrix_sdk::sync", Level::Info));
        assert!(on("matrix_sdk::sync", Level::Warn));
        assert!(on("beeper_gpui::backend", Level::Info));
        assert!(!on("beeper_gpui::backend", Level::Debug));
    }
    /// An icon the UI names but the build does not embed draws as nothing, with only a log line.
    #[test]
    fn every_icon_the_ui_uses_is_embedded() {
        for source in [
            include_str!("ui.rs"),
            include_str!("ui/appearance.rs"),
            include_str!("ui/camera.rs"),
            include_str!("ui/caption.rs"),
            include_str!("ui/cards.rs"),
            include_str!("ui/contacts.rs"),
            include_str!("ui/disappear.rs"),
            include_str!("ui/emojipick.rs"),
            include_str!("ui/forward.rs"),
            include_str!("ui/gallery.rs"),
            include_str!("ui/gifs.rs"),
            include_str!("ui/heart.rs"),
            include_str!("ui/info.rs"),
            include_str!("ui/location.rs"),
            include_str!("ui/mentions.rs"),
            include_str!("ui/menu.rs"),
            include_str!("ui/navigate.rs"),
            include_str!("ui/panels.rs"),
            include_str!("ui/pollnew.rs"),
            include_str!("ui/receipt.rs"),
            include_str!("ui/rich.rs"),
            include_str!("ui/rooms.rs"),
            include_str!("ui/sharein.rs"),
            include_str!("ui/signin.rs"),
            include_str!("ui/verify.rs"),
            include_str!("ui/video.rs"),
            include_str!("ui/viewer.rs"),
            include_str!("ui/voice.rs"),
        ] {
            for name in source.split("IconName::").skip(1) {
                let name: String = name.chars().take_while(char::is_ascii_alphanumeric).collect();
                let mut file = String::new();
                for (ix, c) in name.chars().enumerate() {
                    if (c.is_ascii_uppercase() || c.is_ascii_digit()) && ix > 0 {
                        file.push('-');
                    }
                    file.push(c.to_ascii_lowercase());
                }
                let path = format!("icons/{file}.svg");
                assert!(matches!(Assets.load(&path), Ok(Some(_))), "{path} is used but not embedded");
            }
        }
    }

    #[test]
    fn text_on_every_chat_colour_reaches_4_5() {
        for colour in super::ChatColour::ALL {
            let Some(fill) = colour.fill() else { continue };
            let ratio = super::contrast(fill, super::ink_on(fill));
            assert!(ratio >= 4.5, "{}: {ratio}", colour.label());
        }
        assert_eq!(super::ink_on(super::ChatColour::Orange.fill().unwrap()), 0x1a1a1a);
        assert_eq!(super::ink_on(super::ChatColour::Indigo.fill().unwrap()), 0xffffff);
    }

    #[test]
    fn swipe_icons_reach_4_5_on_their_fill() {
        for (fills, ground) in super::SWIPE.iter().zip([0x000000, 0xffffff]) {
            for fill in fills {
                let ratio = super::contrast(*fill, ground);
                assert!(ratio >= 4.5, "{fill:06x}: {ratio}");
            }
        }
    }

    #[test]
    fn brand_icons_reach_3_on_cards() {
        for (fill, surface) in super::BRAND.into_iter().zip([0x111111, 0xf2f2f2]) {
            let ratio = super::contrast(fill, surface);
            assert!(ratio >= 3., "{fill:06x}: {ratio}");
        }
    }

    #[test]
    fn old_settings_file_loads_with_the_default_colour() {
        let old = super::Settings::parse(br#"{"theme":"Light","incognito":false}"#).unwrap();
        assert_eq!((old.theme, old.incognito, old.chat_colour), (super::ThemeChoice::Light, false, super::ChatColour::Default));
        assert!(old.stay_connected, "files from before the switch keep the background connection");
        assert!(!super::Settings::parse(br#"{"stay_connected":false}"#).unwrap().stay_connected);
        let new = super::Settings::parse(br#"{"chat_colour":"Teal"}"#).unwrap();
        assert_eq!(new.chat_colour, super::ChatColour::Teal);
    }

    #[test]
    fn emoji_font_choice_round_trips() {
        let dir = std::env::temp_dir().join(format!("beeper-gpui-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut settings = super::Settings::load(&dir);
        assert_eq!(settings.emoji_font, None);
        settings.emoji_font = Some("Test Squares Emoji".into());
        settings.save(&dir);
        assert_eq!(super::Settings::load(&dir), settings);
        settings.emoji_font = None;
        settings.save(&dir);
        assert!(!std::fs::read_to_string(dir.join("settings.json")).unwrap().contains("emoji_font"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn swipe_actions_default_and_round_trip() {
        use crate::ui::rowswipe::SwipeAction;
        let old = super::Settings::parse(br#"{"theme":"Light"}"#).unwrap();
        assert_eq!((old.swipe_left, old.swipe_right), (SwipeAction::Archive, SwipeAction::Unread));
        let mut changed = old.clone();
        (changed.swipe_left, changed.swipe_right) = (SwipeAction::Delete, SwipeAction::None);
        assert_eq!(super::Settings::parse(&serde_json::to_vec(&changed).unwrap()).unwrap(), changed);
    }

    #[test]
    fn incognito_keyboard_defaults_on_and_round_trips() {
        let old = super::Settings::parse(br#"{"theme":"Light"}"#).unwrap();
        assert!(old.incognito_keyboard, "files from before the switch keep the keyboard incognito");
        let mut changed = old.clone();
        changed.incognito_keyboard = false;
        assert_eq!(super::Settings::parse(&serde_json::to_vec(&changed).unwrap()).unwrap(), changed);
    }

    #[test]
    fn receipt_style_defaults_to_text_and_round_trips() {
        use crate::ui::receipt::ReceiptStyle;
        let old = super::Settings::parse(br#"{"theme":"Light"}"#).unwrap();
        assert_eq!((old.receipts, old.incoming), (ReceiptStyle::Text, crate::ui::receipt::IncomingStatus::OnTap));
        let always = super::Settings { incoming: crate::ui::receipt::IncomingStatus::Always, ..old.clone() };
        assert_eq!(super::Settings::parse(&serde_json::to_vec(&always).unwrap()).unwrap(), always);
        for style in ReceiptStyle::ALL {
            let changed = super::Settings { receipts: style, ..old.clone() };
            assert_eq!(super::Settings::parse(&serde_json::to_vec(&changed).unwrap()).unwrap(), changed);
        }
    }

    #[test]
    fn language_defaults_to_system_and_decides() {
        use crate::i18n::{LanguageChoice::*, decide};
        let old = super::Settings::parse(br#"{"theme":"Light"}"#).unwrap();
        assert_eq!(old.language, System);
        assert_eq!(super::Settings::parse(br#"{"language":"Polish"}"#).unwrap().language, Polish);
        assert!(decide(System, true) && !decide(System, false));
        assert!(decide(Polish, false) && !decide(English, true));
    }
}

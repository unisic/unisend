//! The tray icon on Linux: a StatusNotifierItem with a two-entry dbusmenu, served on the session
//! bus. KDE Plasma shows it natively; GNOME only with an extension. Nothing polls: the bus wakes
//! the task for a click, and for a tray that (re)starts, to register with it again. While a chat
//! waits, the icon carries an accent dot, as the chat list's header counts.
//!
//! The same connection tells the task manager or dock how many chats wait, by the Unity launcher
//! API (`com.canonical.Unity.LauncherEntry`, a signal to the session bus that KDE Plasma and Dash
//! to Dock read): it finds the app by its desktop file, so an app not installed shows no count.

use std::collections::HashMap;
use std::sync::OnceLock;

use futures_util::StreamExt as _;
use gpui_kit::App;
use tokio::sync::{mpsc, watch};
use zbus::zvariant::{ObjectPath, OwnedValue, Structure, Value};

use crate::i18n::t;

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
/// Clicks waiting for the UI; more are dropped.
const QUEUE: usize = 4;
/// Menu entries' ids; 0 is the menu itself.
const OPEN: i32 = 1;
const QUIT: i32 = 2;
/// The launcher API's names; the URI is the installed desktop file's.
const LAUNCHER: &str = "com.canonical.Unity.LauncherEntry";
const LAUNCHER_PATH: &str = "/dev/gpui/beeper/Launcher";
const LAUNCHER_URI: &str = "application://dev.gpui.beeper.desktop";
/// Shown when the icon theme has no `crate::APP_ID` (the app not installed): 64 x 64 x 4 bytes.
const ICON: &[u8] = if crate::backend::update::DEV {
    include_bytes!("../../packaging/linux/icons-dev/hicolor/64x64/apps/dev.gpui.beeper.dev.png")
} else {
    include_bytes!("../../packaging/linux/icons/hicolor/64x64/apps/dev.gpui.beeper.png")
};

/// The dot: center and radius in the icon's 64 x 64, the clear ring around it, and `Palette::accent`.
const DOT: (f32, f32, f32) = (49.0, 15.0, 12.0);
const RING: f32 = 4.0;
const ACCENT: [u8; 3] = [0xc6, 0xf2, 0x4e];

/// How many chats wait; the bus task redraws the icon and sends the count when it changes.
static WAITING: OnceLock<watch::Sender<usize>> = OnceLock::new();

/// The spec's pixmaps: width, height and ARGB32 bytes.
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

#[derive(Clone, Copy)]
enum Click {
    Open,
    Quit,
}

pub(super) fn start(cx: &mut App) {
    let (tx, mut rx) = mpsc::channel(QUEUE);
    let waiting = WAITING.get_or_init(|| watch::channel(0).0).subscribe();
    cx.background_executor().spawn(async move {
        if let Err(e) = serve(tx, waiting).await {
            log::warn!("tray icon: {e}");
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Some(click) = rx.recv().await {
            cx.update(|cx| match click {
                Click::Open => super::show(cx),
                Click::Quit => cx.quit(),
            });
        }
    })
    .detach();
}

/// Sets the count; a no-op when it is already so or there is no tray.
pub(super) fn waiting(count: usize) {
    if let Some(tx) = WAITING.get() {
        tx.send_if_modified(|w| std::mem::replace(w, count) != count);
    }
}

async fn serve(tx: mpsc::Sender<Click>, mut waiting: watch::Receiver<usize>) -> zbus::Result<()> {
    let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
    let plain = pixmap(false);
    let dotted = pixmap(true);
    let count = *waiting.borrow_and_update();
    let on = count > 0;
    let item = Item { tx: tx.clone(), pixmap: if on { dotted.clone() } else { plain.clone() }, waiting: on };
    let conn = zbus::connection::Builder::session()?
        .name(name.as_str())?
        .serve_at(ITEM_PATH, item)?
        .serve_at(MENU_PATH, Menu { tx })?
        .build()
        .await?;
    let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
    // Plasma restarted, or the tray came after the app: the new watcher knows nothing of the icon.
    let mut watchers = dbus.receive_name_owner_changed_with_args(&[(0, WATCHER)]).await?;
    let item = conn.object_server().interface::<_, Item>(ITEM_PATH).await?;
    let reregister = async {
        register(&conn, &name).await;
        while let Some(change) = watchers.next().await {
            if change.args().is_ok_and(|a| a.new_owner().is_some()) {
                register(&conn, &name).await;
            }
        }
    };
    let redraw = async {
        launcher(&conn, count).await;
        let mut dotted_now = on;
        while waiting.changed().await.is_ok() {
            let count = *waiting.borrow_and_update();
            launcher(&conn, count).await;
            let on = count > 0;
            if std::mem::replace(&mut dotted_now, on) == on {
                continue;
            }
            {
                let mut it = item.get_mut().await;
                it.waiting = on;
                it.pixmap = if on { dotted.clone() } else { plain.clone() };
            }
            if let Err(e) = Item::new_icon(item.signal_emitter()).await {
                log::warn!("tray icon: {e}");
            }
        }
    };
    futures_util::future::join(reregister, redraw).await;
    Ok(())
}

/// Sends `count` to the launcher, hidden at 0.
async fn launcher(conn: &zbus::Connection, count: usize) {
    let props = HashMap::from([("count", Value::from(count as i64)), ("count-visible", Value::from(count > 0))]);
    if let Err(e) = conn.emit_signal(None::<&str>, LAUNCHER_PATH, LAUNCHER, "Update", &(LAUNCHER_URI, props)).await {
        log::warn!("launcher count: {e}");
    }
}

async fn register(conn: &zbus::Connection, name: &str) {
    let call = conn.call_method(Some(WATCHER), "/StatusNotifierWatcher", Some(WATCHER), "RegisterStatusNotifierItem", &(name,)).await;
    if let Err(e) = call {
        log::warn!("tray icon: no tray to show it in ({e})");
    }
}

/// The icon as the spec's ARGB32 pixmap, alpha first in network byte order, with the dot or not.
fn pixmap(dot: bool) -> Pixmaps {
    let Ok(img) = image::load_from_memory_with_format(ICON, image::ImageFormat::Png) else { return Vec::new() };
    let mut img = img.to_rgba8();
    if dot {
        let (cx, cy, r) = DOT;
        for (x, y, p) in img.enumerate_pixels_mut() {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            // Coverage over a pixel's width keeps both edges smooth.
            let fill = (r + 0.5 - d).clamp(0.0, 1.0);
            let clear = (r + RING + 0.5 - d).clamp(0.0, 1.0);
            let kept = (1.0 - clear) * p[3] as f32;
            let alpha = fill * 255.0 + (1.0 - fill) * kept;
            for i in 0..3 {
                let c = fill * 255.0 * ACCENT[i] as f32 + (1.0 - fill) * kept * p[i] as f32;
                p[i] = if alpha > 0.0 { (c / alpha).round() as u8 } else { 0 };
            }
            p[3] = alpha.round() as u8;
        }
    }
    let argb = img.pixels().flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
    vec![(img.width() as i32, img.height() as i32, argb)]
}

struct Item {
    tx: mpsc::Sender<Click>,
    pixmap: Pixmaps,
    waiting: bool,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    fn activate(&self, _x: i32, _y: i32) {
        let _ = self.tx.try_send(Click::Open);
    }

    fn secondary_activate(&self, _x: i32, _y: i32) {
        let _ = self.tx.try_send(Click::Open);
    }

    /// Hosts show `Menu` themselves; only one without dbusmenu calls this.
    fn context_menu(&self, _x: i32, _y: i32) {}

    fn scroll(&self, _delta: i32, _orientation: &str) {}

    /// Tells the tray to read the icon again.
    #[zbus(signal)]
    async fn new_icon(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(property)]
    fn category(&self) -> &str {
        "Communications"
    }

    #[zbus(property)]
    fn id(&self) -> &str {
        crate::APP_ID
    }

    #[zbus(property)]
    fn title(&self) -> &str {
        crate::APP_NAME
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }

    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }

    /// The theme's icon has no dot, so a waiting chat sends the pixmap alone.
    #[zbus(property)]
    fn icon_name(&self) -> &str {
        if self.waiting { "" } else { crate::APP_ID }
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Pixmaps {
        self.pixmap.clone()
    }

    #[zbus(property)]
    fn tool_tip(&self) -> (&str, Pixmaps, &str, &str) {
        ("", Vec::new(), crate::APP_NAME, "")
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn menu(&self) -> ObjectPath<'_> {
        ObjectPath::from_static_str_unchecked(MENU_PATH)
    }
}

/// `com.canonical.dbusmenu`: Open Unisend and Quit, never changing, so revision 1 throughout.
struct Menu {
    tx: mpsc::Sender<Click>,
}

type Props = HashMap<String, OwnedValue>;

fn props(id: i32) -> Props {
    let mut p = Props::new();
    let mut put = |k: &str, v: Value<'_>| {
        if let Ok(v) = v.try_into_owned() {
            p.insert(k.into(), v);
        }
    };
    match id {
        OPEN => put("label", Value::from(t!("Open Unisend"))),
        QUIT => put("label", Value::from(t!("Quit"))),
        _ => put("children-display", Value::from("submenu")),
    }
    p
}

fn entry(id: i32) -> Value<'static> {
    Value::from(Structure::from((id, props(id), Vec::<OwnedValue>::new())))
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    fn get_layout(&self, parent: i32, _depth: i32, _names: Vec<String>) -> (u32, (i32, Props, Vec<Value<'static>>)) {
        let children = if parent == 0 { vec![entry(OPEN), entry(QUIT)] } else { Vec::new() };
        (1, (parent, props(parent), children))
    }

    fn get_group_properties(&self, ids: Vec<i32>, _names: Vec<String>) -> Vec<(i32, Props)> {
        ids.into_iter().map(|id| (id, props(id))).collect()
    }

    fn get_property(&self, id: i32, name: &str) -> zbus::fdo::Result<OwnedValue> {
        props(id).remove(name).ok_or_else(|| zbus::fdo::Error::InvalidArgs(name.into()))
    }

    fn event(&self, id: i32, event: &str, _data: Value<'_>, _time: u32) {
        let click = match id {
            OPEN => Click::Open,
            QUIT => Click::Quit,
            _ => return,
        };
        if event == "clicked" {
            let _ = self.tx.try_send(click);
        }
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        for (id, event, data, time) in events {
            self.event(id, &event, Value::from(data), time);
        }
        Vec::new()
    }

    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }
}

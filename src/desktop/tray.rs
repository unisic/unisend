//! The tray icon on Linux: a StatusNotifierItem with a two-entry dbusmenu, served on the session
//! bus. KDE Plasma shows it natively; GNOME only with an extension. Nothing polls: the bus wakes
//! the task for a click, and for a tray that (re)starts, to register with it again.

use std::collections::HashMap;

use futures_util::StreamExt as _;
use gpui_kit::App;
use tokio::sync::mpsc;
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
/// Shown when the icon theme has no `dev.gpui.beeper` (the app not installed): 64 x 64 x 4 bytes.
const ICON: &[u8] = include_bytes!("../../packaging/linux/icons/hicolor/64x64/apps/dev.gpui.beeper.png");

/// The spec's pixmaps: width, height and ARGB32 bytes.
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

#[derive(Clone, Copy)]
enum Click {
    Open,
    Quit,
}

pub(super) fn start(cx: &mut App) {
    let (tx, mut rx) = mpsc::channel(QUEUE);
    cx.background_executor().spawn(async move {
        if let Err(e) = serve(tx).await {
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

async fn serve(tx: mpsc::Sender<Click>) -> zbus::Result<()> {
    let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
    let conn = zbus::connection::Builder::session()?
        .name(name.as_str())?
        .serve_at(ITEM_PATH, Item { tx: tx.clone(), pixmap: pixmap() })?
        .serve_at(MENU_PATH, Menu { tx })?
        .build()
        .await?;
    let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
    // Plasma restarted, or the tray came after the app: the new watcher knows nothing of the icon.
    let mut watchers = dbus.receive_name_owner_changed_with_args(&[(0, WATCHER)]).await?;
    register(&conn, &name).await;
    while let Some(change) = watchers.next().await {
        if change.args().is_ok_and(|a| a.new_owner().is_some()) {
            register(&conn, &name).await;
        }
    }
    Ok(())
}

async fn register(conn: &zbus::Connection, name: &str) {
    let call = conn.call_method(Some(WATCHER), "/StatusNotifierWatcher", Some(WATCHER), "RegisterStatusNotifierItem", &(name,)).await;
    if let Err(e) = call {
        log::warn!("tray icon: no tray to show it in ({e})");
    }
}

/// The icon as the spec's ARGB32 pixmap, alpha first in network byte order.
fn pixmap() -> Pixmaps {
    let Ok(img) = image::load_from_memory_with_format(ICON, image::ImageFormat::Png) else { return Vec::new() };
    let img = img.to_rgba8();
    let argb = img.pixels().flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
    vec![(img.width() as i32, img.height() as i32, argb)]
}

struct Item {
    tx: mpsc::Sender<Click>,
    pixmap: Pixmaps,
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

    #[zbus(property)]
    fn category(&self) -> &str {
        "Communications"
    }

    #[zbus(property)]
    fn id(&self) -> &str {
        "dev.gpui.beeper"
    }

    #[zbus(property)]
    fn title(&self) -> &str {
        "Unisend"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }

    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }

    #[zbus(property)]
    fn icon_name(&self) -> &str {
        "dev.gpui.beeper"
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Pixmaps {
        self.pixmap.clone()
    }

    #[zbus(property)]
    fn tool_tip(&self) -> (&str, Pixmaps, &str, &str) {
        ("", Vec::new(), "Unisend", "")
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

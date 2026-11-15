//! The window's own title bar where the compositor lets the app draw it (Linux): minimise,
//! maximise and close, a drag to move, a double click to maximise, the right button for the
//! window menu. The component library's root already draws the rest of the frame (resize edges,
//! shadow, a hairline edge) under client decorations. Under server decorations, and on phones,
//! nothing here draws or moves anything.

use super::*;

const TITLE_H: f32 = 36.;
const BUTTON_W: f32 = 40.;
const BUTTON_DISC: f32 = 28.;
/// The hairline the library's frame draws on each side not tiled against the screen.
const EDGE: f32 = 1.;

/// How far the frame moves the app in from each side, from the library's shadow room.
fn content_insets(room: Edges<Pixels>, tiling: Tiling) -> Edges<Pixels> {
    let edge = |tiled: bool| px(if tiled { 0. } else { EDGE });
    Edges { top: room.top + edge(tiling.top), right: room.right + edge(tiling.right), bottom: room.bottom + edge(tiling.bottom), left: room.left + edge(tiling.left) }
}

/// `None` without client decorations.
fn insets(window: &Window) -> Option<Edges<Pixels>> {
    let Decorations::Client { tiling } = window.window_decorations() else { return None };
    Some(content_insets(gpui_kit::component::window_paddings(window), tiling))
}

/// Where the app's area starts in the window: pointer positions are in the window's coordinates.
pub(super) fn origin(window: &Window) -> Point<Pixels> {
    insets(window).map_or_else(Point::default, |e| point(e.left, e.top + px(TITLE_H)))
}

/// A window position in the app area's coordinates, which absolutely placed layers use.
pub(super) fn local(window: &Window, at: Point<Pixels>) -> Point<Pixels> {
    at - origin(window)
}

/// The size of the app's area: the window less the frame and the title bar.
pub(super) fn viewport(window: &Window) -> Size<Pixels> {
    let view = window.viewport_size();
    insets(window).map_or(view, |e| size(view.width - e.left - e.right, view.height - e.top - e.bottom - px(TITLE_H)))
}

/// `content` under the title bar, or `content` alone when the compositor draws the frame.
pub(super) fn wrap(content: AnyElement, window: &mut Window, cx: &mut App) -> AnyElement {
    if insets(window).is_none() {
        return content;
    }
    let p = Palette::of(cx);
    v_flex()
        .size_full()
        .bg(p.ground)
        .child(title_bar(window, cx))
        .child(div().flex_1().min_h_0().child(content))
        .into_any_element()
}

#[derive(Clone, Copy)]
enum Control {
    Minimise,
    Maximise,
    Restore,
    Close,
}

fn control(kind: Control, cx: &App) -> impl IntoElement {
    let p = Palette::of(cx);
    let (id, icon, label) = match kind {
        Control::Minimise => ("window-minimise", IconName::Minus, t!("Minimise")),
        Control::Maximise => ("window-maximise", IconName::Square, t!("Maximise")),
        Control::Restore => ("window-restore", IconName::Copy, t!("Restore")),
        Control::Close => ("window-close", IconName::X, t!("Close")),
    };
    let (hover_bg, hover_fg) = match kind {
        Control::Close => (p.negative, p.ground),
        _ => (p.surface, p.text),
    };
    div()
        .id(id)
        .group(id)
        .w(px(BUTTON_W))
        .h_full()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .role(Role::Button)
        .aria_label(label)
        // Not the title bar's: a press here neither moves the window nor counts toward a double click.
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            cx.stop_propagation();
        })
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            match kind {
                Control::Minimise => window.minimize_window(),
                Control::Maximise | Control::Restore => window.zoom_window(),
                Control::Close => window.remove_window(),
            }
        })
        .child(
            div()
                .id("disc")
                .size(px(BUTTON_DISC))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_color(p.muted)
                .group_hover(id, |s| s.bg(hover_bg).text_color(hover_fg))
                .group_active(id, |s| s.bg(if matches!(kind, Control::Close) { p.negative } else { p.raised }).text_color(hover_fg))
                .child(Icon::new(icon).size_4()),
        )
}

/// A drag moves the window, a double click maximises or restores it, the right button opens the
/// compositor's window menu.
fn title_bar(window: &mut Window, cx: &mut App) -> impl IntoElement {
    let armed = window.use_keyed_state("title-drag", cx, |_, _| false);
    let offered = window.window_controls();
    let maximised = window.is_maximized();
    h_flex()
        .id("title-bar")
        .flex_none()
        .w_full()
        .h(px(TITLE_H))
        .justify_end()
        .pr(px(4.))
        .on_mouse_down(MouseButton::Left, {
            let armed = armed.clone();
            move |e, window, cx| {
                if e.click_count == 2 {
                    window.zoom_window();
                } else {
                    armed.write(cx, true);
                }
            }
        })
        .on_mouse_up(MouseButton::Left, {
            let armed = armed.clone();
            move |_, _, cx| armed.write(cx, false)
        })
        .on_mouse_down_out({
            let armed = armed.clone();
            move |_, _, cx| armed.write(cx, false)
        })
        // Moving only once the pointer moves keeps the second press of a double click for us.
        .on_mouse_move(move |_, window, cx| {
            if *armed.read(cx) {
                armed.write(cx, false);
                window.start_window_move();
            }
        })
        .when(offered.window_menu, |el| el.on_mouse_down(MouseButton::Right, |e, window, _| window.show_window_menu(e.position)))
        .when(offered.minimize, |el| el.child(control(Control::Minimise, cx)))
        .when(offered.maximize, |el| el.child(control(if maximised { Control::Restore } else { Control::Maximise }, cx)))
        .child(control(Control::Close, cx))
}

#[cfg(test)]
mod tests {
    use super::content_insets;
    use gpui_kit::{Edges, Tiling, px};

    #[test]
    fn the_app_starts_inside_the_shadow_and_the_hairline_of_each_free_side() {
        let room = Edges { top: px(20.), right: px(20.), bottom: px(0.), left: px(0.) };
        let tiled_bottom_left = Tiling { top: false, right: false, bottom: true, left: true };
        let e = content_insets(room, tiled_bottom_left);
        assert_eq!((e.top, e.right, e.bottom, e.left), (px(21.), px(21.), px(0.), px(0.)));
        let e = content_insets(Edges::all(px(0.)), Tiling::tiled());
        assert_eq!((e.top, e.right, e.bottom, e.left), (px(0.), px(0.), px(0.), px(0.)));
    }
}

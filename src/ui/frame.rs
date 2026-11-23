//! The window's own controls where the compositor lets the app draw them (Linux): minimize,
//! maximize and close at the right end of the top row, and the top rows themselves as the title
//! bar: a drag moves the window, a double click maximizes it, the right button opens the window
//! menu. The component library's root already draws the rest of the frame (resize edges, shadow, a
//! hairline edge) under client decorations. Under server decorations, and on phones, nothing here
//! draws or moves anything.

use super::*;

const BUTTON_W: f32 = 40.;
const BUTTON_DISC: f32 = 28.;
/// Lines the close button's disc up with the round buttons of a header.
const END_PAD: f32 = GUTTER - 10.;
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
    insets(window).map_or_else(Point::default, |e| point(e.left, e.top))
}

/// A window position in the app area's coordinates, which absolutely placed layers use.
pub(super) fn local(window: &Window, at: Point<Pixels>) -> Point<Pixels> {
    at - origin(window)
}

/// The size of the app's area: the window less the frame.
pub(super) fn viewport(window: &Window) -> Size<Pixels> {
    let view = window.viewport_size();
    insets(window).map_or(view, |e| size(view.width - e.left - e.right, view.height - e.top - e.bottom))
}

/// The width the window buttons take at the right end of the top row, which the header under
/// them leaves free; zero where the compositor draws them.
pub(super) fn reserve(window: &Window) -> f32 {
    if insets(window).is_none() {
        return 0.;
    }
    let offered = window.window_controls();
    (1 + usize::from(offered.minimize) + usize::from(offered.maximize)) as f32 * BUTTON_W + END_PAD
}

/// The window buttons over the top right corner of the app's area, as tall as a header.
pub(super) fn controls(window: &Window, cx: &App) -> Option<AnyElement> {
    insets(window)?;
    let offered = window.window_controls();
    let maximized = window.is_maximized();
    Some(
        h_flex()
            .absolute()
            .top_0()
            .right_0()
            .h(px(BAR_H))
            .pr(px(END_PAD))
            .when(offered.minimize, |el| el.child(control(Control::Minimize, cx)))
            .when(offered.maximize, |el| el.child(control(if maximized { Control::Restore } else { Control::Maximize }, cx)))
            .child(control(Control::Close, cx))
            .into_any_element(),
    )
}

/// `el` as a piece of the title bar. Give it only the empty parts of a header: a press on a button
/// inside it would also arm the move.
pub(super) fn drag(el: Div, window: &mut Window, cx: &mut App) -> Div {
    if insets(window).is_none() {
        return el;
    }
    let armed = window.use_keyed_state("title-drag", cx, |_, _| false);
    let offered = window.window_controls();
    el.on_mouse_down(MouseButton::Left, {
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
}

/// A screen with no header of its own (signing in, the lock) under a top strip that moves the
/// window, with the window buttons over it.
pub(super) fn wrap(content: AnyElement, window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(buttons) = controls(window, cx) else { return content };
    let strip = drag(div().absolute().top_0().left_0().w_full().h(px(BAR_H)), window, cx);
    // The strip is painted first, so whatever the screen puts in the top row stays on top of it.
    div().size_full().relative().child(strip).child(div().size_full().child(content)).child(buttons).into_any_element()
}

#[derive(Clone, Copy)]
enum Control {
    Minimize,
    Maximize,
    Restore,
    Close,
}

fn control(kind: Control, cx: &App) -> impl IntoElement {
    let p = Palette::of(cx);
    let (id, icon, label) = match kind {
        Control::Minimize => ("window-minimize", IconName::Minus, t!("Minimize")),
        Control::Maximize => ("window-maximize", IconName::Square, t!("Maximize")),
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
                Control::Minimize => window.minimize_window(),
                Control::Maximize | Control::Restore => window.zoom_window(),
                #[cfg(not(target_os = "android"))]
                Control::Close => crate::desktop::close(window, cx),
                #[cfg(target_os = "android")]
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

//! Signing in to a Matrix homeserver other than Beeper's, with a user name and password.

use super::*;
use crate::i18n::t;

pub(super) struct Form {
    server: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
}

impl Form {
    pub(super) fn new(window: &mut Window, cx: &mut Context<BeeperApp>, subs: &mut Vec<Subscription>) -> Self {
        let server = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Server, like matrix.org")));
        let user = cx.new(|cx| InputState::new(window, cx).placeholder(t!("User name")));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Password")).masked(true));
        for input in [&server, &user, &password] {
            subs.push(cx.subscribe_in(input, window, |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { shift: false, .. } => this.password_login(window, cx),
                other => keyboard_follows(other, Keyboard::Key, cx),
            }));
        }
        // A full user ID names its server: `@name:matrix.org` fills in `matrix.org`, once typed and
        // only into an empty field, since a server's name and address can differ.
        subs.push(cx.subscribe_in(&user, window, |this, user, ev: &InputEvent, window, cx| {
            if !matches!(ev, InputEvent::Blur) || !this.matrix.server.read(cx).value().trim().is_empty() {
                return;
            }
            let typed = user.read(cx).value();
            if let Some((_, server)) = typed.trim().strip_prefix('@').and_then(|id| id.split_once(':'))
                && !server.is_empty()
            {
                let server = server.to_owned();
                this.matrix.server.update(cx, |s, cx| s.set_value(server, window, cx));
            }
        }));
        Self { server, user, password }
    }
}

impl BeeperApp {
    fn password_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let read = |input: &Entity<InputState>| input.read(cx).value().to_string();
        let (mut server, user, password) = (read(&self.matrix.server), read(&self.matrix.user), read(&self.matrix.password));
        if server.trim().is_empty() && user.trim().starts_with('@') {
            server = user.clone();
        }
        if let Err(e) = crate::backend::server_from_input(&server) {
            self.error = Some(e.into());
            cx.notify();
            return;
        }
        if user.trim().is_empty() || password.is_empty() {
            self.error = Some(t!("Enter your user name and password.").into());
            cx.notify();
            return;
        }
        self.error = None;
        self.busy = self.send(Cmd::PasswordLogin { server, user, password }, cx);
        // The password lives on only in the request just sent.
        if self.busy {
            self.matrix.password.update(cx, |p, cx| p.set_value("", window, cx));
        }
    }

    pub(super) fn render_matrix_login(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p(px(GUTTER + 8.))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(360.))
                    .gap(px(GAP))
                    .child(div().text_size(fs(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.faint).child("Matrix"))
                    .child(screen_title(t!("Sign in to your server")))
                    .child(
                        div()
                            .pb(px(8.))
                            .text_size(fs(14.))
                            .line_height(fs(20.))
                            .text_color(p.muted)
                            .child(t!("Any Matrix server that accepts a password, like matrix.org.")),
                    )
                    .child(Input::new(&self.matrix.server).large())
                    .child(Input::new(&self.matrix.user).large())
                    .child(Input::new(&self.matrix.password).large().mask_toggle())
                    .child(
                        Button::new("matrix-submit")
                            .primary()
                            .h(px(TAP))
                            .label(t!("Sign in"))
                            .loading(self.busy)
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.password_login(window, cx))),
                    )
                    .child(Button::new("matrix-back").ghost().label(t!("Sign in with Beeper instead")).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.screen = Screen::Email;
                            this.error = None;
                            cx.notify();
                        },
                    )))
                    .children(self.error.clone().map(|e| div().text_size(fs(14.)).text_color(p.negative).child(e))),
            )
    }
}

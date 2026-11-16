//! Signing in to a Matrix homeserver other than Beeper's: first the server, then the ways it
//! offers (its own page in a browser, through OAuth or single sign-on, or a password).

use super::*;
use crate::backend::{Methods, Way};
use crate::i18n::t;

pub(super) struct Form {
    server: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    /// What the server last asked offers; stale once the field says another server.
    methods: Option<Methods>,
    /// The browser sign-in under way, by the address its page sends the browser back to.
    pub(super) waiting: Option<String>,
}

impl Form {
    pub(super) fn new(window: &mut Window, cx: &mut Context<BeeperApp>, subs: &mut Vec<Subscription>) -> Self {
        let server = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Server, like matrix.org")));
        let user = cx.new(|cx| InputState::new(window, cx).placeholder(t!("User name")));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Password")).masked(true));
        for input in [&server, &user, &password] {
            subs.push(cx.subscribe_in(input, window, |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { shift: false, .. } => this.matrix_submit(window, cx),
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
        Self { server, user, password, methods: None, waiting: None }
    }
}

impl BeeperApp {
    /// The server in the field, or the one a full user ID names when the field is empty.
    fn typed_server(&self, cx: &App) -> String {
        let server = self.matrix.server.read(cx).value().to_string();
        let user = self.matrix.user.read(cx).value().to_string();
        if server.trim().is_empty() && user.trim().starts_with('@') { user } else { server }
    }

    /// What the server in the field offers, if it was asked since the field last changed.
    fn methods(&self, cx: &App) -> Option<&Methods> {
        let typed = self.typed_server(cx);
        self.matrix.methods.as_ref().filter(|m| m.typed == typed)
    }

    /// Enter in any field: asks the server what it offers, or signs in with the password once it
    /// offers one.
    fn matrix_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.methods(cx) {
            Some(m) if m.password => self.password_login(window, cx),
            Some(_) => {}
            None => self.ask_methods(cx),
        }
    }

    fn ask_methods(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let server = self.typed_server(cx);
        if let Err(e) = crate::backend::server_from_input(&server) {
            self.error = Some(e.into());
            cx.notify();
            return;
        }
        self.error = None;
        self.busy = self.send(Cmd::LoginMethods(server), cx);
    }

    fn matrix_browser_login(&mut self, way: Way, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.error = None;
        self.busy = self.send(Cmd::BrowserLogin { server: self.typed_server(cx), way }, cx);
    }

    fn password_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let read = |input: &Entity<InputState>| input.read(cx).value().to_string();
        let (user, password) = (read(&self.matrix.user), read(&self.matrix.password));
        let server = self.typed_server(cx);
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

    pub(super) fn on_login_methods(&mut self, methods: Methods) {
        self.matrix.methods = Some(methods);
        (self.busy, self.error) = (false, None);
    }

    /// The server's sign-in page is ready: the browser shows it, and the form waits for it.
    pub(super) fn on_sign_in_page(&mut self, url: String, back: String, cx: &mut Context<Self>) {
        self.matrix.waiting = Some(back.clone());
        self.busy = false;
        let Some(answer) = crate::sign_in_page(cx, &url, &back) else { return };
        cx.spawn(async move |this, cx| {
            let url = answer.await;
            let _ = this.update(cx, |this, cx| {
                if this.matrix.waiting.as_ref() != Some(&back) {
                    return;
                }
                match url {
                    Some(url) => this.busy = this.send(Cmd::BrowserDone { back, url: Some(url) }, cx),
                    // Closed before the server sent it back: nothing to report.
                    None => this.cancel_browser(cx),
                }
            });
        })
        .detach();
    }

    fn cancel_browser(&mut self, cx: &mut Context<Self>) {
        if self.matrix.waiting.take().is_some() {
            self.send(Cmd::BrowserCancel, cx);
        }
        self.busy = false;
        cx.notify();
    }

    pub(super) fn render_matrix_login(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        let methods = self.methods(cx).cloned();
        let waiting = self.matrix.waiting.is_some();
        let busy = self.busy;
        let tall = |b: Button| b.h(px(TAP)).loading(busy).disabled(busy);
        let mut ways = v_flex().gap(px(GAP));
        if waiting {
            ways = ways
                .child(div().text_size(fs(14.)).line_height(fs(20.)).text_color(p.muted).child(t!("Finish signing in on your server's page. This screen goes on by itself.")))
                .child(tall(Button::new("matrix-cancel")).label(t!("Cancel")).on_click(cx.listener(|this, _, _, cx| this.cancel_browser(cx))));
        } else if let Some(m) = &methods {
            if m.oauth {
                ways = ways.child(tall(Button::new("matrix-oauth").primary()).label(t!("Continue in the browser")).on_click(cx.listener(|this, _, _, cx| this.matrix_browser_login(Way::OAuth, cx))));
            }
            if let Some(providers) = &m.sso {
                // One primary control per screen: the password's button, when there is one.
                let first_primary = !m.password;
                if providers.is_empty() {
                    let b = tall(Button::new("matrix-sso")).label(t!("Continue in the browser"));
                    ways = ways.child(if first_primary { b.primary() } else { b }.on_click(cx.listener(|this, _, _, cx| this.matrix_browser_login(Way::Sso(None), cx))));
                }
                for (i, (id, name)) in providers.iter().enumerate() {
                    let b = tall(Button::new(("matrix-idp", i))).label(t!("Continue with {provider}", provider = name));
                    let id = id.clone();
                    ways = ways.child(if first_primary && i == 0 { b.primary() } else { b }.on_click(cx.listener(move |this, _, _, cx| this.matrix_browser_login(Way::Sso(Some(id.clone())), cx))));
                }
            }
            if m.password {
                ways = ways
                    .child(Input::new(&self.matrix.user).large())
                    .child(Input::new(&self.matrix.password).large().mask_toggle())
                    .child(tall(Button::new("matrix-submit").primary()).label(t!("Sign in")).on_click(cx.listener(|this, _, window, cx| this.password_login(window, cx))));
            }
            if !m.oauth && !m.password && m.sso.is_none() {
                ways = ways.child(div().text_size(fs(14.)).text_color(p.muted).child(t!("This server signs people in another way, which this app does not support yet.")));
            }
        } else {
            ways = ways.child(tall(Button::new("matrix-next").primary()).label(t!("Continue")).on_click(cx.listener(|this, _, _, cx| this.ask_methods(cx))));
        }
        super::sign_in_screen(
            "matrix-screen",
                v_flex()
                    .w_full()
                    .max_w(px(360.))
                    .gap(px(GAP))
                    .child(div().text_size(fs(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.faint).child("Matrix"))
                    .child(screen_title(t!("Sign in to your server")))
                    .child(div().pb(px(8.)).text_size(fs(14.)).line_height(fs(20.)).text_color(p.muted).child(t!("Any Matrix server, like matrix.org.")))
                    .child(Input::new(&self.matrix.server).large().disabled(waiting))
                    .child(ways)
                    .child(Button::new("matrix-back").ghost().label(t!("Sign in with Beeper instead")).on_click(cx.listener(|this, _, _, cx| {
                        this.cancel_browser(cx);
                        this.screen = Screen::Email;
                        this.error = None;
                        cx.notify();
                    })))
                    .children(self.back_to_accounts(cx))
                    .children(self.error.clone().map(|e| div().text_size(fs(14.)).text_color(p.negative).child(e))),
            )
    }
}

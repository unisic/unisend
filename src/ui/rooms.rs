//! Leaving a chat, and starting one: the question asked before a chat is left or deleted, and the
//! New chat screen, where a person or a group is found on Matrix or on a network the user is
//! signed in to.

use super::*;
use crate::backend::rooms::{Answer, Ask, Caps, Person, Target};
use crate::i18n::t;

/// Rows the New chat screen draws at once; a longer contact list is narrowed by typing.
// shortcut: a capped list instead of a virtual one; use `uniform_list` if people miss contacts.
const MAX_ROWS: usize = 200;
/// People in one new group.
const MAX_PICKED: usize = 256;
/// A pause in typing this long asks the server; each keystroke does not.
const SEARCH_DELAY: Duration = Duration::from_millis(300);
const DIALOG_W: f32 = 340.;

#[derive(Default)]
pub(super) struct People {
    confirm: Option<Confirm>,
    new: Option<NewChat>,
}

/// A chat the user asked to leave; drawn once the backend says whether its bridge can delete it.
struct Confirm {
    room: OwnedRoomId,
    name: String,
    network: Option<String>,
    dm: bool,
    delete: Option<bool>,
}

pub(super) struct NewChat {
    target: Target,
    /// `None` while the network is asked.
    caps: Option<Caps>,
    group: bool,
    query: Entity<InputState>,
    name: Entity<InputState>,
    /// What the placeholder was last set for.
    hint: &'static str,
    _subs: Vec<Subscription>,
    /// The network's contacts, by name; at most `rooms::MAX_PEOPLE`.
    contacts: Vec<Person>,
    /// What the server found for `found_for`.
    found: Vec<Person>,
    found_for: String,
    /// What the list shows for the current query, at most `MAX_ROWS`.
    rows: Vec<Person>,
    picked: Vec<Person>,
    busy: bool,
    error: Option<String>,
    /// The search waiting for typing to pause; dropping it cancels it.
    pending: Option<Task<()>>,
    /// Someone typed was added to the group: the field empties on the next frame, which has a window.
    clear: bool,
}

impl NewChat {
    fn query(&self, cx: &App) -> String {
        self.query.read(cx).value().trim().to_owned()
    }

    /// Server results for what is typed when the network searches; otherwise the contacts that
    /// match it.
    fn refresh(&mut self, cx: &App) {
        let query = self.query(cx);
        let searches = self.caps.as_ref().is_some_and(|c| c.search);
        self.rows = if searches && !query.is_empty() {
            if self.found_for == query { self.found.iter().take(MAX_ROWS).cloned().collect() } else { Vec::new() }
        } else {
            let q = query.to_lowercase();
            self.contacts.iter().filter(|p| q.is_empty() || p.name.to_lowercase().contains(&q) || p.detail.to_lowercase().contains(&q)).take(MAX_ROWS).cloned().collect()
        };
    }

    /// What is typed reads as something the network can look up: a phone number, a username, an
    /// address, or a Matrix ID.
    fn identifier(&self, cx: &App) -> Option<String> {
        let q = self.query(cx);
        let caps = self.caps.as_ref()?;
        looks_like_identifier(&q, &self.target, caps).then_some(q)
    }

    fn placeholder(&self) -> &'static str {
        match (&self.target, &self.caps) {
            (Target::Matrix, _) => t!("Name or Matrix ID"),
            (_, Some(c)) if c.phone && c.username => t!("Name, phone number or username"),
            (_, Some(c)) if c.phone => t!("Name or phone number"),
            (_, Some(c)) if c.username => t!("Name or username"),
            _ => t!("Name"),
        }
    }
}

fn looks_like_identifier(q: &str, target: &Target, caps: &Caps) -> bool {
    if q.is_empty() {
        return false;
    }
    if *target == Target::Matrix {
        return q.starts_with('@') && q.contains(':') && !q.contains(char::is_whitespace);
    }
    let digits = q.chars().filter(char::is_ascii_digit).count();
    let phone = digits >= 5 && q.chars().all(|c| c.is_ascii_digit() || " +-().".contains(c));
    (caps.phone && phone) || (caps.email && q.contains('@') && !q.contains(' ')) || (caps.username && !q.contains(char::is_whitespace))
}

/// What the leave dialog says and its button, in whole sentences.
fn wording(c: &Confirm) -> (String, String, &'static str) {
    let network = c.network.as_deref().unwrap_or(t!("the network"));
    match (c.network.is_some(), c.delete == Some(true), c.dm) {
        (true, true, true) => (
            t!("Delete {name}?", name = c.name),
            t!("The chat is deleted from Murmur and from your {network} account. The other person keeps their copy.", network = network),
            t!("Delete"),
        ),
        (true, true, false) => (
            t!("Delete {name}?", name = c.name),
            t!("You leave the group on {network}, and the chat is deleted from Murmur and from your account. The other members keep their copy.", network = network),
            t!("Delete"),
        ),
        (true, false, _) => (
            t!("Leave {name}?", name = c.name),
            t!("The chat is removed from Murmur only. The {network} bridge cannot delete it there, and new messages may bring it back.", network = network),
            t!("Leave"),
        ),
        (false, _, true) => (t!("Leave {name}?", name = c.name), t!("The chat is removed from your list. The other person keeps the conversation.").to_owned(), t!("Leave")),
        (false, _, false) => (
            t!("Leave {name}?", name = c.name),
            t!("The chat is removed from your list. To come back you need a new invitation.").to_owned(),
            t!("Leave"),
        ),
    }
}

impl BeeperApp {
    /// The last row of a chat's sheet.
    pub(super) fn leave_row(&self, room: OwnedRoomId, live: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = Palette::of(cx);
        let bridged = self.rooms.iter().any(|r| r.id == room && r.network.is_some());
        let row = if bridged {
            sheet_row("chat-delete", IconName::Trash, t!("Delete chat"), p.negative, cx)
        } else {
            sheet_row("chat-leave", IconName::LogOut, t!("Leave chat"), p.negative, cx)
        };
        if !live {
            return row;
        }
        row.on_click(cx.listener(move |this, _, _, cx| {
            this.chat_sheet = None;
            this.ask_leave(room.clone(), cx);
        }))
    }

    fn ask_leave(&mut self, room: OwnedRoomId, cx: &mut Context<Self>) {
        let Some(r) = self.rooms.iter().find(|r| r.id == room) else { return };
        let mut confirm = Confirm { room: room.clone(), name: r.name.clone(), network: r.network.clone(), dm: r.dm, delete: None };
        if confirm.network.is_none() {
            confirm.delete = Some(false);
        } else if !self.send(Cmd::People(Ask::LeaveOptions(room)), cx) {
            return;
        }
        self.people.confirm = Some(confirm);
        cx.notify();
    }

    /// Escape closes the dialog first.
    pub(super) fn close_confirm(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.people.confirm.take().is_some();
        if open {
            cx.notify();
        }
        open
    }

    fn leave(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.people.confirm.take() else { return };
        let delete = c.delete == Some(true);
        if !self.send(Cmd::People(Ask::Leave { room: c.room.clone(), delete }), cx) {
            return;
        }
        // It leaves the list at once; the backend puts it back if leaving fails.
        self.rooms.retain(|r| r.id != c.room);
        self.refilter(cx);
        if self.open.as_ref() == Some(&c.room) {
            self.close_room(cx);
        }
    }

    pub(super) fn render_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let live = self.people.confirm.as_ref().is_some_and(|c| c.delete.is_some());
        let shown = Presence::new("leave-dialog", live).transition(if live { motion::enter() } else { motion::exit() }).sample(window, cx);
        // The words outlive the confirmation so the dialog can fade out showing what it asked.
        let last = window.use_keyed_state("leave-dialog-last", cx, |_, _| None::<(String, String, &'static str)>);
        if let Some(c) = self.people.confirm.as_ref().filter(|c| c.delete.is_some()) {
            let words = wording(c);
            if last.read(cx).as_ref() != Some(&words) {
                last.update(cx, |l, _| *l = Some(words));
            }
        }
        if !shown.should_render() {
            if last.read(cx).is_some() {
                last.update(cx, |l, _| *l = None);
            }
            return None;
        }
        let (title, body, act) = last.read(cx).clone()?;
        let v = shown.progress;
        let p = Palette::of(cx);
        let width = DIALOG_W.min(f32::from(window.viewport_size().width) - 2. * GUTTER);
        let button = |id: &'static str, label: &'static str, bg: Hsla, ink: Hsla| {
            div()
                .id(id)
                .h(px(TAP))
                .px(px(GUTTER + 4.))
                .flex()
                .items_center()
                .rounded_full()
                .bg(bg)
                .text_color(ink)
                .text_size(fs(16.))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .active(|el| el.opacity(0.8))
                .child(label)
        };
        let dialog = v_flex()
            .id("leave-dialog")
            .w(px(width))
            .p(px(20.))
            .gap(px(GAP))
            .rounded(px(RADIUS))
            .bg(p.raised)
            .occlude()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(div().text_size(fs(18.)).font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().text_size(fs(14.)).line_height(fs(20.)).text_color(p.muted).child(body))
            .child(
                h_flex()
                    .pt(px(6.))
                    .gap(px(8.))
                    .justify_end()
                    .child(button("leave-cancel", t!("Cancel"), p.lifted, p.text).when(live, |el| {
                        el.on_click(cx.listener(|this, _, _, cx| {
                            this.close_confirm(cx);
                        }))
                    }))
                    .child(button("leave-confirm", act, p.negative, gpui_kit::white()).when(live, |el| el.on_click(cx.listener(|this, _, _, cx| this.leave(cx))))),
            )
            .opacity(v)
            .top(px(8. * (1. - v)));
        Some(
            div()
                .id("leave-scrim")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui_kit::black().opacity(0.6 * v))
                .occlude()
                .when(live, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| {
                        this.close_confirm(cx);
                    }))
                })
                .child(dialog)
                .into_any_element(),
        )
    }

    /// Where a new chat can be made: Matrix itself, then each login on each network.
    fn targets(&self) -> Vec<(Target, String)> {
        let mut out = vec![(Target::Matrix, if self.beeper { "Beeper".to_owned() } else { "Matrix".to_owned() })];
        for network in self.networks.iter().flatten() {
            for login in &network.logins {
                let label = if network.logins.len() > 1 { format!("{} ({})", network.name, login.name) } else { network.name.clone() };
                out.push((Target::Bridge { bridge: network.bridge.clone(), login: login.id.clone() }, label));
            }
        }
        out
    }

    pub(super) fn open_new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Name or Matrix ID")));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Group name")));
        let subs = vec![
            cx.subscribe_in(&query, window, |this, _, ev: &InputEvent, _, cx| match ev {
                InputEvent::Change => this.query_changed(cx),
                InputEvent::PressEnter { .. } => this.enter_pressed(cx),
                other => keyboard_follows(other, Keyboard::Text, cx),
            }),
            cx.subscribe_in(&name, window, |_, _, ev: &InputEvent, _, cx| keyboard_follows(ev, Keyboard::Text, cx)),
        ];
        if self.beeper && self.networks.is_none() {
            self.working = self.send(Cmd::Networks, cx);
        }
        self.people.new = Some(NewChat {
            target: Target::Matrix,
            caps: None,
            group: false,
            query,
            name,
            hint: "",
            _subs: subs,
            contacts: Vec::new(),
            found: Vec::new(),
            found_for: String::new(),
            rows: Vec::new(),
            picked: Vec::new(),
            busy: false,
            error: None,
            pending: None,
            clear: false,
        });
        self.choose_target(Target::Matrix, window, cx);
        self.open_panel(Panel::NewChat, cx);
        if let Some(new) = &self.people.new {
            new.query.update(cx, |input, cx| input.focus(window, cx));
        }
    }

    fn choose_target(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        let Some(new) = self.people.new.as_mut() else { return };
        if new.caps.is_some() && new.target == target {
            return;
        }
        (new.target, new.caps, new.error, new.pending) = (target.clone(), None, None, None);
        new.contacts.clear();
        new.found.clear();
        new.rows.clear();
        new.picked.clear();
        new.found_for.clear();
        new.query.update(cx, |input, cx| input.set_value("", window, cx));
        self.send(Cmd::People(Ask::Open(target)), cx);
    }

    /// Leaves group mode for the single chat; false when there is nothing to go back to.
    pub(super) fn new_chat_back(&mut self, cx: &mut Context<Self>) -> bool {
        match self.people.new.as_mut() {
            Some(new) if new.group && !new.busy => {
                (new.group, new.error) = (false, None);
                new.picked.clear();
                cx.notify();
                true
            }
            _ => {
                self.people.new = None;
                false
            }
        }
    }

    fn query_changed(&mut self, cx: &mut Context<Self>) {
        let Some(new) = self.people.new.as_mut() else { return };
        new.error = None;
        new.refresh(cx);
        let query = new.query(cx);
        let searches = new.caps.as_ref().is_some_and(|c| c.search);
        new.pending = None;
        if searches && !query.is_empty() && query != new.found_for {
            let target = new.target.clone();
            new.pending = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SEARCH_DELAY).await;
                let _ = this.update(cx, |this, cx| {
                    let still = this.people.new.as_ref().is_some_and(|n| n.query(cx) == query && n.target == target);
                    if still {
                        this.send(Cmd::People(Ask::Find { target, query }), cx);
                    }
                });
            }));
        }
        cx.notify();
    }

    fn enter_pressed(&mut self, cx: &mut Context<Self>) {
        let Some(new) = self.people.new.as_ref() else { return };
        if let Some(who) = new.identifier(cx) {
            self.pick_identifier(who, cx);
        } else if let Some(first) = new.rows.first().cloned() {
            self.pick(first, cx);
        }
    }

    fn pick(&mut self, person: Person, cx: &mut Context<Self>) {
        let Some(new) = self.people.new.as_mut() else { return };
        if new.busy {
            return;
        }
        if new.group {
            match new.picked.iter().position(|p| p.id == person.id) {
                Some(ix) => drop(new.picked.remove(ix)),
                None if new.picked.len() < MAX_PICKED => new.picked.push(person),
                None => new.error = Some(t!("A new group can have up to {max} people", max = MAX_PICKED)),
            }
            cx.notify();
            return;
        }
        let target = new.target.clone();
        self.ask_new(Ask::Start { target, who: person.id }, cx);
    }

    fn pick_identifier(&mut self, who: String, cx: &mut Context<Self>) {
        let Some(new) = self.people.new.as_mut() else { return };
        if new.busy {
            return;
        }
        let target = new.target.clone();
        let ask = if new.group { Ask::Resolve { target, who } } else { Ask::Start { target, who } };
        self.ask_new(ask, cx);
    }

    fn ask_new(&mut self, ask: Ask, cx: &mut Context<Self>) {
        let sent = self.send(Cmd::People(ask), cx);
        if let Some(new) = self.people.new.as_mut() {
            new.busy = sent;
        }
    }

    fn create_group(&mut self, cx: &mut Context<Self>) {
        let Some(new) = self.people.new.as_mut() else { return };
        let Some(group) = new.caps.as_ref().and_then(|c| c.group.clone()) else { return };
        if new.busy || !ready_to_create(new, &group, cx) {
            return;
        }
        let name = if group.name { new.name.read(cx).value().trim().to_owned() } else { String::new() };
        let people = new.picked.iter().map(|p| p.id.clone()).collect();
        let ask = Ask::Group { target: new.target.clone(), kind: group.kind, name, people };
        self.ask_new(ask, cx);
    }

    pub(super) fn on_people(&mut self, answer: Answer, cx: &mut Context<Self>) {
        match answer {
            Answer::LeaveOptions { room, delete } => {
                if let Some(c) = self.people.confirm.as_mut().filter(|c| c.room == room) {
                    c.delete = Some(delete);
                }
            }
            Answer::Caps(target, caps) => {
                if let Some(new) = self.people.new.as_mut().filter(|n| n.target == target) {
                    new.caps = Some(caps);
                    new.refresh(cx);
                }
            }
            Answer::People { target, query, mut people } => {
                let Some(new) = self.people.new.as_mut().filter(|n| n.target == target) else { return };
                if query.is_empty() {
                    people.sort_by_cached_key(|p| p.name.to_lowercase());
                    new.contacts = people;
                } else {
                    (new.found, new.found_for) = (people, query);
                }
                new.refresh(cx);
            }
            Answer::Resolved(target, person) => {
                let Some(new) = self.people.new.as_mut().filter(|n| n.target == target) else { return };
                new.busy = false;
                if !new.picked.iter().any(|p| p.id == person.id) && new.picked.len() < MAX_PICKED {
                    new.picked.push(person);
                }
                new.clear = true;
            }
            Answer::Ready(room) => {
                self.people.new = None;
                self.panel = None;
                self.nav = Some((Instant::now(), true));
                self.open_room(room, cx);
            }
            Answer::Failed(why) => match self.people.new.as_mut() {
                Some(new) => (new.busy, new.error) = (false, Some(why)),
                None => self.error = Some(why.into()),
            },
        }
        cx.notify();
    }

    pub(super) fn render_new_chat(&mut self, narrow: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let targets = self.targets();
        let Some(new) = self.people.new.as_mut() else { return div().into_any_element() };
        if std::mem::take(&mut new.clear) {
            new.query.update(cx, |input, cx| input.set_value("", window, cx));
            new.refresh(cx);
        }
        let hint = new.placeholder();
        if hint != new.hint {
            new.hint = hint;
            new.query.update(cx, |input, cx| input.set_placeholder(hint, window, cx));
        }
        let new = self.people.new.as_ref().expect("checked above");
        let edge = if narrow { GUTTER } else { GUTTER + 6. };
        let group_caps = new.caps.as_ref().and_then(|c| c.group.clone());
        let identifier = new.identifier(cx);
        let query = new.query(cx);
        let loading = new.caps.is_none() || (new.caps.as_ref().is_some_and(|c| c.search) && !query.is_empty() && new.found_for != query);

        let chips = (targets.len() > 1).then(|| {
            h_flex().id("new-chat-networks").flex_none().px(px(edge)).gap(px(4.)).overflow_x_scroll().children(targets.into_iter().enumerate().map(|(ix, (target, label))| {
                let on = target == new.target;
                h_flex().id(("network", ix)).group("network-chip").h(px(TAP)).flex_none().cursor_pointer().on_click(cx.listener(move |this, _, window, cx| this.choose_target(target.clone(), window, cx))).child(
                    h_flex()
                        .id("chip")
                        .h(px(36.))
                        .px(px(GAP))
                        .rounded_full()
                        .text_size(fs(14.))
                        .text_color(if on { p.text } else { p.muted })
                        .when(on, |el| el.bg(p.raised))
                        .when(!on, |el| el.group_active("network-chip", |el| el.bg(p.surface)))
                        .child(label),
                )
            }))
        });

        let field = h_flex()
            .flex_none()
            .mx(px(edge))
            .h(px(TAP))
            .pl(px(GAP))
            .pr(px(GAP))
            .gap(px(8.))
            .rounded_full()
            .bg(p.surface)
            .child(Icon::new(IconName::Search).size_4().flex_none().text_color(p.faint))
            .child(div().flex_1().min_w_0().child(Input::new(&new.query).appearance(false)));

        let name_field = (new.group && group_caps.as_ref().is_some_and(|g| g.name)).then(|| {
            h_flex().flex_none().mx(px(edge)).h(px(TAP)).px(px(GAP)).rounded_full().bg(p.surface).child(div().flex_1().min_w_0().child(Input::new(&new.name).appearance(false)))
        });

        let picked = (new.group && !new.picked.is_empty()).then(|| {
            h_flex().id("picked").flex_none().px(px(edge)).gap(px(6.)).overflow_x_scroll().children(new.picked.iter().enumerate().map(|(ix, person)| {
                let person_ = person.clone();
                h_flex()
                    .id(("picked", ix))
                    .h(px(TAP))
                    .flex_none()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.pick(person_.clone(), cx)))
                    .child(
                        h_flex()
                            .h(px(36.))
                            .pl(px(GAP))
                            .pr(px(10.))
                            .gap(px(6.))
                            .rounded_full()
                            .bg(p.raised)
                            .text_size(fs(14.))
                            .child(person.name.clone())
                            .child(Icon::new(IconName::X).size_3p5().text_color(p.faint)),
                    )
            }))
        });

        let row = |id: SharedString, title: String, detail: String, lead: AnyElement, check: Option<bool>| {
            h_flex()
                .id(id)
                .w_full()
                .h(px(ROOM_ROW_H))
                .px(px(edge))
                .gap(px(GAP))
                .cursor_pointer()
                .active(|el| el.bg(p.raised))
                .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.surface)))
                .child(lead)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().text_size(fs(16.)).child(title))
                        .when(!detail.is_empty(), |el| el.child(div().truncate().text_size(fs(14.)).text_color(p.faint).child(detail))),
                )
                .when_some(check, |el, on| {
                    el.child(
                        div()
                            .size(px(24.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .when(on, |el| el.bg(p.text).text_color(p.ground).child(Icon::new(IconName::Check).size_4()))
                            .when(!on, |el| el.border_2().border_color(p.faint)),
                    )
                })
        };
        let icon_disc = |icon: IconName| div().size(px(AVATAR)).flex_none().flex().items_center().justify_center().rounded_full().bg(p.raised).child(Icon::new(icon).size_5()).into_any_element();

        let mut rows: Vec<AnyElement> = Vec::new();
        if !new.group && group_caps.is_some() && query.is_empty() {
            rows.push(
                row("new-group".into(), t!("New group").into(), String::new(), icon_disc(IconName::Users), None)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(new) = this.people.new.as_mut() {
                            (new.group, new.error) = (true, None);
                            cx.notify();
                        }
                    }))
                    .into_any_element(),
            );
        }
        if let Some(who) = identifier {
            let title = if new.group { t!("Add {who}", who = who) } else { t!("Chat with {who}", who = who) };
            rows.push(
                row("identifier".into(), title, String::new(), icon_disc(if new.group { IconName::Plus } else { IconName::MessageCircle }), None)
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_identifier(who.clone(), cx)))
                    .into_any_element(),
            );
        }
        for (ix, person) in new.rows.iter().enumerate() {
            let check = new.group.then(|| new.picked.iter().any(|p| p.id == person.id));
            let chosen = person.clone();
            rows.push(
                row(SharedString::from(format!("person-{ix}")), person.name.clone(), person.detail.clone(), avatar(&person.name, None, AVATAR, cx).into_any_element(), check)
                    .on_click(cx.listener(move |this, _, _, cx| this.pick(chosen.clone(), cx)))
                    .into_any_element(),
            );
        }
        let note = |text: String, ink: Hsla| div().px(px(edge)).py(px(GAP)).text_size(fs(14.)).line_height(fs(20.)).text_color(ink).child(text);
        let status = if let Some(error) = &new.error {
            Some(note(error.clone(), p.negative))
        } else if new.busy {
            Some(note(if new.group { t!("Creating the group…").to_owned() } else { t!("Starting the chat…").to_owned() }, p.faint))
        } else if loading {
            Some(note(t!("Looking…").to_owned(), p.faint))
        } else if new.rows.is_empty() && !query.is_empty() && rows.is_empty() {
            Some(note(t!("No one found. Try a full phone number, username or ID.").to_owned(), p.faint))
        } else {
            None
        };
        let create = new.group.then(|| {
            let group = group_caps.clone().unwrap_or_default();
            let ready = ready_to_create(new, &group, cx);
            let need = if group.min_people > new.picked.len() {
                t!("Choose at least {n} more", n = group.min_people - new.picked.len())
            } else {
                t!("{n} chosen", n = new.picked.len())
            };
            h_flex()
                .flex_none()
                .px(px(edge))
                .py(px(8.))
                .gap(px(GAP))
                .child(div().flex_1().text_size(fs(14.)).text_color(p.faint).child(need))
                .child(
                    Button::new("create-group")
                        .primary()
                        .h(px(TAP))
                        .px(px(GUTTER + 4.))
                        .label(t!("Create group"))
                        .loading(new.busy)
                        .disabled(!ready || new.busy)
                        .on_click(cx.listener(|this, _, _, cx| this.create_group(cx))),
                )
        });
        let title = if new.group { t!("New group") } else { t!("New chat") };
        v_flex()
            .size_full()
            .child(
                header()
                    .child(round_button("new-chat-back", IconName::ArrowLeft, cx).on_click(cx.listener(|this, _, _, cx| this.back(cx))))
                    .child(screen_title(title)),
            )
            .child(v_flex().flex_none().w_full().max_w(px(SHEET_W)).gap(px(8.)).children(chips).children(name_field).child(field).children(picked))
            .children(status)
            .child(div().id("new-chat-rows").flex_1().min_h_0().overflow_y_scroll().pt(px(4.)).child(v_flex().w_full().max_w(px(SHEET_W)).children(rows)))
            .children(create)
            .into_any_element()
    }
}

fn ready_to_create(new: &NewChat, group: &crate::backend::rooms::GroupCaps, cx: &App) -> bool {
    let name = new.name.read(cx).value().trim().chars().count();
    let enough = new.picked.len() >= group.min_people.max(1) && (group.max_people == 0 || new.picked.len() <= group.max_people);
    let named = !group.name || ((!group.name_required || name > 0) && (group.name_max == 0 || name <= group.name_max));
    enough && named
}

#[cfg(test)]
mod tests {
    use super::{Confirm, looks_like_identifier, wording};
    use crate::backend::rooms::{Caps, Target};

    #[test]
    fn only_what_the_network_looks_up_is_offered_as_an_identifier() {
        let wa = Caps { phone: true, ..Default::default() };
        let bridge = Target::Bridge { bridge: "whatsapp".into(), login: "1".into() };
        assert!(looks_like_identifier("+49 151 1234567", &bridge, &wa));
        assert!(!looks_like_identifier("Ann", &bridge, &wa), "a name is searched, not looked up");
        assert!(!looks_like_identifier("12", &bridge, &wa), "too short for a number");
        let signal = Caps { phone: true, username: true, ..Default::default() };
        assert!(looks_like_identifier("ann.42", &bridge, &signal));
        assert!(looks_like_identifier("@ann:matrix.org", &Target::Matrix, &Caps::matrix()));
        assert!(!looks_like_identifier("ann", &Target::Matrix, &Caps::matrix()));
    }

    #[test]
    fn the_dialog_promises_a_remote_delete_only_when_the_bridge_offers_one() {
        let mut c = Confirm { room: "!a:b".try_into().unwrap(), name: "Ann".into(), network: Some("WhatsApp".into()), dm: true, delete: Some(true) };
        assert_eq!(wording(&c).2, "Delete");
        assert!(wording(&c).1.contains("your WhatsApp account"));
        c.delete = Some(false);
        let (title, body, act) = wording(&c);
        assert_eq!((title.as_str(), act), ("Leave Ann?", "Leave"));
        assert!(body.contains("cannot delete it"));
        c.network = None;
        assert!(!wording(&c).1.contains("WhatsApp"));
    }
}

//! Leaving a chat, and starting one: the question asked before a chat is left or deleted, and the
//! New chat screen, where a person or a group is found on Matrix or on a network the user is
//! signed in to.

use super::*;
use crate::backend::mp4::Unfit;
use crate::backend::rooms::{Answer, Ask, Caps, Person, Target};
use crate::i18n::t;

/// Rows the New chat screen draws at once; a longer contact list is narrowed by typing.
// shortcut: a capped list instead of a virtual one; use `uniform_list` if people miss contacts.
const MAX_ROWS: usize = 200;
/// People in one new group.
const MAX_PICKED: usize = 256;
/// A pause in typing this long asks the server; each keystroke does not.
const SEARCH_DELAY: Duration = Duration::from_millis(300);
pub(super) const DIALOG_W: f32 = 340.;

#[derive(Default)]
pub(super) struct People {
    pub(super) confirm: Option<Confirm>,
    new: Option<NewChat>,
}

/// What a dialog asks the user to confirm.
pub(super) struct Confirm {
    pub(super) name: String,
    pub(super) network: Option<String>,
    pub(super) what: Confirming,
}

pub(super) enum Confirming {
    /// A chat the user asked to leave; drawn once the backend says whether its bridge can delete it.
    /// `block`: block the person behind an invitation or a message request, then leave.
    Leave { room: OwnedRoomId, dm: bool, delete: Option<bool>, block: bool },
    /// Blocking a person: the Matrix ignore list, or on their chat network through `dm`, the
    /// one-to-one chat whose bridge blocks there.
    Person { user: OwnedUserId, dm: Option<OwnedRoomId> },
    /// Signing an account out of a network; `name` is the account's.
    SignOut { bridge: String, login: String },
    /// Turning a Discord server off; `name` is the server's.
    StopGuild { bridge: String, login: String, guild: String },
    /// Making an MP4 of a video that cannot go as it is (`Ev::OfferMp4`), then sending it.
    Mp4 { room: OwnedRoomId, path: PathBuf, reply_to: Option<String>, caption: String, why: Unfit },
}

impl Confirm {
    /// A leave waits to know whether the bridge can delete the chat.
    fn ready(&self) -> bool {
        !matches!(self.what, Confirming::Leave { delete: None, .. })
    }
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
    /// The user asked to join the group a typed link names (`Answer::Asked`).
    asked: bool,
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
    /// address, or a Matrix ID (shown as the ID when a link to it is pasted).
    fn identifier(&self, cx: &App) -> Option<String> {
        let q = self.query(cx);
        if self.target == Target::Matrix {
            return crate::backend::rooms::matrix_id(&q).map(|u| u.to_string());
        }
        let caps = self.caps.as_ref()?;
        looks_like_identifier(&q, caps).then_some(q)
    }

    /// What is typed is a group's link, which a new chat on Matrix joins.
    fn group_link(&self, cx: &App) -> Option<String> {
        let q = self.query(cx);
        (!self.group && self.target == Target::Matrix && crate::backend::rooms::room_link(&q).is_some()).then_some(q)
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

/// For a network's bridge; a Matrix ID is `matrix_id`'s.
fn looks_like_identifier(q: &str, caps: &Caps) -> bool {
    if q.is_empty() {
        return false;
    }
    let digits = q.chars().filter(char::is_ascii_digit).count();
    let phone = digits >= 5 && q.chars().all(|c| c.is_ascii_digit() || " +-().".contains(c));
    (caps.phone && phone) || (caps.email && q.contains('@') && !q.contains(' ')) || (caps.username && !q.contains(char::is_whitespace))
}

/// What the dialog says and its button, in whole sentences.
fn wording(c: &Confirm) -> (String, String, &'static str) {
    let network = c.network.as_deref().unwrap_or(t!("the network"));
    let (delete, dm, block) = match &c.what {
        Confirming::Leave { delete, dm, block, .. } => (*delete, *dm, *block),
        Confirming::Person { dm, .. } => {
            let body = match &c.network {
                Some(network) if dm.is_some() => t!("{network} blocks {name} for you: they can no longer write to you there. They are not told.", name = c.name, network = network),
                Some(network) => t!("You stop getting messages from {name} in Unisend. They are not blocked on {network}: they can still write to you there. They are not told.", name = c.name, network = network),
                None => t!("{name} can no longer send you messages or invitations. They are not told.", name = c.name),
            };
            return (t!("Block {name}?", name = c.name), body, t!("Block"));
        }
        Confirming::StopGuild { .. } => {
            return (
                t!("Turn off {name}?", name = c.name),
                t!("Its channels are removed from your chats in every Beeper app. Turning it on again brings them back.").to_owned(),
                t!("Turn off"),
            );
        }
        Confirming::Mp4 { why, .. } => {
            let body = match why {
                Unfit::Refused => match &c.network {
                    Some(network) => t!("{network} does not take this video's format. Unisend can make an MP4 of it on this device, without its metadata, and send that. A long video takes a while.", network = network),
                    None => t!("This chat's network does not take this video's format. Unisend can make an MP4 of it on this device, without its metadata, and send that. A long video takes a while.").to_owned(),
                },
                Unfit::Unclean => t!("Unisend cannot remove the metadata from this video's format. It can make an MP4 of it on this device, without metadata, and send that. A long video takes a while.").to_owned(),
            };
            return (t!("Convert this video to MP4?").to_owned(), body, t!("Convert"));
        }
        Confirming::SignOut { .. } => {
            return (
                t!("Sign out of {network}?", network = network),
                t!("{name} is signed out of {network} in every Beeper app. Its chats stop updating and may disappear from the list. To bring them back, you sign in and pair again.", name = c.name, network = network),
                t!("Sign out"),
            );
        }
    };
    if block {
        let body = match &c.network {
            Some(network) => t!("{network} blocks them for you and the request is deleted. They are not told.", network = network),
            None => t!("You won't see anything from them in Unisend again, and the invitation is declined. They are not told.").to_owned(),
        };
        return (t!("Block {name}?", name = c.name), body, t!("Block"));
    }
    match (c.network.is_some(), delete == Some(true), dm) {
        (true, true, true) => (
            t!("Delete {name}?", name = c.name),
            t!("The chat is deleted from Unisend and from your {network} account. The other person keeps their copy.", network = network),
            t!("Delete"),
        ),
        (true, true, false) => (
            t!("Delete {name}?", name = c.name),
            t!("You leave the group on {network}, and the chat is deleted from Unisend and from your account. The other members keep their copy.", network = network),
            t!("Delete"),
        ),
        (true, false, _) => (
            t!("Leave {name}?", name = c.name),
            t!("The chat is removed from Unisend only. The {network} bridge cannot delete it there, and new messages may bring it back.", network = network),
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

/// The marks the backend puts before a media summary ("📷 Photo"), drawn as the icon they stand for.
const MEDIA_MARKS: [(&str, IconName); 8] = [
    ("📷", IconName::Camera),
    ("🖼", IconName::Image),
    ("🎬", IconName::Video),
    ("🎤", IconName::Mic),
    ("🎵", IconName::Music),
    ("📍", IconName::MapPin),
    ("📊", IconName::ChartBar),
    ("📎", IconName::Paperclip),
];

/// A preview split around its media mark: the sender part, the icon, the words after it. The mark
/// starts the summary, which starts the preview or follows a sender's name and ": ".
fn media_mark(preview: &str) -> Option<(&str, IconName, &str)> {
    let starts = std::iter::once(0).chain(preview.match_indices(": ").map(|(at, _)| at + 2));
    for start in starts {
        let rest = &preview[start..];
        for (mark, icon) in MEDIA_MARKS {
            if let Some(tail) = rest.strip_prefix(mark) {
                let tail = tail.strip_prefix('\u{fe0f}').unwrap_or(tail);
                if let Some(words) = tail.strip_prefix(' ') {
                    return Some((&preview[..start], icon, words));
                }
            }
        }
    }
    None
}

impl BeeperApp {
    /// A chat row's second line: who is writing, or the newest message with its state and, for
    /// media, the icon of its kind.
    pub(super) fn row_preview(&self, room: &RoomSummary, preview: Option<&str>, cx: &App) -> AnyElement {
        let p = Palette::of(cx);
        if let Some(names) = self.list_typing.get(&room.id).filter(|n| !n.is_empty()) {
            let text = if room.dm {
                t!("typing…").to_owned()
            } else {
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                let (who, many) = people(&names);
                if many { t!("{who} are typing…", who = who) } else { t!("{who} is typing…", who = who) }
            };
            return div().truncate().text_color(p.muted).child(text).into_any_element();
        }
        let text = preview.unwrap_or(t!("Message"));
        let status = self.row_mark(room, cx);
        let Some((before, icon, words)) = media_mark(text) else {
            let line = crate::emoji::styled(text.to_owned(), Vec::new());
            return match status {
                Some(status) => h_flex().gap(px(4.)).child(status).child(div().flex_1().min_w_0().truncate().child(line)).into_any_element(),
                None => line.into_any_element(),
            };
        };
        h_flex()
            .gap(px(4.))
            .children(status)
            .when(!before.is_empty(), |el| el.child(div().flex_initial().min_w_0().truncate().child(crate::emoji::styled(before.trim_end().to_owned(), Vec::new()))))
            .child(Icon::new(icon).size(px(14.)).flex_none().text_color(p.faint))
            .child(div().flex_1().min_w_0().truncate().child(crate::emoji::styled(words.to_owned(), Vec::new())))
            .into_any_element()
    }

    /// The last row of a chat's sheet.
    pub(super) fn leave_row(&self, room: OwnedRoomId, live: bool, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = Palette::of(cx);
        // Splitting a merged chat leaves its room; its chats come back as they were, so no question.
        if self.merged.folded.is_union(&room) {
            let row = sheet_row("chat-split", IconName::Unlink, t!("Split chats"), p.text, window, cx);
            if !live {
                return row;
            }
            return row.on_click(cx.listener(move |this, _, _, cx| {
                this.chat_sheet = None;
                this.send(Cmd::People(Ask::Leave { room: room.clone(), delete: false }), cx);
            }));
        }
        let bridged = self.rooms.iter().any(|r| r.id == room && r.network.is_some());
        let row = if bridged {
            sheet_row("chat-delete", IconName::Trash, t!("Delete chat"), p.negative, window, cx)
        } else {
            sheet_row("chat-leave", IconName::LogOut, t!("Leave chat"), p.negative, window, cx)
        };
        if !live {
            return row;
        }
        row.on_click(cx.listener(move |this, _, _, cx| {
            this.chat_sheet = None;
            this.ask_leave(room.clone(), cx);
        }))
    }

    pub(super) fn ask_leave(&mut self, room: OwnedRoomId, cx: &mut Context<Self>) {
        let Some(r) = self.rooms.iter().find(|r| r.id == room) else { return };
        let local = r.network.is_none();
        let confirm = Confirm { name: r.name.clone(), network: r.network.clone(), what: Confirming::Leave { room: room.clone(), dm: r.dm, delete: local.then_some(false), block: false } };
        if !local && !self.send(Cmd::People(Ask::LeaveOptions(room)), cx) {
            return;
        }
        self.people.confirm = Some(confirm);
        cx.notify();
    }

    /// Asks before blocking whoever is behind a pending chat; the chat is left with it.
    pub(super) fn ask_block(&mut self, room: OwnedRoomId, cx: &mut Context<Self>) {
        let Some(r) = self.rooms.iter().find(|r| r.id == room) else { return };
        let name = match &r.pending {
            Some(Pending::Invite { name, .. }) if !name.is_empty() => name.clone(),
            _ => r.name.clone(),
        };
        self.people.confirm = Some(Confirm { name, network: r.network.clone(), what: Confirming::Leave { room, dm: r.dm, delete: Some(false), block: true } });
        cx.notify();
    }

    /// Asks before a person is put on the ignore list.
    pub(super) fn ask_block_person(&mut self, user: OwnedUserId, name: String, network: Option<String>, dm: Option<OwnedRoomId>, cx: &mut Context<Self>) {
        self.people.confirm = Some(Confirm { name, network, what: Confirming::Person { user, dm } });
        cx.notify();
    }

    /// Turns an invitation down at once: nothing is lost that cannot come back with a new one.
    pub(super) fn decline(&mut self, room: OwnedRoomId, cx: &mut Context<Self>) {
        let Some(r) = self.rooms.iter().find(|r| r.id == room) else { return };
        self.people.confirm = Some(Confirm { name: r.name.clone(), network: None, what: Confirming::Leave { room, dm: r.dm, delete: Some(false), block: false } });
        self.confirmed(cx);
    }

    /// The leave or delete dialog is asking about this chat, or waiting to.
    /// A dialog asks something; the keyboard is its.
    pub(super) fn confirm_open(&self) -> bool {
        self.people.confirm.is_some()
    }

    pub(super) fn confirming(&self, room: &OwnedRoomId) -> bool {
        self.people.confirm.as_ref().is_some_and(|c| matches!(&c.what, Confirming::Leave { room: r, .. } if r == room))
    }

    /// Escape closes the dialog first.
    pub(super) fn close_confirm(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.people.confirm.take().is_some();
        if open {
            cx.notify();
        }
        open
    }

    /// Asks before a Discord server is turned off: its chats go with it.
    pub(super) fn ask_stop_guild(&mut self, bridge: String, login: String, guild: String, name: String, cx: &mut Context<Self>) {
        self.people.confirm = Some(Confirm { name, network: Some("Discord".into()), what: Confirming::StopGuild { bridge, login, guild } });
        cx.notify();
    }

    /// Asks before an account is signed out of a network.
    pub(super) fn ask_sign_out(&mut self, bridge: String, login: String, name: String, network: String, cx: &mut Context<Self>) {
        self.people.confirm = Some(Confirm { name, network: Some(network), what: Confirming::SignOut { bridge, login } });
        cx.notify();
    }

    /// Asks before a video that cannot go as it is becomes an MP4.
    pub(super) fn ask_mp4(&mut self, room: OwnedRoomId, path: PathBuf, reply_to: Option<String>, caption: String, why: Unfit, cx: &mut Context<Self>) {
        let network = self.rooms.iter().find(|r| r.id == room).and_then(|r| r.network.clone());
        // shortcut: a refused offer leaves a pasted or edited source in `pasted/` until sign-out; delete it on Cancel if that grows.
        self.people.confirm = Some(Confirm { name: String::new(), network, what: Confirming::Mp4 { room, path, reply_to, caption, why } });
        cx.notify();
    }

    fn confirmed(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.people.confirm.take() else { return };
        let (room, delete, block) = match c.what {
            Confirming::Mp4 { room, path, reply_to, caption, .. } => {
                self.uploads += 1;
                self.attach_queue.push_back(Cmd::Mp4 { room, path, reply_to, caption });
                self.next_attach(cx);
                return cx.notify();
            }
            Confirming::Leave { room, delete, block, .. } => (room, delete == Some(true), block),
            Confirming::Person { dm: Some(room), .. } => {
                self.block_on_network(room, true, cx);
                return;
            }
            Confirming::Person { user, dm: None } => {
                self.send(Cmd::SetBlocked(user, true), cx);
                return;
            }
            Confirming::StopGuild { bridge, login, guild } => {
                self.set_guild(bridge, login, guild, false, cx);
                return;
            }
            Confirming::SignOut { bridge, login } => {
                self.working = self.send(Cmd::Logout { bridge, login }, cx);
                self.account = None;
                self.panel = Some(Panel::Networks);
                cx.notify();
                return;
            }
        };
        let ask = if block { Ask::Block(room.clone()) } else { Ask::Leave { room: room.clone(), delete } };
        if !self.send(Cmd::People(ask), cx) {
            return;
        }
        // It leaves the list at once; the backend puts it back if leaving fails.
        self.rooms.retain(|r| r.id != room);
        self.refilter(cx);
        if self.open.as_ref() == Some(&room) {
            self.close_room(cx);
        }
    }

    pub(super) fn render_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let live = self.people.confirm.as_ref().is_some_and(Confirm::ready);
        let shown = Presence::new("leave-dialog", live).transition(if live { motion::enter() } else { motion::exit() }).sample(window, cx);
        // The words outlive the confirmation so the dialog can fade out showing what it asked.
        let last = window.use_keyed_state("leave-dialog-last", cx, |_, _| None::<((String, String, &'static str), bool)>);
        if let Some(c) = self.people.confirm.as_ref().filter(|c| c.ready()) {
            // Converting loses nothing: the primary button, not the red one.
            let words = (wording(c), matches!(c.what, Confirming::Mp4 { .. }));
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
        let ((title, body, act), calm) = last.read(cx).clone()?;
        let v = shown.progress;
        let p = Palette::of(cx);
        let width = DIALOG_W.min(f32::from(frame::viewport(window).width) - 2. * GUTTER);
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
                    .child(button("leave-confirm", act, if calm { p.text } else { p.negative }, if calm { p.ground } else { gpui_kit::white() }).when(live, |el| el.on_click(cx.listener(|this, _, _, cx| this.confirmed(cx))))),
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
        crate::ime::follow(&query, window, cx);
        crate::ime::follow(&name, window, cx);
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
            asked: false,
            pending: None,
            clear: false,
        });
        self.choose_target(Target::Matrix, window, cx);
        self.open_panel(Panel::NewChat, cx);
        if let Some(new) = &self.people.new {
            new.query.update(cx, |input, cx| input.focus(window, cx));
        }
    }

    /// A Matrix ID or room alias tapped in a message: New chat opens with it typed, so its row
    /// starts the chat or joins the room only on a second tap.
    pub(super) fn offer_chat(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_new_chat(window, cx);
        // New chat reads a room by its link, not by its bare alias.
        let typed = if id.starts_with('#') { format!("https://matrix.to/#/{id}") } else { id.to_owned() };
        if let Some(new) = &self.people.new {
            new.query.update(cx, |input, cx| input.set_value(typed, window, cx));
        }
        self.query_changed(cx);
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
        (new.error, new.asked) = (None, false);
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
        if let Some(link) = new.group_link(cx) {
            self.ask_new(Ask::JoinLink(link), cx);
        } else if let Some(who) = new.identifier(cx) {
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
                if let Some(Confirm { what: Confirming::Leave { room: asked, delete: answer, .. }, .. }) = self.people.confirm.as_mut()
                    && *asked == room
                {
                    *answer = Some(delete);
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
            Answer::Accepted(room) => {
                let Some(r) = self.rooms.iter_mut().find(|r| r.id == room) else { return };
                let invited = matches!(r.pending, Some(Pending::Invite { .. }));
                r.pending = None;
                self.refilter(cx);
                // A joined room has history to show now.
                if invited && self.open.as_ref() == Some(&room) {
                    self.close_room(cx);
                    self.open_room(room, cx);
                }
            }
            Answer::Ready(room) => {
                self.people.new = None;
                self.panel = None;
                self.open_room(room, cx);
            }
            Answer::Asked => {
                if let Some(new) = self.people.new.as_mut() {
                    (new.busy, new.asked) = (false, true);
                }
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
        let link = new.group_link(cx);
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
            .child(div().flex_1().min_w_0().child(Input::new(&new.query).appearance(false)).child(crate::ime::marks(&new.query)));

        let name_field = (new.group && group_caps.as_ref().is_some_and(|g| g.name)).then(|| {
            h_flex().flex_none().mx(px(edge)).h(px(TAP)).px(px(GAP)).rounded_full().bg(p.surface).child(div().flex_1().min_w_0().child(Input::new(&new.name).appearance(false)).child(crate::ime::marks(&new.name)))
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
        let title = t!("Note to self");
        if !new.group && new.target == Target::Matrix && title.to_lowercase().contains(&query.to_lowercase()) {
            let face = self.rooms.iter().find(|r| r.note).map(|r| room_avatar(r, &self.avatars, AVATAR, cx));
            rows.push(
                row("note-to-self".into(), title.into(), String::new(), face.map_or_else(|| icon_disc(IconName::NotebookPen), |f| f.into_any_element()), None)
                    .on_click(cx.listener(|this, _, _, cx| this.ask_new(Ask::Note, cx)))
                    .into_any_element(),
            );
        }
        if let Some(who) = identifier {
            let what = if new.group { t!("Add to group") } else { t!("New chat") };
            rows.push(
                row("identifier".into(), who.clone(), what.to_owned(), icon_disc(if new.group { IconName::Plus } else { IconName::MessageCircle }), None)
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_identifier(who.clone(), cx)))
                    .into_any_element(),
            );
        }
        if let Some(link) = link {
            rows.push(
                row("group-link".into(), t!("Join group").into(), link.clone(), icon_disc(IconName::Link), None)
                    .on_click(cx.listener(move |this, _, _, cx| this.ask_new(Ask::JoinLink(link.clone()), cx)))
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
        } else if new.asked {
            Some(note(t!("You asked to join. Once a member lets you in, the group comes as an invitation.").to_owned(), p.faint))
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
    use super::{Confirm, Confirming, looks_like_identifier, media_mark, wording};
    use crate::backend::rooms::Caps;

    #[test]
    fn only_what_the_network_looks_up_is_offered_as_an_identifier() {
        let wa = Caps { phone: true, ..Default::default() };
        assert!(looks_like_identifier("+49 151 1234567", &wa));
        assert!(!looks_like_identifier("Ann", &wa), "a name is searched, not looked up");
        assert!(!looks_like_identifier("12", &wa), "too short for a number");
        let signal = Caps { phone: true, username: true, ..Default::default() };
        assert!(looks_like_identifier("ann.42", &signal));
    }

    #[test]
    fn the_dialog_promises_a_remote_delete_only_when_the_bridge_offers_one() {
        let leave = |delete, block| Confirming::Leave { room: "!a:b".try_into().unwrap(), dm: true, delete: Some(delete), block };
        let mut c = Confirm { name: "Ann".into(), network: Some("WhatsApp".into()), what: leave(true, false) };
        assert_eq!(wording(&c).2, "Delete");
        assert!(wording(&c).1.contains("your WhatsApp account"));
        c.what = leave(false, false);
        let (title, body, act) = wording(&c);
        assert_eq!((title.as_str(), act), ("Leave Ann?", "Leave"));
        assert!(body.contains("cannot delete it"));
        c.network = None;
        assert!(!wording(&c).1.contains("WhatsApp"));
        c.what = leave(false, true);
        let (title, body, act) = wording(&c);
        assert_eq!((title.as_str(), act), ("Block Ann?", "Block"));
        assert!(body.contains("invitation is declined"));
        c.network = Some("Instagram".into());
        assert!(wording(&c).1.starts_with("Instagram blocks them"));
    }

    #[test]
    fn blocking_a_person_says_where_it_holds() {
        let user: matrix_sdk::ruma::OwnedUserId = "@ann:x".try_into().unwrap();
        let mut c = Confirm { name: "Ann".into(), network: Some("Signal".into()), what: Confirming::Person { user: user.clone(), dm: None } };
        assert!(wording(&c).1.contains("not blocked on Signal"));
        c.what = Confirming::Person { user, dm: Some("!a:b".try_into().unwrap()) };
        let (title, body, act) = wording(&c);
        assert_eq!((title.as_str(), act), ("Block Ann?", "Block"));
        assert!(body.starts_with("Signal blocks Ann for you"));
    }

    #[test]
    fn signing_out_of_a_network_says_what_is_lost() {
        let c = Confirm { name: "+48 600".into(), network: Some("WhatsApp".into()), what: Confirming::SignOut { bridge: "whatsapp".into(), login: "1".into() } };
        let (title, body, act) = wording(&c);
        assert_eq!((title.as_str(), act), ("Sign out of WhatsApp?", "Sign out"));
        assert!(body.contains("pair again"));
    }

    #[test]
    fn media_marks_become_icons() {
        let split = |p| media_mark(p).map(|(before, _, words)| (before, words));
        assert_eq!(split("Ty: 📷 Zdjęcie: nad jeziorem"), Some(("Ty: ", "Zdjęcie: nad jeziorem")));
        assert_eq!(split("📎 notes.pdf"), Some(("", "notes.pdf")));
        assert_eq!(split("Re: work: 🎬 Film"), Some(("Re: work: ", "Film")), "a name may hold \": \"");
        assert_eq!(split("Ania: and 📷 later"), None, "a mark inside someone's text stays");
        assert_eq!(split("Ola: Cześć"), None);
    }
}

//! The accounts signed in side by side, as Telegram keeps them: each has its own backend, which
//! syncs and notifies whether or not the window shows it. Turning the window to another account
//! builds the screen afresh for it; what the switcher shows of the others is kept here, and an
//! account the window does not show never repaints it.

use super::panels::card;
use super::*;
use crate::accounts::MAX;
use crate::backend::Backend;

/// A signed-in account, or one being signed in.
pub(crate) struct Account {
    slot: usize,
    cmd: mpsc::Sender<Cmd>,
    /// Until `listen` takes it.
    events: Option<mpsc::Receiver<Ev>>,
    thread: Option<std::thread::JoinHandle<()>>,
    _listen: Option<Task<()>>,
    /// What the backend said last of the sign-in, replayed when the window turns to the account.
    user: Option<OwnedUserId>,
    beeper: bool,
    seal: crate::vault::Seal,
    need_login: bool,
    need_recovery: Option<bool>,
    offline: bool,
    synced: bool,
    sync_stopped: bool,
    attention: Vec<(String, String)>,
    /// The switcher's row: the user's name and picture, while another account is shown.
    name: String,
    picture: Option<Arc<Image>>,
    /// Chats waiting to be read, while another account is shown; one entry per such chat.
    unread: HashSet<OwnedRoomId>,
    /// Being signed in from "Add account": leaving the sign-in removes it.
    adding: bool,
    /// The sign-in that finished was an account already here, so it is signed out again.
    duplicate: bool,
}

impl Account {
    fn new(slot: usize, backend: Backend, adding: bool) -> Self {
        Self {
            slot,
            cmd: backend.cmd,
            events: Some(backend.events),
            thread: Some(backend.thread),
            _listen: None,
            user: None,
            beeper: true,
            seal: crate::vault::Seal::Plain,
            need_login: false,
            need_recovery: None,
            offline: false,
            synced: false,
            sync_stopped: false,
            attention: Vec::new(),
            name: String::new(),
            picture: None,
            unread: HashSet::new(),
            adding,
            duplicate: false,
        }
    }

    fn label(&self) -> String {
        match &self.user {
            Some(user) if self.name.is_empty() => user.localpart().to_owned(),
            _ => self.name.clone(),
        }
    }
}

/// Every account's backend and the one the window shows.
#[derive(Default)]
pub(crate) struct Accounts {
    list: Vec<Account>,
    active: usize,
    /// Where leaving an added account's sign-in goes back to.
    back_to: Option<usize>,
    /// The switcher's list is open in Settings.
    pub(super) open: bool,
    /// A switch asked for where no window was at hand (an account signed out, a notification's
    /// chat): made with the next frame, with the chat to open.
    pending: Option<(usize, Option<OwnedRoomId>)>,
    /// Said once the window has turned to the next account.
    notice: Option<SharedString>,
    /// A notification's chat in the account shown, opened once its backend is ready.
    opening: Option<OwnedRoomId>,
}

impl Accounts {
    /// The backends started for the accounts on disk; `preferred` is shown when it is signed in,
    /// else the first that is.
    pub(crate) fn new(backends: Vec<(usize, Backend)>, preferred: usize, base: &std::path::Path) -> Self {
        let signed_in = |slot: usize| crate::vault::signed_in(&crate::accounts::dir(base, slot));
        let slots: Vec<usize> = backends.iter().map(|(slot, _)| *slot).collect();
        let active = Some(preferred).filter(|s| slots.contains(s) && signed_in(*s)).or_else(|| slots.iter().copied().find(|&s| signed_in(s))).unwrap_or(0);
        crate::accounts::set_active(active);
        let list = backends.into_iter().map(|(slot, backend)| Account::new(slot, backend, false)).collect();
        Self { list, active, ..Default::default() }
    }

    pub(super) fn active(&self) -> usize {
        self.active
    }

    fn get(&self, slot: usize) -> Option<&Account> {
        self.list.iter().find(|a| a.slot == slot)
    }

    fn get_mut(&mut self, slot: usize) -> Option<&mut Account> {
        self.list.iter_mut().find(|a| a.slot == slot)
    }

    pub(super) fn cmd(&self) -> mpsc::Sender<Cmd> {
        self.get(self.active).map(|a| a.cmd.clone()).unwrap_or_else(|| mpsc::channel(1).0)
    }

    /// The signed-in accounts other than `slot`.
    fn others(&self, slot: usize) -> impl Iterator<Item = &Account> {
        self.list.iter().filter(move |a| a.slot != slot && a.user.is_some() && !a.duplicate)
    }

    /// Another account stays signed in when the one shown signs out.
    pub(super) fn others_signed_in(&self) -> bool {
        self.others(self.active).next().is_some()
    }

    fn signed_in(&self) -> usize {
        self.list.iter().filter(|a| a.user.is_some() && !a.duplicate).count()
    }

    /// Sends to every account; for settings that hold for all of them.
    pub(super) fn tell_all(&self, cmd: impl Fn() -> Cmd) {
        for a in &self.list {
            let _ = a.cmd.try_send(cmd());
        }
    }

    /// Starts reading the events of the accounts not read yet. Not tied to a window: a desktop
    /// that closed its window and runs on keeps every account's events flowing.
    pub(super) fn listen(&mut self, cx: &mut Context<BeeperApp>) {
        for a in &mut self.list {
            let (Some(mut events), slot) = (a.events.take(), a.slot) else { continue };
            a._listen = Some(cx.spawn(async move |this, cx| {
                while let Some(ev) = events.recv().await {
                    if this.update(cx, |this, cx| this.account_event(slot, ev, cx)).is_err() {
                        break;
                    }
                }
            }));
        }
    }
}

/// A chat that counts as waiting in the chat list's header.
pub(super) fn waiting(r: &RoomSummary) -> bool {
    !r.archived && !r.low_priority && r.is_unread()
}

impl BeeperApp {
    /// The images the screen holds, for their atlas tiles to be freed as it goes.
    // shortcut: the wallpaper, gallery, GIF and caption images go with their decoded copy only; their
    // tiles stay until the window closes, a few per switch. Free them here if switching gets frequent.
    fn let_go(&mut self) -> Vec<Arc<Image>> {
        let mut out: Vec<Arc<Image>> = self.thumbs.drain().map(|(_, i)| i).collect();
        out.extend(self.avatars.drain().filter_map(|(_, i)| i));
        out.extend(self.faces.drain().filter_map(|(_, i)| i));
        out.extend([self.viewer.take(), self.viewer_last.take()].into_iter().flatten().filter_map(|(_, i)| i));
        out.extend(self.qr.take());
        out.extend(self.profile.take().1);
        out
    }

    /// The data directory of the account shown.
    pub(super) fn account_dir(&self) -> std::path::PathBuf {
        crate::accounts::dir(&self.dir, self.accounts.active)
    }

    /// What a notification or share target names the chat by.
    pub(super) fn chat_key(&self, room: &matrix_sdk::ruma::RoomId) -> String {
        crate::accounts::key(self.accounts.active, room.as_str())
    }

    fn account_event(&mut self, slot: usize, ev: Ev, cx: &mut Context<Self>) {
        let shown = slot == self.accounts.active;
        let incognito = self.settings.incognito;
        let duplicate = match &ev {
            Ev::Ready { user, .. } => self.accounts.get(slot).is_some_and(|a| a.adding) && self.accounts.others(slot).any(|a| a.user.as_ref() == Some(user)),
            _ => false,
        };
        let Some(a) = self.accounts.get_mut(slot) else { return };
        let mut changed = false;
        match &ev {
            Ev::Ready { .. } if duplicate => {
                // The new session goes; the account keeps the one it had here.
                a.duplicate = true;
                let _ = a.cmd.try_send(Cmd::SignOut);
                return;
            }
            Ev::Ready { user, beeper, seal } => {
                (a.user, a.beeper, a.seal, a.need_login, a.sync_stopped) = (Some(user.clone()), *beeper, seal.clone(), false, false);
                if std::mem::take(&mut a.adding) {
                    self.accounts.back_to = None;
                }
                let a = self.accounts.get_mut(slot).expect("found above");
                let _ = a.cmd.try_send(Cmd::Incognito(incognito));
                if !shown {
                    let _ = a.cmd.try_send(Cmd::Profile);
                }
                if shown && self.settings.account != slot {
                    self.settings.account = slot;
                    self.settings.save(&self.dir);
                }
            }
            Ev::NeedLogin => {
                let was = a.user.take().is_some() || a.duplicate;
                (a.need_login, a.name, a.unread) = (true, String::new(), HashSet::new());
                if let Some(old) = a.picture.take() {
                    self.released.push(old, cx);
                }
                if was && (self.accounts.others(slot).next().is_some() || slot != 0) {
                    self.drop_account(slot, cx);
                    return;
                }
            }
            Ev::NeedRecovery(need) => a.need_recovery = Some(*need),
            Ev::Online(online) => a.offline = !online,
            Ev::Synced => a.synced = true,
            Ev::SyncStopped => a.sync_stopped = true,
            Ev::Attention(logins) => a.attention = logins.clone(),
            Ev::Profile(profile) if !shown => {
                a.name = profile.name.clone();
                let picture = profile.picture.clone().and_then(|bytes| Some(Arc::new(Image::from_bytes(image_format(&bytes)?, bytes))));
                if let Some(old) = std::mem::replace(&mut a.picture, picture) {
                    self.released.push(old, cx);
                }
                changed = true;
            }
            Ev::Rooms(rooms) if !shown => {
                let before = a.unread.len();
                for r in rooms {
                    if waiting(r) {
                        // Bounded by the account's chats.
                        a.unread.insert(r.id.clone());
                    } else {
                        a.unread.remove(&r.id);
                    }
                }
                changed = a.unread.len() != before;
            }
            Ev::RoomGone(id) if !shown => changed = a.unread.remove(id),
            Ev::Error(e) if !shown => log::warn!("account {slot}: {}", crate::backend::plain(e)),
            _ => {}
        }
        if shown {
            let ready = matches!(ev, Ev::Ready { .. });
            self.on_event(ev, cx);
            if ready && let Some(id) = self.accounts.opening.take() {
                self.show_chat(id, cx);
            }
        } else if changed && self.accounts.open && self.panel == Some(Panel::Settings) {
            cx.notify();
        }
    }

    /// The account in `slot` signed out (here, by the server, or as a duplicate) while another stays:
    /// an added one ends and its folder goes once its backend has stopped; the first one's backend
    /// runs on, signed out, as it holds the device's settings. The window turns to another account.
    fn drop_account(&mut self, slot: usize, cx: &mut Context<Self>) {
        crate::clear_account_notifications(slot);
        if self.settings.mute_ends_of(slot).is_some_and(|ends| !ends.is_empty()) {
            self.settings.mute_ends(slot).clear();
            self.settings.mute_until_added.remove(&slot);
            self.settings.save(&self.dir);
        }
        let duplicate = self.accounts.get(slot).is_some_and(|a| a.duplicate);
        if slot != 0
            && let Some(ix) = self.accounts.list.iter().position(|a| a.slot == slot)
        {
            let mut gone = self.accounts.list.remove(ix);
            crate::backend::forget(slot);
            if let Some(old) = gone.picture.take() {
                self.released.push(old, cx);
            }
            let (thread, dir) = (gone.thread.take(), crate::accounts::dir(&self.dir, slot));
            drop(gone);
            if slot == self.accounts.active {
                // The screen's own sender, the last one, goes now rather than with the screen.
                self.cmd = mpsc::channel(1).0;
            }
            // Joined off the UI thread: the backend's runtime stops its tasks and threads first.
            let ended = std::thread::Builder::new().name("account-end".into()).spawn(move || {
                if let Some(thread) = thread {
                    let _ = thread.join();
                }
                if let Err(e) = std::fs::remove_dir_all(&dir) {
                    log::warn!("signed-out account's folder: {e}");
                }
            });
            if let Err(e) = ended {
                log::warn!("account end: {e}");
            }
        } else if let Some(a) = self.accounts.get_mut(slot) {
            (a.adding, a.duplicate) = (false, false);
        }
        if duplicate {
            self.accounts.notice = Some(t!("That account is already signed in here.").into());
        }
        if slot == self.accounts.active {
            // With none left signed in, the first account's sign-in screen.
            let next = self.accounts.back_to.take().filter(|&s| self.accounts.get(s).is_some_and(|a| a.user.is_some())).or_else(|| self.accounts.others(slot).next().map(|a| a.slot));
            if let Some(next) = next.or((slot != 0).then_some(0)) {
                self.accounts.pending = Some((next, None));
            }
        }
        cx.notify();
    }

    /// Makes a switch asked for without a window at hand; called first in each frame.
    pub(super) fn switch_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((slot, open)) = self.accounts.pending.take() {
            self.switch_account(slot, open, window, cx);
        }
    }

    /// Turns the window to the account in `slot`, opening `open` there.
    pub(super) fn switch_account(&mut self, slot: usize, open: Option<OwnedRoomId>, window: &mut Window, cx: &mut Context<Self>) {
        if self.accounts.get(slot).is_none() {
            return;
        }
        if slot == self.accounts.active {
            match open {
                Some(id) if self.screen == Screen::Main => self.show_chat(id, cx),
                open => self.accounts.opening = open.or(self.accounts.opening.take()),
            }
            return;
        }
        // The account left keeps what its row in the switcher shows; its open chat stops being followed.
        self.drafts.save();
        if self.open.is_some() {
            let _ = self.cmd.try_send(Cmd::Close);
        }
        let unread: HashSet<OwnedRoomId> = self.rooms.iter().filter(|r| waiting(r)).map(|r| r.id.clone()).collect();
        let (name, picture) = self.profile.take();
        let leaving = self.accounts.active;
        if let Some(a) = self.accounts.get_mut(leaving) {
            (a.unread, a.name) = (unread, name);
            if let Some(old) = std::mem::replace(&mut a.picture, picture) {
                self.released.push(old, cx);
            }
        }
        let mut accounts = std::mem::take(&mut self.accounts);
        accounts.active = slot;
        (accounts.open, accounts.opening) = (false, None);
        crate::accounts::set_active(slot);
        let (name, picture) = accounts.get_mut(slot).map(|a| (std::mem::take(&mut a.name), a.picture.take())).unwrap_or_default();
        if let Some(a) = accounts.get_mut(slot) {
            a.unread.clear();
        }
        let notice = accounts.notice.take();
        let fresh = Self::new(accounts, self.dir.clone(), self.settings.clone(), window, cx);
        let mut old = std::mem::replace(self, fresh);
        // What the old screen held is freed with its atlas tiles, as `Released` does.
        let gone = old.let_go();
        old.released.0.extend(gone);
        old.released.free(window, cx);
        drop(old);
        self.profile.show(name, picture);
        // The old screen's focus went with it; keys reach the app again through the new one's.
        window.focus(&self.focus, cx);
        self.replay(open, cx);
        self.error = notice;
        cx.notify();
    }

    /// Feeds the fresh screen what the account's backend said before, then its chat list.
    fn replay(&mut self, open: Option<OwnedRoomId>, cx: &mut Context<Self>) {
        let Some(a) = self.accounts.get(self.accounts.active) else { return };
        let ready = a.user.clone().map(|user| Ev::Ready { user, beeper: a.beeper, seal: a.seal.clone() });
        let (need_login, need_recovery, offline, synced, stopped, attention) = (a.need_login, a.need_recovery, a.offline, a.synced, a.sync_stopped, a.attention.clone());
        match ready {
            Some(ready) => {
                if self.settings.account != self.accounts.active {
                    self.settings.account = self.accounts.active;
                    self.settings.save(&self.dir);
                }
                self.on_event(ready, cx);
                if let Some(need) = need_recovery {
                    self.on_event(Ev::NeedRecovery(need), cx);
                }
                if offline {
                    self.on_event(Ev::Online(false), cx);
                }
                if synced {
                    self.on_event(Ev::Synced, cx);
                }
                if stopped {
                    self.on_event(Ev::SyncStopped, cx);
                }
                self.on_event(Ev::Attention(attention), cx);
                let _ = self.cmd.try_send(Cmd::Replay);
                if let Some(id) = open {
                    self.show_chat(id, cx);
                }
            }
            None if need_login => self.on_event(Ev::NeedLogin, cx),
            // The backend has not answered yet; its first events come by themselves.
            None => self.accounts.opening = open,
        }
    }

    /// A tapped notification's chat, in whichever account it came to.
    pub(super) fn open_account_chat(&mut self, slot: usize, id: OwnedRoomId, cx: &mut Context<Self>) {
        if slot == self.accounts.active && self.screen == Screen::Main {
            self.show_chat(id, cx);
        } else if self.accounts.get(slot).is_some() {
            // Made with the next frame, which has the window at hand.
            self.accounts.pending = Some((slot, Some(id)));
            cx.notify();
        }
    }

    fn show_chat(&mut self, id: OwnedRoomId, cx: &mut Context<Self>) {
        self.panel = None;
        self.chat_sheet = None;
        self.slide.jump = true;
        self.open_room(id, cx);
        cx.notify();
    }

    /// Starts signing in one more account, in a slot of its own; leaving the sign-in comes back.
    fn add_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.accounts.signed_in() >= MAX {
            return;
        }
        let first_free = self.accounts.get(0).is_some_and(|a| a.user.is_none()) && self.accounts.active != 0;
        // A slot whose folder is still there belongs to an account still signing out.
        let slot = if first_free { Some(0) } else { (1..MAX).find(|&s| self.accounts.get(s).is_none() && !crate::accounts::dir(&self.dir, s).exists()) };
        let Some(slot) = slot else {
            self.error = Some(t!("An account is still signing out. Try again in a moment.").into());
            cx.notify();
            return;
        };
        if slot != 0 {
            let dir = crate::accounts::dir(&self.dir, slot);
            if let Err(e) = crate::private_dir(&dir) {
                self.error = Some(e.to_string().into());
                cx.notify();
                return;
            }
            self.accounts.list.push(Account::new(slot, crate::backend::start(dir), true));
            self.accounts.list.sort_by_key(|a| a.slot);
            self.accounts.listen(cx);
        } else if let Some(a) = self.accounts.get_mut(0) {
            a.adding = true;
        }
        self.accounts.back_to = Some(self.accounts.active);
        self.switch_account(slot, None, window, cx);
    }

    /// The sign-in screen's way back to the accounts signed in: the one it was opened from, or any.
    /// False when there is none to go to.
    pub(super) fn leave_sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let slot = self.accounts.active;
        let back = self.accounts.back_to.take().filter(|&s| self.accounts.get(s).is_some_and(|a| a.user.is_some())).or_else(|| self.accounts.others(slot).next().map(|a| a.slot));
        let Some(back) = back else { return false };
        if self.matrix.waiting.take().is_some() {
            let _ = self.cmd.try_send(Cmd::BrowserCancel);
        }
        if slot != 0 && self.accounts.get(slot).is_some_and(|a| a.user.is_none()) {
            self.accounts.back_to = Some(back);
            self.drop_account(slot, cx);
        } else if let Some(a) = self.accounts.get_mut(slot) {
            a.adding = false;
        }
        self.accounts.pending = None;
        self.switch_account(back, None, window, cx);
        true
    }

    /// Under a sign-in screen, when it was reached with other accounts signed in.
    pub(super) fn back_to_accounts(&self, cx: &mut Context<Self>) -> Option<Button> {
        let slot = self.accounts.active;
        let back = self.accounts.back_to.and_then(|s| self.accounts.get(s)).filter(|a| a.user.is_some()).or_else(|| self.accounts.others(slot).next())?;
        let label = t!("Back to {account}", account = back.label());
        Some(Button::new("sign-in-leave").ghost().label(label).on_click(cx.listener(|this, _, window, cx| {
            this.leave_sign_in(window, cx);
        })))
    }

    /// The account switcher, under the profile card: every account with its unread chats, and
    /// adding one.
    pub(super) fn render_accounts(&mut self, cx: &mut Context<Self>) -> Div {
        let p = Palette::of(cx);
        let active = self.accounts.active;
        let mut rows = v_flex();
        for a in self.accounts.list.iter().filter(|a| a.user.is_some() && !a.duplicate) {
            let current = a.slot == active;
            let (name, picture, unread) = if current {
                (self.profile_name(), self.profile.picture(), self.rooms.iter().filter(|r| waiting(r)).count())
            } else {
                (a.label(), a.picture.clone(), a.unread.len())
            };
            let user = a.user.as_ref().map(ToString::to_string).unwrap_or_default();
            let slot = a.slot;
            let label = if current { t!("{name}, shown now", name = name) } else { tn!(unread, "{name}, {n} unread chat", "{name}, {n} unread chats", name = name) };
            rows = rows.child(
                h_flex()
                    .id(("account", slot))
                    .min_h(px(ROW_H))
                    .py(px(8.))
                    .mx(px(-8.))
                    .px(px(8.))
                    .gap(px(GAP))
                    .rounded(px(12.))
                    .role(Role::Button)
                    .aria_label(label)
                    .when(!current, |el| el.cursor_pointer().when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised))).active(|el| el.bg(p.raised)))
                    .child(avatar(&name, Some(&picture), ACCOUNT_FACE, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(fs(16.)).line_height(fs(22.)).truncate().child(name))
                            .child(div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.muted).truncate().child(user)),
                    )
                    .when(current, |el| el.child(Icon::new(IconName::Check).size_5().flex_none().text_color(p.text)))
                    .when(!current && unread > 0, |el| el.child(count_pill(unread as u64, false, cx)))
                    .on_click(cx.listener(move |this, _, window, cx| this.switch_account(slot, None, window, cx))),
            );
        }
        let full = self.accounts.signed_in() >= MAX;
        let limit_text = tn!(MAX, "Up to {n} account can be signed in at once.", "Up to {n} accounts can be signed in at once.");
        let limit = limit_text.clone();
        let add = h_flex()
            .id("account-add")
            .min_h(px(ROW_H))
            .py(px(8.))
            .mx(px(-8.))
            .px(px(8.))
            .gap(px(GAP))
            .rounded(px(12.))
            .role(Role::Button)
            .aria_label(if full { limit } else { t!("Add account").to_owned() })
            .child(div().size(px(ACCOUNT_FACE)).flex_none().flex().items_center().justify_center().child(Icon::new(IconName::UserPlus).size_5().text_color(if full { p.faint } else { p.brand })))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_size(fs(16.)).line_height(fs(22.)).text_color(if full { p.faint } else { p.text }).child(t!("Add account")))
                    .when(full, |el| el.child(div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.muted).child(limit_text))),
            )
            .when(!full, |el| {
                el.cursor_pointer()
                    .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised)))
                    .active(|el| el.bg(p.raised))
                    .on_click(cx.listener(|this, _, window, cx| this.add_account(window, cx)))
            });
        card(cx).child(rows).child(add)
    }

    /// The profile card's button that opens and closes the switcher, with the other accounts'
    /// unread chats on it while it is closed.
    pub(super) fn accounts_toggle(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = Palette::of(cx);
        let open = self.accounts.open;
        let unread: usize = self.accounts.others(self.accounts.active).map(|a| a.unread.len()).sum();
        h_flex()
            .id("accounts-toggle")
            .flex_none()
            .h(px(TAP))
            .px(px(10.))
            .gap(px(6.))
            .rounded_full()
            .role(Role::Button)
            .aria_label(t!("Accounts"))
            .aria_expanded(open)
            .cursor_pointer()
            .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised)))
            .active(|el| el.bg(p.raised))
            .when(!open && unread > 0, |el| el.child(count_pill(unread as u64, false, cx)))
            .child(Icon::new(if open { IconName::ChevronUp } else { IconName::ChevronDown }).size_5().text_color(p.muted))
            .on_click(cx.listener(|this, _, _, cx| {
                this.accounts.open = !this.accounts.open;
                cx.notify();
            }))
    }
}

/// The pictures in the switcher's rows.
const ACCOUNT_FACE: f32 = 40.;
/// As Settings' rows.
const ROW_H: f32 = 52.;

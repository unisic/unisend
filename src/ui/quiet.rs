//! Notifications as Signal sets them per chat and by profile, on this device (`crate::quiet` holds
//! the rules and decides). A chat's info screen opens its own settings: sound and vibration,
//! whether a muted group's mentions notify, and which profiles let the chat through. Settings,
//! Notifications, Notification profiles makes, schedules and turns on the profiles.

use super::chatlook::ChatPage;
use super::disappear::timer_row;
use super::panels::{Page, ROW_H, card, footnote, link, row};
use super::*;
use crate::quiet::{self, ChatNotify, MAX_CHATS, MAX_PROFILES, Manual, Schedule};
use gpui_kit::component::switch::Switch;
use std::collections::BTreeMap;

const HOUR_MS: u64 = 3_600_000;
/// The step of a schedule's start and end.
const STEP: u16 = 30;

pub(super) struct Ui {
    /// The new profile's name.
    field: Entity<InputState>,
    adding: bool,
}

impl Ui {
    pub(super) fn new(window: &mut Window, cx: &mut Context<BeeperApp>, subs: &mut Vec<Subscription>) -> Self {
        let field = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Profile name")));
        crate::ime::follow(&field, window, cx);
        subs.push(cx.subscribe_in(&field, window, |this, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { shift: false, .. } => this.add_profile(cx),
            other => keyboard_follows(other, Keyboard::Text, cx),
        }));
        Self { field, adding: false }
    }
}

fn now_ms() -> u64 {
    Local::now().timestamp_millis().max(0) as u64
}

fn clock(minute: u16) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

/// Keeps a chat's settings, within `MAX_CHATS`: chats gone from the list let theirs go first.
fn keep(chats: &mut BTreeMap<OwnedRoomId, ChatNotify>, room: &OwnedRoomId, set: ChatNotify, listed: impl Fn(&OwnedRoomId) -> bool) {
    if set.is_default() {
        chats.remove(room);
        return;
    }
    if chats.len() >= MAX_CHATS && !chats.contains_key(room) {
        chats.retain(|id, _| listed(id));
        if chats.len() >= MAX_CHATS {
            chats.pop_first();
        }
    }
    chats.insert(room.clone(), set);
}

impl BeeperApp {
    /// Writes the settings and hands the rules to the notifications.
    fn save_quiet(&mut self, cx: &mut Context<Self>) {
        self.settings.save(&self.dir);
        quiet::apply(&self.settings.chat_notify, &self.settings.profiles);
        cx.notify();
    }

    fn set_chat_notify(&mut self, room: &OwnedRoomId, change: impl FnOnce(&mut ChatNotify), cx: &mut Context<Self>) {
        let mut set = self.settings.chat_notify.get(room).copied().unwrap_or_default();
        change(&mut set);
        let rooms = &self.rooms;
        keep(&mut self.settings.chat_notify, room, set, |id| rooms.iter().any(|r| &r.id == id));
        self.save_quiet(cx);
    }

    /// The chat's entry to its notification settings, on its info screen.
    pub(super) fn notify_entry(&self, room: &OwnedRoomId, cx: &mut Context<Self>) -> Stateful<Div> {
        let room = room.clone();
        timer_row("notify-entry", Some(IconName::Bell), t!("Notifications"), cx).on_click(cx.listener(move |this, _, _, cx| {
            this.open_chat_page(room.clone(), ChatPage::Notify, cx);
        }))
    }

    /// The chat's notification settings, as rows of its sheet.
    pub(super) fn chat_notify_rows(&self, room: &RoomSummary, live: bool, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let p = Palette::of(cx);
        let set = self.settings.chat_notify.get(&room.id).copied().unwrap_or_default();
        let label = |id: &'static str, text: &'static str| h_flex().id(id).px(px(GUTTER + 4.)).pt(px(8.)).text_size(fs(13.)).text_color(p.faint).child(text);
        let check = |on: bool| on.then_some(IconName::Check);
        let tap = |row: Stateful<Div>, change: fn(&mut ChatNotify), cx: &mut Context<Self>| {
            let id = room.id.clone();
            row.when(live, |el| el.on_click(cx.listener(move |this, _, _, cx| this.set_chat_notify(&id, change, cx))))
        };
        let mut rows = vec![tap(timer_row("notify-sound", check(!set.quiet), t!("Sound and vibration"), cx), |s| s.quiet = !s.quiet, cx)];
        if !room.dm {
            rows.push(label("notify-mentions", t!("Mentions while muted")));
            rows.push(tap(timer_row("notify-mentions-on", check(!set.skip_mentions), t!("Always notify me"), cx), |s| s.skip_mentions = false, cx));
            rows.push(tap(timer_row("notify-mentions-off", check(set.skip_mentions), t!("Don't notify me"), cx), |s| s.skip_mentions = true, cx));
        }
        if !self.settings.profiles.list.is_empty() {
            rows.push(label("notify-profiles", t!("Comes through these notification profiles")));
        }
        for profile in &self.settings.profiles.list {
            let (pid, on) = (profile.id, profile.allowed.contains(&room.id));
            let row = timer_row(("notify-profile", pid), check(on), profile.name.clone(), cx);
            let id = room.id.clone();
            rows.push(row.when(live, |el| {
                el.on_click(cx.listener(move |this, _, _, cx| {
                    if this.settings.profiles.toggle(pid, &id) {
                        this.save_quiet(cx);
                    } else {
                        this.error = Some(t!("This profile lets through as many chats as it can.").into());
                        cx.notify();
                    }
                }))
            }));
        }
        rows
    }

    /// The summary on Settings' Notification profiles row: the one on now, if any.
    pub(super) fn profiles_summary(&self) -> SharedString {
        match self.settings.profiles.active(now_ms(), Local::now().naive_local()) {
            Some(p) => t!("{name} is on", name = p.name).into(),
            None if self.settings.profiles.list.is_empty() => SharedString::default(),
            None => t!("Off").into(),
        }
    }

    pub(super) fn profiles_entry(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        link("notification-profiles", t!("Notification profiles"), self.profiles_summary(), None, Some(IconName::ChevronRight), cx).on_click(cx.listener(|this, _, _, cx| {
            this.quiet.adding = false;
            this.open_page(Page::Profiles, cx);
        }))
    }

    fn add_profile(&mut self, cx: &mut Context<Self>) {
        let name = self.quiet.field.read(cx).value().to_string();
        if self.settings.profiles.add(&name).is_some() {
            self.quiet.adding = false;
            self.save_quiet(cx);
        }
    }

    fn change_profile(&mut self, id: u32, change: impl FnOnce(&mut quiet::Profile), cx: &mut Context<Self>) {
        if let Some(p) = self.settings.profiles.list.iter_mut().find(|p| p.id == id) {
            change(p);
            self.save_quiet(cx);
        }
    }

    /// Turns the profile on by hand, for `for_ms` or until turned off.
    fn profile_on(&mut self, id: u32, for_ms: Option<u64>, cx: &mut Context<Self>) {
        self.settings.profiles.manual = Some(Manual { on: Some(id), until: for_ms.map(|ms| now_ms() + ms) });
        self.save_quiet(cx);
    }

    /// Turns off what is on: a schedule that holds stays off until its run ends, as in Signal.
    fn profiles_off(&mut self, cx: &mut Context<Self>) {
        let profiles = &mut self.settings.profiles;
        profiles.manual = None;
        let now = Local::now();
        let end = profiles.active(now_ms(), now.naive_local()).and_then(|p| p.schedule).and_then(|s| s.run_end(now.naive_local()));
        if let Some(end) = end {
            let until = end.and_local_timezone(Local).earliest().map(|t| t.timestamp_millis().max(0) as u64);
            profiles.manual = Some(Manual { on: None, until });
        }
        self.save_quiet(cx);
    }

    /// Settings > Notifications > Notification profiles.
    pub(super) fn render_profiles(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let now = Local::now();
        let active = self.settings.profiles.active(now_ms(), now.naive_local()).map(|p| p.id);
        let manual = self.settings.profiles.manual;
        let names: HashMap<&OwnedRoomId, &str> = self.rooms.iter().map(|r| (&r.id, r.name.as_str())).collect();
        let mut page = v_flex().gap(px(GAP * 2.)).child(footnote(
            t!("While a profile is on, only the chats it lets through notify; the rest arrive quietly. Profiles stay on this device."),
            cx,
        ));
        for profile in self.settings.profiles.list.clone() {
            let id = profile.id;
            let on = active == Some(id);
            let state = match manual.filter(|m| on && m.on == Some(id)) {
                Some(Manual { until: Some(until), .. }) => match Local.timestamp_millis_opt(until as i64).single() {
                    Some(t) => t!("On until {time}", time = t.format("%H:%M")),
                    None => t!("On").to_owned(),
                },
                Some(_) => t!("On").to_owned(),
                None if on => t!("On by its schedule").to_owned(),
                None => t!("Off").to_owned(),
            };
            let button = |key: &'static str, label: &'static str| Button::new((key, id)).label(label).h(px(TAP)).px(px(GAP));
            let turn = if on {
                h_flex().gap(px(8.)).pb(px(8.)).child(button("profile-off", t!("Turn off")).on_click(cx.listener(|this, _, _, cx| this.profiles_off(cx))))
            } else {
                h_flex()
                    .gap(px(8.))
                    .pb(px(8.))
                    .flex_wrap()
                    .child(button("profile-hour", t!("For 1 hour")).on_click(cx.listener(move |this, _, _, cx| this.profile_on(id, Some(HOUR_MS), cx))))
                    .child(button("profile-on", t!("Until I turn it off")).on_click(cx.listener(move |this, _, _, cx| this.profile_on(id, None, cx))))
            };
            let weak = cx.entity().downgrade();
            let mentions = Switch::new(("profile-mentions", id)).checked(profile.mentions).on_click(move |on, _, cx| {
                let on = *on;
                let _ = weak.update(cx, |this, cx| this.change_profile(id, |p| p.mentions = on, cx));
            });
            let weak = cx.entity().downgrade();
            let scheduled = Switch::new(("profile-schedule", id)).checked(profile.schedule.is_some()).on_click(move |on, _, cx| {
                let on = *on;
                let _ = weak.update(cx, |this, cx| this.change_profile(id, |p| p.schedule = on.then(Schedule::default), cx));
            });
            let mut body = card(cx)
                .child(
                    h_flex()
                        .min_h(px(ROW_H))
                        .gap(px(GAP))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .py(px(8.))
                                .child(div().text_size(fs(16.)).line_height(fs(22.)).truncate().child(profile.name.clone()))
                                .child(div().text_size(fs(13.)).line_height(fs(18.)).text_color(if on { p.accent_ink } else { p.muted }).child(state)),
                        )
                        .child(round_button(("profile-delete", id), IconName::Trash, cx).on_click(cx.listener(move |this, _, _, cx| {
                            this.settings.profiles.remove(id);
                            this.save_quiet(cx);
                        }))),
                )
                .child(turn)
                .child(row(t!("Allow all mentions"), t!("A message that mentions you notifies from any chat."), cx).child(mentions))
                .child(row(t!("Schedule"), "", cx).child(scheduled));
            if let Some(schedule) = profile.schedule {
                body = body.child(self.schedule_rows(id, schedule, cx));
            }
            let allowed: Vec<_> = profile.allowed.iter().filter_map(|r| names.get(r).map(|name| (r.clone(), name.to_string()))).collect();
            body = body.child(row(t!("Chats that come through"), if allowed.is_empty() { t!("None yet. Add them from a chat's info, Notifications.") } else { "" }, cx));
            for (room, name) in allowed {
                body = body.child(
                    h_flex()
                        .min_h(px(TAP))
                        .gap(px(GAP))
                        .child(div().flex_1().min_w_0().text_size(fs(15.)).truncate().child(name))
                        .child(round_button(SharedString::from(format!("profile-{id}-drop-{room}")), IconName::X, cx).on_click(cx.listener(move |this, _, _, cx| {
                            if this.settings.profiles.toggle(id, &room) {
                                this.save_quiet(cx);
                            }
                        }))),
                );
            }
            page = page.child(body);
        }
        let full = self.settings.profiles.list.len() >= MAX_PROFILES;
        if self.quiet.adding {
            page = page.child(
                card(cx)
                    .py(px(8.))
                    .gap(px(8.))
                    .child(div().text_size(fs(12.)).text_color(p.faint).child(t!("Name")))
                    .child(Input::new(&self.quiet.field))
                    .child(crate::ime::marks(&self.quiet.field))
                    .child(
                        h_flex()
                            .pb(px(4.))
                            .gap(px(8.))
                            .child(Button::new("profile-save").primary().label(t!("Add")).on_click(cx.listener(|this, _, _, cx| this.add_profile(cx))))
                            .child(Button::new("profile-cancel").label(t!("Cancel")).on_click(cx.listener(|this, _, _, cx| {
                                this.quiet.adding = false;
                                cx.notify();
                            }))),
                    ),
            );
        } else if !full {
            page = page.child(card(cx).child(link("profile-new", t!("New profile"), SharedString::default(), Some(IconName::Plus), None, cx).on_click(cx.listener(
                |this, _, window, cx| {
                    this.quiet.field.update(cx, |input, cx| {
                        input.set_value("", window, cx);
                        input.focus(window, cx);
                    });
                    this.quiet.adding = true;
                    cx.notify();
                },
            ))));
        }
        page.into_any_element()
    }

    /// A schedule's days as seven round toggles, and its start and end in steps of `STEP`.
    fn schedule_rows(&self, id: u32, s: Schedule, cx: &mut Context<Self>) -> Div {
        let p = Palette::of(cx);
        let monday = Local.with_ymd_and_hms(2024, 1, 1, 12, 0, 0).single();
        let days = h_flex().gap(px(4.)).pb(px(8.)).flex_wrap().children((0..7u8).map(|d| {
            let on = s.days & (1 << d) != 0;
            let name = monday.map(|m| crate::i18n::weekday_short(&(m + chrono::Duration::days(i64::from(d))))).unwrap_or_default();
            div()
                .id(("profile-day", u32::from(d) + id * 8))
                .size(px(TAP))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_size(fs(13.))
                .bg(if on { p.text } else { p.raised })
                .text_color(if on { p.ground } else { p.muted })
                .role(Role::CheckBox)
                .aria_label(SharedString::from(name.clone()))
                .aria_toggled(on.into())
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.change_profile(id, |p| p.schedule = p.schedule.map(|s| Schedule { days: s.days ^ (1 << d), ..s }), cx)))
                .child(name)
        }));
        let time = |key: &'static str, label: &'static str, minute: u16, start: bool, cx: &mut Context<Self>| {
            let shift = move |this: &mut Self, back: bool, cx: &mut Context<Self>| {
                let day = 24 * 60;
                let next = |m: u16| if back { (m + day - STEP) % day } else { (m + STEP) % day };
                this.change_profile(id, |p| p.schedule = p.schedule.map(|s| if start { Schedule { start: next(s.start), ..s } } else { Schedule { end: next(s.end), ..s } }), cx);
            };
            row(label, "", cx)
                .child(round_button((key, id * 2), IconName::Minus, cx).on_click(cx.listener(move |this, _, _, cx| shift(this, true, cx))))
                .child(div().w(px(56.)).text_center().text_size(fs(16.)).child(clock(minute)))
                .child(round_button((key, id * 2 + 1), IconName::Plus, cx).on_click(cx.listener(move |this, _, _, cx| shift(this, false, cx))))
        };
        v_flex().child(days).child(time("profile-start", t!("From"), s.start, true, cx)).child(time("profile-end", t!("Until"), s.end, false, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::{BTreeMap, ChatNotify, MAX_CHATS, OwnedRoomId, clock, keep};

    #[test]
    fn chat_settings_stay_bounded_and_defaults_are_not_kept() {
        let id = |n: usize| OwnedRoomId::try_from(format!("!r{n:04}:x")).unwrap();
        let quiet = ChatNotify { quiet: true, skip_mentions: false };
        let mut chats = BTreeMap::new();
        for n in 0..MAX_CHATS {
            keep(&mut chats, &id(n), quiet, |_| true);
        }
        keep(&mut chats, &id(9999), quiet, |r| r != &id(5));
        assert!(chats.len() == MAX_CHATS && !chats.contains_key(&id(5)) && chats.contains_key(&id(9999)));
        keep(&mut chats, &id(10_000), quiet, |_| true);
        assert!(chats.len() == MAX_CHATS && !chats.contains_key(&id(0)));
        keep(&mut chats, &id(9999), ChatNotify::default(), |_| true);
        assert!(!chats.contains_key(&id(9999)));
        assert_eq!((clock(0), clock(21 * 60 + 30)), ("00:00".to_owned(), "21:30".to_owned()));
    }
}

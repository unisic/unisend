//! What decides, on this device, whether a message's notification shows and whether it makes a
//! sound: each chat's own notification settings (Signal's custom notifications) and the
//! notification profiles (Signal's notification profiles: a name, the chats that still come
//! through, a schedule, or turned on by hand). Nothing here leaves the device or touches the
//! unread counts; the push rules on the server still say which messages notify at all.
//!
//! The UI owns the settings (`settings.json`) and hands a copy to `apply`; the backend asks
//! `judge` once per notification, from its own thread. A profile's schedule is read at that moment
//! from the local clock, so no timer runs for it.

use std::collections::BTreeMap;
use std::sync::Mutex;

use chrono::{Datelike as _, Local, NaiveDateTime, Timelike as _};
use matrix_sdk::ruma::{OwnedRoomId, RoomId};

/// Profiles kept; Signal's list is short too.
pub(crate) const MAX_PROFILES: usize = 8;
/// Chats a profile lets through.
pub(crate) const MAX_ALLOWED: usize = 64;
/// Chats with notification settings of their own.
pub(crate) const MAX_CHATS: usize = 256;
pub(crate) const NAME_MAX: usize = 32;

/// One chat's own notification settings; the default is the app's behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ChatNotify {
    /// The chat's notifications make no sound and do not vibrate.
    #[serde(default, skip_serializing_if = "is_false")]
    pub quiet: bool,
    /// A muted group's mentions do not notify either (Signal's "Mentions: Don't notify me").
    #[serde(default, skip_serializing_if = "is_false")]
    pub skip_mentions: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl ChatNotify {
    pub(crate) fn is_default(self) -> bool {
        self == Self::default()
    }
}

/// When a profile turns itself on: on `days` (bit 0 Monday to bit 6 Sunday), from `start` to
/// `end`, in minutes after midnight, local time. An end at or before the start runs past
/// midnight into the next day; start and end equal make the whole day.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Schedule {
    pub days: u8,
    pub start: u16,
    pub end: u16,
}

impl Default for Schedule {
    /// Signal's default for a new schedule: weeknights, 21:00 to 07:00.
    fn default() -> Self {
        Self { days: 0b001_1111, start: 21 * 60, end: 7 * 60 }
    }
}

const DAY: u16 = 24 * 60;

impl Schedule {
    fn has(&self, weekday: u32) -> bool {
        self.days & (1 << (weekday % 7)) != 0
    }

    /// Whether the schedule holds at `minute` of the day `weekday` (0 Monday).
    pub(crate) fn holds(&self, weekday: u32, minute: u16) -> bool {
        let (start, end) = (self.start % DAY, self.end % DAY);
        let yesterday = (weekday + 6) % 7;
        if start < end {
            self.has(weekday) && (start..end).contains(&minute)
        } else if start == end {
            self.has(weekday)
        } else {
            self.has(weekday) && minute >= start || self.has(yesterday) && minute < end
        }
    }

    /// The local time at which the run holding at `at` ends, for "off until the schedule ends";
    /// `None` when it does not hold. Looks at most a week ahead.
    pub(crate) fn run_end(&self, at: NaiveDateTime) -> Option<NaiveDateTime> {
        let minute_of = |t: NaiveDateTime| (t.hour() * 60 + t.minute()) as u16;
        let holds = |t: NaiveDateTime| self.holds(t.weekday().num_days_from_monday(), minute_of(t));
        if !holds(at) {
            return None;
        }
        // Whole minutes, walked by the hour then by the minute: at most 168 + 60 steps.
        let mut t = at.with_second(0)?.with_nanosecond(0)?;
        for _ in 0..7 * 24 {
            let next = t + chrono::Duration::hours(1);
            if !holds(next) {
                break;
            }
            t = next;
        }
        for _ in 0..60 {
            t += chrono::Duration::minutes(1);
            if !holds(t) {
                return Some(t);
            }
        }
        Some(t)
    }
}

/// A notification profile.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Profile {
    pub id: u32,
    pub name: String,
    /// The chats whose messages still notify while the profile is on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<OwnedRoomId>,
    /// A message that mentions the user notifies from any chat.
    #[serde(default)]
    pub mentions: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<Schedule>,
}

/// A profile turned on or off by hand: `on` is the profile, or `None` for all off, until `until`
/// (ms since the epoch) or, without one, until changed again. It wins over every schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Manual {
    pub on: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Profiles {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub list: Vec<Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual: Option<Manual>,
}

impl Profiles {
    pub(crate) fn is_empty(&self) -> bool {
        self.list.is_empty() && self.manual.is_none()
    }

    /// The profile in force at `now_ms`, local time `local`: one turned on by hand while its time
    /// lasts, else the first whose schedule holds.
    pub(crate) fn active(&self, now_ms: u64, local: NaiveDateTime) -> Option<&Profile> {
        if let Some(m) = self.manual.filter(|m| m.until.is_none_or(|until| now_ms < until)) {
            return m.on.and_then(|id| self.list.iter().find(|p| p.id == id));
        }
        let (day, minute) = (local.weekday().num_days_from_monday(), (local.hour() * 60 + local.minute()) as u16);
        self.list.iter().find(|p| p.schedule.is_some_and(|s| s.holds(day, minute)))
    }

    /// Adds a profile named `name`; `None` when the name is empty or the list is full.
    pub(crate) fn add(&mut self, name: &str) -> Option<u32> {
        let name = clean(name)?;
        if self.list.len() >= MAX_PROFILES {
            return None;
        }
        let id = self.list.iter().map(|p| p.id).max().map_or(1, |m| m + 1);
        self.list.push(Profile { id, name, ..Default::default() });
        Some(id)
    }

    pub(crate) fn remove(&mut self, id: u32) {
        self.list.retain(|p| p.id != id);
        if self.manual.is_some_and(|m| m.on == Some(id)) {
            self.manual = None;
        }
    }

    /// Lets the chat through the profile or stops letting it; `false` when the profile is full.
    pub(crate) fn toggle(&mut self, id: u32, room: &OwnedRoomId) -> bool {
        let Some(p) = self.list.iter_mut().find(|p| p.id == id) else { return false };
        if let Some(ix) = p.allowed.iter().position(|r| r == room) {
            p.allowed.remove(ix);
            return true;
        }
        if p.allowed.len() >= MAX_ALLOWED {
            return false;
        }
        p.allowed.push(room.clone());
        true
    }
}

/// A name as kept: trimmed, at most `NAME_MAX` characters, not empty.
pub(crate) fn clean(name: &str) -> Option<String> {
    let name: String = name.trim().chars().take(NAME_MAX).collect();
    (!name.is_empty()).then_some(name)
}

#[derive(Default)]
struct Rules {
    chats: BTreeMap<OwnedRoomId, ChatNotify>,
    profiles: Profiles,
}

static RULES: Mutex<Option<Rules>> = Mutex::new(None);

/// Makes these settings the ones notifications follow.
pub(crate) fn apply(chats: &BTreeMap<OwnedRoomId, ChatNotify>, profiles: &Profiles) {
    *RULES.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Rules { chats: chats.clone(), profiles: profiles.clone() });
}

/// What becomes of a chat's notification.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Shown; `alert` false for a quiet one.
    Show { alert: bool },
    /// Muted group whose mentions the user turned off.
    MentionsOff,
    /// A notification profile holds it back.
    Profile,
}

/// The verdict for a notification of `room`; `mention` when one of its messages mentions the user.
pub(crate) fn judge(room: &RoomId, muted: bool, group: bool, mention: bool) -> Verdict {
    let now = Local::now();
    let rules = RULES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(rules) = rules.as_ref() else { return Verdict::Show { alert: true } };
    decide(rules, room, muted, group, mention, now.timestamp_millis().max(0) as u64, now.naive_local())
}

fn decide(rules: &Rules, room: &RoomId, muted: bool, group: bool, mention: bool, now_ms: u64, local: NaiveDateTime) -> Verdict {
    let own = rules.chats.get(room).copied().unwrap_or_default();
    if muted && group && own.skip_mentions {
        return Verdict::MentionsOff;
    }
    if let Some(p) = rules.profiles.active(now_ms, local)
        && !p.allowed.iter().any(|r| r == room)
        && !(p.mentions && mention)
    {
        return Verdict::Profile;
    }
    Verdict::Show { alert: !own.quiet }
}

#[cfg(test)]
mod tests {
    use super::{ChatNotify, Manual, Profile, Profiles, Rules, Schedule, Verdict, decide};
    use chrono::{NaiveDate, NaiveDateTime};
    use matrix_sdk::ruma::OwnedRoomId;

    /// 2024-01-01 was a Monday.
    fn at(day: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2024, 1, 1 + day).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    fn id(s: &str) -> OwnedRoomId {
        OwnedRoomId::try_from(format!("!{s}:x")).unwrap()
    }

    #[test]
    fn schedules_hold_on_their_days_and_run_past_midnight() {
        let night = Schedule { days: 0b000_0001, start: 22 * 60, end: 7 * 60 };
        assert!(night.holds(0, 22 * 60) && night.holds(0, 23 * 60 + 59));
        // Monday's night runs into Tuesday morning, not into Monday morning.
        assert!(night.holds(1, 6 * 60 + 59) && !night.holds(1, 7 * 60) && !night.holds(0, 60));
        assert!(!night.holds(1, 22 * 60));
        let work = Schedule { days: 0b001_1111, start: 9 * 60, end: 17 * 60 };
        assert!(work.holds(4, 9 * 60) && !work.holds(4, 17 * 60) && !work.holds(5, 10 * 60));
        let all_day = Schedule { days: 0b100_0000, start: 0, end: 0 };
        assert!(all_day.holds(6, 0) && all_day.holds(6, 1439) && !all_day.holds(0, 0));
    }

    #[test]
    fn a_run_ends_where_the_schedule_stops() {
        let night = Schedule { days: 0b000_0001, start: 22 * 60, end: 7 * 60 };
        assert_eq!(night.run_end(at(0, 23, 30)), Some(at(1, 7, 0)));
        assert_eq!(night.run_end(at(0, 12, 0)), None);
        let all_week = Schedule { days: 0b111_1111, start: 0, end: 0 };
        assert!(all_week.run_end(at(0, 1, 0)).is_some());
    }

    fn rules() -> Rules {
        let mut r = Rules::default();
        r.profiles.list.push(Profile { id: 1, name: "Sleep".into(), allowed: vec![id("mum")], mentions: false, schedule: Some(Schedule { days: 0b111_1111, start: 22 * 60, end: 7 * 60 }) });
        r.profiles.list.push(Profile { id: 2, name: "Work".into(), allowed: vec![], mentions: true, schedule: None });
        r.chats.insert(id("loud"), ChatNotify { quiet: true, skip_mentions: false });
        r.chats.insert(id("club"), ChatNotify { quiet: false, skip_mentions: true });
        r
    }

    #[test]
    fn a_profile_lets_through_its_chats_and_by_choice_mentions() {
        let mut r = rules();
        let night = at(2, 23, 0);
        let day = at(2, 12, 0);
        assert_eq!(decide(&r, &id("a"), false, false, false, 0, night), Verdict::Profile);
        assert_eq!(decide(&r, &id("mum"), false, false, false, 0, night), Verdict::Show { alert: true });
        assert_eq!(decide(&r, &id("a"), false, true, true, 0, night), Verdict::Profile);
        assert_eq!(decide(&r, &id("a"), false, false, false, 0, day), Verdict::Show { alert: true });
        // By hand: on for a while wins over the schedule, then the schedule is back.
        r.profiles.manual = Some(Manual { on: Some(2), until: Some(1000) });
        assert_eq!(decide(&r, &id("a"), false, true, true, 500, day), Verdict::Show { alert: true });
        assert_eq!(decide(&r, &id("a"), false, true, false, 500, day), Verdict::Profile);
        assert_eq!(decide(&r, &id("a"), false, true, false, 1000, day), Verdict::Show { alert: true });
        // Off by hand during the schedule.
        r.profiles.manual = Some(Manual { on: None, until: None });
        assert_eq!(decide(&r, &id("a"), false, false, false, 0, night), Verdict::Show { alert: true });
    }

    #[test]
    fn a_chat_may_be_quiet_and_a_muted_group_may_drop_mentions() {
        let r = rules();
        let day = at(0, 12, 0);
        assert_eq!(decide(&r, &id("loud"), false, false, false, 0, day), Verdict::Show { alert: false });
        assert_eq!(decide(&r, &id("club"), true, true, true, 0, day), Verdict::MentionsOff);
        // Not muted, or a direct chat: the choice has nothing to do.
        assert_eq!(decide(&r, &id("club"), false, true, true, 0, day), Verdict::Show { alert: true });
        assert_eq!(decide(&r, &id("club"), true, false, true, 0, day), Verdict::Show { alert: true });
    }

    #[test]
    fn profiles_are_capped_and_removing_one_ends_its_manual_turn() {
        let mut p = Profiles::default();
        assert_eq!(p.add("  "), None);
        for n in 0..super::MAX_PROFILES {
            assert!(p.add(&format!("P{n}")).is_some());
        }
        assert_eq!(p.add("one more"), None);
        p.manual = Some(Manual { on: Some(3), until: None });
        p.remove(3);
        assert!(p.manual.is_none() && p.list.len() == super::MAX_PROFILES - 1);
        let room = id("r");
        assert!(p.toggle(1, &room) && p.list[0].allowed == [room.clone()]);
        assert!(p.toggle(1, &room) && p.list[0].allowed.is_empty());
        for n in 0..super::MAX_ALLOWED {
            assert!(p.toggle(1, &id(&format!("r{n}"))));
        }
        assert!(!p.toggle(1, &room));
    }
}

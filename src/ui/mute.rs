//! Muting a chat as Signal offers it: for an hour, eight hours, a day, a week or for good. The mode
//! is the room's push rules, so other Matrix clients agree; Matrix has no end time, so the end of a
//! timed mute is kept in `settings.json` and one timer lifts it.

use super::*;
use super::disappear::plain_row;
use matrix_sdk::notification_settings::RoomNotificationMode;
use std::collections::BTreeMap;

/// Timed mutes kept at once; past this the one ending soonest is dropped, and stays muted.
const MAX_TIMED: usize = 256;

const HOUR_MS: u64 = 3_600_000;

/// A mute as Beeper's apps write and read it: a room rule without a notify action. Mentions still
/// notify through the server's override rules, as in Signal's muted groups by default.
pub(super) const MUTE: RoomNotificationMode = RoomNotificationMode::MentionsAndKeywordsOnly;

fn now_ms() -> u64 {
    Local::now().timestamp_millis().max(0) as u64
}

impl BeeperApp {
    /// Sets how the chat notifies; `for_ms` ends a mute after that long, `None` keeps it.
    pub(super) fn set_mute(&mut self, room: OwnedRoomId, mode: RoomNotificationMode, for_ms: Option<u64>, cx: &mut Context<Self>) -> bool {
        if !self.send(Cmd::Chat(room.clone(), ChatAction::Mute(mode)), cx) {
            return false;
        }
        let ends = &mut self.settings.mute_until;
        match for_ms {
            Some(ms) => remember(ends, room, now_ms() + ms),
            None if ends.remove(&room).is_none() => return true,
            None => {}
        }
        self.settings.save(&self.dir);
        self.unmute_due(false, cx);
        true
    }

    /// Lifts the mutes whose time is up, then waits for the next one to end. Called once signed in,
    /// back online and when the window comes back, as a suspended machine's timer runs late, and
    /// on every change of the chats with `send` off, to forget the ends of mutes lifted.
    pub(super) fn unmute_due(&mut self, send: bool, cx: &mut Context<Self>) {
        let now = now_ms();
        let rooms = &self.rooms;
        let (lift, forget, next) = due(&self.settings.mute_until, now, |id| match rooms.iter().find(|r| &r.id == id) {
            Some(r) => Some(r.muted),
            // Before the list is read nothing is known; after, a chat missing from it is gone.
            None if rooms.is_empty() => Some(true),
            None => None,
        });
        if send {
            // The end is kept until the chat notifies again: an unmute that fails offline is sent again.
            // shortcut: a chat muted for good elsewhere after a timed mute here is unmuted at its end; a
            // mode change on another client would need to clear the time, read from the push rules.
            for room in lift {
                self.send(Cmd::Chat(room, ChatAction::Mute(RoomNotificationMode::AllMessages)), cx);
            }
        }
        if !forget.is_empty() {
            for room in &forget {
                self.settings.mute_until.remove(room);
            }
            self.settings.save(&self.dir);
        }
        self.mute_timer = next.map(|next| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(next - now)).await;
                this.update(cx, |this, cx| this.unmute_due(true, cx)).ok();
            })
        });
    }

    /// The chat's mute in words: "Muted until 14:30" or "Muted"; `None` when it notifies.
    pub(super) fn mute_state(&self, room: &RoomSummary) -> Option<String> {
        if !room.muted {
            return None;
        }
        Some(match self.settings.mute_until.get(&room.id).and_then(|&at| local(at)) {
            Some(t) if t.date_naive() == Local::now().date_naive() => t!("Muted until {time}", time = t.format("%H:%M")),
            Some(t) => t!("Muted until {time}", time = format!("{}, {}", crate::i18n::date_short(&t), t.format("%H:%M"))),
            None => t!("Muted").to_owned(),
        })
    }

    /// The sheet's choices for muting the chat: how long; a group's says that mentions still notify.
    pub(super) fn mute_sheet(&self, room: &RoomSummary, live: bool, cx: &mut Context<Self>) -> (String, Vec<Stateful<Div>>) {
        let choices = [
            (t!("1 hour|mute"), Some(HOUR_MS)),
            (t!("8 hours|mute"), Some(8 * HOUR_MS)),
            (t!("1 day|mute"), Some(24 * HOUR_MS)),
            (t!("7 days|mute"), Some(7 * 24 * HOUR_MS)),
            (t!("Always"), None),
        ];
        let mut rows: Vec<_> = choices
            .into_iter()
            .enumerate()
            .map(|(ix, (label, ms))| {
                let row = plain_row(("mute-choice", ix), label, cx);
                if !live {
                    return row;
                }
                let room = room.id.clone();
                row.on_click(cx.listener(move |this, _, _, cx| {
                    this.set_mute(room.clone(), MUTE, ms, cx);
                    (this.chat_sheet, this.mute_picking) = (None, false);
                    cx.notify();
                }))
            })
            .collect();
        if !room.dm {
            let p = Palette::of(cx);
            rows.push(
                h_flex()
                    .id("mute-note")
                    .h(px(SHEET_ROW_H))
                    .px(px(GUTTER + 4.))
                    .text_size(fs(13.))
                    .line_height(fs(18.))
                    .text_color(p.faint)
                    .child(div().flex_1().min_w_0().child(t!("Mentions still notify."))),
            );
        }
        (t!("Mute for…").to_owned(), rows)
    }

    /// A chat action from a menu, the info screen or a swipe: muting asks for how long first where
    /// a sheet can, and unmuting forgets the mute's end.
    pub(super) fn chat_action(&mut self, room: OwnedRoomId, action: ChatAction, ask: bool, cx: &mut Context<Self>) -> bool {
        match action {
            ChatAction::Mute(MUTE) if ask => {
                self.chat_sheet = Some(room);
                (self.mute_picking, self.disappear.picking) = (true, false);
                self.info.member = None;
                cx.notify();
                true
            }
            ChatAction::Mute(mode) => self.set_mute(room, mode, None, cx),
            action => self.send(Cmd::Chat(room, action), cx),
        }
    }
}

/// The timed mutes at `now`: the chats whose time is up and that are still muted, so to unmute;
/// those whose time is up and that notify again or are gone (`muted` says `None`), so to forget;
/// and when the next one ends.
fn due(ends: &BTreeMap<OwnedRoomId, u64>, now: u64, muted: impl Fn(&OwnedRoomId) -> Option<bool>) -> (Vec<OwnedRoomId>, Vec<OwnedRoomId>, Option<u64>) {
    let (mut lift, mut forget) = (Vec::new(), Vec::new());
    for (id, _) in ends.iter().filter(|(_, at)| **at <= now) {
        if muted(id) == Some(true) { lift.push(id.clone()) } else { forget.push(id.clone()) }
    }
    (lift, forget, ends.values().copied().filter(|at| *at > now).min())
}

/// Keeps when the chat's mute ends, within `MAX_TIMED` chats.
fn remember(ends: &mut BTreeMap<OwnedRoomId, u64>, room: OwnedRoomId, at: u64) {
    if ends.len() >= MAX_TIMED
        && !ends.contains_key(&room)
        && let Some(soonest) = ends.iter().min_by_key(|(_, at)| **at).map(|(id, _)| id.clone())
    {
        ends.remove(&soonest);
    }
    ends.insert(room, at);
}

#[cfg(test)]
mod tests {
    use super::{BTreeMap, MAX_TIMED, OwnedRoomId, due, remember};

    #[test]
    fn a_mute_whose_time_is_up_is_forgotten_only_once_the_chat_notifies() {
        let id = |n: usize| OwnedRoomId::try_from(format!("!r{n}:x")).unwrap();
        let ends: BTreeMap<_, _> = [(id(1), 100), (id(2), 100), (id(3), 100), (id(4), 500), (id(5), 300)].into();
        let muted = |r: &OwnedRoomId| match r.as_str() {
            "!r1:x" => Some(true),
            "!r2:x" => Some(false),
            "!r3:x" => None,
            _ => Some(true),
        };
        assert_eq!(due(&ends, 200, muted), (vec![id(1)], vec![id(2), id(3)], Some(300)));
        assert_eq!(due(&ends, 50, muted), (vec![], vec![], Some(100)));
        assert_eq!(due(&ends, 600, muted).2, None);
    }

    #[test]
    fn timed_mutes_stay_bounded_and_drop_the_soonest() {
        let id = |n: usize| OwnedRoomId::try_from(format!("!r{n}:x")).unwrap();
        let mut ends = Default::default();
        for n in 0..MAX_TIMED {
            remember(&mut ends, id(n), 1000 + n as u64);
        }
        remember(&mut ends, id(0), 5);
        assert_eq!(ends.len(), MAX_TIMED);
        remember(&mut ends, id(MAX_TIMED), 9999);
        assert_eq!(ends.len(), MAX_TIMED);
        assert!(!ends.contains_key(&id(0)) && ends.contains_key(&id(1)) && ends[&id(MAX_TIMED)] == 9999);
    }
}

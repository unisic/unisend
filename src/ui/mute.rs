//! Muting a chat as Signal offers it: for an hour, eight hours, a day, a week or for good, and in a
//! group, mentions only. The mode is the room's push rules, so other Matrix clients agree; Matrix
//! has no end time, so the end of a timed mute is kept in `settings.json` and one timer lifts it.

use super::*;
use super::disappear::timer_row;
use matrix_sdk::notification_settings::RoomNotificationMode;

/// Timed mutes kept at once; past this the one ending soonest is dropped, and stays muted.
const MAX_TIMED: usize = 256;

const HOUR_MS: u64 = 3_600_000;

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
        self.unmute_due(cx);
        true
    }

    /// Lifts the mutes whose time is up, then waits for the next one to end. Called once signed in
    /// and when the window comes back, as a suspended machine's timer runs late.
    pub(super) fn unmute_due(&mut self, cx: &mut Context<Self>) {
        let now = now_ms();
        let due: Vec<OwnedRoomId> = self.settings.mute_until.iter().filter(|(_, at)| **at <= now).map(|(id, _)| id.clone()).collect();
        for room in &due {
            // shortcut: a chat muted for good elsewhere after a timed mute here is unmuted at its end; a
            // mode change on another client would need to clear the time, read from the push rules.
            if self.send(Cmd::Chat(room.clone(), ChatAction::Mute(RoomNotificationMode::AllMessages)), cx) {
                self.settings.mute_until.remove(room);
            }
        }
        if due.iter().any(|room| !self.settings.mute_until.contains_key(room)) {
            self.settings.save(&self.dir);
        }
        self.mute_timer = self.settings.mute_until.values().filter(|at| **at > now).min().map(|&next| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(next - now)).await;
                this.update(cx, |this, cx| this.unmute_due(cx)).ok();
            })
        });
    }

    /// The chat's mute in words: "Muted until 14:30", "Mentions only", "Muted"; `None` when it notifies.
    pub(super) fn mute_state(&self, room: &RoomSummary) -> Option<String> {
        if !room.muted {
            return None;
        }
        if room.mentions_only {
            return Some(t!("Mentions only").to_owned());
        }
        Some(match self.settings.mute_until.get(&room.id).and_then(|&at| local(at)) {
            Some(t) if t.date_naive() == Local::now().date_naive() => t!("Muted until {time}", time = t.format("%H:%M")),
            Some(t) => t!("Muted until {time}", time = format!("{}, {}", crate::i18n::date_short(&t), t.format("%H:%M"))),
            None => t!("Muted").to_owned(),
        })
    }

    /// The sheet's choices for muting the chat: how long, and in a group, mentions only.
    pub(super) fn mute_sheet(&self, room: &RoomSummary, live: bool, cx: &mut Context<Self>) -> (String, Vec<Stateful<Div>>) {
        let mut choices = vec![
            (t!("1 hour|mute"), Some(HOUR_MS)),
            (t!("8 hours|mute"), Some(8 * HOUR_MS)),
            (t!("1 day|mute"), Some(24 * HOUR_MS)),
            (t!("7 days|mute"), Some(7 * 24 * HOUR_MS)),
            (t!("Always"), None),
        ]
        .into_iter()
        .map(|(label, ms)| (label, RoomNotificationMode::Mute, ms))
        .collect::<Vec<_>>();
        if !room.dm {
            choices.push((t!("Mentions only"), RoomNotificationMode::MentionsAndKeywordsOnly, None));
        }
        let rows = choices
            .into_iter()
            .enumerate()
            .map(|(ix, (label, mode, ms))| {
                let row = timer_row(("mute-choice", ix), None, label, cx);
                if !live {
                    return row;
                }
                let room = room.id.clone();
                row.on_click(cx.listener(move |this, _, _, cx| {
                    this.set_mute(room.clone(), mode, ms, cx);
                    (this.chat_sheet, this.mute_picking) = (None, false);
                    cx.notify();
                }))
            })
            .collect();
        (t!("Mute for…").to_owned(), rows)
    }

    /// A chat action from a menu, the info screen or a swipe: muting asks for how long first where
    /// a sheet can, and unmuting forgets the mute's end.
    pub(super) fn chat_action(&mut self, room: OwnedRoomId, action: ChatAction, ask: bool, cx: &mut Context<Self>) -> bool {
        match action {
            ChatAction::Mute(RoomNotificationMode::Mute) if ask => {
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

/// Keeps when the chat's mute ends, within `MAX_TIMED` chats.
fn remember(ends: &mut std::collections::BTreeMap<OwnedRoomId, u64>, room: OwnedRoomId, at: u64) {
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
    use super::{MAX_TIMED, OwnedRoomId, remember};

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

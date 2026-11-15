//! Disappearing messages on Beeper's bridges (mautrix bridgev2). The chat's timer is the state
//! event `com.beeper.disappearing_timer` (state key ""), content `{"type", "timer"}` with the
//! timer in milliseconds, or `{}` when off. The bridge deletes an expired message by redacting
//! it, so the timeline needs nothing more for that. Whether the user may change the timer from
//! here is in the bridge's `com.beeper.room_features` state event, field `disappearing_timer`:
//! absent means the network takes no change from Matrix.

use matrix_sdk::{
    Room,
    deserialized_responses::RawAnySyncOrStrippedState,
    ruma::events::StateEventType,
};
use serde_json::{Value, json};
use crate::i18n::{t, tn};

pub const TIMER: &str = "com.beeper.disappearing_timer";
const FEATURES: &str = "com.beeper.room_features";
/// Offered when the bridge takes any timer (Signal): Signal's own choices.
const ANY_TIMER: [u64; 7] = [30 * SECOND, 5 * MINUTE, HOUR, 8 * HOUR, DAY, WEEK, 4 * WEEK];
/// A bridge's list longer than this is someone else's data; the rest is ignored.
const MAX_CHOICES: usize = 16;

const SECOND: u64 = 1000;
const MINUTE: u64 = 60 * SECOND;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const WEEK: u64 = 7 * DAY;

/// A chat's disappearing messages, as the chat's header and its menu need them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Disappearing {
    /// The timer in milliseconds; 0 is off.
    pub ms: u64,
    /// The timer starts once a message is read (Signal), not once it is sent (WhatsApp).
    pub after_read: bool,
    /// What the bridge accepts from here; `None` when it takes no change from Matrix.
    pub choices: Option<Choices>,
    /// The user's power level lets them send the timer event.
    pub allowed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Choices {
    /// The `type` sent with a timer.
    kind: String,
    /// Timers offered besides Off, shortest first.
    pub timers: Vec<u64>,
}

impl Disappearing {
    /// The timer can be changed from here.
    pub fn settable(&self) -> bool {
        self.allowed && self.choices.is_some()
    }

    /// The content of the event that sets the timer to `ms`; `None` when this chat does not take it.
    pub fn content(&self, ms: u64) -> Option<Value> {
        let choices = self.choices.as_ref()?;
        if ms == 0 {
            return Some(json!({}));
        }
        choices.timers.contains(&ms).then(|| json!({ "type": choices.kind, "timer": ms }))
    }
}

/// The timer in an event's content (the state event's, or a message's `com.beeper.disappearing_timer`)
/// and whether it runs from reading; 0 when off.
pub fn timer(content: &Value) -> (u64, bool) {
    let kind = content.get("type").and_then(Value::as_str).unwrap_or_default();
    let ms = content.get("timer").and_then(Value::as_u64).unwrap_or_default();
    if kind.is_empty() || ms == 0 { (0, false) } else { (ms, kind.starts_with("after_read")) }
}

/// What a bridge's `disappearing_timer` capability offers.
fn choices(capability: &Value) -> Option<Choices> {
    let kinds = capability.get("types")?.as_array()?;
    let kind = kinds.iter().filter_map(Value::as_str).next()?.to_owned();
    let mut timers: Vec<u64> = match capability.get("timers").and_then(Value::as_array) {
        Some(list) => list.iter().take(MAX_CHOICES).filter_map(Value::as_u64).filter(|&ms| ms > 0).collect(),
        None => ANY_TIMER.to_vec(),
    };
    timers.sort_unstable();
    timers.dedup();
    (!timers.is_empty()).then_some(Choices { kind, timers })
}

/// The chat's timer and what may be done with it, from the state the client has.
pub(super) async fn read(room: &Room) -> Disappearing {
    let (ms, after_read) = state(room, TIMER).await.first().map(timer).unwrap_or_default();
    let choices = state(room, FEATURES).await.iter().find_map(|features| choices(features.get("disappearing_timer")?));
    let allowed = match room.power_levels().await {
        Ok(levels) => levels.user_can_send_state(room.own_user_id(), StateEventType::from(TIMER)),
        Err(e) => {
            log::debug!("power levels: {e}");
            false
        }
    };
    Disappearing { ms, after_read, choices, allowed }
}

/// The contents of a room's state events of one type, every state key.
pub(super) async fn state(room: &Room, kind: &str) -> Vec<Value> {
    let Ok(events) = room.get_state_events(StateEventType::from(kind)).await else { return Vec::new() };
    events
        .iter()
        .filter_map(|e| {
            let RawAnySyncOrStrippedState::Sync(raw) = e else { return None };
            raw.get_field::<Value>("content").ok()?
        })
        .collect()
}

/// A timer as words: "30 seconds", "1 hour", "4 weeks", "90 days".
pub fn label(ms: u64) -> String {
    if ms == 0 {
        return t!("Off").to_owned();
    }
    let unit = [WEEK, DAY, HOUR, MINUTE].into_iter().find(|unit| ms.is_multiple_of(*unit)).unwrap_or(SECOND);
    let n = (ms / unit).max(1);
    match unit {
        WEEK => tn!(n, "{n} week", "{n} weeks"),
        DAY => tn!(n, "{n} day", "{n} days"),
        HOUR => tn!(n, "{n} hour", "{n} hours"),
        MINUTE => tn!(n, "{n} minute", "{n} minutes"),
        _ => tn!(n, "{n} second", "{n} seconds"),
    }
}

/// A change of the timer as a line of the timeline; `None` when nothing changed. `actor` is who
/// changed it, when that is known and someone in the chat.
pub fn notice(content: &Value, prev: Option<&Value>, actor: Option<&str>, own: bool) -> Option<String> {
    let (ms, _) = timer(content);
    let before = prev.map(timer).map(|(ms, _)| ms);
    // A bridge sets the timer on every chat it creates; only one that is on is news then.
    if before == Some(ms) || (before.is_none() && ms == 0) {
        return None;
    }
    Some(match (ms, actor) {
        (0, Some(_)) if own => t!("You turned off disappearing messages").to_owned(),
        (0, Some(actor)) => t!("{actor} turned off disappearing messages", actor = actor),
        (0, None) => t!("Disappearing messages were turned off").to_owned(),
        (ms, Some(_)) if own => t!("You set disappearing messages to {time}", time = label(ms)),
        (ms, Some(actor)) => t!("{actor} set disappearing messages to {time}", actor = actor, time = label(ms)),
        (ms, None) => t!("Disappearing messages were set to {time}", time = label(ms)),
    })
}

/// The bridge's own notice of a timer it changed (`com.beeper.action_message`), told already by
/// the line of the state event. One it made by itself ("implicit") says why, and stays.
pub fn repeats_state(content: &Value) -> bool {
    content
        .get("com.beeper.action_message")
        .is_some_and(|action| action.get("type").and_then(Value::as_str) == Some("disappearing_timer") && action.get("implicit").and_then(Value::as_bool) != Some(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timers_read_as_words() {
        assert_eq!(label(0), "Off");
        assert_eq!(label(30 * SECOND), "30 seconds");
        assert_eq!(label(HOUR), "1 hour");
        assert_eq!(label(8 * HOUR), "8 hours");
        assert_eq!(label(DAY), "1 day");
        assert_eq!(label(4 * WEEK), "4 weeks");
        assert_eq!(label(90 * DAY), "90 days");
        assert_eq!(label(90 * MINUTE), "90 minutes");
        assert_eq!(label(1500), "1 second");
    }

    #[test]
    fn a_timer_is_its_type_and_milliseconds() {
        assert_eq!(timer(&json!({})), (0, false));
        assert_eq!(timer(&json!({ "type": "after_send", "timer": DAY })), (DAY, false));
        assert_eq!(timer(&json!({ "type": "after_read", "timer": HOUR })), (HOUR, true));
        assert_eq!(timer(&json!({ "type": "", "timer": HOUR })), (0, false));
    }

    #[test]
    fn whatsapp_offers_its_three_and_signal_any() {
        let whatsapp = choices(&json!({ "types": ["after_send"], "timers": [7 * DAY, DAY, 90 * DAY] })).unwrap();
        assert_eq!(whatsapp.timers, [DAY, 7 * DAY, 90 * DAY]);
        let signal = choices(&json!({ "types": ["after_read"] })).unwrap();
        assert_eq!(signal.timers, ANY_TIMER);
        assert_eq!(choices(&json!({ "types": [] })), None);
        let long: Vec<u64> = (1..100).collect();
        assert_eq!(choices(&json!({ "types": ["after_send"], "timers": long })).unwrap().timers.len(), MAX_CHOICES);
    }

    #[test]
    fn only_offered_timers_are_sent() {
        let chat = Disappearing { ms: 0, after_read: false, choices: choices(&json!({ "types": ["after_send"], "timers": [DAY] })), allowed: true };
        assert_eq!(chat.content(DAY), Some(json!({ "type": "after_send", "timer": DAY })));
        assert_eq!(chat.content(0), Some(json!({})));
        assert_eq!(chat.content(HOUR), None);
        let meta = Disappearing { choices: None, ..chat };
        assert!(!meta.settable());
        assert_eq!(meta.content(DAY), None);
    }

    #[test]
    fn changes_read_as_sentences() {
        let on = json!({ "type": "after_read", "timer": WEEK });
        let off = json!({});
        assert_eq!(notice(&on, Some(&off), Some("Ann"), false).as_deref(), Some("Ann set disappearing messages to 1 week"));
        assert_eq!(notice(&off, Some(&on), None, false).as_deref(), Some("Disappearing messages were turned off"));
        assert_eq!(notice(&on, None, None, false).as_deref(), Some("Disappearing messages were set to 1 week"));
        assert_eq!(notice(&off, None, Some("Ann"), false), None, "first setting, off");
        assert_eq!(notice(&on, Some(&on), Some("Ann"), false), None, "no change");
    }

    #[test]
    fn the_bridges_own_notice_is_hidden_unless_it_explains_itself() {
        let notice = |implicit: bool| json!({ "msgtype": "m.notice", "com.beeper.action_message": { "type": "disappearing_timer", "timer": DAY, "implicit": implicit } });
        assert!(repeats_state(&notice(false)));
        assert!(!repeats_state(&notice(true)));
        assert!(!repeats_state(&json!({ "msgtype": "m.notice" })));
    }
}

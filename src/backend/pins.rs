//! Pinned messages: the room's `m.room.pinned_events`, each pin resolved to something a bar can show.
//! Pins are Matrix state; a bridge may or may not carry them to its network, and nothing here
//! pretends it does.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use futures_util::StreamExt as _;
use matrix_sdk::{
    Room,
    event_handler::EventHandlerDropGuard,
    ruma::{
        EventId, OwnedEventId,
        events::{
            AnySyncMessageLikeEvent, AnySyncStateEvent, AnySyncTimelineEvent, StateEventType, SyncMessageLikeEvent,
            room::message::MessageType,
        },
        serde::Raw,
    },
};
use tokio::sync::{Mutex, mpsc};

use super::{Ev, Quote, QuoteState, content, display_name};

/// Pins resolved and shown, newest first. Matrix sets no limit and every pin costs a lookup (the
/// event cache first, then one request), so the oldest beyond this are left out.
pub const MAX_PINS: usize = 50;
/// Lookups in flight at once.
const LOOKUPS: usize = 4;

#[derive(Clone, Default)]
pub struct Pins {
    /// Newest first; a pin that could not be read (deleted, not visible) is left out.
    pub list: Vec<Quote>,
    /// The user's power level lets them pin and unpin here.
    pub can: bool,
}

/// Pins as the state holds them (oldest first), cut to the newest `MAX_PINS` and turned newest first.
fn newest_first(ids: Vec<OwnedEventId>) -> Vec<OwnedEventId> {
    ids.into_iter().rev().take(MAX_PINS).collect()
}

pub(super) async fn read(room: &Room) -> Pins {
    let can = match room.power_levels().await {
        Ok(levels) => levels.user_can_send_state(room.own_user_id(), StateEventType::RoomPinnedEvents),
        Err(e) => {
            log::debug!("power levels: {e}");
            false
        }
    };
    let ids = match room.pinned_event_ids() {
        Some(ids) => ids,
        None => room.load_pinned_events().await.ok().flatten().unwrap_or_default(),
    };
    let list = futures_util::stream::iter(newest_first(ids))
        .map(|id| async move { quote(room, &id).await })
        .buffered(LOOKUPS)
        .filter_map(|q| async move { q })
        .collect()
        .await;
    Pins { list, can }
}

/// The pinned message as a reply would quote it. `None` when it cannot be read or is a redaction.
async fn quote(room: &Room, id: &EventId) -> Option<Quote> {
    let event = match room.load_or_fetch_event(id, None).await {
        Ok(event) => event,
        Err(e) => {
            log::debug!("pinned event {id}: {e}");
            return None;
        }
    };
    let AnySyncTimelineEvent::MessageLike(event) = event.raw().deserialize().ok()? else { return None };
    let mut q = Quote::unknown(id.to_string(), QuoteState::Ready);
    let sender = match event {
        AnySyncMessageLikeEvent::RoomMessage(SyncMessageLikeEvent::Original(e)) => {
            let (media, text) = content(&e.content.msgtype);
            q.place = matches!(e.content.msgtype, MessageType::Location(_));
            (q.media, q.text) = (media, text);
            e.sender
        }
        // Not a plain message (a poll, a sticker): shown by its sender alone.
        other => other.sender().to_owned(),
    };
    q.own = sender == room.own_user_id();
    q.sender = display_name(room, &sender).await;
    Some(q)
}

/// Sends the pins now, and again whenever the room's pins or power levels change. One at a time, so
/// a slow read cannot arrive after a newer one.
pub(super) async fn follow(room: &Room, ev: &mpsc::Sender<Ev>) -> EventHandlerDropGuard {
    let gate = Arc::new(Mutex::new(()));
    let send = {
        let (ev, gate) = (ev.clone(), gate.clone());
        move |room: Room| {
            let (ev, gate) = (ev.clone(), gate.clone());
            async move {
                let _one = gate.lock().await;
                let pins = read(&room).await;
                let _ = ev.send(Ev::Pins { room: room.room_id().to_owned(), pins }).await;
            }
        }
    };
    send(room.clone()).await;
    let handle = room.client().add_room_event_handler(room.room_id(), move |raw: Raw<AnySyncStateEvent>, room: Room| {
        let send = send.clone();
        async move {
            if matches!(raw.get_field::<String>("type").ok().flatten().as_deref(), Some("m.room.pinned_events" | "m.room.power_levels")) {
                send(room).await;
            }
        }
    });
    room.client().event_handler_drop_guard(handle)
}

pub(super) async fn set(room: &Room, target: &str, pin: bool) -> Result<()> {
    let id = EventId::parse(target).context("not an event id")?;
    if pin { room.pin_event(&id).await? } else { room.unpin_event(&id).await? };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_pin_first_and_the_oldest_are_cut() {
        let ids: Vec<OwnedEventId> = (0..MAX_PINS + 5).map(|n| EventId::parse(format!("$e{n}")).unwrap()).collect();
        let shown = newest_first(ids);
        assert_eq!(shown.len(), MAX_PINS);
        assert_eq!(shown[0].as_str(), format!("$e{}", MAX_PINS + 4));
        assert_eq!(shown[MAX_PINS - 1].as_str(), "$e5");
    }
}

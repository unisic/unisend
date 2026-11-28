//! Marking every unread chat the list holds as read at once, as Signal's "Mark all read": the
//! chats of the section and folder shown (or of the open forum), each through its own account.

use super::*;
use std::collections::BTreeMap;

/// Chats marked at once; a longer run waits for the next tap.
const MAX_READ: usize = 500;

impl BeeperApp {
    /// The unread chats of the section, folder or forum the list shows; invitations and message
    /// requests are answered, not read.
    fn unread_in_list(&self) -> impl Iterator<Item = &RoomSummary> {
        let pick = self.effective_pick();
        self.rooms.iter().filter(move |r| {
            r.is_unread()
                && r.pending.is_none()
                && match &self.forum {
                    Some(forum) => r.forum.as_ref() == Some(forum),
                    None => in_view(self.view, r) && self.in_folder(&pick, r),
                }
        })
    }

    /// The list header's button, while the list holds an unread chat and no search runs.
    pub(super) fn read_all_button(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        (!self.searching && self.unread_in_list().next().is_some())
            .then(|| round_button("read-all", IconName::CheckCheck, t!("Mark all as read"), cx).on_click(cx.listener(|this, _, _, cx| this.read_all(cx))))
    }

    fn read_all(&mut self, cx: &mut Context<Self>) {
        let mut by_slot: BTreeMap<usize, Vec<OwnedRoomId>> = BTreeMap::new();
        for r in self.unread_in_list().take(MAX_READ) {
            by_slot.entry(self.slot_of_room(&r.id)).or_default().push(r.id.clone());
        }
        for ids in by_slot.into_values() {
            let first = ids[0].clone();
            self.send_for(&first, Cmd::ReadAll(ids), cx);
        }
    }
}

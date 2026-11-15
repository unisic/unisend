//! Double tap on a message: a red heart, as in Instagram and Messenger. Only its badge rises under
//! the bubble: a heart popping over the finger read as kitsch on a frequent action.

use super::*;

impl BeeperApp {
    /// A double tap on message `id`: gives the heart, or takes ours back.
    pub(super) fn double_tap(&mut self, id: &str, cx: &mut Context<Self>) {
        // Where the heart is not taken, the double tap does nothing rather than raise an error.
        let ours = self.msgs.iter().any(|m| m.id == id && m.reactions.iter().any(|r| r.own && menu::same_emoji(&r.key, LIKE)));
        if !ours && !self.caps().reacts_with(LIKE) {
            return;
        }
        self.toggle_reaction(id, LIKE, cx);
        #[cfg(target_os = "android")]
        motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Light);
    }
}

//! Double tap on a message: a red heart, as in Instagram and Messenger. Only its badge rises under
//! the bubble: a heart popping over the finger read as kitsch on a frequent action.

use super::*;

impl BeeperApp {
    /// A double tap on message `id`: gives the heart, or takes ours back.
    pub(super) fn double_tap(&mut self, id: &str, cx: &mut Context<Self>) {
        self.toggle_reaction(id, LIKE, cx);
        #[cfg(target_os = "android")]
        motion::haptic(gpui_mobile::packages::vibration::HapticFeedback::Light);
    }
}

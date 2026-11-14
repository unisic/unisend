//! What a video's event says about how to play it.

/// Whether the sender's network shows this video as a GIF: on repeat and without sound. The
/// mautrix bridges mark those in `info` with `fi.mau.gif` (WhatsApp, Signal, Meta), and WhatsApp
/// and Meta add `fi.mau.loop`. `content` is the event's content.
pub(super) fn looped(content: &serde_json::Value) -> bool {
    let flag = |key| content.pointer(&format!("/info/{key}")).and_then(serde_json::Value::as_bool) == Some(true);
    flag("fi.mau.gif") || flag("fi.mau.loop")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::looped;

    #[test]
    fn bridged_gifs_loop_and_videos_do_not() {
        assert!(looped(&json!({ "msgtype": "m.video", "info": { "fi.mau.gif": true } })));
        assert!(looped(&json!({ "msgtype": "m.video", "info": { "fi.mau.loop": true, "fi.mau.gif": false } })));
        assert!(!looped(&json!({ "msgtype": "m.video", "info": { "duration": 3000 } })));
        assert!(!looped(&json!({ "msgtype": "m.video", "fi.mau.gif": true })), "only inside info");
        assert!(!looped(&json!({ "msgtype": "m.video", "info": { "fi.mau.gif": "true" } })));
        assert!(!looped(&json!({ "msgtype": "m.video" })));
    }
}

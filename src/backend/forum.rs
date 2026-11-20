//! Telegram forums (supergroups with topics), as mautrix-telegram's bridgev2 connector bridges
//! them (v0.2611.0, `pkg/connector/chatinfo.go`, `ids/ids.go`): one Matrix room per topic, all
//! children of a space that stands for the forum. A topic's `m.bridge` names the space in
//! `network`, whose `id` is the portal ID `channel:<id>:-1`; its `m.room.name` is
//! "<topic> - <forum>", and "#General - <forum>" for the General topic. Topic icons are not
//! bridged. The list draws a forum as one row and its topics as chats inside it.

use super::RoomSummary;

/// The forum a topic room belongs to: the `network` id of its `m.bridge`, when the bridge is
/// Telegram's and that id is a forum's (`ids.MakeForumParentPortalID`). A supergroup in a linked
/// community names the community instead, `channel:<id>`, and is no topic.
pub fn forum_id(protocol: Option<&str>, network: Option<&str>) -> Option<String> {
    let network = network?;
    let channel = network.strip_prefix("channel:")?.strip_suffix(":-1")?;
    let is_forum = protocol == Some("telegram")
        && !channel.is_empty()
        && channel.bytes().all(|b| b.is_ascii_digit());
    is_forum.then(|| network.to_owned())
}

/// A topic's own name: the room's name without the " - <forum>" the bridge appends.
pub fn topic_title<'a>(name: &'a str, forum: Option<&str>) -> &'a str {
    forum
        .and_then(|forum| name.strip_suffix(forum)?.strip_suffix(" - "))
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(name)
}

/// What a forum's row shows of its topics: their unread messages and mentions added up, muted
/// only when every topic is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Totals {
    pub unread: u64,
    pub mentions: u64,
    pub marked_unread: bool,
    pub muted: bool,
}

impl Totals {
    pub fn of(topic: &RoomSummary) -> Self {
        Self {
            unread: topic.unread,
            mentions: topic.mentions,
            marked_unread: topic.marked_unread,
            muted: topic.muted,
        }
    }

    pub fn add(&mut self, topic: &RoomSummary) {
        self.unread += topic.unread;
        self.mentions += topic.mentions;
        self.marked_unread |= topic.marked_unread;
        self.muted &= topic.muted;
    }

    pub fn is_unread(&self) -> bool {
        self.unread > 0 || self.marked_unread
    }
}

#[cfg(test)]
mod tests {
    use super::{Totals, forum_id, topic_title};
    use crate::backend::RoomSummary;

    #[test]
    fn only_a_telegram_forum_space_makes_a_topic() {
        assert_eq!(
            forum_id(Some("telegram"), Some("channel:1234:-1")).as_deref(),
            Some("channel:1234:-1")
        );
        // A supergroup under a linked community, a topic's own portal ID, another bridge's server.
        assert_eq!(forum_id(Some("telegram"), Some("channel:1234")), None);
        assert_eq!(forum_id(Some("telegram"), Some("channel:1234:5")), None);
        assert_eq!(forum_id(Some("discordgo"), Some("channel:1234:-1")), None);
        assert_eq!(forum_id(Some("telegram"), Some("channel::-1")), None);
        assert_eq!(forum_id(Some("telegram"), Some("user:1:-1")), None);
        assert_eq!(forum_id(Some("telegram"), None), None);
        assert_eq!(forum_id(None, Some("channel:1234:-1")), None);
    }

    #[test]
    fn a_topic_drops_the_forum_name_the_bridge_appends() {
        assert_eq!(
            topic_title("Releases - Rust PL", Some("Rust PL")),
            "Releases"
        );
        assert_eq!(
            topic_title("#General - Rust PL", Some("Rust PL")),
            "#General"
        );
        assert_eq!(topic_title("A - B - Rust PL", Some("Rust PL")), "A - B");
        // Renamed forum, or a name that is the forum's alone: kept whole.
        assert_eq!(
            topic_title("Releases - Old name", Some("Rust PL")),
            "Releases - Old name"
        );
        assert_eq!(topic_title(" - Rust PL", Some("Rust PL")), " - Rust PL");
        assert_eq!(topic_title("Releases", None), "Releases");
    }

    fn topic(unread: u64, mentions: u64, muted: bool) -> RoomSummary {
        RoomSummary {
            id: "!t:x".try_into().unwrap(),
            name: String::new(),
            preview: None,
            preview_id: None,
            ts: 0,
            network: Some("Telegram".into()),
            parent: Some("Forum".into()),
            forum: Some("channel:1:-1".into()),
            unread,
            mentions,
            dm: false,
            note: false,
            net_blocked: false,
            peer: None,
            pinned: false,
            low_priority: false,
            muted,
            marked_unread: false,
            archived: false,
            order: 0,
            dated: true,
            pending: None,
            moved_to: None,
            own_last: false,
            read_by: Vec::new(),
        }
    }

    #[test]
    fn a_forum_adds_up_its_topics_and_is_muted_only_when_all_are() {
        let mut t = Totals::of(&topic(2, 1, true));
        t.add(&topic(3, 0, false));
        assert_eq!((t.unread, t.mentions, t.muted), (5, 1, false));
        assert!(t.is_unread());
        let mut quiet = Totals::of(&topic(0, 0, true));
        quiet.add(&topic(0, 0, true));
        assert!(quiet.muted && !quiet.is_unread());
        let marked = Totals::of(&RoomSummary {
            marked_unread: true,
            ..topic(0, 0, false)
        });
        assert!(marked.is_unread());
    }
}

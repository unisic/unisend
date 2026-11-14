//! Making a poll and ending one: what is sent, and the limits the bridges put on it. No Matrix
//! client and no UI in here.
//!
//! Sources: MSC3381 and mautrix/go `event/poll.go` (the `org.matrix.msc3381.poll.start` shape);
//! mautrix/go `bridgev2/matrix/connector.go` subscribes to the unstable start and response types
//! only, so a stable `m.poll.start` never reaches a bridge; mautrix/meta `pkg/msgconv/to-meta-poll.go`
//! (at least 2 answers, 1 to 140 UTF-16 units for question and answers, unique answers, `kind` empty or
//! disclosed); mautrix/whatsapp `pkg/msgconv/matrixpoll.go` (non-empty question, unique answers,
//! `max_selections` at or above the answer count means any number).

use serde_json::{Value, json};

pub const MIN_ANSWERS: usize = 2;
/// MSC3381's cap on answers.
pub const MAX_ANSWERS: usize = 20;
/// Messenger's cap, the tightest of the bridges, in UTF-16 code units.
pub const TEXT_MAX: usize = 140;

fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

/// A poll that passed the checks, ready to send.
#[derive(Debug, PartialEq)]
pub struct NewPoll {
    pub question: String,
    pub answers: Vec<String>,
    pub multiple: bool,
}

impl NewPoll {
    /// Trims, drops empty answers (blank rows in the form) and checks what the bridges check.
    pub fn check(question: &str, answers: &[String], multiple: bool) -> Result<Self, &'static str> {
        let question = question.trim();
        if question.is_empty() {
            return Err("Write a question.");
        }
        if units(question) > TEXT_MAX {
            return Err("The question is too long, 140 characters at most.");
        }
        let answers: Vec<String> = answers.iter().map(|a| a.trim().to_owned()).filter(|a| !a.is_empty()).collect();
        if answers.len() < MIN_ANSWERS {
            return Err("Give at least two options.");
        }
        if answers.len() > MAX_ANSWERS {
            return Err("A poll has 20 options at most.");
        }
        if answers.iter().any(|a| units(a) > TEXT_MAX) {
            return Err("An option is too long, 140 characters at most.");
        }
        let lower: Vec<String> = answers.iter().map(|a| a.to_lowercase()).collect();
        if lower.iter().enumerate().any(|(n, a)| lower[..n].contains(a)) {
            return Err("Options must differ from each other.");
        }
        Ok(Self { question: question.to_owned(), answers, multiple })
    }

    /// The event type and content of the start event.
    pub fn event(&self) -> (&'static str, Value) {
        let answers: Vec<Value> = self.answers.iter().enumerate().map(|(n, a)| json!({ "id": format!("a{}", n + 1), "org.matrix.msc1767.text": a })).collect();
        // A client that cannot draw a poll shows this text (MSC3381's fallback).
        let fallback: String = std::iter::once(self.question.clone()).chain(self.answers.iter().enumerate().map(|(n, a)| format!("{}. {a}", n + 1))).collect::<Vec<_>>().join("\n");
        let content = json!({
            "org.matrix.msc1767.text": fallback,
            "org.matrix.msc3381.poll.start": {
                "kind": "org.matrix.msc3381.poll.disclosed",
                "max_selections": if self.multiple { self.answers.len() } else { 1 },
                "question": { "org.matrix.msc1767.text": self.question },
                "answers": answers,
            },
        });
        ("org.matrix.msc3381.poll.start", content)
    }
}

/// Event type and content that ends the poll event `target`. Polls shown in the app are always the
/// unstable kind, so this is too.
pub fn end(target: &str) -> (&'static str, Value) {
    (
        "org.matrix.msc3381.poll.end",
        json!({
            "org.matrix.msc1767.text": "The poll has ended.",
            "org.matrix.msc3381.poll.end": {},
            "m.relates_to": { "rel_type": "m.reference", "event_id": target },
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Poll;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn checks() {
        assert!(NewPoll::check("  ", &s(&["a", "b"]), false).is_err());
        assert!(NewPoll::check("q", &s(&["a", ""]), false).is_err());
        assert!(NewPoll::check("q", &s(&["a", "A "]), false).is_err());
        assert!(NewPoll::check(&"x".repeat(141), &s(&["a", "b"]), false).is_err());
        assert!(NewPoll::check("q", &s(&["a", &"x".repeat(141)]), false).is_err());
        let many: Vec<String> = (0..21).map(|n| n.to_string()).collect();
        assert!(NewPoll::check("q", &many, false).is_err());
        assert!(NewPoll::check("q", &many[..20], false).is_ok());
        // Blank rows are dropped, not an error.
        assert_eq!(NewPoll::check(" q ", &s(&["a", "", " b "]), true).unwrap().answers, ["a", "b"]);
        // Emoji count as two UTF-16 units.
        assert!(NewPoll::check(&"😀".repeat(71), &s(&["a", "b"]), false).is_err());
    }

    #[test]
    fn the_start_event_reads_back() {
        let (ty, content) = NewPoll::check("Lunch?", &s(&["Pizza", "Soup", "Rice"]), true).unwrap().event();
        assert_eq!(ty, "org.matrix.msc3381.poll.start");
        let poll = Poll::parse(&content, true).unwrap();
        assert_eq!((poll.question.as_str(), poll.answers.len(), poll.max_selections), ("Lunch?", 3, 3));
        assert_eq!(poll.answers[1].text, "Soup");
        let (_, single) = NewPoll::check("q", &s(&["a", "b"]), false).unwrap().event();
        assert_eq!(single["org.matrix.msc3381.poll.start"]["max_selections"], 1);
        assert_eq!(content["org.matrix.msc1767.text"], "Lunch?\n1. Pizza\n2. Soup\n3. Rice");
    }

    #[test]
    fn the_end_event_points_at_the_poll() {
        let (ty, c) = end("$p");
        assert_eq!((ty, c["m.relates_to"]["event_id"].as_str(), c["m.relates_to"]["rel_type"].as_str()), ("org.matrix.msc3381.poll.end", Some("$p"), Some("m.reference")));
    }
}

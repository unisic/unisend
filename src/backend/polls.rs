//! Polls and locations: what a message of either kind holds, how it is read from an event, and how
//! a poll's votes are counted. No Matrix client and no UI in here, so all of it can be tested.

use std::collections::HashMap;

use serde_json::{Value, json};

/// The Matrix limit on a poll's answers; more in an event are ignored.
const MAX_ANSWERS: usize = 20;
/// Votes kept per poll: the newest voters. An answer or a vote from a bridge is someone else's data.
const MAX_VOTERS: usize = 1000;
/// Longest question, answer or place description kept.
const TEXT_MAX: usize = 300;
/// Longest answer id kept; ids are only ever echoed back to the server.
const ID_MAX: usize = 200;

#[derive(Clone)]
pub struct Answer {
    pub id: String,
    pub text: String,
}

/// A poll as its start event described it, with the votes counted so far. Votes come from the
/// response events the client has loaded: history is paged backwards from the newest event, so
/// every response newer than a loaded poll is loaded too, but a poll cannot see older ones.
#[derive(Clone)]
pub struct Poll {
    pub question: String,
    pub answers: Vec<Answer>,
    pub max_selections: usize,
    /// The start event used the `org.matrix.msc3381` names; a vote is sent the same way.
    pub unstable: bool,
    /// When it ended, ms since the epoch.
    pub ended: Option<u64>,
    /// Voter's user ID to the time of their newest response and the answers it chose.
    votes: HashMap<String, (u64, Vec<String>)>,
}

#[derive(Clone)]
pub enum Update {
    Vote(Vec<String>),
    End,
}

/// A poll event read from raw JSON.
pub enum Event {
    Start(Poll),
    Update { target: String, update: Update },
}

#[derive(Clone)]
pub struct Place {
    pub lat: f64,
    pub lon: f64,
    /// Empty when the message only gave the coordinates.
    pub description: String,
}

fn clip(text: &str) -> String {
    text.trim().chars().take(TEXT_MAX).collect()
}

/// The text of a question or answer: the stable `m.text` list, or the unstable plain string.
fn text_of(v: &Value) -> String {
    let list = |key: &str| {
        let parts = v.get(key)?.as_array()?;
        let part = parts.iter().find(|p| p.get("mimetype").and_then(Value::as_str).is_none_or(|m| m == "text/plain"))?;
        Some(clip(part.get("body")?.as_str()?))
    };
    let plain = |key: &str| v.get(key)?.as_str().map(clip);
    list("m.text").or_else(|| plain("org.matrix.msc1767.text")).or_else(|| list("org.matrix.msc1767.message")).unwrap_or_default()
}

impl Poll {
    fn parse(content: &Value, unstable: bool) -> Option<Self> {
        let block = content.get(if unstable { "org.matrix.msc3381.poll.start" } else { "m.poll" })?;
        let question = text_of(block.get("question")?);
        let mut answers: Vec<Answer> = Vec::new();
        for a in block.get("answers")?.as_array()?.iter() {
            let id = a.get(if unstable { "id" } else { "m.id" }).and_then(Value::as_str).filter(|id| !id.is_empty() && id.len() <= ID_MAX);
            let text = text_of(a);
            if let Some(id) = id.filter(|id| !text.is_empty() && answers.iter().all(|x| x.id != *id)) {
                answers.push(Answer { id: id.to_owned(), text });
            }
            if answers.len() == MAX_ANSWERS {
                break;
            }
        }
        let max = block.get("max_selections").and_then(Value::as_u64).unwrap_or(1).max(1);
        let max_selections = usize::try_from(max).unwrap_or(usize::MAX);
        (!question.is_empty() && !answers.is_empty()).then_some(Self { question, answers, max_selections, unstable, ended: None, votes: HashMap::new() })
    }

    /// Applies a response or an end that `from` sent at `ts`. `by_creator` is whether `from` sent
    /// the poll: only they may end it.
    pub fn update(&mut self, from: &str, ts: u64, update: &Update, by_creator: bool) {
        match update {
            Update::Vote(chosen) => {
                if self.ended.is_some_and(|end| ts > end) {
                    return;
                }
                // More answers than allowed spoils the response, which then counts as no vote.
                let mut valid: Vec<String> = Vec::new();
                for id in chosen {
                    if self.answers.iter().any(|a| a.id == *id) && !valid.contains(id) {
                        valid.push(id.clone());
                    }
                }
                if valid.len() > self.max_selections {
                    valid.clear();
                }
                if self.votes.get(from).is_some_and(|(at, _)| *at > ts) {
                    return;
                }
                if !self.votes.contains_key(from) && self.votes.len() >= MAX_VOTERS {
                    let oldest = self.votes.iter().min_by_key(|(_, (at, _))| *at).map(|(who, _)| who.clone());
                    if let Some(who) = oldest {
                        self.votes.remove(&who);
                    }
                }
                self.votes.insert(from.to_owned(), (ts, valid));
            }
            Update::End if by_creator => {
                let end = self.ended.map_or(ts, |e| e.min(ts));
                self.ended = Some(end);
                // shortcut: a vote sent after the end that already replaced an earlier one is lost
                // with it; upgrade by keeping each voter's history if that ever shows.
                self.votes.retain(|_, (at, _)| *at <= end);
            }
            Update::End => {}
        }
    }

    /// Votes per answer, in the order of `answers`, and how many people voted.
    pub fn tally(&self) -> (Vec<usize>, usize) {
        let counts = self.answers.iter().map(|a| self.votes.values().filter(|(_, v)| v.contains(&a.id)).count()).collect();
        (counts, self.votes.values().filter(|(_, v)| !v.is_empty()).count())
    }

    pub fn choice(&self, who: &str) -> &[String] {
        self.votes.get(who).map_or(&[], |(_, v)| v)
    }

    /// The answers `who` would have after touching `answer`; none when that would be too many.
    pub fn touch(&self, who: &str, answer: &str) -> Option<Vec<String>> {
        let now = self.choice(who);
        if now.iter().any(|a| a == answer) {
            return Some(now.iter().filter(|a| *a != answer).cloned().collect());
        }
        if self.max_selections == 1 {
            return Some(vec![answer.to_owned()]);
        }
        (now.len() < self.max_selections).then(|| now.iter().cloned().chain([answer.to_owned()]).collect())
    }
}

/// Event type and content of a vote for the poll event `target`.
pub fn response(unstable: bool, target: &str, answers: Vec<String>) -> (&'static str, Value) {
    let relates = json!({ "rel_type": "m.reference", "event_id": target });
    if unstable {
        ("org.matrix.msc3381.poll.response", json!({ "org.matrix.msc3381.poll.response": { "answers": answers }, "m.relates_to": relates }))
    } else {
        ("m.poll.response", json!({ "m.selections": answers, "m.relates_to": relates }))
    }
}

/// Whether an event of this type is worth reading with `read`.
pub fn is_poll(kind: &str) -> bool {
    kind.starts_with("m.poll.") || kind.starts_with("org.matrix.msc3381.poll.")
}

/// Reads a poll start, response or end from an event's type and content; `None` for anything else.
pub fn read(kind: &str, content: &Value) -> Option<Event> {
    let unstable = match kind {
        "m.poll.start" => false,
        "org.matrix.msc3381.poll.start" => true,
        kind @ ("m.poll.response" | "org.matrix.msc3381.poll.response" | "m.poll.end" | "org.matrix.msc3381.poll.end") => {
            let relates = content.get("m.relates_to")?;
            if relates.get("rel_type")?.as_str()? != "m.reference" {
                return None;
            }
            let target = relates.get("event_id")?.as_str()?.to_owned();
            let update = if kind.ends_with(".end") {
                Update::End
            } else {
                let chosen = if kind.starts_with("m.") { content.get("m.selections") } else { content.get("org.matrix.msc3381.poll.response")?.get("answers") };
                let ids = chosen.and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
                Update::Vote(ids.iter().filter_map(Value::as_str).take(MAX_ANSWERS + 1).map(str::to_owned).collect())
            };
            return Some(Event::Update { target, update });
        }
        _ => return None,
    };
    Poll::parse(content, unstable).map(Event::Start)
}

impl Place {
    /// From a `geo:` URI (RFC 5870: `geo:<lat>,<lon>[,<alt>][;parameters]`) and the message's
    /// description; `None` when the coordinates are missing or out of range.
    pub fn parse(geo_uri: &str, description: &str) -> Option<Self> {
        let uri = geo_uri.trim();
        let rest = uri.get(4..).filter(|_| uri[..4].eq_ignore_ascii_case("geo:"))?;
        let mut parts = rest.split(';').next()?.split(',');
        let (lat, lon): (f64, f64) = (parts.next()?.trim().parse().ok()?, parts.next()?.trim().parse().ok()?);
        if !(lat.is_finite() && lon.is_finite() && lat.abs() <= 90. && lon.abs() <= 180.) {
            return None;
        }
        let text = clip(description);
        let bare = text.starts_with("geo:") || text.eq_ignore_ascii_case("location") || text.chars().all(|c| c.is_ascii_digit() || " .,;-+".contains(c));
        Some(Self { lat, lon, description: if bare { String::new() } else { text } })
    }

    pub fn coordinates(&self) -> String {
        format!("{:.5}, {:.5}", self.lat, self.lon)
    }

    /// For the user's maps app on Android.
    #[cfg(any(target_os = "android", test))]
    pub fn geo_url(&self) -> String {
        format!("geo:{lat:.5},{lon:.5}?q={lat:.5},{lon:.5}", lat = self.lat, lon = self.lon)
    }

    pub fn web_url(&self) -> String {
        format!("https://www.openstreetmap.org/?mlat={lat:.5}&mlon={lon:.5}#map=16/{lat:.5}/{lon:.5}", lat = self.lat, lon = self.lon)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poll(max: u64, unstable: bool) -> Poll {
        let content = if unstable {
            json!({ "org.matrix.msc3381.poll.start": { "max_selections": max, "question": { "org.matrix.msc1767.text": "Lunch?" },
                "answers": [{ "id": "a", "org.matrix.msc1767.text": "Pizza" }, { "id": "b", "org.matrix.msc1767.text": "Sushi" }, { "id": "c", "org.matrix.msc1767.text": "Soup" }] } })
        } else {
            json!({ "m.poll": { "max_selections": max, "question": { "m.text": [{ "body": "Lunch?" }] },
                "answers": [{ "m.id": "a", "m.text": [{ "body": "Pizza" }] }, { "m.id": "b", "m.text": [{ "body": "Sushi" }] }, { "m.id": "c", "m.text": [{ "body": "Soup" }] }] } })
        };
        match read(if unstable { "org.matrix.msc3381.poll.start" } else { "m.poll.start" }, &content) {
            Some(Event::Start(p)) => p,
            _ => panic!("not a poll"),
        }
    }

    fn vote(p: &mut Poll, who: &str, ts: u64, ids: &[&str]) {
        p.update(who, ts, &Update::Vote(ids.iter().map(|s| (*s).to_owned()).collect()), false);
    }

    #[test]
    fn reads_both_flavours() {
        for unstable in [false, true] {
            let p = poll(1, unstable);
            assert_eq!((p.question.as_str(), p.answers.len(), p.max_selections, p.unstable), ("Lunch?", 3, 1, unstable));
            assert_eq!(p.answers[1].text, "Sushi");
        }
    }

    #[test]
    fn caps_answers_and_text() {
        let answers: Vec<_> = (0..30).map(|n| json!({ "m.id": n.to_string(), "m.text": [{ "body": "x".repeat(1000) }] })).collect();
        let content = json!({ "m.poll": { "question": { "m.text": [{ "body": "q" }] }, "answers": answers } });
        let Some(Event::Start(p)) = read("m.poll.start", &content) else { panic!("not a poll") };
        assert_eq!((p.answers.len(), p.answers[0].text.len()), (MAX_ANSWERS, TEXT_MAX));
    }

    #[test]
    fn newest_response_replaces_and_empty_withdraws() {
        let mut p = poll(1, false);
        vote(&mut p, "@u:x", 10, &["a"]);
        vote(&mut p, "@u:x", 20, &["b"]);
        assert_eq!(p.tally(), (vec![0, 1, 0], 1));
        vote(&mut p, "@u:x", 15, &["c"]);
        assert_eq!(p.choice("@u:x"), ["b"]);
        vote(&mut p, "@u:x", 30, &[]);
        assert_eq!(p.tally(), (vec![0, 0, 0], 0));
    }

    #[test]
    fn unknown_answers_ignored_and_too_many_spoil() {
        let mut p = poll(2, true);
        vote(&mut p, "@u:x", 1, &["zzz", "a"]);
        assert_eq!(p.choice("@u:x"), ["a"]);
        vote(&mut p, "@u:x", 2, &["a", "b", "c"]);
        assert_eq!(p.tally().1, 0);
    }

    #[test]
    fn a_response_changes_only_its_sender() {
        let mut p = poll(1, false);
        vote(&mut p, "@a:x", 1, &["a"]);
        vote(&mut p, "@b:x", 2, &["b"]);
        vote(&mut p, "@b:x", 3, &[]);
        assert_eq!(p.choice("@a:x"), ["a"]);
    }

    #[test]
    fn only_the_creator_ends_and_later_votes_do_not_count() {
        let mut p = poll(1, false);
        vote(&mut p, "@a:x", 5, &["a"]);
        p.update("@b:x", 6, &Update::End, false);
        assert!(p.ended.is_none());
        p.update("@c:x", 10, &Update::End, true);
        vote(&mut p, "@b:x", 11, &["b"]);
        vote(&mut p, "@b:x", 9, &["c"]);
        assert_eq!(p.tally(), (vec![1, 0, 1], 2));
    }

    #[test]
    fn end_arriving_after_a_late_vote_drops_it() {
        let mut p = poll(1, false);
        vote(&mut p, "@a:x", 20, &["a"]);
        p.update("@c:x", 10, &Update::End, true);
        assert_eq!(p.tally().1, 0);
    }

    #[test]
    fn voters_are_bounded() {
        let mut p = poll(1, false);
        for n in 0..MAX_VOTERS + 50 {
            vote(&mut p, &format!("@{n}:x"), n as u64, &["a"]);
        }
        assert_eq!(p.tally().1, MAX_VOTERS);
        assert!(p.choice("@0:x").is_empty() && !p.choice("@1049:x").is_empty());
    }

    #[test]
    fn touching_toggles() {
        let mut one = poll(1, false);
        assert_eq!(one.touch("@u:x", "a"), Some(vec!["a".to_owned()]));
        vote(&mut one, "@u:x", 1, &["a"]);
        assert_eq!(one.touch("@u:x", "a"), Some(vec![]));
        assert_eq!(one.touch("@u:x", "b"), Some(vec!["b".to_owned()]));
        let mut two = poll(2, false);
        vote(&mut two, "@u:x", 1, &["a", "b"]);
        assert_eq!(two.touch("@u:x", "c"), None);
        assert_eq!(two.touch("@u:x", "a"), Some(vec!["b".to_owned()]));
    }

    #[test]
    fn reads_responses_and_ends() {
        let relates = json!({ "rel_type": "m.reference", "event_id": "$p" });
        let stable = json!({ "m.selections": ["a"], "m.relates_to": relates });
        let unstable = json!({ "m.relates_to": relates });
        assert!(matches!(read("m.poll.response", &stable), Some(Event::Update { target, update: Update::Vote(v) }) if target == "$p" && v == ["a"]));
        assert!(matches!(read("org.matrix.msc3381.poll.end", &unstable), Some(Event::Update { update: Update::End, .. })));
        assert!(read("m.room.message", &json!({})).is_none() && !is_poll("m.room.message"));
    }

    #[test]
    fn response_has_the_polls_flavour() {
        let (ty, body) = response(true, "$p", vec!["a".into()]);
        assert_eq!((ty, body["org.matrix.msc3381.poll.response"]["answers"][0].as_str()), ("org.matrix.msc3381.poll.response", Some("a")));
        let (ty, body) = response(false, "$p", vec![]);
        assert_eq!((ty, body["m.selections"].as_array().map(Vec::len)), ("m.poll.response", Some(0)));
        assert_eq!(body["m.relates_to"]["event_id"], "$p");
    }

    #[test]
    fn places() {
        let p = Place::parse("geo:52.5200,13.4050;u=35", "Brandenburg Gate").unwrap();
        assert_eq!((p.coordinates().as_str(), p.description.as_str()), ("52.52000, 13.40500", "Brandenburg Gate"));
        assert_eq!(p.web_url(), "https://www.openstreetmap.org/?mlat=52.52000&mlon=13.40500#map=16/52.52000/13.40500");
        assert_eq!(p.geo_url(), "geo:52.52000,13.40500?q=52.52000,13.40500");
        assert!(Place::parse("GEO:-33.9,151.2,10;crs=wgs84", "-33.9,151.2").unwrap().description.is_empty());
        for bad in ["", "geo:", "geo:91,0", "geo:0,181", "geo:a,b", "geo:NaN,1", "geo:1", "https://x.test/?1,2"] {
            assert!(Place::parse(bad, "x").is_none(), "{bad}");
        }
    }
}

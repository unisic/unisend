//! Polls and locations: what a message of either kind holds and how it is read from an event. No
//! Matrix client and no UI in here, so all of it can be tested.

use std::collections::HashMap;

use serde_json::{Value, json};

/// The Matrix limit on a poll's answers; more in an event are ignored.
const MAX_ANSWERS: usize = 20;
/// Voters kept per poll. An answer or a vote from a bridge is someone else's data.
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

/// A poll as its start event described it. In the open conversation it also holds the votes, which
/// the SDK's timeline has counted from the responses it has seen.
#[derive(Clone)]
pub struct Poll {
    pub question: String,
    pub answers: Vec<Answer>,
    pub max_selections: usize,
    /// The start event used the `org.matrix.msc3381` names; a vote is sent the same way.
    pub unstable: bool,
    /// When it ended, ms since the epoch.
    pub ended: Option<u64>,
    /// Voter's user ID to the answers they chose.
    votes: HashMap<String, Vec<String>>,
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
    /// From the content of a start event of either flavor.
    pub(super) fn parse(content: &Value, unstable: bool) -> Option<Self> {
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

    /// A poll whose votes are already counted: `votes` is each answer's ID with its voters.
    pub(super) fn counted(
        question: &str,
        answers: impl IntoIterator<Item = (String, String)>,
        max_selections: u64,
        ended: Option<u64>,
        chosen: impl IntoIterator<Item = (String, Vec<String>)>,
    ) -> Self {
        let answers: Vec<Answer> = answers.into_iter().take(MAX_ANSWERS).map(|(id, text)| Answer { id, text: clip(&text) }).collect();
        let mut votes: HashMap<String, Vec<String>> = HashMap::new();
        for (answer, voters) in chosen {
            for voter in voters {
                if votes.len() < MAX_VOTERS || votes.contains_key(&voter) {
                    votes.entry(voter).or_default().push(answer.clone());
                }
            }
        }
        let max_selections = usize::try_from(max_selections.max(1)).unwrap_or(usize::MAX);
        // The timeline reads only the `org.matrix.msc3381` events, so a vote goes out as one.
        Self { question: clip(question), answers, max_selections, unstable: true, ended, votes }
    }

    /// Votes per answer, in the order of `answers`, and how many people voted.
    pub fn tally(&self) -> (Vec<usize>, usize) {
        let counts = self.answers.iter().map(|a| self.votes.values().filter(|v| v.contains(&a.id)).count()).collect();
        (counts, self.votes.values().filter(|v| !v.is_empty()).count())
    }

    pub fn choice(&self, who: &str) -> &[String] {
        self.votes.get(who).map_or(&[], |v| v)
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

/// Whether an event of this type starts a poll.
pub fn is_start(kind: &str) -> bool {
    matches!(kind, "m.poll.start" | "org.matrix.msc3381.poll.start")
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
        Poll::parse(&content, unstable).expect("a poll")
    }

    fn counted(max: u64, chosen: &[(&str, &[&str])]) -> Poll {
        let answers = [("a", "Pizza"), ("b", "Sushi"), ("c", "Soup")].map(|(id, text)| (id.to_owned(), text.to_owned()));
        let chosen = chosen.iter().map(|(answer, voters)| ((*answer).to_owned(), voters.iter().map(|v| (*v).to_owned()).collect()));
        Poll::counted("Lunch?", answers, max, None, chosen)
    }

    #[test]
    fn reads_both_flavors() {
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
        let p = Poll::parse(&content, false).expect("a poll");
        assert_eq!((p.answers.len(), p.answers[0].text.len()), (MAX_ANSWERS, TEXT_MAX));
    }

    #[test]
    fn counted_votes_are_tallied_per_answer_and_per_person() {
        let p = counted(2, &[("a", &["@u:x", "@v:x"]), ("b", &["@u:x"]), ("c", &[])]);
        assert_eq!(p.tally(), (vec![2, 1, 0], 2));
        assert_eq!(p.choice("@u:x"), ["a", "b"]);
        assert!(p.choice("@nobody:x").is_empty() && p.unstable);
    }

    #[test]
    fn voters_are_bounded() {
        let voters: Vec<String> = (0..MAX_VOTERS + 50).map(|n| format!("@{n}:x")).collect();
        let p = Poll::counted("q", [("a".to_owned(), "A".to_owned())], 1, None, [("a".to_owned(), voters)]);
        assert_eq!(p.tally().1, MAX_VOTERS);
    }

    #[test]
    fn touching_toggles() {
        assert_eq!(counted(1, &[]).touch("@u:x", "a"), Some(vec!["a".to_owned()]));
        let one = counted(1, &[("a", &["@u:x"])]);
        assert_eq!(one.touch("@u:x", "a"), Some(vec![]));
        assert_eq!(one.touch("@u:x", "b"), Some(vec!["b".to_owned()]));
        let two = counted(2, &[("a", &["@u:x"]), ("b", &["@u:x"])]);
        assert_eq!(two.touch("@u:x", "c"), None);
        assert_eq!(two.touch("@u:x", "a"), Some(vec!["b".to_owned()]));
        assert!(is_start("m.poll.start") && is_start("org.matrix.msc3381.poll.start") && !is_start("m.poll.response"));
    }

    #[test]
    fn response_has_the_polls_flavor() {
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

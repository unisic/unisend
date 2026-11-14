//! What leaves the composer: CommonMark turned into Matrix HTML the way Element sends it, and
//! people picked from the mention list as pills plus `m.mentions`.
//!
//! The mautrix bridges read `formatted_body` and drop newlines in its text, so a line break is
//! `<br>`; WhatsApp and Signal turn a pill into a native mention only when its user is also in
//! `m.mentions`, and Meta writes `@` before the name itself, so the pill's text is the bare name.

use markdown::mdast::Node;
use markdown::{Constructs, ParseOptions};
use matrix_sdk::Room;
use matrix_sdk::RoomMemberships;
use matrix_sdk::ruma::events::Mentions;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContentWithoutRelation;
use matrix_sdk::ruma::{OwnedUserId, UserId};

/// Members handed to the mention list; a bigger chat is cut to its first ones.
pub(super) const MAX_MEMBERS: usize = 1000;

/// Joined members of a chat other than `own`, as (user, name shown in the chat).
pub(super) async fn members(room: &Room, own: &UserId) -> anyhow::Result<Vec<(OwnedUserId, String)>> {
    let mut list: Vec<(OwnedUserId, String)> = room
        .members(RoomMemberships::JOIN)
        .await?
        .into_iter()
        .filter(|m| m.user_id() != own)
        .take(MAX_MEMBERS)
        .map(|m| (m.user_id().to_owned(), m.name().to_owned()))
        .collect();
    list.sort_by_cached_key(|(_, name)| name.to_lowercase());
    Ok(list)
}

/// The message for `body` as typed. `picked` are the people chosen from the mention list, as
/// (name, user); each `@name` still in the text becomes a pill.
pub(super) fn content(body: &str, picked: &[(String, OwnedUserId)]) -> RoomMessageEventContentWithoutRelation {
    let pills = find_pills(body, picked);
    let users: Vec<OwnedUserId> = pills.iter().map(|p| picked[p.who].1.clone()).collect();
    let msg = match html(body, &pills, picked) {
        Some(html) => RoomMessageEventContentWithoutRelation::text_html(body, html),
        None => RoomMessageEventContentWithoutRelation::text_plain(body),
    };
    // Sent even when empty, as Element does: it tells the server the message mentions no one else.
    msg.add_mentions(Mentions::with_user_ids(users))
}

struct Pill {
    at: std::ops::Range<usize>,
    who: usize,
}

/// Each `@name` of a picked person that stands as a word of its own, longest names first.
fn find_pills(body: &str, picked: &[(String, OwnedUserId)]) -> Vec<Pill> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut order: Vec<usize> = (0..picked.len()).filter(|&i| !picked[i].0.is_empty()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(picked[i].0.len()));
    let mut found: Vec<Pill> = Vec::new();
    for who in order {
        let tag = format!("@{}", picked[who].0);
        for (start, _) in body.match_indices(&tag) {
            let at = start..start + tag.len();
            let alone = !body[..start].ends_with(word) && !body[at.end..].starts_with(word);
            if alone && found.iter().all(|p| p.at.end <= at.start || at.end <= p.at.start) {
                found.push(Pill { at, who });
            }
        }
    }
    found.sort_by_key(|p| p.at.start);
    found
}

/// Stands in for a pill while the text is parsed: private-use characters, which Markdown reads as
/// letters, around the pill's index.
const OPEN: char = '\u{E000}';
const CLOSE: char = '\u{E001}';

/// The HTML form of `body`, or None when it is plain text with no pill.
fn html(body: &str, pills: &[Pill], picked: &[(String, OwnedUserId)]) -> Option<String> {
    let mut marked = String::with_capacity(body.len());
    let mut from = 0;
    for (i, pill) in pills.iter().enumerate() {
        marked.push_str(&body[from..pill.at.start]);
        marked.push(OPEN);
        marked.push_str(&i.to_string());
        marked.push(CLOSE);
        from = pill.at.end;
    }
    marked.push_str(&body[from..]);

    let options = ParseOptions {
        constructs: Constructs {
            gfm_strikethrough: true,
            // Typed HTML stays text; images would need an upload; reference definitions are rare
            // and turn a line into nothing.
            html_flow: false,
            html_text: false,
            label_start_image: false,
            definition: false,
            ..Constructs::default()
        },
        gfm_strikethrough_single_tilde: false,
        ..ParseOptions::default()
    };
    let tree = markdown::to_mdast(&marked, &options).ok()?;
    let Node::Root(root) = &tree else { return None };
    let names: Vec<(&str, &UserId)> = pills.iter().map(|p| (picked[p.who].0.as_str(), picked[p.who].1.as_ref())).collect();
    let html = Html { pills: &names };
    // One paragraph goes out without its `<p>`, as the spec's examples and Element do.
    let children = match root.children.as_slice() {
        [Node::Paragraph(p)] => {
            let plain = p.children.iter().all(|n| matches!(n, Node::Text(_)));
            if pills.is_empty() && plain && tree.to_string() == marked {
                return None;
            }
            &p.children
        }
        all => all,
    };
    let mut out = String::with_capacity(body.len() * 2);
    for node in children {
        html.node(node, &mut out, false, false);
    }
    Some(out)
}

struct Html<'a> {
    pills: &'a [(&'a str, &'a UserId)],
}

impl Html<'_> {
    fn node(&self, node: &Node, out: &mut String, tight: bool, in_link: bool) {
        let wrap = |out: &mut String, tag: &str, kids: &[Node]| {
            out.push_str(&format!("<{tag}>"));
            self.kids(kids, out, tight, in_link);
            out.push_str(&format!("</{tag}>"));
        };
        match node {
            Node::Paragraph(n) if tight => self.kids(&n.children, out, tight, in_link),
            Node::Paragraph(n) => wrap(out, "p", &n.children),
            Node::Text(t) => self.text(&t.value, out, in_link),
            Node::Strong(n) => wrap(out, "strong", &n.children),
            Node::Emphasis(n) => wrap(out, "em", &n.children),
            Node::Delete(n) => wrap(out, "del", &n.children),
            Node::Blockquote(n) => {
                out.push_str("<blockquote>");
                self.kids(&n.children, out, false, in_link);
                out.push_str("</blockquote>");
            }
            Node::Heading(n) => wrap(out, &format!("h{}", n.depth.clamp(1, 6)), &n.children),
            Node::List(l) => {
                match (l.ordered, l.start) {
                    (true, Some(start)) if start != 1 => out.push_str(&format!("<ol start=\"{start}\">")),
                    (true, _) => out.push_str("<ol>"),
                    (false, _) => out.push_str("<ul>"),
                }
                for item in &l.children {
                    out.push_str("<li>");
                    let kids = item.children().map(Vec::as_slice).unwrap_or_default();
                    self.kids(kids, out, !l.spread, in_link);
                    out.push_str("</li>");
                }
                out.push_str(if l.ordered { "</ol>" } else { "</ul>" });
            }
            Node::InlineCode(c) => {
                out.push_str("<code>");
                escape(&self.restore(&c.value), out);
                out.push_str("</code>");
            }
            Node::Code(c) => {
                let lang: Option<&str> = c.lang.as_deref().filter(|l| l.chars().all(|ch| ch.is_ascii_alphanumeric() || "+-_#.".contains(ch)));
                match lang {
                    Some(lang) => out.push_str(&format!("<pre><code class=\"language-{lang}\">")),
                    None => out.push_str("<pre><code>"),
                }
                escape(&self.restore(&c.value), out);
                out.push_str("</code></pre>");
            }
            Node::Break(_) => out.push_str("<br>"),
            Node::ThematicBreak(_) => out.push_str("<hr>"),
            Node::Link(l) if !in_link && safe_link(&l.url) => {
                out.push_str("<a href=\"");
                escape(&l.url, out);
                out.push_str("\">");
                self.kids(&l.children, out, tight, true);
                out.push_str("</a>");
            }
            Node::Link(l) => self.kids(&l.children, out, tight, in_link),
            other => match other.children() {
                Some(kids) => self.kids(kids, out, tight, in_link),
                None => self.text(&other.to_string(), out, in_link),
            },
        }
    }

    fn kids(&self, kids: &[Node], out: &mut String, tight: bool, in_link: bool) {
        for kid in kids {
            self.node(kid, out, tight, in_link);
        }
    }

    /// Text with its line breaks as `<br>` and its pill marks as links (as `@name` inside a link).
    fn text(&self, text: &str, out: &mut String, in_link: bool) {
        let mut rest = text;
        while let Some(open) = rest.find(OPEN) {
            let (before, after) = rest.split_at(open);
            self.lines(before, out);
            let after = &after[OPEN.len_utf8()..];
            let Some(close) = after.find(CLOSE) else {
                rest = after;
                continue;
            };
            match after[..close].parse::<usize>().ok().and_then(|i| self.pills.get(i)) {
                Some((name, user)) if !in_link => {
                    out.push_str("<a href=\"https://matrix.to/#/");
                    escape(user.as_str(), out);
                    out.push_str("\">");
                    escape(name, out);
                    out.push_str("</a>");
                }
                Some((name, _)) => {
                    out.push('@');
                    escape(name, out);
                }
                None => {}
            }
            rest = &after[close + CLOSE.len_utf8()..];
        }
        self.lines(rest, out);
    }

    fn lines(&self, text: &str, out: &mut String) {
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                out.push_str("<br>");
            }
            escape(line, out);
        }
    }

    /// Pill marks back to `@name`, for code, where nothing is a link.
    fn restore(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(open) = rest.find(OPEN) {
            out.push_str(&rest[..open]);
            let after = &rest[open + OPEN.len_utf8()..];
            let Some(close) = after.find(CLOSE) else {
                rest = after;
                continue;
            };
            if let Some((name, _)) = after[..close].parse::<usize>().ok().and_then(|i| self.pills.get(i)) {
                out.push('@');
                out.push_str(name);
            }
            rest = &after[close + CLOSE.len_utf8()..];
        }
        out.push_str(rest);
        out
    }
}

/// Links a reader can follow without harm; anything else keeps only its text.
fn safe_link(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    ["https://", "http://", "mailto:", "matrix:"].iter().any(|scheme| lower.starts_with(scheme))
}

fn escape(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::events::room::message::MessageType;
    use matrix_sdk::ruma::owned_user_id;

    use super::*;

    fn sent(body: &str, picked: &[(String, OwnedUserId)]) -> (Option<String>, Vec<OwnedUserId>) {
        let msg = content(body, picked);
        let MessageType::Text(text) = msg.msgtype else { panic!("not text") };
        assert_eq!(text.body, body, "the body is what was typed");
        (text.formatted.map(|f| f.body), msg.mentions.expect("m.mentions is always sent").user_ids.into_iter().collect())
    }

    #[test]
    fn plain_text_has_no_html() {
        for body in ["hello", "two\nlines", "5 * 3 = 15", "snake_case_name", "~5 min", "a <b> c", "see https://example.com", "#tag"] {
            assert_eq!(sent(body, &[]), (None, vec![]), "{body}");
        }
    }

    #[test]
    fn markdown_becomes_html() {
        let cases = [
            ("**bold** and _it_", "<strong>bold</strong> and <em>it</em>"),
            ("~~gone~~ `x < y`", "<del>gone</del> <code>x &lt; y</code>"),
            ("```rust\nfn a() {}\n```", "<pre><code class=\"language-rust\">fn a() {}</code></pre>"),
            ("> quoted\n\nreply", "<blockquote><p>quoted</p></blockquote><p>reply</p>"),
            ("[site](https://example.com)", "<a href=\"https://example.com\">site</a>"),
            ("[bad](javascript:alert(1))", "bad"),
            ("- one\n- two", "<ul><li>one</li><li>two</li></ul>"),
            ("3. three\n4. four", "<ol start=\"3\"><li>three</li><li>four</li></ol>"),
            ("*a*\nb", "<em>a</em><br>b"),
            ("**<script>**", "<strong>&lt;script&gt;</strong>"),
        ];
        for (body, html) in cases {
            assert_eq!(sent(body, &[]).0.as_deref(), Some(html), "{body}");
        }
    }

    #[test]
    fn picked_people_become_pills() {
        let ann = owned_user_id!("@ann:example.org");
        let bo = owned_user_id!("@bo:example.org");
        let picked = vec![("Ann Lee".to_owned(), ann.clone()), ("Bo".to_owned(), bo.clone())];
        let (html, users) = sent("hi @Ann Lee and **@Bo**", &picked);
        assert_eq!(
            html.as_deref(),
            Some("hi <a href=\"https://matrix.to/#/@ann:example.org\">Ann Lee</a> and <strong><a href=\"https://matrix.to/#/@bo:example.org\">Bo</a></strong>")
        );
        assert_eq!(users, [ann.clone(), bo]);
        // A name deleted from the text, or inside a longer word, mentions no one.
        assert_eq!(sent("hi @Bob", &picked[1..]), (None, vec![]));
        let (html, _) = sent("`@Ann Lee`", &picked);
        assert_eq!(html.as_deref(), Some("<code>@Ann Lee</code>"));
    }
}

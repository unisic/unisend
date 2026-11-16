//! A message's `formatted_body` (HTML) as a small styled model, and its plain text.
//!
//! Bridges send HTML next to a markdown-ish plain `body`. The model keeps what the bubble can
//! draw: bold, italic, strikethrough, underline, inline code, links, quotes, code blocks, lists.
//! Everything else keeps its text. Spoilers (`data-mx-spoiler`) are plain text, not hidden.

use matrix_sdk::ruma::html::{Html, NodeData, NodeRef};

/// Longest HTML parsed, and deepest nesting followed; a message past either keeps its plain body
/// or, for depth, only the text below that level.
const MAX_HTML: usize = 64 * 1024;
const MAX_DEPTH: usize = 24;

#[derive(Clone, Debug, PartialEq)]
pub struct Rich {
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Para(Vec<Run>),
    Quote(Vec<Block>),
    /// Verbatim text, and where it starts in the plain text.
    Code(String, usize),
    /// `start` is the first number of an ordered list; `None` is a bulleted one.
    List { start: Option<u64>, items: Vec<Vec<Block>> },
}

/// Text in one style. `at` is where the text starts in the message's plain text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Run {
    pub text: String,
    pub at: usize,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub underline: bool,
    pub code: bool,
    pub href: Option<String>,
}

#[derive(Clone, Default)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    code: bool,
    href: Option<String>,
}

#[derive(Default)]
struct Builder {
    plain: String,
    /// The next text starts a new block: a line break first, unless the plain text is empty.
    apart: bool,
}

impl Builder {
    fn start(&mut self) {
        if self.apart && !self.plain.is_empty() && !self.plain.ends_with('\n') {
            self.plain.push('\n');
        }
        self.apart = false;
    }
}

/// The plain text and the model of an HTML message body; `None` when it holds no text.
pub fn parse(html: &str) -> Option<(String, Rich)> {
    if html.len() > MAX_HTML {
        return None;
    }
    let doc = Html::parse(html);
    let mut b = Builder::default();
    let (mut blocks, mut cur) = (Vec::new(), Vec::new());
    for child in doc.children() {
        walk(&child, &Style::default(), 0, &mut blocks, &mut cur, &mut b);
    }
    flush(&mut blocks, &mut cur, &mut b);
    (!b.plain.is_empty()).then_some((b.plain, Rich { blocks }))
}

/// An emote ("/me waves") as the others read it: "* Ann waves", all in italic. `body` and `rich`
/// gain the prefix; returns its length, by which the caller moves its own ranges into `body`.
pub fn emote(actor: &str, body: &mut String, rich: &mut Option<Rich>) -> usize {
    let prefix = format!("* {actor} ");
    let n = prefix.len();
    let mut blocks = match rich.take() {
        Some(rich) => rich.blocks,
        None => vec![Block::Para(vec![Run { text: body.clone(), ..Run::default() }])],
    };
    shift(&mut blocks, n);
    let lead = Run { text: prefix.clone(), italic: true, ..Run::default() };
    match blocks.first_mut() {
        Some(Block::Para(runs)) => runs.insert(0, lead),
        _ => blocks.insert(0, Block::Para(vec![lead])),
    }
    body.insert_str(0, &prefix);
    *rich = Some(Rich { blocks });
    n
}

/// Moves every offset `n` bytes on and sets the text in italic.
fn shift(blocks: &mut [Block], n: usize) {
    for block in blocks {
        match block {
            Block::Para(runs) => runs.iter_mut().for_each(|r| (r.at, r.italic) = (r.at + n, true)),
            Block::Quote(inside) => shift(inside, n),
            Block::Code(_, at) => *at += n,
            Block::List { items, .. } => items.iter_mut().for_each(|item| shift(item, n)),
        }
    }
}

fn flush(blocks: &mut Vec<Block>, cur: &mut Vec<Run>, b: &mut Builder) {
    trim_end(cur, b);
    if !cur.is_empty() {
        blocks.push(Block::Para(std::mem::take(cur)));
    }
    b.apart = true;
}

/// A paragraph does not end in spaces or line breaks.
fn trim_end(cur: &mut Vec<Run>, b: &mut Builder) {
    while let Some(last) = cur.last_mut() {
        let kept = last.text.trim_end().len();
        b.plain.truncate(b.plain.len() - (last.text.len() - kept));
        last.text.truncate(kept);
        if kept > 0 {
            break;
        }
        cur.pop();
    }
}

fn text(s: &str, st: &Style, cur: &mut Vec<Run>, b: &mut Builder) {
    // HTML collapses white space; a paragraph does not start with it.
    let mut out = String::with_capacity(s.len());
    let mut space = cur.last().is_none_or(|r| r.text.ends_with(char::is_whitespace));
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(ch);
            space = false;
        }
    }
    if out.is_empty() {
        return;
    }
    push(out, st, cur, b);
}

fn push(out: String, st: &Style, cur: &mut Vec<Run>, b: &mut Builder) {
    if cur.is_empty() {
        b.start();
    }
    let at = b.plain.len();
    b.plain.push_str(&out);
    let same = |r: &Run| {
        (r.bold, r.italic, r.strike, r.underline, r.code, &r.href) == (st.bold, st.italic, st.strike, st.underline, st.code, &st.href)
    };
    match cur.last_mut() {
        Some(last) if same(last) => last.text.push_str(&out),
        _ => cur.push(Run {
            text: out,
            at,
            bold: st.bold,
            italic: st.italic,
            strike: st.strike,
            underline: st.underline,
            code: st.code,
            href: st.href.clone(),
        }),
    }
}

fn attr(node: &NodeRef, name: &str) -> Option<String> {
    let el = node.as_element()?;
    let attrs = el.attrs.borrow();
    attrs.iter().find(|a| &*a.name.local == name).map(|a| a.value.to_string())
}

fn raw_text(node: &NodeRef, out: &mut String) {
    match node.data() {
        NodeData::Text(t) => out.push_str(&t.borrow()),
        _ => {
            for c in node.children() {
                raw_text(&c, out);
            }
        }
    }
}

/// A link a bubble may open. Mentions (`matrix.to` links to users) stay marked by name instead.
fn link(href: &str) -> Option<String> {
    let web = href.starts_with("https://") || href.starts_with("http://");
    (web && !href.contains("matrix.to/#/@")).then(|| href.to_owned())
}

fn walk(node: &NodeRef, st: &Style, depth: usize, blocks: &mut Vec<Block>, cur: &mut Vec<Run>, b: &mut Builder) {
    let el = match node.data() {
        NodeData::Text(t) => return text(&t.borrow(), st, cur, b),
        NodeData::Element(el) => el,
        _ => return,
    };
    let name = &*el.name.local;
    let kids = |st: &Style, blocks: &mut Vec<Block>, cur: &mut Vec<Run>, b: &mut Builder| {
        for c in node.children() {
            walk(&c, st, depth + 1, blocks, cur, b);
        }
    };
    if depth >= MAX_DEPTH {
        let mut s = String::new();
        raw_text(node, &mut s);
        return text(&s, st, cur, b);
    }
    let mut inner = st.clone();
    match name {
        "mx-reply" | "script" | "style" | "head" => {}
        "br" => push("\n".into(), st, cur, b),
        "b" | "strong" => {
            inner.bold = true;
            kids(&inner, blocks, cur, b)
        }
        "i" | "em" | "cite" => {
            inner.italic = true;
            kids(&inner, blocks, cur, b)
        }
        "s" | "strike" | "del" => {
            inner.strike = true;
            kids(&inner, blocks, cur, b)
        }
        "u" | "ins" => {
            inner.underline = true;
            kids(&inner, blocks, cur, b)
        }
        "code" | "tt" | "kbd" => {
            inner.code = true;
            kids(&inner, blocks, cur, b)
        }
        "a" => {
            inner.href = attr(node, "href").and_then(|h| link(&h)).or(inner.href);
            kids(&inner, blocks, cur, b)
        }
        "img" => {
            if let Some(alt) = attr(node, "alt") {
                text(&alt, st, cur, b);
            }
        }
        "pre" => {
            flush(blocks, cur, b);
            let mut code = String::new();
            raw_text(node, &mut code);
            let code = code.strip_suffix('\n').unwrap_or(&code).to_owned();
            if !code.is_empty() {
                b.start();
                blocks.push(Block::Code(code.clone(), b.plain.len()));
                b.plain.push_str(&code);
            }
            b.apart = true;
        }
        "blockquote" => {
            flush(blocks, cur, b);
            let mut inside = Vec::new();
            kids(st, &mut inside, cur, b);
            flush(&mut inside, cur, b);
            if !inside.is_empty() {
                blocks.push(Block::Quote(inside));
            }
        }
        "ul" | "ol" => {
            flush(blocks, cur, b);
            let ordered = name == "ol";
            let start = ordered.then(|| attr(node, "start").and_then(|s| s.parse::<u64>().ok()).unwrap_or(1));
            let mut items = Vec::new();
            for (n, li) in node.children().filter(|c| c.as_element().is_some()).enumerate() {
                let mut inside = Vec::new();
                b.start();
                let before = b.plain.len();
                match start {
                    Some(first) => b.plain.push_str(&format!("{}. ", first.saturating_add(n as u64))),
                    None => b.plain.push_str("\u{2022} "),
                }
                // The marker is drawn by the list; the item's text follows it on the same line.
                let mark = b.plain.len();
                let mut sub = Vec::new();
                walk_item(&li, st, depth + 1, &mut inside, &mut sub, b);
                flush(&mut inside, &mut sub, b);
                if b.plain.len() == mark {
                    b.plain.truncate(before);
                } else {
                    items.push(inside);
                }
                b.apart = true;
            }
            if !items.is_empty() {
                blocks.push(Block::List { start, items });
            }
            b.apart = true;
        }
        "p" | "div" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "table" | "tr" | "thead" | "tbody" | "details" | "summary" | "hr" => {
            flush(blocks, cur, b);
            if name.starts_with('h') && name.len() == 2 {
                inner.bold = true;
            }
            kids(&inner, blocks, cur, b);
            flush(blocks, cur, b);
        }
        // A table reads as one line per row, its cells apart.
        "td" | "th" => {
            if !cur.is_empty() {
                trim_end(cur, b);
                push(" | ".into(), st, cur, b);
            }
            kids(&inner, blocks, cur, b)
        }
        // Unknown tags and spans (spoilers, colours, mention pills) keep their text.
        _ => kids(&inner, blocks, cur, b),
    }
}

/// The content of one list item. An `li` is not a block of its own here: the list draws it.
fn walk_item(li: &NodeRef, st: &Style, depth: usize, blocks: &mut Vec<Block>, cur: &mut Vec<Run>, b: &mut Builder) {
    for c in li.children() {
        walk(&c, st, depth, blocks, cur, b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(rich: &Rich, n: usize) -> &[Run] {
        match &rich.blocks[n] {
            Block::Para(runs) => runs,
            other => panic!("not a paragraph: {other:?}"),
        }
    }

    #[test]
    fn story_reply() {
        let html = "<blockquote>\n<p>Shared your story</p>\n</blockquote>\n<p><a href=\"https://www.instagram.com/stories/x/?reel_id=1\">https://www.instagram.com/stories/x/</a></p>";
        let (plain, rich) = parse(html).unwrap();
        assert_eq!(plain, "Shared your story\nhttps://www.instagram.com/stories/x/");
        assert_eq!(rich.blocks.len(), 2);
        let Block::Quote(inside) = &rich.blocks[0] else { panic!("no quote") };
        assert_eq!(inside, &[Block::Para(vec![Run { text: "Shared your story".into(), ..Run::default() }])]);
        let link = &para(&rich, 1)[0];
        assert_eq!(link.href.as_deref(), Some("https://www.instagram.com/stories/x/?reel_id=1"));
        assert_eq!(link.at, "Shared your story\n".len());
    }

    #[test]
    fn nested_styles_and_offsets() {
        let (plain, rich) = parse("a <b>bold <i>both</i></b> <s>gone</s> <u>under</u> <code>x = 1</code>").unwrap();
        assert_eq!(plain, "a bold both gone under x = 1");
        let runs = para(&rich, 0);
        let both = runs.iter().find(|r| r.text == "both").unwrap();
        assert!(both.bold && both.italic);
        assert_eq!(&plain[both.at..both.at + 4], "both");
        assert!(runs.iter().any(|r| r.text == "gone" && r.strike && !r.bold));
        assert!(runs.iter().any(|r| r.text == "under" && r.underline));
        assert!(runs.iter().any(|r| r.text == "x = 1" && r.code));
    }

    #[test]
    fn link_text_differs_from_href_and_mentions_are_not_links() {
        let (plain, rich) = parse("<a href=\"https://example.com/a\">the site</a> <a href=\"https://matrix.to/#/@bob:hs\">Bob</a> <a href=\"javascript:x\">bad</a>").unwrap();
        assert_eq!(plain, "the site Bob bad");
        let hrefs: Vec<_> = para(&rich, 0).iter().map(|r| r.href.clone()).collect();
        assert_eq!(hrefs[0].as_deref(), Some("https://example.com/a"));
        assert_eq!(hrefs[1], None);
    }

    #[test]
    fn reply_fallback_is_stripped() {
        let (plain, _) = parse("<mx-reply><blockquote><a href=\"https://matrix.to/#/!r:hs/$e\">In reply to</a> <a href=\"https://matrix.to/#/@a:hs\">@a:hs</a><br>old text</blockquote></mx-reply>the answer").unwrap();
        assert_eq!(plain, "the answer");
    }

    #[test]
    fn lists_code_and_breaks() {
        let (plain, rich) = parse("<p>Todo:</p><ul><li>one</li><li>two <b>2</b></li></ul><ol start=\"3\"><li>x</li></ol><pre><code>fn a() {\n    1\n}\n</code></pre>l1<br>l2").unwrap();
        assert_eq!(plain, "Todo:\n\u{2022} one\n\u{2022} two 2\n3. x\nfn a() {\n    1\n}\nl1\nl2");
        let Block::List { start: None, items } = &rich.blocks[1] else { panic!("no list") };
        assert_eq!(items.len(), 2);
        let Block::List { start: Some(3), .. } = &rich.blocks[2] else { panic!("no ordered list") };
        let Block::Code(code, at) = &rich.blocks[3] else { panic!("no code") };
        assert_eq!(code, "fn a() {\n    1\n}");
        assert_eq!(&plain[*at..*at + code.len()], code);
    }

    #[test]
    fn headings_unknown_tags_images_and_spoilers() {
        let (plain, rich) = parse("<h2>Title</h2><span data-mx-spoiler=\"\">secret</span> <blink>keep</blink><img src=\"mxc://x/y\" alt=\"pic\">").unwrap();
        assert_eq!(plain, "Title\nsecret keeppic");
        assert!(para(&rich, 0)[0].bold);
    }

    #[test]
    fn empty_and_huge_give_nothing() {
        assert!(parse("<p> </p>").is_none());
        assert!(parse(&"a".repeat(MAX_HTML + 1)).is_none());
        let deep = "<div>".repeat(500) + "x";
        assert_eq!(parse(&deep).unwrap().0, "x");
    }

    #[test]
    fn emotes_lead_with_the_actor() {
        let (mut body, mut rich) = ("waves".to_owned(), None);
        assert_eq!(emote("Ann", &mut body, &mut rich), "* Ann ".len());
        assert_eq!(body, "* Ann waves");
        let runs = para(rich.as_ref().unwrap(), 0);
        assert!(runs.iter().all(|r| r.italic) && runs.iter().all(|r| body[r.at..].starts_with(&r.text)));

        let (mut body, rich) = parse("waves at <b>Bob</b>").unwrap();
        let mut rich = Some(rich);
        emote("Ann", &mut body, &mut rich);
        let bob = para(rich.as_ref().unwrap(), 0).iter().find(|r| r.bold).unwrap();
        assert_eq!((body.as_str(), &body[bob.at..bob.at + 3]), ("* Ann waves at Bob", "Bob"));
        assert!(bob.italic);
    }

    #[test]
    fn table_cells_and_rows_stay_apart() {
        let html = "<table><thead><tr><th>h1</th><th>h2</th></tr></thead>\n<tbody><tr>\n<td>c1</td>\n<td><b>c2</b></td>\n</tr><tr><td></td><td>d2</td></tr></tbody></table>";
        assert_eq!(parse(html).unwrap().0, "h1 | h2\nc1 | c2\nd2");
    }

    #[test]
    fn list_start_from_a_sender_does_not_overflow() {
        let (plain, rich) = parse("<ol start=\"18446744073709551615\"><li>a</li><li>b</li></ol>").unwrap();
        assert_eq!(plain, "18446744073709551615. a\n18446744073709551615. b");
        assert!(matches!(rich.blocks[0], Block::List { start: Some(u64::MAX), .. }));
        assert_eq!(parse("<ol start=\"-3\"><li>a</li></ol>").unwrap().0, "1. a");
    }
}

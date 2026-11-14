//! Reads the contact cards bridges and other apps send as `.vcf` files. Only what a card shows
//! is kept (name, numbers, addresses); every other field is ignored, and the input is bounded so
//! a hostile file costs little.

use crate::i18n::t;

/// Largest file read, in bytes. Real cards are a few hundred bytes; ones with a photo, some KB.
pub const MAX_BYTES: usize = 256 * 1024;
const MAX_CARDS: usize = 50;
const MAX_NUMBERS: usize = 10;
const MAX_EMAILS: usize = 5;
/// Longest value kept, in characters.
const MAX_FIELD: usize = 200;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Card {
    pub name: String,
    /// Phone numbers with their kind ("Mobile", "Work", ...), or an empty label.
    pub numbers: Vec<(String, String)>,
    pub emails: Vec<String>,
}

/// Whether a file is a contact card by its type or its name. Signal says `text/vcf`, WhatsApp
/// `text/vcard`, Android `text/x-vcard`.
pub fn is_vcard(mime: Option<&str>, name: &str) -> bool {
    let by_type = mime.is_some_and(|m| {
        let m = m.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        matches!(m.as_str(), "text/vcard" | "text/x-vcard" | "text/vcf" | "text/directory")
    });
    by_type || name.to_ascii_lowercase().ends_with(".vcf")
}

/// The cards in `bytes`: a file may hold several. Text after `MAX_BYTES` is not read.
pub fn parse(bytes: &[u8]) -> Vec<Card> {
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_BYTES)]);
    let mut cards = Vec::new();
    let mut current: Option<Card> = None;
    let (mut family_given, mut has_fn) = (String::new(), false);
    for line in unfold(&text) {
        let Some((head, value)) = line.split_once(':') else { continue };
        let mut parts = head.split(';');
        let name = parts.next().unwrap_or("");
        // "item1.TEL" is a property grouped with others.
        let name = name.rsplit('.').next().unwrap_or(name).to_ascii_uppercase();
        let params: Vec<String> = parts.map(|p| p.to_ascii_uppercase()).collect();
        match (name.as_str(), &mut current) {
            ("BEGIN", _) if value.trim().eq_ignore_ascii_case("VCARD") => {
                current = Some(Card::default());
                (family_given, has_fn) = (String::new(), false);
            }
            ("END", Some(_)) if value.trim().eq_ignore_ascii_case("VCARD") => {
                let mut card = current.take().unwrap_or_default();
                if !has_fn {
                    card.name = family_given.clone();
                }
                if card.name.is_empty() {
                    card.name = card.numbers.first().map(|n| n.1.clone()).unwrap_or_default();
                }
                if !(card.name.is_empty() && card.emails.is_empty()) {
                    cards.push(card);
                }
                if cards.len() >= MAX_CARDS {
                    break;
                }
            }
            ("FN", Some(card)) => {
                let v = clean(&decode(value, &params));
                if !v.is_empty() {
                    card.name = v;
                    has_fn = true;
                }
            }
            ("N", Some(_)) => {
                let v = decode(value, &params);
                let mut f = v.split(';').map(|s| s.trim());
                let (family, given) = (f.next().unwrap_or(""), f.next().unwrap_or(""));
                family_given = clean(&format!("{given} {family}"));
            }
            ("TEL", Some(card)) if card.numbers.len() < MAX_NUMBERS => {
                let v = clean(decode(value, &params).trim_start_matches("tel:"));
                if !v.is_empty() {
                    card.numbers.push((kind(&params), v));
                }
            }
            ("EMAIL", Some(card)) if card.emails.len() < MAX_EMAILS => {
                let v = clean(&decode(value, &params));
                if !v.is_empty() {
                    card.emails.push(v);
                }
            }
            _ => {}
        }
    }
    cards
}

/// Lines with their continuations joined: a line starting with a space or tab continues the
/// one before, and a quoted-printable line ending in `=` continues on the next.
fn unfold(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut open_qp = false;
    for raw in text.lines() {
        if open_qp {
            let last = out.last_mut().expect("a line is open");
            last.pop();
            last.push_str(raw);
            open_qp = last.ends_with('=');
        } else if raw.starts_with([' ', '\t']) && !out.is_empty() {
            out.last_mut().expect("non-empty").push_str(&raw[1..]);
        } else {
            out.push(raw.to_owned());
            open_qp = raw.to_ascii_uppercase().contains("QUOTED-PRINTABLE") && raw.ends_with('=');
        }
    }
    out
}

/// A value with its quoted-printable encoding undone, if it has one, and its escapes resolved.
fn decode(value: &str, params: &[String]) -> String {
    if params.iter().any(|p| p.contains("QUOTED-PRINTABLE")) {
        return unescape(&quoted_printable(value));
    }
    unescape(value)
}

fn quoted_printable(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (bytes[i], bytes.get(i + 1).and_then(|b| hex(*b)), bytes.get(i + 2).and_then(|b| hex(*b))) {
            (b'=', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (b, ..) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push(' '),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Trimmed, without control characters, at most `MAX_FIELD` characters.
fn clean(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).take(MAX_FIELD).collect::<String>().trim().to_owned()
}

/// What kind of number a TEL is, from `TYPE=CELL,VOICE`, `TYPE=CELL;TYPE=VOICE` or bare `CELL`.
fn kind(params: &[String]) -> String {
    let words = params.iter().flat_map(|p| p.trim_start_matches("TYPE=").trim_matches('"').split(',').map(str::to_owned).collect::<Vec<_>>());
    for word in words {
        match word.as_str() {
            "CELL" | "MOBILE" | "IPHONE" => return t!("Mobile").into(),
            "WORK" => return t!("Work").into(),
            "HOME" => return t!("Home").into(),
            "FAX" => return t!("Fax").into(),
            _ => {}
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whatsapp_card() {
        let v = "BEGIN:VCARD\nVERSION:3.0\nN:;Ala Kowalska;;;\nFN:Ala Kowalska\nTEL;type=CELL;waid=48123456789:+48 123 456 789\nEND:VCARD\n";
        let c = parse(v.as_bytes());
        assert_eq!(c, vec![Card { name: "Ala Kowalska".into(), numbers: vec![("Mobile".into(), "+48 123 456 789".into())], emails: vec![] }]);
    }

    #[test]
    fn android_v21_quoted_printable_and_folding() {
        let v = "BEGIN:VCARD\r\nVERSION:2.1\r\nN;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:=C5=81ukasz;Jan;;;\r\nFN;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:Jan =C5=81ukasz=\r\n=20Jr\r\nTEL;CELL:600 100 200\r\nTEL;WORK;VOICE:+48221234567\r\nEMAIL;HOME:a@b.pl\r\nEND:VCARD\r\n";
        let c = parse(v.as_bytes());
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "Jan Łukasz Jr");
        assert_eq!(c[0].numbers, vec![("Mobile".into(), "600 100 200".into()), ("Work".into(), "+48221234567".into())]);
        assert_eq!(c[0].emails, vec!["a@b.pl".to_owned()]);
    }

    #[test]
    fn name_from_n_when_no_fn_and_grouped_tel() {
        let v = "BEGIN:VCARD\nVERSION:4.0\nN:Nowak;Piotr;;;\nitem1.TEL;TYPE=\"home,voice\":tel:+48111\nEND:VCARD";
        let c = parse(v.as_bytes());
        assert_eq!(c[0].name, "Piotr Nowak");
        assert_eq!(c[0].numbers, vec![("Home".into(), "+48111".into())]);
    }

    #[test]
    fn several_cards_unknown_fields_and_junk() {
        let v = "garbage\nBEGIN:VCARD\nFN:A\nX-FOO:bar\nPHOTO;ENCODING=b:AAAA\nEND:VCARD\nBEGIN:VCARD\nTEL:1\nEND:VCARD\nBEGIN:VCARD\nEND:VCARD\nBEGIN:VCARD\nFN:unfinished";
        let c = parse(v.as_bytes());
        assert_eq!(c.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["A", "1"]);
    }

    #[test]
    fn bounded() {
        let mut v = String::new();
        for i in 0..200 {
            v.push_str(&format!("BEGIN:VCARD\nFN:P{i}\nEND:VCARD\n"));
        }
        assert_eq!(parse(v.as_bytes()).len(), MAX_CARDS);
        let mut big = String::from("BEGIN:VCARD\nFN:X\n");
        for _ in 0..100 {
            big.push_str("TEL:123\n");
        }
        big.push_str("END:VCARD\n");
        assert_eq!(parse(big.as_bytes())[0].numbers.len(), MAX_NUMBERS);
        assert!(parse(&vec![0xff; 10 * MAX_BYTES]).is_empty());
        let long = format!("BEGIN:VCARD\nFN:{}\nEND:VCARD", "x".repeat(5000));
        assert_eq!(parse(long.as_bytes())[0].name.len(), MAX_FIELD);
    }

    #[test]
    fn detects_by_type_or_name() {
        assert!(is_vcard(Some("text/vcf"), "x"));
        assert!(is_vcard(Some("text/vcard; charset=utf-8"), "x"));
        assert!(is_vcard(None, "Ala.VCF"));
        assert!(!is_vcard(Some("text/plain"), "a.txt"));
    }
}

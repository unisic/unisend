use std::collections::HashSet;

use chrono::TimeZone as _;

use super::*;

#[test]
fn polish_plural_forms() {
    let form = |n| ["one", "few", "many"][polish_form(n)];
    assert_eq!(form(1), "one");
    for n in [2, 3, 4, 22, 23, 24, 32, 102, 104, 1002] {
        assert_eq!(form(n), "few", "{n}");
    }
    for n in [0, 5, 6, 10, 11, 12, 13, 14, 15, 19, 20, 21, 25, 101, 111, 112, 113, 114, 1000] {
        assert_eq!(form(n), "many", "{n}");
    }
}

#[test]
fn locale_follows_the_first_set_variable() {
    let s = |v: &str| Some(v.to_owned());
    assert!(is_polish_locale(&[None, None, s("pl_PL.UTF-8")]));
    assert!(is_polish_locale(&[s("pl"), None, None]));
    assert!(is_polish_locale(&[s(""), s("pl_PL@euro"), s("en_US.UTF-8")]));
    assert!(!is_polish_locale(&[s("C"), None, s("pl_PL.UTF-8")]), "LC_ALL wins over LANG");
    assert!(!is_polish_locale(&[None, s("en_GB.UTF-8"), s("pl_PL.UTF-8")]));
    assert!(!is_polish_locale(&[None, None, None]));
    assert!(!is_polish_locale(&[None, None, s("plt_XX")]));
}

#[test]
fn fill_replaces_named_placeholders() {
    assert_eq!(fill("{a} and {b}", &[("a", &1), ("b", &"two")]), "1 and two");
    assert_eq!(fill("{a}{a}", &[("a", &"x")]), "xx");
    assert_eq!(fill("{missing} {", &[("a", &1)]), "{missing} {", "unknown names stay as written");
    assert_eq!(fill("„{name}”", &[("name", &"Żółć")]), "„Żółć”");
}

#[test]
fn english_plural_and_text_are_the_source() {
    assert_eq!(plural(1, "{n} vote", "{n} votes"), "{n} vote");
    assert_eq!(plural(0, "{n} vote", "{n} votes"), "{n} votes");
    assert_eq!(tr("Archive|verb"), "Archive", "the context is not shown");
}

#[test]
fn polish_dates() {
    let t = chrono::Utc.with_ymd_and_hms(2024, 5, 6, 12, 0, 0).unwrap();
    assert_eq!(date_in(&t, false, true), "6 maja");
    assert_eq!(date_in(&t, true, true), "6 maja 2024");
    assert_eq!(date_in(&t, true, false), "6 May 2024");
    assert_eq!(weekday_in(&t, true), "Poniedziałek");
    assert_eq!(weekday_in(&t, false), "Monday");
}

fn placeholders(text: &str) -> HashSet<&str> {
    text.match_indices('{').filter_map(|(i, _)| text[i + 1..].split_once('}').map(|(name, _)| name)).collect()
}

/// The string literal at the start of `src` (after spaces), unescaped, and the rest.
fn literal(src: &str) -> Option<(String, &str)> {
    let src = src.trim_start().strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = src.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &src[i + 1..])),
            '\\' => match chars.next()?.1 {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                other => panic!("unexpected escape \\{other} in a translated text"),
            },
            c => out.push(c),
        }
    }
    None
}

/// Every key the code passes to `t!`, `n_!` and `tn!`, read from the sources.
fn keys_in_code() -> (HashSet<String>, HashSet<String>) {
    let (mut text, mut plural) = (HashSet::new(), HashSet::new());
    let mut stack = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))];
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            stack.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") || path.starts_with(concat!(env!("CARGO_MANIFEST_DIR"), "/src/i18n")) {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        for (at, _) in src.match_indices("!(") {
            let name = src[..at].rsplit(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap();
            let rest = &src[at + 2..];
            match name {
                "t" | "n_" => {
                    let (key, _) = literal(rest).unwrap_or_else(|| panic!("{}: t! without a literal", path.display()));
                    text.insert(key);
                }
                "tn" => {
                    let one = &rest[rest.find('"').unwrap()..];
                    let (key, after) = literal(one).unwrap();
                    assert!(after.trim_start().starts_with(','), "{}: tn! needs two forms", path.display());
                    plural.insert(key);
                }
                _ => {}
            }
        }
    }
    (text, plural)
}

#[test]
fn every_key_in_code_has_polish_and_no_stale_entries() {
    let (text, plural) = keys_in_code();
    assert!(text.len() > 400 && plural.len() > 10, "the scan found too little: {} {}", text.len(), plural.len());
    let missing: Vec<_> = text.iter().filter(|k| !table().contains_key(k.as_str())).collect();
    assert!(missing.is_empty(), "no Polish for {missing:?}");
    let missing: Vec<_> = plural.iter().filter(|k| !plurals().contains_key(k.as_str())).collect();
    assert!(missing.is_empty(), "no Polish plural for {missing:?}");
    let stale: Vec<_> = pl::TEXT.iter().map(|(k, _)| *k).filter(|k| !text.contains(*k)).collect();
    assert!(stale.is_empty(), "Polish for texts the code no longer has: {stale:?}");
    let stale: Vec<_> = pl::PLURAL.iter().map(|(k, _)| *k).filter(|k| !plural.contains(*k)).collect();
    assert!(stale.is_empty(), "Polish plurals the code no longer has: {stale:?}");
    assert_eq!(table().len(), pl::TEXT.len(), "duplicate keys");
    assert_eq!(plurals().len(), pl::PLURAL.len(), "duplicate plural keys");
}

#[test]
fn polish_keeps_the_placeholders() {
    for (en, pl) in pl::TEXT {
        assert_eq!(placeholders(en), placeholders(pl), "{en:?} -> {pl:?}");
    }
    for (en, forms) in pl::PLURAL {
        let mut allowed = placeholders(en);
        allowed.insert("n");
        for form in forms {
            assert!(placeholders(form).is_subset(&allowed), "{en:?} -> {form:?}");
        }
    }
}

#[test]
fn choices_map_to_android_language_tags_and_back() {
    for choice in [LanguageChoice::System, LanguageChoice::English, LanguageChoice::Polish] {
        assert_eq!(from_tag(tag(choice)), choice);
    }
    assert_eq!(tag(LanguageChoice::System), "", "no tag: the app follows the phone");
    assert_eq!(from_tag("pl-PL"), LanguageChoice::Polish);
    assert_eq!(from_tag("en_GB"), LanguageChoice::English);
    assert_eq!(from_tag("de"), LanguageChoice::System);
}

#[test]
fn tr_in_ignores_the_current_language() {
    assert_eq!(tr_in("You", true), "Ty");
    assert_eq!(tr_in("You", false), "You");
    assert_eq!(tr_in("Archive|verb", false), "Archive");
}

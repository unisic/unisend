//! The interface in English or Polish, following the system language. English is the source
//! text and the lookup key; `pl.rs` holds the Polish. Log lines and anything sent to other
//! people stay in English.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Datelike as _, Local};

mod pl;

static POLISH: AtomicBool = AtomicBool::new(false);

/// `t!("Reply")` is the text in the current language; `t!("Seen by {who}", who = x)` fills in
/// the named placeholders and returns a `String`.
macro_rules! t {
    ($en:literal) => {
        $crate::i18n::tr($en)
    };
    ($en:literal, $($key:ident = $value:expr),+ $(,)?) => {
        {
            // A statement of its own, so the borrowed arguments never live across an `.await`.
            let text = $crate::i18n::fill($crate::i18n::tr($en), &[$((stringify!($key), &$value as &dyn std::fmt::Display)),+]);
            text
        }
    };
}

/// `tn!(count, "{n} vote", "{n} votes")`: the plural form for `count`, with `{n}` and any other
/// named placeholders filled in.
macro_rules! tn {
    ($count:expr, $one:literal, $other:literal $(, $key:ident = $value:expr)* $(,)?) => {{
        let count: u64 = $count as u64;
        let text = $crate::i18n::fill($crate::i18n::plural(count, $one, $other), &[("n", &count as &dyn std::fmt::Display) $(, (stringify!($key), &$value as &dyn std::fmt::Display))*]);
        text
    }};
}

/// Marks a literal kept in a constant for translation where it is shown, through `tr`.
macro_rules! n_ {
    ($en:literal) => {
        $en
    };
}

pub(crate) use {n_, t, tn};

/// Picks the language once at start-up. Android passes the device language ("pl"); the desktop
/// reads the POSIX locale variables in their order of precedence.
pub(crate) fn init(android_language: Option<String>) {
    let polish = match android_language {
        Some(lang) => lang == "pl",
        None => is_polish_locale(
            ["LC_ALL", "LC_MESSAGES", "LANG"].into_iter().map(|k| std::env::var(k).ok()).collect::<Vec<_>>().as_slice(),
        ),
    };
    POLISH.store(polish, Ordering::Relaxed);
}

/// The first set, non-empty variable decides, as in POSIX: `pl_PL.UTF-8`, `pl` and `pl_PL@euro`
/// are Polish; `C`, `en_US` and nothing at all are English.
fn is_polish_locale(vars: &[Option<String>]) -> bool {
    vars.iter().flatten().find(|v| !v.is_empty()).is_some_and(|v| {
        let lang = v.split(['_', '.', '@', '-']).next().unwrap_or_default();
        lang == "pl"
    })
}

pub(crate) fn polish() -> bool {
    POLISH.load(Ordering::Relaxed)
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| pl::TEXT.iter().copied().collect())
}

fn plurals() -> &'static HashMap<&'static str, [&'static str; 3]> {
    static TABLE: OnceLock<HashMap<&'static str, [&'static str; 3]>> = OnceLock::new();
    TABLE.get_or_init(|| pl::PLURAL.iter().copied().collect())
}

/// The text for `en` in the current language; English when Polish has no entry. A key may end in
/// `|context` to tell apart two meanings of one English word ("Archive|verb"); English drops it.
pub(crate) fn tr(en: &'static str) -> &'static str {
    if polish()
        && let Some(pl) = table().get(en)
    {
        return pl;
    }
    en.split_once('|').map_or(en, |(text, _)| text)
}

/// Polish has three forms: one (1), few (2-4, 22-24, but not 12-14) and many (the rest, 0 too).
pub(crate) fn polish_form(n: u64) -> usize {
    match (n % 10, n % 100) {
        _ if n == 1 => 0,
        (2..=4, r) if !(12..=14).contains(&r) => 1,
        _ => 2,
    }
}

pub(crate) fn plural(n: u64, one: &'static str, other: &'static str) -> &'static str {
    if !polish() {
        return if n == 1 { one } else { other };
    }
    match plurals().get(one) {
        Some(forms) => forms[polish_form(n)],
        None => if n == 1 { one } else { other },
    }
}

/// Replaces each `{key}` with its value.
pub(crate) fn fill(text: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}').and_then(|close| args.iter().find(|(k, _)| *k == &after[..close]).map(|a| (close, a))) {
            Some((close, (_, value))) => {
                use std::fmt::Write as _;
                let _ = write!(out, "{value}");
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

const PL_WEEKDAYS: [&str; 7] = ["poniedziałek", "wtorek", "środa", "czwartek", "piątek", "sobota", "niedziela"];
const PL_WEEKDAYS_SHORT: [&str; 7] = ["pon.", "wt.", "śr.", "czw.", "pt.", "sob.", "niedz."];
/// Genitive, as in "5 maja".
const PL_MONTHS: [&str; 12] =
    ["stycznia", "lutego", "marca", "kwietnia", "maja", "czerwca", "lipca", "sierpnia", "września", "października", "listopada", "grudnia"];
const PL_MONTHS_SHORT: [&str; 12] = ["sty", "lut", "mar", "kwi", "maj", "cze", "lip", "sie", "wrz", "paź", "lis", "gru"];

/// "Monday" / "Poniedziałek", capitalised to stand alone.
pub(crate) fn weekday(t: &DateTime<Local>) -> String {
    weekday_in(t, polish())
}

fn weekday_in<Tz: chrono::TimeZone>(t: &DateTime<Tz>, polish: bool) -> String
where
    Tz::Offset: Display,
{
    if !polish {
        return t.format("%A").to_string();
    }
    let day = PL_WEEKDAYS[t.weekday().num_days_from_monday() as usize];
    let mut chars = day.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// "Mon" / "pon.".
pub(crate) fn weekday_short(t: &DateTime<Local>) -> String {
    if !polish() {
        return t.format("%a").to_string();
    }
    PL_WEEKDAYS_SHORT[t.weekday().num_days_from_monday() as usize].to_owned()
}

/// "5 May" or "5 May 2024"; "5 maja" or "5 maja 2024".
pub(crate) fn date(t: &DateTime<Local>, with_year: bool) -> String {
    date_in(t, with_year, polish())
}

fn date_in<Tz: chrono::TimeZone>(t: &DateTime<Tz>, with_year: bool, polish: bool) -> String
where
    Tz::Offset: Display,
{
    match (polish, with_year) {
        (false, false) => t.format("%-d %B").to_string(),
        (false, true) => t.format("%-d %B %Y").to_string(),
        (true, false) => format!("{} {}", t.day(), PL_MONTHS[t.month0() as usize]),
        (true, true) => format!("{} {} {}", t.day(), PL_MONTHS[t.month0() as usize], t.year()),
    }
}

/// "5 May" / "5 maj", for the chat list.
pub(crate) fn date_short(t: &DateTime<Local>) -> String {
    if !polish() {
        return t.format("%-d %b").to_string();
    }
    format!("{} {}", t.day(), PL_MONTHS_SHORT[t.month0() as usize])
}

#[cfg(test)]
mod tests;

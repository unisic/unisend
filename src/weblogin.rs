//! Signing in to a chat network in an embedded browser on Linux, the desktop twin of Android's
//! `WebLogin.java`. The app starts [`HELPER`], installed beside it, with the request on stdin;
//! that process opens a WebKitGTK window with an ephemeral profile, so GTK's main loop never meets
//! GPUI's and nothing of the site stays on disk once it ends. The app itself links no WebKitGTK:
//! it starts without it, and a missing helper or library falls back to the system's browser. The
//! helper answers with one line on stdout: the cookies by name, local storage by key and the
//! bridge script's result by field ID, as one JSON object, or nothing when the user closed the
//! window before the sign-in was complete.

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::bridges;

/// The browser's binary (`src/bin/beeper-gpui-weblogin.rs`), in the app's own directory.
pub const HELPER: &str = "beeper-gpui-weblogin";
/// Starts the answer line, so whatever else a library prints to stdout is never taken for it.
const MARK: &str = "unisend-web-login:";

/// What the browser needs, built from the bridge's cookie step the way `web_login` builds it for
/// Android. The helper reads it as its own `Request` (`weblogin/src/browser.rs`); the two keep the
/// same fields.
#[derive(Debug, Serialize)]
pub(crate) struct Request {
    url: String,
    user_agent: String,
    /// Done once the address matches this (Go syntax, which Rust's `regex` shares) and every
    /// required field has a value; an empty or broken pattern leaves only the fields.
    wait_for: String,
    /// Every domain a field's cookie lives on: their cookies are the answer.
    domains: Vec<String>,
    /// One entry per required field, its sources: `c:name@domain`, `s:key`, `f:id`.
    required: Vec<Vec<String>>,
    /// Local storage keys to read.
    storage: Vec<String>,
    /// The bridge's script; it resolves to an object keyed by field ID.
    script: String,
}

pub(crate) fn request(wanted: &bridges::Cookies) -> Request {
    let mut domains: Vec<String> = wanted
        .fields
        .iter()
        .flat_map(|f| &f.cookies)
        .map(|c| c.1.clone())
        .filter(|d| !d.is_empty())
        .collect();
    domains.sort_unstable();
    domains.dedup();
    let required = wanted
        .fields
        .iter()
        .filter(|f| f.required)
        .map(|f| {
            let cookies = f.cookies.iter().map(|c| format!("c:{}@{}", c.0, c.1));
            let storage = f.storage.iter().map(|k| format!("s:{k}"));
            let script = (!wanted.extract_js.is_empty()).then(|| format!("f:{}", f.id));
            cookies.chain(storage).chain(script).collect()
        })
        .collect();
    Request {
        url: wanted.url.clone(),
        user_agent: wanted.user_agent.clone(),
        wait_for: wanted.wait_for.clone(),
        domains,
        required,
        storage: wanted
            .fields
            .iter()
            .flat_map(|f| f.storage.clone())
            .collect(),
        script: wanted.extract_js.clone(),
    }
}

/// Opens the browser and blocks until it closes. `Err` when it could not open or ended without
/// an answer: the caller falls back to the system's browser and pasting.
pub(crate) fn run(request: &Request) -> Result<Option<String>, String> {
    let helper = std::env::current_exe()
        .map_err(|e| format!("own path: {e}"))?
        .with_file_name(HELPER);
    let mut child = Command::new(&helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("start {}: {e}", helper.display()))?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err("no pipes".into());
    };
    let sent = serde_json::to_string(request)
        .map_err(|e| e.to_string())
        .and_then(|line| writeln!(stdin, "{line}").map_err(|e| format!("write: {e}")));
    let mut answer = None;
    if sent.is_ok() {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(found) = line.strip_prefix(MARK) {
                answer = Some(found.to_owned());
                break;
            }
        }
    }
    // stdin stays open until here: the browser ends when it closes, so it cannot outlive the app.
    drop(stdin);
    let status = child.wait().map_err(|e| format!("wait: {e}"))?;
    sent?;
    parse_answer(answer.as_deref()).ok_or_else(|| format!("no answer ({status})"))
}

/// The answer line's text: a JSON object, or empty for nothing. `None` when there was none.
fn parse_answer(answer: Option<&str>) -> Option<Option<String>> {
    let answer = answer?.trim();
    if answer.is_empty() {
        return Some(None);
    }
    serde_json::from_str::<Map<String, Value>>(answer)
        .ok()
        .map(|m| (!m.is_empty()).then(|| answer.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridges::{CookieField, Cookies};

    fn messenger() -> Cookies {
        let field = |id: &str, required, cookies: &[(&str, &str)], storage: &[&str]| CookieField {
            id: id.into(),
            required,
            cookies: cookies
                .iter()
                .map(|(n, d)| ((*n).into(), (*d).into()))
                .collect(),
            storage: storage.iter().map(|s| (*s).into()).collect(),
        };
        Cookies {
            url: "https://www.messenger.com/".into(),
            user_agent: "UA".into(),
            wait_for: "^https://www\\.messenger\\.com/.*$".into(),
            fields: vec![
                field(
                    "c_user",
                    true,
                    &[("c_user", "messenger.com"), ("c_user", "facebook.com")],
                    &[],
                ),
                field("xs", true, &[("xs", "messenger.com")], &["xs_key"]),
                field("fr", false, &[("fr", "messenger.com"), ("fr", "")], &[]),
            ],
            extract_js: String::new(),
        }
    }

    #[test]
    fn request_lists_domains_and_required_sources() {
        let r = request(&messenger());
        assert_eq!(r.domains, ["facebook.com", "messenger.com"]);
        assert_eq!(
            r.required,
            [
                vec!["c:c_user@messenger.com", "c:c_user@facebook.com"],
                vec!["c:xs@messenger.com", "s:xs_key"]
            ]
        );
        assert_eq!(r.storage, ["xs_key"]);
        let mut with_script = messenger();
        with_script.extract_js = "f()".into();
        assert_eq!(
            request(&with_script).required[0].last().map(String::as_str),
            Some("f:c_user")
        );
        // The line the helper reads: its test parses the same file.
        let line: Value = serde_json::from_str(include_str!("../weblogin/request.json")).unwrap();
        assert_eq!(serde_json::to_value(&r).unwrap(), line);
    }

    #[test]
    fn missing_helper_is_an_error_so_the_system_browser_opens() {
        // Tests run from target/*/deps, where Cargo puts no `beeper-gpui-weblogin`.
        let e = run(&request(&messenger())).unwrap_err();
        assert!(e.starts_with("start "), "{e}");
    }

    #[test]
    fn answer_line_tells_nothing_from_failure() {
        assert_eq!(parse_answer(None), None);
        assert_eq!(parse_answer(Some("")), Some(None));
        assert_eq!(parse_answer(Some("{}")), Some(None));
        assert_eq!(parse_answer(Some("not json")), None);
        assert_eq!(
            parse_answer(Some("{\"a\":\"1\"}")),
            Some(Some("{\"a\":\"1\"}".into()))
        );
    }
}

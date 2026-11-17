//! Signing in to a chat network in an embedded browser on Linux, the desktop twin of Android's
//! `WebLogin.java`. The app starts its own binary again with [`ARG`] and the request on stdin; that
//! process opens a WebKitGTK window with an ephemeral profile, so GTK's main loop never meets
//! GPUI's, and nothing of the site stays on disk once it ends. It answers with one line on stdout:
//! the cookies by name, local storage by key and the bridge script's result by field ID, as one
//! JSON object, or nothing when the user closed the window before the sign-in was complete.

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::bridges;

/// The hidden argument that makes the binary the browser instead of the app.
pub const ARG: &str = "--web-login";
/// Starts the answer line, so whatever else a library prints to stdout is never taken for it.
const MARK: &str = "unisend-web-login:";

/// What the browser needs, built from the bridge's cookie step the way `web_login` builds it for
/// Android.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
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
    // The running binary even when an update replaced the file on disk.
    let mut child = Command::new("/proc/self/exe")
        .arg(ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("start: {e}"))?;
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

/// Cookies by domain, `(name, value)`, whether set for the bare domain or for `www.`.
type Jars = BTreeMap<String, Vec<(String, String)>>;

/// Takes what the probe read from the page, given as the JSON text it built: local storage under
/// `s:key`, and under "" the script's result, itself JSON text of an object keyed by field ID.
fn take(text: &str, found: &mut BTreeMap<String, String>) {
    let Ok(all) = serde_json::from_str::<Map<String, Value>>(text) else {
        return;
    };
    let usable = |v: &Value| {
        Some(v.as_str().map_or_else(|| v.to_string(), str::to_owned))
            .filter(|v| !v.is_empty() && v != "null")
    };
    for (key, v) in all {
        if !key.is_empty() {
            if let Some(v) = usable(&v) {
                found.insert(key, v);
            }
            continue;
        }
        let Some(fields) = v
            .as_str()
            .and_then(|s| serde_json::from_str::<Map<String, Value>>(s).ok())
        else {
            continue;
        };
        for (id, f) in fields {
            if let Some(f) = usable(&f) {
                found.insert(format!("f:{id}"), f);
            }
        }
    }
}

/// Every required field has a value from one of its sources.
fn has_required(required: &[Vec<String>], found: &BTreeMap<String, String>, jars: &Jars) -> bool {
    let has = |source: &String| match source.strip_prefix("c:") {
        Some(cookie) => {
            let (name, domain) = cookie.split_once('@').unwrap_or((cookie, ""));
            jars.get(domain)
                .is_some_and(|jar| jar.iter().any(|(n, _)| n == name))
        }
        None => found.contains_key(source),
    };
    required.iter().all(|field| field.iter().any(has))
}

/// The answer: the domains' cookies by name, then local storage by key and the script's result
/// by field ID. `None` when there is nothing.
fn answer(domains: &[String], found: &BTreeMap<String, String>, jars: &Jars) -> Option<String> {
    let mut all = Map::new();
    for (name, value) in domains.iter().filter_map(|d| jars.get(d)).flatten() {
        all.insert(name.clone(), value.clone().into());
    }
    for (key, value) in found {
        all.insert(key[2..].to_owned(), value.clone().into());
    }
    (!all.is_empty()).then(|| Value::Object(all).to_string())
}

/// The script, run once per page; the probe reads its result back.
fn start_script(script: &str) -> String {
    format!(
        "(function(){{if(window.__unisendLogin!==undefined)return;window.__unisendLogin=null;Promise.resolve({script}).then(function(r){{window.__unisendLogin=JSON.stringify(r||{{}});}},function(){{}});}})()"
    )
}

/// Reads the script's result and local storage, as JSON text.
fn probe_script(storage: &[String]) -> String {
    let mut js = String::from("JSON.stringify({\"\":window.__unisendLogin||null");
    for key in storage {
        let (field, key) = (Value::from(format!("s:{key}")), Value::from(key.as_str()));
        js.push_str(&format!(",{field}:localStorage.getItem({key})"));
    }
    js + "})"
}

/// The browser process: reads the request, shows the page, prints the answer. Returns the exit
/// code: 0 once answered, 1 for a bad request, 2 when no window can open.
pub fn browser() -> i32 {
    let mut line = String::new();
    let request = std::io::stdin()
        .lock()
        .read_line(&mut line)
        .ok()
        .and_then(|_| serde_json::from_str::<Request>(&line).ok());
    let Some(request) = request else {
        eprintln!("web login: no request");
        return 1;
    };
    // The app closed our stdin: it is gone, nobody is left to answer.
    std::thread::spawn(|| {
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
        std::process::exit(0);
    });
    match window::show(request) {
        Ok(answer) => {
            println!("{MARK}{}", answer.unwrap_or_default());
            0
        }
        Err(e) => {
            eprintln!("web login: {e}");
            2
        }
    }
}

mod window {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use std::time::Duration;

    use gtk::prelude::*;
    use webkit2gtk::glib;
    use webkit2gtk::{
        CookieAcceptPolicy, CookieManagerExt as _, LoadEvent, PermissionRequestExt as _,
        SettingsExt as _, URIRequestExt as _, WebContext, WebContextExt as _, WebView, WebViewExt,
    };

    use super::{Jars, Request};

    /// How often the page is asked for local storage and the script's result.
    const PROBE: Duration = Duration::from_millis(500);

    struct State {
        request: Request,
        done: Option<regex::Regex>,
        web: WebView,
        cookies: webkit2gtk::CookieManager,
        found: RefCell<BTreeMap<String, String>>,
        answer: RefCell<Option<Option<String>>>,
        finished: Cell<bool>,
    }

    /// Shows the page until the sign-in is done or the window closes; the answer, if complete.
    pub fn show(request: Request) -> Result<Option<String>, String> {
        glib::set_prgname(Some("dev.gpui.beeper"));
        gtk::init().map_err(|e| e.to_string())?;
        // Kept in memory only: no cookies, storage or cache reach the disk.
        let context = WebContext::new_ephemeral();
        let cookies = context.cookie_manager().ok_or("no cookie manager")?;
        // Captchas run in other sites' frames, which need their cookies, as on Android.
        cookies.set_accept_policy(CookieAcceptPolicy::Always);
        let web = WebView::with_context(&context);
        if let Some(settings) = WebViewExt::settings(&web) {
            if !request.user_agent.is_empty() {
                settings.set_user_agent(Some(&request.user_agent));
            }
            settings.set_javascript_can_open_windows_automatically(true);
            #[cfg(debug_assertions)]
            settings.set_enable_developer_extras(true);
        }
        let done = Some(&request.wait_for)
            .filter(|p| !p.is_empty())
            .and_then(|p| regex::Regex::new(p).ok());
        let state = Rc::new(State {
            request,
            done,
            web: web.clone(),
            cookies,
            found: RefCell::default(),
            answer: RefCell::default(),
            finished: Cell::new(false),
        });

        // Location, camera, microphone, notifications: none is needed to sign in.
        web.connect_permission_request(|_, request| {
            request.deny();
            true
        });
        // A new window loads in this one: there is only one view to show it in.
        web.connect_create(|web, action| {
            if let Some(uri) = action
                .request()
                .and_then(|r| r.uri())
                .filter(|u| !u.is_empty())
            {
                web.load_uri(&uri);
            }
            None
        });
        let s = state.clone();
        web.connect_load_changed(move |web, event| {
            if event == LoadEvent::Finished {
                if !s.request.script.is_empty() {
                    web.evaluate_javascript(
                        &super::start_script(&s.request.script),
                        None,
                        None,
                        None::<&gtk::gio::Cancellable>,
                        |_| (),
                    );
                }
                check(&s);
            }
        });
        // Pages that change the address without loading one.
        let s = state.clone();
        web.connect_uri_notify(move |_| check(&s));

        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title("Unisend");
        window.set_default_size(1000, 760);
        window.set_icon_name(Some("dev.gpui.beeper"));
        window.add(&web);
        let w = window.clone();
        web.connect_title_notify(move |web| {
            w.set_title(
                web.title()
                    .as_deref()
                    .filter(|t| !t.is_empty())
                    .unwrap_or("Unisend"),
            )
        });
        let s = state.clone();
        window.connect_delete_event(move |window, _| {
            window.hide();
            close(&s);
            glib::Propagation::Stop
        });
        window.show_all();
        web.load_uri(&state.request.url);
        if !state.request.storage.is_empty() || !state.request.script.is_empty() {
            schedule_probe(state.clone());
        }
        gtk::main();
        let answer = state.answer.borrow_mut().take();
        Ok(answer.flatten())
    }

    fn schedule_probe(state: Rc<State>) {
        glib::timeout_add_local_once(PROBE, move || {
            if state.finished.get() {
                return;
            }
            let s = state.clone();
            state.web.evaluate_javascript(
                &super::probe_script(&state.request.storage),
                None,
                None,
                None::<&gtk::gio::Cancellable>,
                move |value| {
                    if let Ok(value) = value {
                        super::take(&value.to_string(), &mut s.found.borrow_mut());
                    }
                    check(&s);
                    schedule_probe(s);
                },
            );
        });
    }

    /// Ends with the answer once the address matches and every required field has a value.
    fn check(state: &Rc<State>) {
        if state.finished.get() || state.request.required.is_empty() {
            return;
        }
        let uri = state.web.uri();
        if state
            .done
            .as_ref()
            .is_some_and(|p| !uri.as_deref().is_some_and(|u| p.is_match(u)))
        {
            return;
        }
        let s = state.clone();
        jars(state, move |jars| {
            if super::has_required(&s.request.required, &s.found.borrow(), &jars) {
                finish(&s, &jars);
            }
        });
    }

    /// The window closed: answers only when every required field has a value, so a step that
    /// requires nothing still answers, as on Android.
    fn close(state: &Rc<State>) {
        let s = state.clone();
        jars(state, move |jars| {
            if super::has_required(&s.request.required, &s.found.borrow(), &jars) {
                finish(&s, &jars);
            } else if !s.finished.replace(true) {
                gtk::main_quit();
            }
        });
    }

    fn finish(state: &State, jars: &Jars) {
        if state.finished.replace(true) {
            return;
        }
        *state.answer.borrow_mut() = Some(super::answer(
            &state.request.domains,
            &state.found.borrow(),
            jars,
        ));
        gtk::main_quit();
    }

    /// Reads every domain's cookies, for the bare domain and for `www.`, then calls `then`.
    fn jars(state: &State, then: impl FnOnce(Jars) + 'static) {
        let hosts: Vec<(String, String)> = state
            .request
            .domains
            .iter()
            .flat_map(|d| [(d.clone(), d.clone()), (d.clone(), format!("www.{d}"))])
            .collect();
        let left = Rc::new(Cell::new(hosts.len()));
        let jars = Rc::new(RefCell::new(Jars::new()));
        let then = Rc::new(RefCell::new(Some(then)));
        if hosts.is_empty() {
            if let Some(then) = then.borrow_mut().take() {
                then(Jars::new());
            }
            return;
        }
        for (domain, host) in hosts {
            let (left, jars, then) = (left.clone(), jars.clone(), then.clone());
            state.cookies.cookies(
                &format!("https://{host}"),
                None::<&gtk::gio::Cancellable>,
                move |found| {
                    let jar: Vec<(String, String)> = found
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|mut c| Some((c.name()?.to_string(), c.value()?.to_string())))
                        .collect();
                    jars.borrow_mut().entry(domain).or_default().extend(jar);
                    left.set(left.get() - 1);
                    if left.get() == 0
                        && let Some(then) = then.borrow_mut().take()
                    {
                        then(jars.take());
                    }
                },
            );
        }
    }
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
        // What the browser process reads back.
        assert_eq!(
            serde_json::from_str::<Request>(&serde_json::to_string(&r).unwrap()).unwrap(),
            r
        );
    }

    #[test]
    fn take_keeps_values_and_skips_empty_ones() {
        let mut found = BTreeMap::new();
        let fields = serde_json::json!({"token": "abc", "n": 5, "gone": null, "e": ""}).to_string();
        take(
            &serde_json::json!({"": fields, "s:a": "1", "s:b": null, "s:c": "null", "s:d": ""})
                .to_string(),
            &mut found,
        );
        let found: Vec<_> = found
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(found, [("f:n", "5"), ("f:token", "abc"), ("s:a", "1")]);
        let mut none = BTreeMap::new();
        take("not json", &mut none);
        take(&serde_json::json!({"": null}).to_string(), &mut none);
        assert!(none.is_empty());
    }

    #[test]
    fn required_needs_one_source_per_field() {
        let r = request(&messenger());
        let mut jars = Jars::new();
        let mut found = BTreeMap::new();
        assert!(!has_required(&r.required, &found, &jars));
        jars.insert("facebook.com".into(), vec![("c_user".into(), "1".into())]);
        assert!(!has_required(&r.required, &found, &jars));
        found.insert("s:xs_key".into(), "v".into());
        assert!(has_required(&r.required, &found, &jars));
        // A cookie counts only on the domain it was asked for.
        let other: Jars = [(
            "messenger.com".to_owned(),
            vec![("xs_other".to_owned(), "1".to_owned())],
        )]
        .into();
        assert!(!has_required(
            &[vec!["c:xs@messenger.com".into()]],
            &BTreeMap::new(),
            &other
        ));
        assert!(has_required(&[], &BTreeMap::new(), &Jars::new()));
    }

    #[test]
    fn answer_has_cookies_by_name_and_found_by_key() {
        let jars: Jars = [
            (
                "messenger.com".to_owned(),
                vec![
                    ("c_user".to_owned(), "1".to_owned()),
                    ("xs".to_owned(), "2".to_owned()),
                ],
            ),
            (
                "other.com".to_owned(),
                vec![("skip".to_owned(), "x".to_owned())],
            ),
        ]
        .into();
        let found: BTreeMap<String, String> = [
            ("s:xs_key".to_owned(), "v".to_owned()),
            ("f:token".to_owned(), "t".to_owned()),
        ]
        .into();
        let text = answer(&["messenger.com".into()], &found, &jars).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            serde_json::json!({"c_user": "1", "xs": "2", "xs_key": "v", "token": "t"})
        );
        assert_eq!(
            answer(&["messenger.com".into()], &BTreeMap::new(), &Jars::new()),
            None
        );
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

    #[test]
    fn scripts_quote_storage_keys() {
        assert_eq!(
            probe_script(&["a\"b".into()]),
            "JSON.stringify({\"\":window.__unisendLogin||null,\"s:a\\\"b\":localStorage.getItem(\"a\\\"b\")})"
        );
        assert!(start_script("f()").contains("Promise.resolve(f()).then"));
    }
}

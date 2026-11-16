//! Chat networks on a Beeper account: which bridges run, who is logged in to them, and the login
//! flow for adding an account.
//!
//! Beeper proxies each cloud bridge's mautrix provisioning API under the homeserver, authorised
//! with the Matrix access token:
//! `{homeserver}/_matrix/client/unstable/com.beeper.bridge/{bridge}/_matrix/provision/v3/...?user_id=`.
//! The login steps are mautrix bridgev2's, the same for every network: fill in fields, hand over
//! browser cookies, or show a code and wait.

use std::{collections::HashMap, time::Duration};

use anyhow::{Context as _, Result, anyhow, bail};
use matrix_sdk::reqwest::{Client, Method};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::i18n::t;

/// Bridges Beeper hosts that log in through this API, with the names its apps show.
/// Google Chat's bridge is the legacy one, without this API, and Bluesky's runs only on the
/// device in Beeper's apps (Beeper Desktop's bridge table: `bridge-local-bluesky`), so neither is here.
pub const KNOWN: [(&str, &str); 11] = [
    ("whatsapp", "WhatsApp"),
    ("signal", "Signal"),
    ("telegram", "Telegram"),
    ("facebookgo", "Messenger"),
    ("instagramgo", "Instagram"),
    ("twitter", "X"),
    ("discordgo", "Discord"),
    ("slackgo", "Slack"),
    ("linkedin", "LinkedIn"),
    ("gmessages", "Google Messages"),
    ("gvoice", "Google Voice"),
];

/// Broken logins kept for the chat list's notice; more than this many still read as "several".
const MAX_ATTENTION: usize = 16;
/// Waiting for a scan or an approval on the other device.
const WAIT_TIMEOUT: Duration = Duration::from_secs(600);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// A cloud bridge takes a while to come up after it is started.
const START_ATTEMPTS: u32 = 30;

/// A login the user has to sign in to again; the other states pass by themselves.
pub fn needs_sign_in(state: &str) -> bool {
    matches!(state, "BAD_CREDENTIALS" | "UNKNOWN_ERROR")
}

/// Logins an account can hold that need signing in again, as Beeper's account API reports them:
/// bridge and login name.
fn attention(whoami: &Value) -> Vec<(String, String)> {
    let bridges = whoami.pointer("/user/bridges").and_then(Value::as_object).into_iter().flatten();
    let logins = bridges.filter(|(bridge, _)| *bridge != "hungryserv").flat_map(|(bridge, info)| {
        let states = info.get("remoteState").and_then(Value::as_object).into_iter().flatten();
        states.filter(|(_, state)| state.get("state_event").and_then(Value::as_str).is_some_and(needs_sign_in)).map(move |(id, state)| {
            let name = state.get("remote_name").and_then(Value::as_str).filter(|n| !n.is_empty()).unwrap_or(id);
            (bridge.clone(), name.to_owned())
        })
    });
    logins.take(MAX_ATTENTION).collect()
}

pub fn display_name(bridge: &str) -> &str {
    KNOWN.iter().find(|k| k.0 == bridge).map_or(bridge, |k| k.1)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Network {
    /// Bridge name in Beeper's API, e.g. `facebookgo`.
    pub bridge: String,
    pub name: String,
    /// Beeper's state for the bridge itself: `RUNNING`, `STOPPED`, ...
    pub state: String,
    pub logins: Vec<Login>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Login {
    pub id: String,
    pub name: String,
    /// `CONNECTED`, `BAD_CREDENTIALS`, ...
    pub state: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Flow {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub bridge: String,
    pub login_id: String,
    pub step_id: String,
    pub instructions: String,
    pub kind: StepKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StepKind {
    Input(Vec<Field>),
    Cookies(Cookies),
    /// `kind` is `qr`, `emoji`, `code` or `nothing`.
    Show { kind: String, data: String },
    /// A passkey (WebAuthn) the bridge wants signed; Unisend cannot sign one yet.
    Passkey,
    Done,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Field {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// `username`, `phone_number`, `email`, `password`, `2fa_code`, `token`, `url`, `domain`, `select`.
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub default_value: String,
    /// A regex the value matches, e.g. `^[0-9]{6}$` for Messenger's history PIN.
    #[serde(default)]
    pub pattern: String,
}

/// A pattern that only digits match, such as `^[0-9]{6}$` or `^\d+$`: a number pad fits it.
pub fn digits_only(pattern: &str) -> bool {
    let body = pattern.trim_start_matches('^').trim_end_matches('$');
    let rest = body.replace("[0-9]", "").replace("\\d", "");
    rest.len() < body.len() && rest.chars().all(|c| c.is_ascii_digit() || "{},+*?".contains(c))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cookies {
    /// Page to sign in on.
    pub url: String,
    pub user_agent: String,
    /// The browser has finished when its address matches this.
    pub wait_for: String,
    pub fields: Vec<CookieField>,
    /// JavaScript the page runs to find fields itself; it resolves to an object keyed by field ID.
    pub extract_js: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CookieField {
    pub id: String,
    pub required: bool,
    /// Cookie names that can fill this field, with the domain each lives on.
    pub cookies: Vec<(String, String)>,
    /// Keys in the site's local storage that can fill it.
    pub storage: Vec<String>,
}

/// Where the two APIs live and who is asking.
#[derive(Clone)]
pub struct Api {
    pub http: Client,
    /// Beeper's account API.
    pub api: String,
    /// The homeserver, which proxies the bridges.
    pub homeserver: String,
    pub token: String,
    pub user_id: String,
}

impl Api {
    pub(crate) async fn send(&self, method: Method, url: String, body: Option<Value>, timeout: Duration) -> Result<Value> {
        let mut request = self.http.request(method, &url).bearer_auth(&self.token).timeout(timeout);
        if let Some(body) = body {
            request = request.header("Content-Type", "application/json").body(body.to_string());
        }
        let response = request.send().await?;
        let status = response.status();
        let json: Value = serde_json::from_slice(&response.bytes().await?).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(json);
        }
        // Both APIs put a readable reason in one of these.
        let reason = ["error", "message"].iter().find_map(|k| json.get(k)?.as_str());
        bail!("{} (HTTP {})", reason.unwrap_or("request failed"), status.as_u16())
    }

    async fn bridge(&self, bridge: &str, method: Method, path: &str, body: Option<Value>, timeout: Duration) -> Result<Value> {
        self.provision(bridge, method, &format!("v3/{path}"), None, body, timeout).await
    }

    /// `path` under the bridge's `/_matrix/provision/`, each segment percent-encoded; `login` picks
    /// one of the user's logins where the call is about one.
    async fn provision(&self, bridge: &str, method: Method, path: &str, login: Option<&str>, body: Option<Value>, timeout: Duration) -> Result<Value> {
        let base = format!("{}/_matrix/client/unstable/com.beeper.bridge/{bridge}/_matrix/provision", self.homeserver.trim_end_matches('/'));
        let mut url = matrix_sdk::reqwest::Url::parse_with_params(&base, [("user_id", &self.user_id)])?;
        url.path_segments_mut().map_err(|()| anyhow!("bad homeserver URL"))?.extend(path.split('/'));
        if let Some(login) = login {
            url.query_pairs_mut().append_pair("login_id", login);
        }
        self.send(method, url.into(), body, timeout).await
    }

    /// Every bridge on the account with the accounts logged in to it.
    pub async fn networks(&self) -> Result<Vec<Network>> {
        let whoami = self.send(Method::GET, format!("{}/whoami", self.api), None, CALL_TIMEOUT).await?;
        let bridges = whoami.pointer("/user/bridges").and_then(Value::as_object).context("no bridges in whoami")?;
        let mut out = Vec::with_capacity(bridges.len());
        for (bridge, info) in bridges {
            // Beeper's own homeserver shows up here as a bridge.
            if bridge == "hungryserv" {
                continue;
            }
            let state = info.pointer("/bridgeState/stateEvent").and_then(Value::as_str).unwrap_or("UNKNOWN").to_owned();
            // A bridge that is down still belongs in the list; it just has no logins to show.
            let detail = self.bridge(bridge, Method::GET, "whoami", None, CALL_TIMEOUT).await.unwrap_or(Value::Null);
            out.push(network(bridge, state, &detail));
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Logins that need signing in again, from one call to Beeper's account API: the states it last
    /// heard from each bridge (`remoteState`, bridge-manager api/beeperapi/whoami.go).
    pub async fn attention(&self) -> Result<Vec<(String, String)>> {
        Ok(attention(&self.send(Method::GET, format!("{}/whoami", self.api), None, CALL_TIMEOUT).await?))
    }

    /// Ways to log in to a bridge, starting the bridge first when the account does not run it yet.
    pub async fn flows(&self, bridge: &str) -> Result<Vec<Flow>> {
        if self.bridge(bridge, Method::GET, "login/flows", None, CALL_TIMEOUT).await.is_err() {
            let start = format!("{}/bridge/{bridge}/start", self.api);
            self.send(Method::POST, start, Some(json!({})), CALL_TIMEOUT).await.with_context(|| t!("Beeper did not start {network}", network = display_name(bridge)))?;
        }
        let mut last = anyhow!(t!("the bridge did not come up"));
        for _ in 0..START_ATTEMPTS {
            match self.bridge(bridge, Method::GET, "login/flows", None, CALL_TIMEOUT).await {
                Ok(json) => return Ok(serde_json::from_value(json.get("flows").cloned().unwrap_or(json!([])))?),
                Err(e) => last = e,
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(last)
    }

    /// `relogin` names an existing login to sign in again in place of adding one
    /// (`provisioninglogin.go` `PostLoginStart`, the `Override` login).
    pub async fn start(&self, bridge: &str, flow: &str, relogin: Option<&str>) -> Result<Step> {
        let json = self.provision(bridge, Method::POST, &format!("v3/login/start/{flow}"), relogin, Some(json!({})), CALL_TIMEOUT).await?;
        let login_id = json.get("login_id").and_then(Value::as_str).context("no login_id")?.to_owned();
        step(bridge, &login_id, &json)
    }

    /// Answers the current step and returns the next one. `values` is empty for a step that waits.
    pub async fn submit(&self, current: &Step, values: &HashMap<String, String>) -> Result<Step> {
        let (kind, body, timeout) = match current.kind {
            StepKind::Input(_) => ("user_input", json!(values), CALL_TIMEOUT),
            StepKind::Cookies(_) => ("cookies", json!(values), CALL_TIMEOUT),
            StepKind::Show { .. } => ("display_and_wait", json!({}), WAIT_TIMEOUT),
            StepKind::Passkey => bail!(t!("Unisend cannot sign in with a passkey yet")),
            StepKind::Done => bail!(t!("the login is already complete")),
        };
        let path = format!("login/step/{}/{}/{kind}", current.login_id, current.step_id);
        let json = self.bridge(&current.bridge, Method::POST, &path, Some(body), timeout).await?;
        step(&current.bridge, &current.login_id, &json)
    }

    /// Ends a login that is still waiting, so the bridge stops waiting for it too.
    pub async fn cancel(&self, bridge: &str, login_id: &str) -> Result<()> {
        self.bridge(bridge, Method::POST, &format!("login/cancel/{login_id}"), Some(json!({})), CALL_TIMEOUT).await?;
        Ok(())
    }

    pub async fn logout(&self, bridge: &str, login: &str) -> Result<()> {
        self.bridge(bridge, Method::POST, &format!("logout/{login}"), Some(json!({})), CALL_TIMEOUT).await?;
        Ok(())
    }

    /// The account's email address (`userInfo.email`, bridge-manager api/beeperapi/whoami.go).
    pub async fn email(&self) -> Result<String> {
        let whoami = self.send(Method::GET, format!("{}/whoami", self.api), None, CALL_TIMEOUT).await?;
        whoami.pointer("/userInfo/email").and_then(Value::as_str).filter(|e| !e.is_empty()).map(str::to_owned).context(t!("Beeper did not say which email address this account uses"))
    }
}

fn network(bridge: &str, state: String, detail: &Value) -> Network {
    let text = |v: &Value, path: &str| v.pointer(path).and_then(Value::as_str).map(str::to_owned);
    let logins = detail.get("logins").and_then(Value::as_array).into_iter().flatten().filter_map(|login| {
        let id = text(login, "/id")?;
        Some(Login {
            name: text(login, "/name").filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone()),
            state: text(login, "/state/state_event").or_else(|| text(login, "/state_event")).unwrap_or_default(),
            id,
        })
    });
    Network {
        bridge: bridge.to_owned(),
        name: KNOWN
            .iter()
            .find(|k| k.0 == bridge)
            .map(|k| k.1.to_owned())
            .or_else(|| text(detail, "/network/displayname"))
            .unwrap_or_else(|| bridge.to_owned()),
        state,
        logins: logins.collect(),
    }
}

fn step(bridge: &str, login_id: &str, json: &Value) -> Result<Step> {
    let text = |path: &str| json.pointer(path).and_then(Value::as_str).unwrap_or_default().to_owned();
    let kind = match json.get("type").and_then(Value::as_str) {
        Some("user_input") => {
            StepKind::Input(serde_json::from_value(json.pointer("/user_input/fields").cloned().unwrap_or(json!([])))?)
        }
        Some("cookies") => {
            let fields = json.pointer("/cookies/fields").and_then(Value::as_array).into_iter().flatten().filter_map(|f| {
                let sources = f.get("sources").and_then(Value::as_array).into_iter().flatten();
                let of = |kind: &'static str| sources.clone().filter(move |s| s.get("type").and_then(Value::as_str) == Some(kind)).filter_map(|s| Some((s, s.get("name")?.as_str()?.to_owned())));
                let cookies = of("cookie").map(|(s, name)| (name, s.get("cookie_domain").and_then(Value::as_str).unwrap_or_default().to_owned()));
                Some(CookieField {
                    id: f.get("id")?.as_str()?.to_owned(),
                    required: f.get("required").and_then(Value::as_bool).unwrap_or(false),
                    cookies: cookies.collect(),
                    storage: of("local_storage").map(|(_, name)| name).collect(),
                })
            });
            StepKind::Cookies(Cookies {
                url: text("/cookies/url"),
                user_agent: text("/cookies/user_agent"),
                wait_for: text("/cookies/wait_for_url_pattern"),
                fields: fields.collect(),
                extract_js: text("/cookies/extract_js"),
            })
        }
        Some("display_and_wait") => StepKind::Show { kind: text("/display_and_wait/type"), data: text("/display_and_wait/data") },
        Some("webauthn") => StepKind::Passkey,
        Some("complete") => StepKind::Done,
        other => bail!(t!("this app does not know the login step {step}", step = format!("{other:?}"))),
    };
    Ok(Step {
        bridge: bridge.to_owned(),
        login_id: login_id.to_owned(),
        step_id: text("/step_id"),
        instructions: text("/instructions"),
        kind,
    })
}

/// Cookie names and values out of whatever a browser hands over: a `Cookie:` header, the
/// `name=value; ...` string itself, a "Copy as cURL" command, or a JSON object.
pub fn parse_cookies(pasted: &str) -> HashMap<String, String> {
    let pasted = pasted.trim();
    if let Ok(Value::Object(map)) = serde_json::from_str(pasted) {
        return map.into_iter().filter_map(|(k, v)| Some((k, v.as_str()?.to_owned()))).collect();
    }
    // In a cURL command the cookies sit in `-H 'cookie: ...'` or `-b '...'`.
    let lower = pasted.to_ascii_lowercase();
    let header = ["cookie: ", "-b '", "-b \"", "--cookie '", "--cookie \""].iter().find_map(|marker| {
        let rest = &pasted[lower.find(marker)? + marker.len()..];
        Some(rest.split(['\'', '"', '\n']).next().unwrap_or(rest))
    });
    header
        .unwrap_or(pasted)
        .split(';')
        .filter_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (!name.is_empty() && !name.contains(char::is_whitespace)).then(|| (name.to_owned(), value.to_owned()))
        })
        .collect()
}

/// Values for the step's fields from what the browser gave: cookies and local storage by their
/// names, and anything else (an `extract_js` result, pasted JSON) by field ID. `Err` names a
/// required value that is missing.
pub fn fill_cookies(fields: &[CookieField], jar: &HashMap<String, String>) -> Result<HashMap<String, String>, String> {
    let mut values = HashMap::new();
    for field in fields {
        let names = field.cookies.iter().map(|c| &c.0).chain(&field.storage).chain([&field.id]);
        match names.filter_map(|name| jar.get(name)).find(|v| !v.is_empty()) {
            Some(value) => {
                values.insert(field.id.clone(), value.clone());
            }
            None if field.required => {
                return Err(field.cookies.first().map_or_else(|| field.id.clone(), |c| c.0.clone()));
            }
            None => {}
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_from_what_browsers_give() {
        let want: HashMap<String, String> = [("c_user", "100"), ("xs", "a%3Ab")].map(|(k, v)| (k.to_owned(), v.to_owned())).into();
        assert_eq!(parse_cookies("c_user=100; xs=a%3Ab"), want);
        assert_eq!(parse_cookies("Cookie: c_user=100; xs=a%3Ab"), want);
        assert_eq!(parse_cookies(r#"{"c_user": "100", "xs": "a%3Ab"}"#), want);
        let curl = "curl 'https://www.messenger.com/' \\\n  -H 'accept: */*' \\\n  -H 'cookie: c_user=100; xs=a%3Ab' \\\n  -H 'user-agent: Mozilla/5.0 (X11; Linux x86_64)'";
        assert_eq!(parse_cookies(curl), want);
        assert_eq!(parse_cookies("curl https://x -b 'c_user=100; xs=a%3Ab' --compressed"), want);
        assert!(parse_cookies("not cookies at all").is_empty());
    }

    #[test]
    fn required_cookies_are_checked() {
        let field = |id: &str, required| CookieField { id: id.to_owned(), required, cookies: vec![(id.to_owned(), "facebook.com".to_owned())], storage: vec![] };
        let fields = [field("c_user", true), field("xs", true), field("datr", false)];
        let jar = parse_cookies("c_user=100; xs=tok");
        assert_eq!(fill_cookies(&fields, &jar).unwrap().len(), 2);
        assert_eq!(fill_cookies(&fields, &parse_cookies("c_user=100")), Err("xs".to_owned()));

        // Discord's token has no cookie: its page script, or pasted JSON, gives it by field ID.
        let token = CookieField { id: "token".into(), required: true, cookies: vec![], storage: vec!["token".into()] };
        let x = CookieField { id: "castle_token".into(), required: true, cookies: vec![], storage: vec![] };
        let got = fill_cookies(&[token, x], &parse_cookies(r#"{"token": "abc", "castle_token": "c1"}"#)).unwrap();
        assert_eq!((got["token"].as_str(), got["castle_token"].as_str()), ("abc", "c1"));
        let empty = CookieField { id: "token".into(), required: true, cookies: vec![], storage: vec![] };
        assert_eq!(fill_cookies(&[empty], &parse_cookies(r#"{"token": ""}"#)), Err("token".to_owned()));
    }

    #[test]
    fn steps_parse() {
        let cookies = json!({
            "login_id": "L1", "type": "cookies", "step_id": "fi.mau.meta.cookies", "instructions": "Sign in",
            "cookies": {
                "url": "https://www.messenger.com/", "user_agent": "UA", "wait_for_url_pattern": "^https://www\\.messenger\\.com/.*$",
                "fields": [
                    { "id": "xs", "required": true, "sources": [
                        { "type": "cookie", "name": "xs", "cookie_domain": "messenger.com" },
                        { "type": "request_header", "name": "x" }
                    ] }
                ]
            }
        });
        let parsed = step("facebookgo", "L1", &cookies).unwrap();
        let StepKind::Cookies(c) = &parsed.kind else { panic!("{parsed:?}") };
        assert_eq!((parsed.step_id.as_str(), c.url.as_str(), c.fields.len()), ("fi.mau.meta.cookies", "https://www.messenger.com/", 1));
        assert_eq!(c.fields[0].cookies, [("xs".to_owned(), "messenger.com".to_owned())]);

        let discord = json!({ "type": "cookies", "step_id": "fi.mau.discord.cookies", "cookies": {
            "url": "https://discord.com/login", "extract_js": "new Promise(r => r({}))",
            "fields": [{ "id": "token", "required": true, "sources": [
                { "type": "special", "name": "fi.mau.discord.token" }, { "type": "local_storage", "name": "token" }
            ] }]
        } });
        let StepKind::Cookies(d) = step("discordgo", "L", &discord).unwrap().kind else { panic!() };
        assert_eq!((d.extract_js.as_str(), d.fields[0].cookies.len(), d.fields[0].storage.as_slice()), ("new Promise(r => r({}))", 0, ["token".to_owned()].as_slice()));

        let pin = json!({ "type": "user_input", "step_id": "fi.mau.meta.history_pin", "user_input": { "fields": [
            { "type": "password", "id": "pin", "name": "PIN", "optional": true, "pattern": "^[0-9]{6}$" },
            { "type": "select", "id": "m", "name": "Method", "options": ["SMS", "App"], "default_value": "App" }
        ] } });
        let StepKind::Input(f) = step("facebookgo", "L", &pin).unwrap().kind else { panic!() };
        assert!(digits_only(&f[0].pattern));
        assert_eq!((f[1].options.len(), f[1].default_value.as_str()), (2, "App"));
        assert_eq!(step("whatsapp", "L", &json!({ "type": "webauthn", "step_id": "fi.mau.whatsapp.login.passkey" })).unwrap().kind, StepKind::Passkey);

        let input = json!({ "type": "user_input", "step_id": "s", "user_input": { "fields": [
            { "type": "phone_number", "id": "phone", "name": "Phone number", "description": "With country code" }
        ] } });
        assert!(matches!(step("signal", "L", &input).unwrap().kind, StepKind::Input(f) if f[0].kind == "phone_number"));

        let qr = json!({ "type": "display_and_wait", "step_id": "s", "display_and_wait": { "type": "qr", "data": "2@abc" } });
        assert_eq!(step("whatsapp", "L", &qr).unwrap().kind, StepKind::Show { kind: "qr".into(), data: "2@abc".into() });
        assert_eq!(step("whatsapp", "L", &json!({ "type": "complete", "step_id": "s" })).unwrap().kind, StepKind::Done);
        assert!(step("whatsapp", "L", &json!({ "type": "hologram" })).is_err());
    }

    #[test]
    fn broken_logins_from_beepers_whoami() {
        let whoami = json!({ "user": { "bridges": {
            "whatsapp": { "remoteState": { "48600": { "state_event": "CONNECTED", "remote_name": "+48 600" } } },
            "facebookgo": { "remoteState": {
                "100": { "state_event": "BAD_CREDENTIALS", "remote_name": "Dean" },
                "200": { "state_event": "LOGGED_OUT", "remote_name": "Old" },
                "300": { "state_event": "UNKNOWN_ERROR" }
            } },
            "signal": { "bridgeState": { "stateEvent": "RUNNING" } },
            "hungryserv": { "remoteState": { "x": { "state_event": "BAD_CREDENTIALS" } } }
        } } });
        let want = [("facebookgo", "Dean"), ("facebookgo", "300")].map(|(b, n)| (b.to_owned(), n.to_owned()));
        assert_eq!(attention(&whoami), want);
        assert!(attention(&Value::Null).is_empty());
    }

    #[test]
    fn number_pad_only_for_digit_patterns() {
        for digits in ["^[0-9]{6}$", "^\\d+$", "[0-9]{4,8}", "^\\d{6}$"] {
            assert!(digits_only(digits), "{digits}");
        }
        for other in ["", "^[a-z0-9]+$", "^.{6}$", "^\\+?[0-9]+$", "^(SMS|App)$"] {
            assert!(!digits_only(other), "{other}");
        }
    }

    #[test]
    fn networks_parse() {
        let detail = json!({ "network": { "displayname": "WhatsApp" }, "logins": [
            { "id": "1", "name": "+48 600", "state": { "state_event": "CONNECTED" } },
            { "id": "2", "name": "", "state_event": "BAD_CREDENTIALS" }
        ] });
        let n = network("whatsapp", "RUNNING".into(), &detail);
        assert_eq!((n.name.as_str(), n.logins.len()), ("WhatsApp", 2));
        assert_eq!((n.logins[0].name.as_str(), n.logins[0].state.as_str()), ("+48 600", "CONNECTED"));
        assert_eq!((n.logins[1].name.as_str(), n.logins[1].state.as_str()), ("2", "BAD_CREDENTIALS"));
        assert!(network("facebookgo", "STOPPED".into(), &Value::Null).logins.is_empty());
    }
}

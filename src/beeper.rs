//! Beeper account login: email code exchanged for a JWT, which the Matrix
//! homeserver accepts as `org.matrix.login.jwt`.
//! Endpoints and payloads mirror github.com/beeper/bridge-manager api/beeperapi/login.go.

use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use matrix_sdk::reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

pub const API_URL: &str = "https://api.beeper.com";
pub const HOMESERVER_URL: &str = "https://matrix.beeper.com";
// Static bearer used by Beeper's own clients for the pre-auth login endpoints.
const LOGIN_AUTH: &str = "Bearer BEEPER-PRIVATE-API-PLEASE-DONT-USE";
const APP_TYPE: &str = "bbctl";

#[derive(Debug)]
pub struct InvalidCode {
    pub retries_left: u64,
}

impl std::fmt::Display for InvalidCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::i18n::tn!(self.retries_left, "Invalid code, {n} attempt left", "Invalid code, {n} attempts left"))
    }
}

impl std::error::Error for InvalidCode {}

async fn post(http: &Client, path: &str, body: Value) -> Result<Value> {
    let resp = http
        .post(format!("{API_URL}{path}"))
        .header("Authorization", LOGIN_AUTH)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .with_context(|| format!("POST {path}"))?;
    let status = resp.status();
    let json: Value = serde_json::from_slice(&resp.bytes().await?).unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(json);
    }
    if status == StatusCode::FORBIDDEN
        && path == "/user/login/response"
        && let Some(retries) = json.get("retries").and_then(Value::as_u64).filter(|r| *r > 0)
    {
        return Err(InvalidCode { retries_left: retries }.into());
    }
    match json.get("error").and_then(Value::as_str) {
        Some(msg) => bail!("Beeper API error (HTTP {status}): {msg}"),
        None => bail!("Beeper API returned HTTP {status}"),
    }
}

/// Starts a login and asks Beeper to email a code. Returns the request id.
pub async fn send_code(http: &Client, email: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct Start {
        request: String,
    }
    let start: Start = serde_json::from_value(post(http, "/user/login", json!({})).await?)
        .context("unexpected /user/login response")?;
    post(
        http,
        "/user/login/email",
        json!({ "request": start.request, "email": email, "appType": APP_TYPE, "onlyExistingAccounts": true }),
    )
    .await?;
    Ok(start.request)
}

/// Exchanges the emailed code for a JWT accepted by the homeserver.
pub async fn submit_code(http: &Client, request: &str, code: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct Token {
        token: String,
    }
    let resp = post(
        http,
        "/user/login/response",
        json!({ "request": request, "response": code, "appType": APP_TYPE, "onlyExistingAccounts": true }),
    )
    .await?;
    Ok(serde_json::from_value::<Token>(resp).context("unexpected /user/login/response")?.token)
}

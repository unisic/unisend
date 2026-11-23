//! Signing in through the homeserver's own web page: OAuth 2.0 (next-gen auth, MSC3861) when the
//! server has an authorization service, else single sign-on (`m.login.sso`). The browser comes
//! back to a listener on this machine's loopback address, which takes one answer and closes.

use std::net::Ipv4Addr;

use matrix_sdk::{
    authentication::oauth::{
        ClientId, OAuthSession, UserSession,
        registration::{ApplicationType, ClientMetadata, Localized, OAuthGrantType},
    },
    reqwest::Url,
    ruma::api::client::discovery::get_authorization_server_metadata::v1::Prompt,
    utils::UrlOrQuery,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;

/// How long the browser has to come back before the listener closes.
const WAIT: Duration = Duration::from_secs(10 * 60);
/// Connections read at once: a browser may open a spare one it never sends on.
const CONNECTIONS: usize = 4;
/// The most of a request's head that is read.
const HEAD: usize = 8 << 10;
/// How long a connection may take to send its request line.
const READ: Duration = Duration::from_secs(10);
/// Where the authorization page links the app's name to.
const HOME_PAGE: &str = "https://github.com/unisic/unisend";

/// The ways a homeserver takes a sign-in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Methods {
    /// The server as it was typed, so the form knows when the field changed since.
    pub typed: String,
    pub oauth: bool,
    /// Its authorization service also opens a new account (`prompt=create`).
    pub create: bool,
    /// Single sign-on, by identity provider (ID, name); empty when the server names none.
    pub sso: Option<Vec<(String, String)>>,
    pub password: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Way {
    OAuth,
    /// A new account on the server's authorization service, signed in once made.
    Create,
    /// Single sign-on, with the identity provider picked, if the server named any.
    Sso(Option<String>),
}

/// A sign-in waiting for the browser.
pub(super) struct Pending {
    client: Client,
    way: Way,
    /// The loopback address the browser comes back to.
    back: String,
    _listen: AbortOnDrop,
}

impl State {
    /// The homeserver `server` names, found the way the SDK discovers it, in a client built by
    /// `builder`. Fails unless it is reached over https (or on this machine).
    pub(super) async fn find(&self, builder: matrix_sdk::ClientBuilder, server: &str) -> Result<Client> {
        let not_reached = || anyhow!(t!("Could not reach {server}. Check the address and your connection.", server = server));
        // The SDK retries a silent host for minutes; someone waiting at the form should hear sooner.
        let discovery = builder.server_name_or_homeserver_url(server).build();
        let client = tokio::time::timeout(Duration::from_secs(30), discovery).await.map_err(|_| not_reached())?.map_err(|e| {
            log::warn!("homeserver discovery: {e}");
            match unreachable(&e) {
                true => not_reached(),
                false => anyhow!(t!("No Matrix server found at {server}.", server = server)),
            }
        })?;
        // A server's .well-known may point anywhere; secrets only go over https.
        let homeserver = client.homeserver();
        if homeserver.scheme() != "https" && !homeserver.host_str().is_some_and(is_local) {
            return Err(anyhow!(t!("{server} does not offer an encrypted (https) connection, so this app will not send it a password.", server = server)));
        }
        Ok(client)
    }

    /// What the server at `typed` offers to sign in with. Nothing is stored.
    pub(super) async fn methods(&self, typed: String) -> Result<Methods> {
        let server = server_from_input(&typed).map_err(|e| anyhow!(e))?;
        let client = self.find(Client::builder().http_client(self.http.clone()), &server).await?;
        // shortcut: only OAuth servers open accounts here; the older registration (captcha, email and
        // terms steps) would need its own forms, worth it if a server people pick lacks OAuth.
        let (oauth, create) = match client.oauth().server_metadata().await {
            Ok(m) => (true, m.prompt_values_supported.contains(&Prompt::Create)),
            Err(matrix_sdk::authentication::oauth::error::OAuthDiscoveryError::NotSupported) => (false, false),
            Err(e) => {
                log::warn!("oauth discovery: {e}");
                (false, false)
            }
        };
        let types = client.matrix_auth().get_login_types().await.map_err(|e| match unreachable(&e) {
            true => anyhow!(t!("Could not reach {server}. Check your connection.", server = server)),
            false => anyhow!(t!("{server} did not answer like a Matrix server: {error}", server = server, error = plain(&e))),
        })?;
        Ok(Methods { create, ..methods_of(typed, oauth, &types.flows) })
    }

    /// Opens the server's sign-in page: the UI shows it, and the answer comes as `BrowserDone`.
    pub(super) async fn browser_login(&mut self, typed: &str, way: Way) -> Result<()> {
        let server = server_from_input(typed).map_err(|e| anyhow!(e))?;
        self.browser = None;
        self.fresh_store()?;
        let client = self.find(self.client_builder(), &server).await?;
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let back = Url::parse(&format!("http://127.0.0.1:{}/", listener.local_addr()?.port()))?;
        let url = match &way {
            Way::OAuth => client.oauth().login(back.clone(), None, Some(registration(&back).into()), None).build().await?.url,
            Way::Create => client.oauth().login(back.clone(), None, Some(registration(&back).into()), None).prompt(vec![Prompt::Create]).build().await?.url,
            Way::Sso(idp) => Url::parse(&client.matrix_auth().get_sso_login_url(back.as_str(), idp.as_deref()).await?)?,
        };
        let back = back.to_string();
        let answer = back.clone();
        let slot = crate::accounts::slot_of(&self.dir);
        let listen = AbortOnDrop(tokio::spawn(async move {
            let url = tokio::time::timeout(WAIT, listen(listener)).await.ok().flatten();
            if let Some(tx) = commands(slot) {
                let _ = tx.send(Cmd::BrowserDone { back: answer, url }).await;
            }
        }));
        self.browser = Some(Pending { client, way, back: back.clone(), _listen: listen });
        self.emit(Ev::SignInPage { url: url.to_string(), back }).await;
        Ok(())
    }

    /// The browser came back to `back` at `url`, or gave up (none).
    pub(super) async fn browser_done(&mut self, back: &str, url: Option<String>) -> Result<()> {
        // An answer to a sign-in that was cancelled or replaced since.
        if self.browser.as_ref().is_none_or(|p| p.back != back) {
            return Ok(());
        }
        let Some(Pending { client, way, .. }) = self.browser.take() else { return Ok(()) };
        let url = url.filter(|u| u.starts_with(back)).ok_or_else(|| anyhow!(t!("The sign-in in the browser was not finished. Try again.")))?;
        let url = UrlOrQuery::Url(Url::parse(&url)?);
        let homeserver = client.homeserver().to_string();
        keep_tokens(&client, &self.dir, &self.seal, Some(homeserver.clone()))?;
        match way {
            Way::OAuth | Way::Create => client.oauth().finish_login(url).await.map_err(|e| anyhow!(t!("Could not sign in: {error}", error = plain(&e))))?,
            Way::Sso(_) => {
                client
                    .matrix_auth()
                    .login_with_sso_callback(url)?
                    .initial_device_display_name("Unisend")
                    .await
                    .map_err(|e| anyhow!(t!("Could not sign in: {error}", error = plain(&e))))?;
            }
        }
        (self.homeserver, self.beeper) = (homeserver, false);
        let saved = saved_session(&client, Some(self.homeserver.clone())).ok_or_else(|| anyhow!(t!("Login returned no session")))?;
        self.seal.write(&self.dir, vault::SESSION, &serde_json::to_vec(&saved)?)?;
        self.go_live(client).await;
        Ok(())
    }
}

/// The form's choices from the server's answers: OAuth alone when it has it (its login types then
/// only stand in for older apps), else single sign-on and a password as listed.
fn methods_of(typed: String, oauth: bool, flows: &[LoginType]) -> Methods {
    let mut methods = Methods { typed, oauth, ..Default::default() };
    if oauth {
        return methods;
    }
    for flow in flows {
        match flow {
            LoginType::Password(_) => methods.password = true,
            LoginType::Sso(sso) => {
                let named = sso.identity_providers.iter().map(|p| (p.id.clone(), p.name.clone()));
                methods.sso.get_or_insert_default().extend(named);
            }
            _ => {}
        }
    }
    methods
}

/// This app as it registers with the server's authorization service, which shows the user its name.
fn registration(back: &Url) -> Raw<ClientMetadata> {
    let mut metadata = ClientMetadata::new(
        ApplicationType::Native,
        vec![OAuthGrantType::AuthorizationCode { redirect_uris: vec![back.clone()] }],
        Localized::new(Url::parse(HOME_PAGE).expect("valid home page"), []),
    );
    metadata.client_name = Some(Localized::new("Unisend".to_owned(), []));
    Raw::new(&metadata).expect("client metadata serializes")
}

/// The session as `session.json` keeps it, with the OAuth client when it signed in that way.
pub(super) fn saved_session(client: &Client, homeserver: Option<String>) -> Option<SavedSession> {
    let session = MatrixSession { meta: client.session_meta()?.clone(), tokens: client.session_tokens()? };
    Some(SavedSession { session, homeserver, oauth: client.oauth().client_id().map(|id| id.as_str().to_owned()) })
}

/// Restores a saved session in the client, the way it was signed in.
pub(super) async fn restore(client: &Client, saved: SavedSession) -> matrix_sdk::Result<()> {
    let MatrixSession { meta, tokens } = saved.session;
    match saved.oauth {
        Some(id) => client.oauth().restore_session(OAuthSession { client_id: ClientId::new(id), user: UserSession { meta, tokens } }, RoomLoadSettings::default()).await,
        None => client.matrix_auth().restore_session(MatrixSession { meta, tokens }, RoomLoadSettings::default()).await,
    }
}

/// Writes the session again each time the SDK refreshes its tokens: an OAuth server takes back
/// the old refresh token once the new one is used, so a stale file would sign the device out.
pub(super) fn keep_tokens(client: &Client, dir: &Path, seal: &Seal, homeserver: Option<String>) -> Result<()> {
    let (dir, seal) = (dir.to_owned(), seal.clone());
    client.set_session_callbacks(
        Box::new(|client| client.session_tokens().ok_or_else(|| "signed out".into())),
        Box::new(move |client| {
            let saved = saved_session(&client, homeserver.clone()).ok_or("signed out")?;
            seal.write(&dir, vault::SESSION, &serde_json::to_vec(&saved)?)?;
            Ok(())
        }),
    )?;
    Ok(())
}

/// Waits for the browser's request with a query, answers it with a page saying the app has it,
/// and returns its full URL. Anything else gets a 404; a silent connection is dropped.
async fn listen(listener: tokio::net::TcpListener) -> Option<String> {
    let port = listener.local_addr().ok()?.port();
    let mut reading = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    // Out of file descriptors, say: no busy loop while it lasts.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                };
                if reading.len() < CONNECTIONS {
                    reading.spawn(tokio::time::timeout(READ, answer(stream, port)));
                }
            }
            Some(done) = reading.join_next(), if !reading.is_empty() => {
                if let Ok(Ok(Some(url))) = done {
                    return Some(url);
                }
            }
        }
    }
}

/// Reads one request; a GET with a query is the browser coming back.
async fn answer(mut stream: tokio::net::TcpStream, port: u16) -> Option<String> {
    let mut head = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < HEAD {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&chunk[..n]);
    }
    let url = request_url(&head, port);
    let reply = match url {
        Some(_) => {
            let page = format!(
                "<!doctype html><meta charset=utf-8><title>Unisend</title><p style=\"font:16px sans-serif;margin:3em auto;max-width:30em\">{}</p>",
                t!("Signed in. You can close this page and return to Unisend.")
            );
            format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{page}", page.len())
        }
        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
    };
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
    url
}

/// The full URL of a `GET /?query` request head, as the browser was sent to it.
fn request_url(head: &[u8], port: u16) -> Option<String> {
    let line = std::str::from_utf8(head.split(|&b| b == b'\r').next()?).ok()?;
    let target = line.strip_prefix("GET ")?.rsplit_once(' ')?.0;
    let query = target.strip_prefix("/?").filter(|q| !q.is_empty())?;
    Some(format!("http://127.0.0.1:{port}/?{query}"))
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::api::client::session::get_login_types::v3::{IdentityProvider, PasswordLoginType, SsoLoginType, TokenLoginType};

    use super::*;

    #[test]
    fn request_url_takes_only_a_get_with_a_query() {
        let url = request_url(b"GET /?code=abc&state=xyz HTTP/1.1\r\nHost: 127.0.0.1:5\r\n\r\n", 5);
        assert_eq!(url.as_deref(), Some("http://127.0.0.1:5/?code=abc&state=xyz"));
        assert_eq!(request_url(b"GET /favicon.ico HTTP/1.1\r\n\r\n", 5), None);
        assert_eq!(request_url(b"GET / HTTP/1.1\r\n\r\n", 5), None);
        assert_eq!(request_url(b"GET /? HTTP/1.1\r\n\r\n", 5), None);
        assert_eq!(request_url(b"POST /?code=a HTTP/1.1\r\n\r\n", 5), None);
        assert_eq!(request_url(b"", 5), None);
    }

    #[test]
    fn oauth_hides_the_older_ways() {
        let flows = [LoginType::Password(PasswordLoginType::new()), LoginType::Sso(SsoLoginType::new()), LoginType::Token(TokenLoginType::new())];
        let m = methods_of("matrix.org".into(), true, &flows);
        assert_eq!(m, Methods { typed: "matrix.org".into(), oauth: true, create: false, sso: None, password: false });
        let m = methods_of("a".into(), false, &flows);
        assert_eq!((m.password, m.sso), (true, Some(vec![])));
        let mut sso = SsoLoginType::new();
        sso.identity_providers = vec![IdentityProvider::new("gitlab".into(), "GitLab".into())];
        let m = methods_of("a".into(), false, &[LoginType::Sso(sso)]);
        assert_eq!((m.password, m.sso), (false, Some(vec![("gitlab".into(), "GitLab".into())])));
    }

    #[test]
    fn registration_names_the_app_and_its_loopback_address() {
        let back = Url::parse("http://127.0.0.1:4567/").unwrap();
        let json: serde_json::Value = registration(&back).deserialize_as_unchecked().unwrap();
        assert_eq!(json["application_type"], "native");
        assert_eq!(json["redirect_uris"][0], "http://127.0.0.1:4567/");
        assert_eq!(json["client_name"], "Unisend");
        assert_eq!(json["token_endpoint_auth_method"], "none");
    }

    #[tokio::test]
    async fn listener_skips_stray_requests_and_answers_the_browser() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(listen(listener));
        // A connection that never sends, as a browser's spare one, does not hold up the answer.
        let _idle = tokio::net::TcpStream::connect(addr).await.unwrap();
        let get = |path: &'static str| async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes()).await.unwrap();
            let mut reply = String::new();
            s.read_to_string(&mut reply).await.unwrap();
            reply
        };
        assert!(get("/favicon.ico").await.starts_with("HTTP/1.1 404"));
        assert!(get("/?loginToken=t").await.starts_with("HTTP/1.1 200"));
        let url = tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
        assert_eq!(url, Some(format!("http://127.0.0.1:{}/?loginToken=t", addr.port())));
        // The listener is gone with its task.
        assert!(tokio::net::TcpStream::connect(addr).await.is_err());
    }
}

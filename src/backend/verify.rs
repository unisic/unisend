//! The account's devices and interactive verification between them: emoji comparison (`m.sas.v1`,
//! Matrix spec "Short Authentication String (SAS) verification"), driven by matrix-sdk's
//! `VerificationRequest` and `SasVerification`. Also signing out another device, which the server
//! guards with user-interactive auth. Other people are verified the same way, in a one-to-one chat
//! with them, and the open chat's people whose keys changed since this device first saw them are
//! followed, as Element X warns of them.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use futures_util::StreamExt as _;
use matrix_sdk::{
    Client,
    encryption::verification::{CancelInfo, SasState, SasVerification, VerificationRequest, VerificationRequestState},
    Room,
    ruma::{
        OwnedDeviceId, OwnedUserId, UserId,
        api::client::uiaa::{AuthData, MatrixUserIdentifier, Password, UiaaInfo, UserIdentifier},
        api::error::ErrorKind,
        events::{
            key::verification::{cancel::CancelCode, request::ToDeviceKeyVerificationRequestEvent},
            room::message::{MessageType, OriginalSyncRoomMessageEvent},
        },
        serde::JsonObject,
    },
};
use tokio::sync::mpsc;

use super::{Ev, plain};
use crate::i18n::t;

/// Synapse refuses longer device names (`MAX_DEVICE_DISPLAY_NAME_LEN`, synapse/handlers/device.py).
pub const NAME_MAX: usize = 100;
/// Devices listed; an account with more is someone else's problem, not this phone's memory.
const MAX_DEVICES: usize = 200;
/// How long the side that accepted a request waits for the other to pick emoji before it does.
const START_WAIT: Duration = Duration::from_secs(3);
/// The UIA stage Beeper's homeserver adds for accounts without a password (beeper/synapse,
/// "Add JWT UI auth flow", synapse/handlers/ui_auth/checkers.py `JwtAuthChecker`).
const JWT: &str = "org.matrix.login.jwt";
const PASSWORD: &str = "m.login.password";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Trust {
    /// Signed by the account's cross-signing key.
    Verified,
    Unverified,
    /// The device never published encryption keys (a bot, a bridge, a very old client).
    NoKeys,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub last_seen: Option<u64>,
    pub this: bool,
    pub trust: Trust,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CrossSigning {
    /// The account has no cross-signing identity, so nothing can vouch for a device.
    Missing,
    /// The account has one; this device does not hold its private keys.
    Elsewhere,
    /// This device holds all three private keys and can verify others.
    Here,
}

#[derive(Clone, Debug)]
pub struct Devices {
    /// This device first, then the most recently seen.
    pub list: Vec<Device>,
    pub this_verified: bool,
    pub cross_signing: CrossSigning,
}

/// Where a verification stands, as the sheet shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum Verify {
    /// Someone asks to verify; `who` names their device, or them.
    Incoming {
        who: String,
    },
    /// Sent, or accepted: the other side has to act.
    Waiting,
    /// The seven emoji with their names, in order.
    Emoji(Vec<(String, String)>),
    /// This side said they match; the other has not yet.
    Confirmed,
    Done,
    Failed(String),
}

/// Someone in the open chat whose keys changed since this device first saw them.
#[derive(Clone, Debug, PartialEq)]
pub struct Changed {
    pub user: OwnedUserId,
    pub name: String,
    /// The user had verified them, which makes the change serious.
    pub was_verified: bool,
}

/// How long the end of a person's verification waits for their keys to read as verified.
const SIGNED_TRIES: usize = 50;
const SIGNED_STEP: Duration = Duration::from_millis(100);
/// People named in a chat's warning; more changes at once say nothing more useful.
const MAX_CHANGED: usize = 20;
/// People followed per chat; past it a new change is not tracked.
const MAX_TRACKED: usize = 200;

/// What signing out another device asks of the user.
#[derive(Clone, Debug, PartialEq)]
pub enum Auth {
    Password,
    /// A Beeper account: a code emailed to this address proves it is the user.
    Code {
        email: String,
    },
    CodeSent,
    /// The server offers no way this app can complete.
    Unsupported,
}

/// The user's answer to `Auth`.
pub enum Proof {
    /// Nothing yet: find out what the server asks for.
    Ask,
    Password(String),
    SendCode,
    Code(String),
}

struct Flow {
    request: VerificationRequest,
    sas: Option<SasVerification>,
    task: Option<tokio::task::AbortHandle>,
}

/// One verification at a time, shared with the event handlers. Never held across an await.
type Slot = Arc<Mutex<Option<Flow>>>;

/// A device being signed out: its ID, the server's UIA session, and the Beeper login request
/// that emailed the code.
struct Pending {
    device: OwnedDeviceId,
    session: Option<String>,
    email: Option<String>,
    request: Option<String>,
}

#[derive(Default)]
pub struct Verifier {
    slot: Slot,
    pending: Option<Pending>,
    /// Lists the devices again whenever this device's trust changes.
    watch: Option<tokio::task::AbortHandle>,
}

impl Verifier {
    /// Hears requests from the user's other devices (to-device) and from other people (in a room).
    /// The handlers live as long as `client`; the watcher until `stop`.
    pub fn listen(&mut self, client: &Client, ev: &mpsc::Sender<Ev>) {
        // A device becomes verified some time after its verification ends: once the other device
        // has uploaded its signature and this one has fetched it.
        let (watched, tx) = (client.clone(), ev.clone());
        self.watch = Some(
            tokio::spawn(async move {
                let mut states = watched.encryption().verification_state();
                // Listing the devices queries this account's keys, and the SDK sets the state again
                // after every such query, changed or not: without this check that is a loop.
                let mut last = None;
                while let Some(state) = states.next().await {
                    if last.replace(state) == Some(state) {
                        continue;
                    }
                    match devices(&watched).await {
                        Ok(list) => {
                            if tx.send(Ev::Devices(list)).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => log::debug!("devices: {e:#}"),
                    }
                }
            })
            .abort_handle(),
        );
        let (slot, tx) = (self.slot.clone(), ev.clone());
        client.add_event_handler(move |e: ToDeviceKeyVerificationRequestEvent, client: Client| {
            let (slot, ev) = (slot.clone(), tx.clone());
            async move {
                let request = client.encryption().get_verification_request(&e.sender, &e.content.transaction_id).await;
                incoming(client, request, slot, ev).await;
            }
        });
        let (slot, tx) = (self.slot.clone(), ev.clone());
        client.add_event_handler(move |e: OriginalSyncRoomMessageEvent, client: Client| {
            let (slot, ev) = (slot.clone(), tx.clone());
            async move {
                if !matches!(e.content.msgtype, MessageType::VerificationRequest(_)) || Some(&*e.sender) == client.user_id() {
                    return;
                }
                let request = client.encryption().get_verification_request(&e.sender, &e.event_id).await;
                incoming(client, request, slot, ev).await;
            }
        });
    }

    /// Ends whatever is running; on sign-out.
    pub fn stop(&mut self) {
        if let Some(flow) = self.slot.lock().unwrap().take()
            && let Some(task) = flow.task
        {
            task.abort();
        }
        if let Some(watch) = self.watch.take() {
            watch.abort();
        }
        self.pending = None;
    }

    /// Asks the user's other devices to verify this one, or with `person` asks them, in the
    /// one-to-one chat with them (which the SDK makes when there is none).
    pub async fn start(&self, client: &Client, ev: &mpsc::Sender<Ev>, person: Option<&UserId>) -> Result<()> {
        if self.slot.lock().unwrap().is_some() {
            return Err(anyhow!(t!("A verification is already running")));
        }
        let own = client.user_id().context(t!("Not signed in"))?;
        let identity = identity_of(client, person.unwrap_or(own)).await?.ok_or_else(|| match person {
            Some(_) => anyhow!(t!("They have no keys to verify: their app does not support it")),
            None => anyhow!(t!("This account has no cross-signing keys yet, so another device cannot vouch for this one")),
        })?;
        let request = identity.request_verification().await?;
        follow(client.clone(), request, self.slot.clone(), ev.clone());
        Ok(())
    }

    pub async fn accept(&self) -> Result<()> {
        let request = self.slot.lock().unwrap().as_ref().map(|f| f.request.clone()).context(t!("No verification is running"))?;
        request.accept().await?;
        Ok(())
    }

    /// The user compared the emoji: `true` when they match.
    pub async fn answer(&self, matched: bool) -> Result<()> {
        let sas = self.slot.lock().unwrap().as_ref().and_then(|f| f.sas.clone()).context(t!("No emoji to compare"))?;
        if matched {
            sas.confirm().await?
        } else {
            sas.mismatch().await?
        }
        Ok(())
    }

    pub async fn cancel(&self) -> Result<()> {
        let flow = self.slot.lock().unwrap().as_ref().map(|f| (f.request.clone(), f.sas.clone()));
        match flow {
            Some((_, Some(sas))) => sas.cancel().await?,
            Some((request, None)) => request.cancel().await?,
            None => {}
        }
        Ok(())
    }

    /// One step of signing out `device`, which is never this one.
    pub async fn sign_out(&mut self, client: &Client, beeper: Option<&crate::bridges::Api>, device: String, proof: Proof) -> Result<Option<Auth>> {
        let device = OwnedDeviceId::from(device);
        if client.device_id() == Some(&*device) {
            return Err(anyhow!(t!("Sign this device out from Settings, under Account")));
        }
        if self.pending.as_ref().is_none_or(|p| p.device != device) {
            self.pending = Some(Pending { device: device.clone(), session: None, email: None, request: None });
        }
        let pending = self.pending.as_mut().expect("set above");
        let auth = match proof {
            Proof::Ask => None,
            Proof::Password(password) => {
                let user = client.user_id().context(t!("Not signed in"))?.to_string();
                let mut password = Password::new(UserIdentifier::Matrix(MatrixUserIdentifier::new(user)), password);
                password.session = pending.session.clone();
                Some(AuthData::Password(password))
            }
            Proof::SendCode => {
                let api = beeper.context(t!("Codes are a Beeper feature"))?;
                let email = pending.email.clone().context(t!("No email address for this account"))?;
                pending.request = Some(crate::beeper::send_code(&api.http, &email).await?);
                return Ok(Some(Auth::CodeSent));
            }
            Proof::Code(code) => {
                let api = beeper.context(t!("Codes are a Beeper feature"))?;
                let request = pending.request.clone().context(t!("Ask for a code first"))?;
                let jwt = crate::beeper::submit_code(&api.http, &request, code.trim()).await?;
                Some(jwt_auth(jwt, pending.session.clone()))
            }
        };
        let tried = auth.is_some();
        match client.delete_devices(std::slice::from_ref(&device), auth).await {
            Ok(_) => {
                self.pending = None;
                Ok(None)
            }
            Err(e) => {
                let Some(info) = e.as_uiaa_response() else {
                    return Err(anyhow!(t!("Could not sign the device out: {error}", error = plain(&e))));
                };
                pending.session = info.session.clone().or(pending.session.take());
                if tried && let Some(error) = &info.auth_error {
                    return Err(match error.kind {
                        ErrorKind::Forbidden if pending.request.is_none() => {
                            anyhow!(t!("Wrong password."))
                        }
                        _ => anyhow!(t!("Not accepted: {error}", error = error.message)),
                    });
                }
                let need = needs(info, beeper.is_some());
                if matches!(need, Auth::Code { .. }) {
                    let email = beeper.context(t!("Codes are a Beeper feature"))?.email().await?;
                    pending.email = Some(email.clone());
                    return Ok(Some(Auth::Code { email }));
                }
                Ok(Some(need))
            }
        }
    }
}

/// The JWT stage's body: the same token login takes (`JwtAuthChecker.check_auth` reads `token`).
fn jwt_auth(jwt: String, session: Option<String>) -> AuthData {
    let mut data = JsonObject::new();
    data.insert("token".into(), jwt.into());
    AuthData::new(JWT, session, data).expect("a custom stage always builds")
}

/// What the server's flows let this app do: one-stage flows only. `Code` carries no address yet.
fn needs(info: &UiaaInfo, beeper: bool) -> Auth {
    let single = |stage: &str| info.flows.iter().any(|f| f.stages.len() == 1 && f.stages[0].as_str() == stage);
    if single(PASSWORD) {
        Auth::Password
    } else if beeper && single(JWT) {
        Auth::Code { email: String::new() }
    } else {
        Auth::Unsupported
    }
}

pub async fn rename(client: &Client, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(anyhow!(t!("Enter a name for this device.")));
    }
    if name.chars().count() > NAME_MAX {
        return Err(anyhow!(t!("A device name has at most {max} characters.", max = NAME_MAX)));
    }
    let device = client.device_id().context(t!("Not signed in"))?;
    client.rename_device(device, name).await.map_err(|e| anyhow!(t!("Could not rename this device: {error}", error = plain(&e))))?;
    Ok(())
}

pub async fn devices(client: &Client) -> Result<Devices> {
    let own = client.user_id().context(t!("Not signed in"))?;
    let this = client.device_id().context(t!("Not signed in"))?;
    let enc = client.encryption();
    let listed = client.devices().await.map_err(|e| anyhow!(t!("Could not load your devices: {error}", error = plain(&e))))?.devices;
    // Asked of the server each time (one /keys/query): what sync last fetched may predate a new
    // device or the account's cross-signing keys.
    let identity = enc.request_user_identity(own).await?;
    let keys = enc.get_user_devices(own).await?;
    let here = enc.cross_signing_status().await.is_some_and(|s| s.is_complete());
    // Not `is_verified`: the SDK trusts its own device locally, so that is always true here.
    let this_verified = keys.get(this).is_some_and(|d| d.is_verified_with_cross_signing());
    let mut list: Vec<Device> = listed
        .into_iter()
        .map(|d| {
            let crypto = keys.get(&d.device_id);
            // What the account's own key says of the device, whether or not this one trusts that key yet.
            let trust = match &crypto {
                None => Trust::NoKeys,
                Some(c) if c.is_cross_signed_by_owner() => Trust::Verified,
                Some(_) => Trust::Unverified,
            };
            let name = d.display_name.or_else(|| crypto.as_ref().and_then(|c| c.display_name().map(ToOwned::to_owned)));
            Device {
                this: d.device_id == this,
                name: name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| d.device_id.to_string()),
                id: d.device_id.to_string(),
                last_seen: d.last_seen_ts.map(|t| t.get().into()),
                trust,
            }
        })
        .collect();
    order(&mut list);
    list.truncate(MAX_DEVICES);
    let cross_signing = match (identity.is_some(), here) {
        (_, true) => CrossSigning::Here,
        (true, false) => CrossSigning::Elsewhere,
        (false, false) => CrossSigning::Missing,
    };
    Ok(Devices { list, this_verified, cross_signing })
}

fn order(list: &mut [Device]) {
    list.sort_by(|a, b| b.this.cmp(&a.this).then(b.last_seen.cmp(&a.last_seen)).then(a.name.cmp(&b.name)));
}

/// A request arrived. Only one runs at a time; a second is turned down.
async fn incoming(client: Client, request: Option<VerificationRequest>, slot: Slot, ev: mpsc::Sender<Ev>) {
    let Some(request) = request else { return };
    if request.we_started() {
        return;
    }
    let busy = slot.lock().unwrap().as_ref().is_some_and(|f| f.request.flow_id() != request.flow_id());
    if busy {
        if let Err(e) = request.cancel().await {
            log::debug!("turning down a second verification: {e}");
        }
        return;
    }
    follow(client, request, slot, ev);
}

/// Follows a request through to its end, reporting each step; the slot holds it meanwhile.
fn follow(client: Client, request: VerificationRequest, slot: Slot, ev: mpsc::Sender<Ev>) {
    let flow_id = request.flow_id().to_owned();
    {
        let mut slot = slot.lock().unwrap();
        if slot.as_ref().is_some_and(|f| f.request.flow_id() == flow_id) {
            return;
        }
        *slot = Some(Flow { request: request.clone(), sas: None, task: None });
    }
    let task = tokio::spawn({
        let (slot, flow_id) = (slot.clone(), flow_id.clone());
        async move {
            let end = drive(&request, &slot, &ev).await;
            // A person counts as verified once the signature on their keys is stored, a moment after
            // the end; the chat's screen asks right after `Done`.
            if end == Verify::Done && !request.is_self_verification() {
                for _ in 0..SIGNED_TRIES {
                    if verified(&client, request.other_user_id()).await == Some(true) {
                        break;
                    }
                    tokio::time::sleep(SIGNED_STEP).await;
                }
            }
            {
                let mut slot = slot.lock().unwrap();
                if slot.as_ref().is_some_and(|f| f.request.flow_id() == flow_id) {
                    *slot = None;
                }
            }
            let _ = ev.send(Ev::Verify(end)).await;
            if let Ok(list) = devices(&client).await {
                let _ = ev.send(Ev::Devices(list)).await;
            }
        }
    });
    // Already gone when the flow ended first; then there is nothing left to abort.
    if let Some(flow) = slot.lock().unwrap().as_mut().filter(|f| f.request.flow_id() == flow_id) {
        flow.task = Some(task.abort_handle());
    }
}

/// Returns how the verification ended: `Done` or `Failed`.
async fn drive(request: &VerificationRequest, slot: &Slot, ev: &mpsc::Sender<Ev>) -> Verify {
    let mut changes = request.changes();
    let mut state = request.state();
    loop {
        match state {
            VerificationRequestState::Created { .. } => send(ev, Verify::Waiting).await,
            VerificationRequestState::Requested { other_device_data, .. } => {
                let who = if request.is_self_verification() {
                    other_device_data.display_name().map_or_else(|| t!("Your device {name}", name = other_device_data.device_id()), |n| t!("Your device {name}", name = n))
                } else {
                    request.other_user_id().to_string()
                };
                send(ev, Verify::Incoming { who }).await;
            }
            VerificationRequestState::Ready { .. } => {
                send(ev, Verify::Waiting).await;
                // The side that asked picks the method; the other gives it a moment, then does it
                // itself (simultaneous starts are settled by the SDK, as the spec says).
                let wait = if request.we_started() { Duration::ZERO } else { START_WAIT };
                match tokio::time::timeout(wait, changes.next()).await {
                    Ok(Some(next)) => {
                        state = next;
                        continue;
                    }
                    Ok(None) => return lost(),
                    Err(_) => {
                        if let Err(e) = request.start_sas().await {
                            return Verify::Failed(t!("Could not start comparing emoji: {error}", error = e));
                        }
                    }
                }
            }
            VerificationRequestState::Transitioned { verification } => match verification.sas() {
                Some(sas) => {
                    if let Some(flow) = slot.lock().unwrap().as_mut() {
                        flow.sas = Some(sas.clone());
                    }
                    return emoji(&sas, ev).await;
                }
                None => {
                    let _ = request.cancel().await;
                    return Verify::Failed(t!("The other device chose a method this app does not offer.").into());
                }
            },
            VerificationRequestState::Done => return Verify::Done,
            VerificationRequestState::Cancelled(info) => return failed(&info),
        }
        state = match changes.next().await {
            Some(s) => s,
            None => return lost(),
        };
    }
}

async fn emoji(sas: &SasVerification, ev: &mpsc::Sender<Ev>) -> Verify {
    let mut changes = sas.changes();
    let mut state = sas.state();
    let mut accepted = false;
    loop {
        match state {
            SasState::Started { .. } if !sas.we_started() && !accepted => {
                accepted = true;
                if let Err(e) = sas.accept().await {
                    return Verify::Failed(t!("Could not accept: {error}", error = e));
                }
            }
            SasState::KeysExchanged { .. } => match sas.emoji() {
                Some(list) => send(ev, Verify::Emoji(list.iter().map(|e| (e.symbol.to_owned(), e.description.to_owned())).collect())).await,
                None => {
                    let _ = sas.cancel().await;
                    return Verify::Failed(t!("The other device cannot show emoji.").into());
                }
            },
            SasState::Confirmed => send(ev, Verify::Confirmed).await,
            SasState::Done { .. } => return Verify::Done,
            SasState::Cancelled(info) => return failed(&info),
            _ => {}
        }
        state = match changes.next().await {
            Some(s) => s,
            None => return lost(),
        };
    }
}

/// A person's keys, fetched from the server when this device does not hold them yet (right after
/// sign-in, or a person never met).
async fn identity_of(client: &Client, user: &UserId) -> Result<Option<matrix_sdk::encryption::identities::UserIdentity>> {
    let enc = client.encryption();
    Ok(match enc.get_user_identity(user).await? {
        Some(identity) => Some(identity),
        None => enc.request_user_identity(user).await?,
    })
}

/// Whether the user verified `user`; `None` when they have no keys to verify (a bridged contact, an
/// app without cross-signing).
pub async fn verified(client: &Client, user: &UserId) -> Option<bool> {
    identity_of(client, user).await.inspect_err(|e| log::debug!("identity: {e:#}")).ok().flatten().map(|i| i.is_verified())
}

/// Takes a person's new keys as theirs, Element X's OK on a changed identity. For someone the user
/// had verified, the verification is withdrawn, as only a new one can bring it back.
pub async fn accept_identity(client: &Client, user: &UserId) -> Result<()> {
    let identity = identity_of(client, user).await?.context(t!("They have no keys to verify: their app does not support it"))?;
    if identity.has_verification_violation() {
        identity.withdraw_verification().await?;
    } else {
        identity.pin().await?;
    }
    Ok(())
}

/// Follows the people of the open chat whose keys changed, as `Ev::Identities`, until aborted.
pub(super) async fn watch_identities(room: Room, ev: mpsc::Sender<Ev>) {
    use matrix_sdk_crypto::IdentityState;
    let stream = match room.subscribe_to_identity_status_changes().await {
        Ok(stream) => stream,
        Err(e) => return log::debug!("identity changes: {e:#}"),
    };
    let mut stream = std::pin::pin!(stream);
    let mut changed: BTreeMap<OwnedUserId, bool> = BTreeMap::new();
    while let Some(batch) = stream.next().await {
        for c in batch {
            let was_verified = match c.changed_to {
                IdentityState::PinViolation => false,
                IdentityState::VerificationViolation => true,
                _ => {
                    changed.remove(&c.user_id);
                    continue;
                }
            };
            if changed.len() < MAX_TRACKED || changed.contains_key(&c.user_id) {
                changed.insert(c.user_id, was_verified);
            }
        }
        let mut list = Vec::new();
        for (user, &was_verified) in changed.iter().take(MAX_CHANGED) {
            let name = match room.get_member_no_sync(user).await {
                Ok(Some(member)) => member.name().to_owned(),
                _ => user.localpart().to_owned(),
            };
            list.push(Changed { user: user.clone(), name, was_verified });
        }
        if ev.send(Ev::Identities(room.room_id().to_owned(), list)).await.is_err() {
            break;
        }
    }
}

fn lost() -> Verify {
    Verify::Failed(t!("The verification stopped.").into())
}

fn failed(info: &CancelInfo) -> Verify {
    Verify::Failed(match (info.cancel_code(), info.cancelled_by_us()) {
        (CancelCode::MismatchedSas, _) => t!("The emoji did not match, so nothing was verified.").into(),
        (CancelCode::User, true) => t!("You canceled the verification.").into(),
        (CancelCode::User, false) => t!("The other side canceled the verification.").into(),
        (CancelCode::Timeout, _) => t!("The other side did not answer in time.").into(),
        (CancelCode::Accepted, _) => t!("Another of your devices answered this request.").into(),
        (_, _) => t!("Verification stopped: {reason}", reason = info.reason()),
    })
}

async fn send(ev: &mpsc::Sender<Ev>, v: Verify) {
    let _ = ev.send(Ev::Verify(v)).await;
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::api::client::uiaa::{AuthFlow, AuthType, UiaaInfo};

    use super::*;

    fn info(flows: &[&[&str]]) -> UiaaInfo {
        UiaaInfo::new(flows.iter().map(|f| AuthFlow::new(f.iter().map(|s| AuthType::from(*s)).collect())).collect())
    }

    #[test]
    fn only_a_stage_this_app_can_answer_is_offered() {
        assert_eq!(needs(&info(&[&["m.login.password"]]), false), Auth::Password);
        assert_eq!(needs(&info(&[&["m.login.sso"], &["m.login.password"]]), true), Auth::Password);
        assert_eq!(needs(&info(&[&["org.matrix.login.jwt"]]), true), Auth::Code { email: String::new() });
        // Another homeserver's JWT stage is not Beeper's email code.
        assert_eq!(needs(&info(&[&["org.matrix.login.jwt"]]), false), Auth::Unsupported);
        assert_eq!(needs(&info(&[&["m.login.sso"]]), true), Auth::Unsupported);
        assert_eq!(needs(&info(&[]), true), Auth::Unsupported);
        assert_eq!(needs(&info(&[&["m.login.password", "m.login.email.identity"]]), false), Auth::Unsupported);
    }

    #[test]
    fn the_jwt_stage_carries_the_token_and_session() {
        let body = serde_json::to_value(jwt_auth("t.o.k".into(), Some("s1".into()))).unwrap();
        assert_eq!(body, serde_json::json!({ "type": "org.matrix.login.jwt", "token": "t.o.k", "session": "s1" }));
    }

    #[test]
    fn this_device_comes_first_then_the_latest_seen() {
        let d = |id: &str, last_seen: Option<u64>, this: bool| Device { id: id.into(), name: id.into(), last_seen, this, trust: Trust::Unverified };
        let mut list = vec![d("old", Some(1), false), d("never", None, false), d("me", Some(0), true), d("new", Some(9), false)];
        order(&mut list);
        assert_eq!(list.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["me", "new", "old", "never"]);
    }
}

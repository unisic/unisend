//! Settings handed from one of the account's own devices to another, with no file in between.
//! Over the server: Olm-encrypted to-device messages, taken only from a device whose Olm key the
//! account's cross-signing vouches for (as this device trusts it), and answered once the user
//! there picks what to send.
//! Over the local network, with no server at all: the offering device announces itself by UDP
//! broadcast, the other connects over TCP, and the two talk through a vodozemac ECIES channel
//! whose check code the user compares on both screens; only then do they tell each other their
//! account, and the settings go only to the same account. The settings are the UI's
//! (`ui/portable.rs`); this carries their bytes.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use matrix_sdk::Client;
use matrix_sdk::deserialized_responses::{AlgorithmInfo, EncryptionInfo};
use matrix_sdk::encryption::identities::Device;
use matrix_sdk::ruma::events::AnyToDeviceEvent;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{OwnedDeviceId, TransactionId};
use matrix_sdk_crypto::CollectStrategy;
use matrix_sdk_crypto::vodozemac::Curve25519PublicKey;
use matrix_sdk_crypto::vodozemac::ecies::{Ecies, EstablishedEcies, InboundCreationResult, InitialMessage, Message, OutboundCreationResult};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use super::{AbortOnDrop, Ev};
use crate::i18n::t;

const ASK: &str = "dev.unisend.settings.request";
const GIVE: &str = "dev.unisend.settings";
/// The most settings sent through the server; an exported file is a few KB.
pub const MAX_SETTINGS: usize = 48 * 1024;
/// The UDP port the offering device announces itself to.
const PORT: u16 = 47613;
const ANNOUNCE: &str = "UNISEND-SETTINGS/1";
/// Keeps these channels' keys apart from every other use of ECIES.
const INFO: &str = "UNISEND_SETTINGS_HANDOFF";
const HELLO: &[u8] = b"unisend-settings/1";
/// How long a device offers, or looks, and waits for the user on the other screen.
const WAIT: Duration = Duration::from_secs(120);
/// How long one answer of the other device may take when no user is involved.
const STEP: Duration = Duration::from_secs(20);
const MAX_LINE: u64 = 256 * 1024;
const MAX_PEERS: usize = 8;

/// The to-device request and the local-network exchange of one account.
#[derive(Default)]
pub struct Handoff {
    /// The device asked for its settings and the request's id, until it answers.
    asked: Arc<Mutex<Option<(OwnedDeviceId, String)>>>,
    /// The local-network exchange running; one at a time.
    lan: Option<AbortOnDrop>,
    /// The offering side waits on this for the user's yes to the check code.
    confirm: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    /// The offering devices found while looking.
    peers: Arc<Mutex<Vec<Peer>>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// This device offers and waits for the other to look.
    Offering,
    /// Looking for an offering device; the local addresses of those found so far.
    Seeking(Vec<String>),
    /// The channel is up and this number shows on both screens. The offering side sends once
    /// the user says they match; the other waits for it.
    Code { code: String, offering: bool },
    /// The offering side sent the settings.
    Sent,
    Failed(String),
}

#[derive(Clone, Debug)]
struct Peer {
    addr: SocketAddr,
    key: Curve25519PublicKey,
}

#[derive(Serialize, Deserialize)]
struct Asked {
    request: String,
}

#[derive(Serialize, Deserialize)]
struct Given {
    request: String,
    settings: String,
}

#[derive(Serialize, Deserialize)]
struct Who {
    user: String,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    content: serde_json::Value,
}

impl Handoff {
    /// Answers requests from the account's other devices and takes the answer to this one's.
    pub fn listen(&self, client: &Client, ev: &mpsc::Sender<Ev>) {
        let (asked, ev) = (self.asked.clone(), ev.clone());
        client.add_event_handler(move |raw: Raw<AnyToDeviceEvent>, info: Option<EncryptionInfo>, client: Client| {
            let (asked, ev) = (asked.clone(), ev.clone());
            async move {
                let Ok(envelope) = raw.deserialize_as_unchecked::<Envelope>() else { return };
                if envelope.kind != ASK && envelope.kind != GIVE {
                    return;
                }
                let Some(device) = own_verified_sender(&client, info.as_ref()).await else {
                    log::info!("settings handoff: refused a message not from a verified own device");
                    return;
                };
                if envelope.kind == ASK {
                    let Ok(Asked { request }) = serde_json::from_value(envelope.content) else { return };
                    if request.len() <= 64 {
                        let _ = ev.send(Ev::SettingsAsked { device: device.to_string(), request }).await;
                    }
                    return;
                }
                let Ok(Given { request, settings }) = serde_json::from_value(envelope.content) else { return };
                let expected = {
                    let mut asked = asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    let ours = asked.as_ref().is_some_and(|(d, r)| *d == device && *r == request);
                    ours.then(|| asked.take()).is_some()
                };
                if expected {
                    let _ = ev.send(Ev::SettingsArrived(settings.into_bytes())).await;
                }
            }
        });
    }

    /// Asks the account's device `device` for its settings, through the server.
    pub async fn ask(&self, client: &Client, device: &str) -> Result<()> {
        let target = verified_own(client, device).await?;
        let request = TransactionId::new().to_string();
        *self.asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((target.device_id().to_owned(), request.clone()));
        send(client, &target, ASK, &Asked { request }).await
    }

    pub fn cancel_ask(&self) {
        self.asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    }

    /// Sends these settings to the device that asked for them.
    pub async fn give(client: &Client, device: &str, request: String, settings: Vec<u8>) -> Result<()> {
        if settings.len() > MAX_SETTINGS {
            bail!(t!("Too many settings to send this way. Use the settings file."));
        }
        let target = verified_own(client, device).await?;
        let settings = String::from_utf8(settings).context("settings are not text")?;
        send(client, &target, GIVE, &Given { request, settings }).await
    }

    /// Offers these settings on the local network.
    pub fn offer(&mut self, client: &Client, settings: Vec<u8>, ev: &mpsc::Sender<Ev>) -> Result<()> {
        let own = client.user_id().context(t!("Not signed in"))?.to_string();
        let (confirm, ev) = (self.confirm.clone(), ev.clone());
        self.lan = Some(AbortOnDrop(tokio::spawn(async move {
            if let Err(e) = offer(&own, &settings, &confirm, &ev).await {
                let _ = ev.send(Ev::Handoff(Step::Failed(format!("{e:#}")))).await;
            }
        })));
        Ok(())
    }

    /// Looks for a device offering its settings on the local network.
    pub fn seek(&mut self, ev: &mpsc::Sender<Ev>) {
        let (peers, ev) = (self.peers.clone(), ev.clone());
        peers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        self.lan = Some(AbortOnDrop(tokio::spawn(async move {
            if let Err(e) = seek(&peers, &ev).await {
                let _ = ev.send(Ev::Handoff(Step::Failed(format!("{e:#}")))).await;
            }
        })));
    }

    /// Takes the settings of the device found `ix`th.
    pub fn pick(&mut self, client: &Client, ix: usize, ev: &mpsc::Sender<Ev>) -> Result<()> {
        let own = client.user_id().context(t!("Not signed in"))?.to_string();
        let peer = self.peers.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(ix).cloned().context(t!("That device is gone"))?;
        let ev = ev.clone();
        self.lan = Some(AbortOnDrop(tokio::spawn(async move {
            if let Err(e) = take(&peer, &own, &ev).await {
                let _ = ev.send(Ev::Handoff(Step::Failed(format!("{e:#}")))).await;
            }
        })));
        Ok(())
    }

    /// The user saw the same number on both screens: the offering side sends.
    pub fn confirm(&self) {
        if let Some(yes) = self.confirm.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
            let _ = yes.send(());
        }
    }

    pub fn stop(&mut self) {
        self.lan = None;
        self.confirm.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        self.cancel_ask();
    }
}

/// The account's own device `device`, when this device trusts it through cross-signing.
async fn verified_own(client: &Client, device: &str) -> Result<Device> {
    let own = client.user_id().context(t!("Not signed in"))?;
    let id: OwnedDeviceId = device.into();
    let target = client.encryption().get_device(own, &id).await?.context(t!("That device is gone"))?;
    if !target.is_verified_with_cross_signing() {
        bail!(t!("Verify this device and the other one first, in Settings, Devices."));
    }
    Ok(target)
}

/// The sending device, when it is one of the account's own, the message came under its Olm key,
/// and this device trusts it through cross-signing.
async fn own_verified_sender(client: &Client, info: Option<&EncryptionInfo>) -> Option<OwnedDeviceId> {
    let info = info?;
    let own = client.user_id()?;
    if info.sender != own {
        return None;
    }
    let AlgorithmInfo::OlmV1Curve25519AesSha2 { curve25519_public_key_base64 } = &info.algorithm_info else { return None };
    let id = info.sender_device.clone()?;
    let device = client.encryption().get_device(own, &id).await.ok()??;
    let same_key = device.curve25519_key().is_some_and(|key| key.to_base64() == *curve25519_public_key_base64);
    (same_key && device.is_verified_with_cross_signing()).then_some(id)
}

async fn send(client: &Client, to: &Device, kind: &str, content: &impl Serialize) -> Result<()> {
    let raw = Raw::from_json(serde_json::value::to_raw_value(content)?);
    let failed = client.encryption().encrypt_and_send_raw_to_device(vec![to], kind, raw, CollectStrategy::AllDevices).await?;
    if !failed.is_empty() {
        bail!(t!("Could not reach that device."));
    }
    Ok(())
}

/// Announces this device until one connects and takes the settings, or the time runs out. A
/// connection that does not speak this exchange is dropped and the offer goes on, with a new key.
async fn offer(own: &str, settings: &[u8], confirm: &Mutex<Option<oneshot::Sender<()>>>, ev: &mpsc::Sender<Ev>) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    let port = listener.local_addr()?.port();
    let udp = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    udp.set_broadcast(true)?;
    let deadline = Instant::now() + WAIT;
    let _ = ev.send(Ev::Handoff(Step::Offering)).await;
    loop {
        let ecies = Ecies::with_info(INFO);
        let line = format!("{ANNOUNCE} {port} {}", ecies.public_key().to_base64());
        let stream = loop {
            if Instant::now() >= deadline {
                bail!(t!("No device came for the settings."));
            }
            if let Err(e) = udp.send_to(line.as_bytes(), (Ipv4Addr::BROADCAST, PORT)).await {
                log::debug!("settings handoff announce: {e}");
            }
            if let Ok(accepted) = tokio::time::timeout(Duration::from_secs(1), listener.accept()).await {
                break accepted?.0;
            }
        };
        if serve(stream, ecies, own, settings, confirm, ev).await? {
            return Ok(());
        }
    }
}

/// The offering side of one connection. False when the peer does not speak this exchange.
async fn serve(stream: TcpStream, ecies: Ecies, own: &str, settings: &[u8], confirm: &Mutex<Option<oneshot::Sender<()>>>, ev: &mpsc::Sender<Ev>) -> Result<bool> {
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let Ok(first) = read_line(&mut read, STEP).await else { return Ok(false) };
    let Ok(initial) = InitialMessage::decode(&first) else { return Ok(false) };
    let Ok(InboundCreationResult { ecies: mut channel, message }) = ecies.establish_inbound_channel(&initial) else { return Ok(false) };
    if message != HELLO {
        return Ok(false);
    }
    let (yes, said) = oneshot::channel();
    *confirm.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(yes);
    let _ = ev.send(Ev::Handoff(Step::Code { code: code(&channel), offering: true })).await;
    tokio::time::timeout(WAIT, said).await.map_err(|_| anyhow!(t!("No answer in time.")))?.map_err(|_| anyhow!(t!("Canceled.")))?;
    write_sealed(&mut write, &mut channel, &serde_json::to_vec(&Who { user: own.to_owned() })?).await?;
    let theirs: Who = read_sealed_json(&mut read, &mut channel, STEP).await?;
    if theirs.user != own {
        bail!(t!("That device is signed in to another account."));
    }
    write_sealed(&mut write, &mut channel, settings).await?;
    write.shutdown().await?;
    let _ = ev.send(Ev::Handoff(Step::Sent)).await;
    Ok(true)
}

/// Collects the devices announcing themselves until the time runs out or one is picked.
async fn seek(peers: &Mutex<Vec<Peer>>, ev: &mpsc::Sender<Ev>) -> Result<()> {
    let udp = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, PORT)).await.map_err(|e| anyhow!(t!("Could not listen on the local network: {error}", error = e)))?;
    let deadline = Instant::now() + WAIT;
    let _ = ev.send(Ev::Handoff(Step::Seeking(Vec::new()))).await;
    let mut buf = [0u8; 256];
    loop {
        let got = tokio::time::timeout_at(deadline, udp.recv_from(&mut buf)).await;
        let (n, from) = got.map_err(|_| anyhow!(t!("No device offers its settings on this network.")))??;
        let Some(peer) = announced(&buf[..n], from) else { continue };
        let found = {
            let mut peers = peers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if peers.len() >= MAX_PEERS || peers.iter().any(|p| p.key == peer.key) {
                continue;
            }
            peers.push(peer);
            peers.iter().map(|p| p.addr.ip().to_string()).collect()
        };
        let _ = ev.send(Ev::Handoff(Step::Seeking(found))).await;
    }
}

/// The device behind an announcement: its address with the TCP port it named, and its key.
fn announced(datagram: &[u8], from: SocketAddr) -> Option<Peer> {
    let text = std::str::from_utf8(datagram).ok()?;
    let mut parts = text.split(' ');
    if parts.next()? != ANNOUNCE {
        return None;
    }
    let port: u16 = parts.next()?.parse().ok().filter(|p| *p != 0)?;
    let key = Curve25519PublicKey::from_base64(parts.next()?).ok()?;
    parts.next().is_none().then_some(Peer { addr: SocketAddr::new(from.ip(), port), key })
}

/// The receiving side: connects, shows the code, and takes the settings once the other side's
/// user agrees and both are the same account.
async fn take(peer: &Peer, own: &str, ev: &mpsc::Sender<Ev>) -> Result<()> {
    let stream = tokio::time::timeout(STEP, TcpStream::connect(peer.addr)).await.map_err(|_| anyhow!(t!("The other device stopped answering.")))??;
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let OutboundCreationResult { ecies: mut channel, message } = Ecies::with_info(INFO).establish_outbound_channel(peer.key, HELLO)?;
    write_line(&mut write, &message.encode()).await?;
    let _ = ev.send(Ev::Handoff(Step::Code { code: code(&channel), offering: false })).await;
    let theirs: Who = read_sealed_json(&mut read, &mut channel, WAIT).await?;
    if theirs.user != own {
        bail!(t!("That device is signed in to another account."));
    }
    write_sealed(&mut write, &mut channel, &serde_json::to_vec(&Who { user: own.to_owned() })?).await?;
    let settings = read_sealed(&mut read, &mut channel, STEP).await?;
    let _ = ev.send(Ev::SettingsArrived(settings)).await;
    Ok(())
}

/// The check code as five digits: both bytes, so a stranger in the middle matches 1 in 65536.
fn code(channel: &EstablishedEcies) -> String {
    format!("{:05}", u16::from_be_bytes(*channel.check_code().as_bytes()))
}

async fn read_line(read: &mut (impl AsyncBufRead + Unpin), wait: Duration) -> Result<String> {
    let mut line = String::new();
    let n = tokio::time::timeout(wait, read.take(MAX_LINE).read_line(&mut line)).await.map_err(|_| anyhow!(t!("The other device stopped answering.")))??;
    if n == 0 || !line.ends_with('\n') {
        bail!(t!("The other device stopped answering."));
    }
    Ok(line.trim_end().to_owned())
}

async fn write_line(write: &mut (impl AsyncWrite + Unpin), line: &str) -> Result<()> {
    write.write_all(format!("{line}\n").as_bytes()).await?;
    write.flush().await?;
    Ok(())
}

async fn write_sealed(write: &mut (impl AsyncWrite + Unpin), channel: &mut EstablishedEcies, plain: &[u8]) -> Result<()> {
    write_line(write, &channel.encrypt(plain).encode()).await
}

async fn read_sealed(read: &mut (impl AsyncBufRead + Unpin), channel: &mut EstablishedEcies, wait: Duration) -> Result<Vec<u8>> {
    let line = read_line(read, wait).await?;
    let message = Message::decode(&line).map_err(|_| anyhow!(t!("The other device sent something unreadable.")))?;
    channel.decrypt(&message).map_err(|_| anyhow!(t!("The other device sent something unreadable.")))
}

async fn read_sealed_json<T: DeserializeOwned>(read: &mut (impl AsyncBufRead + Unpin), channel: &mut EstablishedEcies, wait: Duration) -> Result<T> {
    let bytes = read_sealed(read, channel, wait).await?;
    serde_json::from_slice(&bytes).map_err(|_| anyhow!(t!("The other device sent something unreadable.")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the offering side on one connection, saying yes to the code, against `take`.
    async fn exchange(offering_user: &str, taking_user: &str) -> (Vec<Ev>, Vec<Ev>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let ecies = Ecies::with_info(INFO);
        let peer = Peer { addr: listener.local_addr().unwrap(), key: ecies.public_key() };
        let (give_tx, mut give_rx) = mpsc::channel(8);
        let (take_tx, mut take_rx) = mpsc::channel(8);
        let confirm: Arc<Mutex<Option<oneshot::Sender<()>>>> = Arc::new(Mutex::new(None));
        let taking = take_user_task(peer, taking_user.to_owned(), take_tx);
        let (stream, _) = listener.accept().await.unwrap();
        let yes = confirm.clone();
        let saying = tokio::spawn(async move {
            while yes.lock().unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            yes.lock().unwrap().take().unwrap().send(()).unwrap();
        });
        let served = serve(stream, ecies, offering_user, b"{\"settings\":1}", &confirm, &give_tx).await;
        if let Err(e) = served {
            let _ = give_tx.send(Ev::Handoff(Step::Failed(format!("{e:#}")))).await;
        }
        saying.await.unwrap();
        taking.await.unwrap();
        drop(give_tx);
        let (mut gave, mut took) = (Vec::new(), Vec::new());
        while let Some(e) = give_rx.recv().await {
            gave.push(e);
        }
        while let Some(e) = take_rx.recv().await {
            took.push(e);
        }
        (gave, took)
    }

    fn take_user_task(peer: Peer, user: String, ev: mpsc::Sender<Ev>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = take(&peer, &user, &ev).await {
                let _ = ev.send(Ev::Handoff(Step::Failed(format!("{e:#}")))).await;
            }
        })
    }

    fn codes(evs: &[Ev]) -> Vec<String> {
        evs.iter().filter_map(|e| if let Ev::Handoff(Step::Code { code, .. }) = e { Some(code.clone()) } else { None }).collect()
    }

    #[tokio::test]
    async fn the_same_account_takes_the_settings_after_one_code() {
        let (gave, took) = exchange("@a:x", "@a:x").await;
        assert_eq!(codes(&gave), codes(&took));
        assert_eq!(codes(&gave).len(), 1);
        assert!(matches!(gave.last(), Some(Ev::Handoff(Step::Sent))));
        assert!(matches!(took.last(), Some(Ev::SettingsArrived(s)) if s == b"{\"settings\":1}"));
    }

    #[tokio::test]
    async fn another_account_gets_nothing() {
        let (gave, took) = exchange("@a:x", "@b:x").await;
        assert!(matches!(gave.last(), Some(Ev::Handoff(Step::Failed(_)))));
        assert!(matches!(took.last(), Some(Ev::Handoff(Step::Failed(_)))));
        assert!(!took.iter().any(|e| matches!(e, Ev::SettingsArrived(_))));
    }

    #[test]
    fn only_a_well_formed_announcement_names_a_device() {
        let key = Ecies::new().public_key();
        let from: SocketAddr = "192.168.1.5:5000".parse().unwrap();
        let peer = announced(format!("{ANNOUNCE} 4000 {}", key.to_base64()).as_bytes(), from).unwrap();
        assert_eq!((peer.addr, peer.key), ("192.168.1.5:4000".parse().unwrap(), key));
        assert!(announced(format!("OTHER 4000 {}", key.to_base64()).as_bytes(), from).is_none());
        assert!(announced(format!("{ANNOUNCE} 0 {}", key.to_base64()).as_bytes(), from).is_none());
        assert!(announced(format!("{ANNOUNCE} 4000 {} extra", key.to_base64()).as_bytes(), from).is_none());
        assert!(announced(b"\xff\xfe", from).is_none());
    }
}

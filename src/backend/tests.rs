//! End-to-end check of the backend against a real homeserver with JWT login.
//! Run: MATRIX_TEST_HS=http://127.0.0.1:8008 MATRIX_TEST_JWT_SECRET=... cargo test -- --ignored

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac as _};
use matrix_sdk::ruma::events::room::message::ReplacementMetadata;
use sha2::Sha256;

use super::*;

fn jwt(secret: &str, sub: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::json!({ "sub": sub }).to_string());
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{header}.{payload}").as_bytes());
    let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    format!("{header}.{payload}.{sig}")
}

fn state(dir: PathBuf, hs: &str) -> (State, mpsc::Receiver<Ev>) {
    std::fs::create_dir_all(&dir).unwrap();
    let (ev, rx) = mpsc::channel(CHANNEL_CAP);
    let st = State {
        dir,
        homeserver: hs.to_owned(),
        http: http_client().unwrap(),
        ev,
        client: None,
        login_request: None,
        open: None,
    };
    (st, rx)
}

async fn wait_for<T>(rx: &mut mpsc::Receiver<Ev>, what: &str, mut pick: impl FnMut(Ev) -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let ev = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .expect("event channel closed");
        if let Ev::Error(e) = &ev {
            eprintln!("backend error while waiting for {what}: {e}");
        }
        if let Some(v) = pick(ev) {
            return v;
        }
    }
}

fn bodies(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Msg(m) => Some(m.body.clone()),
            Item::Edit { .. } => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local homeserver with JWT login, see module docs"]
async fn login_sync_decrypt_send_recover() {
    let hs = std::env::var("MATRIX_TEST_HS").expect("MATRIX_TEST_HS");
    let secret = std::env::var("MATRIX_TEST_JWT_SECRET").expect("MATRIX_TEST_JWT_SECRET");
    let tag = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    let root = std::env::temp_dir().join(format!("beeper-gpui-test-{tag}"));
    let (alice_name, bob_name) = (format!("alice{tag}"), format!("bob{tag}"));

    // Alice goes through the production login path.
    let (mut alice, mut alice_rx) = state(root.join("alice"), &hs);
    alice.login_jwt(jwt(&secret, &alice_name)).await.unwrap();
    wait_for(&mut alice_rx, "Ready", |e| matches!(e, Ev::Ready).then_some(())).await;
    assert!(root.join("alice/session.json").exists());

    // Bob is a plain matrix-sdk client.
    let bob = Client::builder().homeserver_url(&hs).sqlite_store(root.join("bob"), None).build().await.unwrap();
    let mut data = JsonObject::new();
    data.insert("token".into(), jwt(&secret, &bob_name).into());
    bob.matrix_auth().login_custom("org.matrix.login.jwt", data).unwrap().await.unwrap();
    bob.sync_once(SyncSettings::default()).await.unwrap();

    let alice_client = alice.client().unwrap().clone();
    let room = alice_client.create_dm(bob.user_id().unwrap()).await.unwrap();
    let room_id = room.room_id().to_owned();
    assert!(room.latest_encryption_state().await.unwrap().is_encrypted());
    wait_for(&mut alice_rx, "room in list", |e| match e {
        Ev::Rooms(r) => r.iter().any(|s| s.id == room_id).then_some(()),
        _ => None,
    })
    .await;

    bob.sync_once(SyncSettings::default()).await.unwrap();
    bob.join_room_by_id(&room_id).await.unwrap();
    bob.sync_once(SyncSettings::default()).await.unwrap();
    let bob_room = bob.get_room(&room_id).unwrap();
    let sent = bob_room.send(RoomMessageEventContent::text_plain("hello from bob")).await.unwrap();
    let edit = RoomMessageEventContent::text_plain("hello from bob (edited)")
        .make_replacement(ReplacementMetadata::new(sent.response.event_id.clone(), None));
    bob_room.send(edit).await.unwrap();

    // Live path: decrypted message arrives through the sync loop and becomes the preview.
    let live = wait_for(&mut alice_rx, "live message", |e| match e {
        Ev::Live { room, items } if room == room_id && bodies(&items).iter().any(|b| b == "hello from bob") => {
            Some(items)
        }
        _ => None,
    })
    .await;
    assert!(matches!(live.iter().find(|i| matches!(i, Item::Msg(_))), Some(Item::Msg(m)) if !m.own && !m.sender.is_empty()));

    // History path: Open returns the message (decrypted) and the edit targeting it.
    alice.handle(Cmd::Open(room_id.clone())).await.unwrap();
    let history = wait_for(&mut alice_rx, "history", |e| match e {
        Ev::History { room, items, reset: true, .. } if room == room_id => Some(items),
        _ => None,
    })
    .await;
    assert!(bodies(&history).contains(&"hello from bob".to_owned()), "history: {:?}", bodies(&history));
    assert!(history.iter().any(|i| matches!(i, Item::Edit { target, body } if *target == sent.response.event_id.to_string() && body == "hello from bob (edited)")));
    assert!(!bodies(&history).iter().any(|b| b == UTD));

    // Send path: Bob receives Alice's message decrypted.
    alice.handle(Cmd::Send(room_id.clone(), "hi bob".into())).await.unwrap();
    let mut got = false;
    for _ in 0..20 {
        let resp = bob.sync_once(SyncSettings::default().timeout(Duration::from_secs(2))).await.unwrap();
        if let Some(update) = resp.rooms.joined.get(&room_id) {
            got |= bodies(&to_items(&bob_room, &update.timeline.events).await).contains(&"hi bob".to_owned());
        }
        if got {
            break;
        }
    }
    assert!(got, "bob never saw alice's message");

    // Recovery: a second Alice device unlocks history with the recovery key.
    // Beeper accounts already have cross-signing; a fresh test account needs it bootstrapped.
    alice_client.encryption().bootstrap_cross_signing_if_needed(None).await.unwrap();
    let key = alice_client.encryption().recovery().enable().await.unwrap();
    alice_client.encryption().backups().wait_for_steady_state().await.unwrap();
    let (mut alice2, mut alice2_rx) = state(root.join("alice2"), &hs);
    alice2.login_jwt(jwt(&secret, &alice_name)).await.unwrap();
    wait_for(&mut alice2_rx, "NeedRecovery(true)", |e| matches!(e, Ev::NeedRecovery(true)).then_some(())).await;
    alice2.handle(Cmd::Recover(key)).await.unwrap();
    wait_for(&mut alice2_rx, "NeedRecovery(false)", |e| matches!(e, Ev::NeedRecovery(false)).then_some(())).await;

    let mut decrypted_after = None;
    for attempt in 0..10 {
        alice2.handle(Cmd::Open(room_id.clone())).await.unwrap();
        let items = wait_for(&mut alice2_rx, "history on device 2", |e| match e {
            Ev::History { room, items, .. } if room == room_id => Some(items),
            _ => None,
        })
        .await;
        if bodies(&items).contains(&"hello from bob".to_owned()) {
            decrypted_after = Some(attempt);
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    eprintln!("device 2 decrypted history after {decrypted_after:?} re-opens");
    assert!(decrypted_after.is_some(), "history stayed undecryptable after recovery");

    let _ = std::fs::remove_dir_all(&root);
}

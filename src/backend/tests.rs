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
        rooms: Rooms::default(),
        fetches: Arc::new(tokio::sync::Semaphore::new(ROW_FETCHES)),
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

    wait_for(&mut alice2_rx, "room on device 2", |e| match e {
        Ev::Rooms(r) => r.iter().any(|s| s.id == room_id).then_some(()),
        _ => None,
    })
    .await;

    // Before recovery the history is locked.
    alice2.handle(Cmd::Open(room_id.clone())).await.unwrap();
    let locked = wait_for(&mut alice2_rx, "locked history", |e| match e {
        Ev::History { room, items, .. } if room == room_id => Some(items),
        _ => None,
    })
    .await;
    assert!(bodies(&locked).iter().any(|b| b == UTD), "expected UTD before recovery: {:?}", bodies(&locked));

    let other = matrix_sdk_crypto::secret_storage::SecretStorageKey::new().to_base58();
    let wrong = alice2.handle(Cmd::Recover(other)).await.unwrap_err().to_string();
    assert!(wrong.contains("does not match"), "{wrong}");
    let garbage = alice2.handle(Cmd::Recover("not a key".into())).await.unwrap_err().to_string();
    assert!(garbage.contains("not a recovery code"), "{garbage}");

    // Grouped and padded the way recovery codes are displayed.
    let spaced: String = key.chars().collect::<Vec<_>>().chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ");
    alice2.handle(Cmd::Recover(format!("  {spaced}\n"))).await.unwrap();
    wait_for(&mut alice2_rx, "NeedRecovery(false)", |e| matches!(e, Ev::NeedRecovery(false)).then_some(())).await;

    // The first open after recovery pulls the missing keys from the backup itself.
    alice2.handle(Cmd::Open(room_id.clone())).await.unwrap();
    let items = wait_for(&mut alice2_rx, "history on device 2", |e| match e {
        Ev::History { room, items, .. } if room == room_id => Some(items),
        _ => None,
    })
    .await;
    assert!(bodies(&items).contains(&"hello from bob".to_owned()), "after recovery: {:?}", bodies(&items));

    alice2.handle(Cmd::Previews(vec![room_id.clone()])).await.unwrap();
    let preview = wait_for(&mut alice2_rx, "refreshed preview", |e| match e {
        Ev::Rooms(r) => r.into_iter().find(|s| s.id == room_id && s.preview.as_deref().is_some_and(|p| !p.ends_with(UTD))),
        _ => None,
    })
    .await;
    assert_eq!(preview.preview.as_deref(), Some("You: hi bob"));

    // A summary cached by an older build: no `dm`, an unread count that has since changed.
    let stale = alice2.rooms.lock().unwrap().get_mut(&room_id).map(|r| (r.dm, r.unread) = (false, 99));
    assert!(stale.is_some());
    let fixed = refresh_cached(alice2.client().unwrap(), &alice2.rooms).await;
    let fixed = fixed.iter().find(|s| s.id == room_id).unwrap();
    assert!(fixed.dm && fixed.unread == 0, "dm {} unread {}", fixed.dm, fixed.unread);

    let _ = std::fs::remove_dir_all(&root);
}

/// Not a check: seeds a homeserver for manual GUI runs that mirror a Beeper account opened on a
/// new device. Writes `session.json` (a fresh, never-used device) and `recovery-key` to
/// GUI_FIXTURE_DIR; reads `avatar.png` from there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "GUI fixture, needs a local homeserver"]
async fn gui_fixture() {
    let hs = std::env::var("MATRIX_TEST_HS").expect("MATRIX_TEST_HS");
    let secret = std::env::var("MATRIX_TEST_JWT_SECRET").expect("MATRIX_TEST_JWT_SECRET");
    let out = PathBuf::from(std::env::var("GUI_FIXTURE_DIR").expect("GUI_FIXTURE_DIR"));
    let tag = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    let store = std::env::temp_dir().join(format!("beeper-gpui-fixture-{tag}"));
    let http = reqwest::Client::new();

    let login = |name: String| {
        let (hs, secret, store) = (hs.clone(), secret.clone(), store.clone());
        async move {
            let client = Client::builder().homeserver_url(&hs).sqlite_store(store.join(&name), None).build().await.unwrap();
            let mut data = JsonObject::new();
            data.insert("token".into(), jwt(&secret, &name).into());
            client.matrix_auth().login_custom("org.matrix.login.jwt", data).unwrap().initial_device_display_name(&name).await.unwrap();
            client.sync_once(SyncSettings::default()).await.unwrap();
            client
        }
    };
    let alice = login(format!("alice{tag}")).await;
    let bob = login(format!("bob{tag}")).await;
    let carol = login(format!("carol{tag}")).await;
    alice.encryption().bootstrap_cross_signing_if_needed(None).await.unwrap();
    let key = alice.encryption().recovery().enable().await.unwrap();

    let png = std::fs::read(out.join("avatar.png")).unwrap();
    let upload: serde_json::Value = http
        .post(format!("{hs}/_matrix/media/v3/upload"))
        .bearer_auth(alice.access_token().unwrap())
        .header("Content-Type", "image/png")
        .body(png)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .map(|b| serde_json::from_slice(&b).unwrap())
        .unwrap();

    async fn join(who: &Client, room: &RoomId) -> Room {
        who.sync_once(SyncSettings::default()).await.unwrap();
        who.join_room_by_id(room).await.unwrap();
        who.sync_once(SyncSettings::default()).await.unwrap();
        who.get_room(room).unwrap()
    }
    async fn say(room: &Room, text: &str) {
        room.send(RoomMessageEventContent::text_plain(text)).await.unwrap();
    }

    let dm = alice.create_dm(bob.user_id().unwrap()).await.unwrap();
    let bob_dm = join(&bob, dm.room_id()).await;
    say(&bob_dm, "Are we still on for Saturday?").await;
    say(&dm, "Yes, 10 at the station").await;
    say(&bob_dm, "Great, I'll bring the map").await;

    let mut req = matrix_sdk::ruma::api::client::room::create_room::v3::Request::new();
    req.name = Some("Weekend trip".into());
    req.invite = vec![bob.user_id().unwrap().to_owned(), carol.user_id().unwrap().to_owned()];
    let group = alice.create_room(req).await.unwrap();
    group.enable_encryption().await.unwrap();
    group.send_state_event_raw("m.room.avatar", "", serde_json::json!({ "url": upload["content_uri"] })).await.unwrap();
    let (bob_group, carol_group) = (join(&bob, group.room_id()).await, join(&carol, group.room_id()).await);
    say(&carol_group, "Booked the cabin").await;
    say(&bob_group, "Who drives?").await;
    say(&carol_group, "I can, my car fits four").await;

    let mut req = matrix_sdk::ruma::api::client::room::create_room::v3::Request::new();
    req.name = Some("Mum".into());
    req.invite = vec![bob.user_id().unwrap().to_owned()];
    let bridged = alice.create_room(req).await.unwrap();
    let bridge = serde_json::json!({ "protocol": { "id": "whatsapp", "displayname": "WhatsApp" } });
    bridged.send_state_event_raw("m.bridge", "fixture", bridge).await.unwrap();
    let bob_bridged = join(&bob, bridged.room_id()).await;
    say(&bob_bridged, "Call me when you land").await;
    say(&bob_bridged, "And take a jacket, it is cold here").await;

    alice.sync_once(SyncSettings::default()).await.unwrap();
    alice.encryption().backups().wait_for_steady_state().await.unwrap();

    // The app's device: logged in over plain HTTP so no crypto store claims its keys.
    let body = serde_json::json!({ "type": "org.matrix.login.jwt", "token": jwt(&secret, &format!("alice{tag}")) });
    let session: serde_json::Value =
        serde_json::from_slice(&http.post(format!("{hs}/_matrix/client/v3/login")).body(body.to_string()).send().await.unwrap().bytes().await.unwrap()).unwrap();
    let saved = serde_json::json!({ "session": {
        "user_id": session["user_id"], "device_id": session["device_id"], "access_token": session["access_token"],
    }});
    std::fs::write(out.join("session.json"), serde_json::to_vec(&saved).unwrap()).unwrap();
    std::fs::write(out.join("recovery-key"), &key).unwrap();
    let _ = std::fs::remove_dir_all(&store);
}

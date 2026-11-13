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
        notif: None,
        incognito: true,
        tasks: Vec::new(),
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

/// A landscape test picture, written next to the fixture's other files.
fn root_photo(dir: &Path) -> PathBuf {
    let path = dir.join("lake.jpg");
    image::RgbImage::from_fn(1600, 1000, |x, y| {
        let sky = y < 520;
        image::Rgb(if sky { [90 + (y / 6) as u8, 150 + (y / 10) as u8, 235] } else { [30, 90 + ((x + y) % 60) as u8, 120] })
    })
    .save(&path)
    .unwrap();
    path
}

async fn next_live(rx: &mut mpsc::Receiver<Ev>, room_id: &OwnedRoomId) -> Vec<Item> {
    wait_for(rx, "live items", |e| match e {
        Ev::Live { room, items } if room == *room_id => Some(items),
        _ => None,
    })
    .await
}

fn bodies(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Msg(m) => Some(m.body.clone()),
            _ => None,
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
    wait_for(&mut alice_rx, "Ready", |e| matches!(e, Ev::Ready(_)).then_some(())).await;
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
    assert!(matches!(live.iter().find(|i| matches!(i, Item::Msg(_))), Some(Item::Msg(m)) if !m.own && !m.sender.is_empty() && m.notify));

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
    alice.handle(Cmd::Send { room: room_id.clone(), body: "hi bob".into(), reply_to: None, edit: None }).await.unwrap();
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
    let notif = alice2.notif.clone().unwrap();
    let fixed = refresh_cached(alice2.client().unwrap(), &notif, &alice2.rooms).await;
    let fixed = fixed.iter().find(|s| s.id == room_id).unwrap();
    assert!(fixed.dm && fixed.unread == 0, "dm {} unread {}", fixed.dm, fixed.unread);

    // Chat actions round-trip through the server and come back in the summary.
    for (action, check) in [
        (ChatAction::Pin(true), (|s| s.pinned) as fn(&RoomSummary) -> bool),
        (ChatAction::Pin(false), |s| !s.pinned),
        (ChatAction::LowPriority(true), |s| s.low_priority),
        (ChatAction::LowPriority(false), |s| !s.low_priority),
        (ChatAction::Mute(true), |s| s.muted),
        (ChatAction::Mute(false), |s| !s.muted),
        (ChatAction::Unread(true), |s| s.marked_unread && s.is_unread()),
        (ChatAction::Unread(false), |s| !s.is_unread()),
        (ChatAction::Archive(true), |s| s.archived),
        (ChatAction::Archive(false), |s| !s.archived),
    ] {
        alice2.handle(Cmd::Chat(room_id.clone(), action)).await.unwrap();
        wait_for(&mut alice2_rx, &format!("{action:?}"), |e| match e {
            Ev::Rooms(r) => r.iter().any(|s| s.id == room_id && check(s)).then_some(()),
            _ => None,
        })
        .await;
    }

    // A message from someone else brings an archived chat back.
    alice2.handle(Cmd::Chat(room_id.clone(), ChatAction::Archive(true))).await.unwrap();
    wait_for(&mut alice2_rx, "archived", |e| match e {
        Ev::Rooms(r) => r.iter().any(|s| s.id == room_id && s.archived).then_some(()),
        _ => None,
    })
    .await;
    let ping = bob_room.send(RoomMessageEventContent::text_plain("are you there?")).await.unwrap().response.event_id;
    wait_for(&mut alice2_rx, "unarchived by a new message", |e| match e {
        Ev::Rooms(r) => r.iter().any(|s| s.id == room_id && !s.archived).then_some(()),
        _ => None,
    })
    .await;

    // React, reply, edit, delete: each comes back through sync as the item the UI applies.
    let room = room_id.clone();
    alice2.handle(Cmd::React { room: room.clone(), target: ping.to_string(), key: "👍".into() }).await.unwrap();
    let reaction_id = loop {
        let found = next_live(&mut alice2_rx, &room_id).await.into_iter().find_map(|i| match i {
            Item::Reaction { target, reaction } if target == ping.as_str() && reaction.key == "👍" && reaction.own => Some(reaction.id),
            _ => None,
        });
        if let Some(id) = found {
            break id;
        }
    };
    alice2.handle(Cmd::Send { room: room.clone(), body: "yes".into(), reply_to: Some(ping.to_string()), edit: None }).await.unwrap();
    let reply_id = loop {
        let found = next_live(&mut alice2_rx, &room_id).await.into_iter().find_map(|i| match i {
            Item::Msg(m) if m.body == "yes" && m.reply_to.as_deref() == Some(ping.as_str()) => Some(m.id),
            _ => None,
        });
        if let Some(id) = found {
            break id;
        }
    };
    alice2.handle(Cmd::Send { room: room.clone(), body: "yes!".into(), reply_to: None, edit: Some(reply_id.clone()) }).await.unwrap();
    loop {
        if next_live(&mut alice2_rx, &room_id).await.iter().any(|i| matches!(i, Item::Edit { target, body } if *target == reply_id && body == "yes!")) {
            break;
        }
    }
    for target in [reaction_id, reply_id.clone()] {
        alice2.handle(Cmd::Delete { room: room.clone(), target: target.clone() }).await.unwrap();
        loop {
            if next_live(&mut alice2_rx, &room_id).await.iter().any(|i| matches!(i, Item::Redact { target: t } if *t == target)) {
                break;
            }
        }
    }
    // Media: a picture sent to an encrypted room comes back with its shape, a thumbnail and a
    // viewing copy no larger than their limits, and downloads byte for byte.
    let picture = root.join("holiday photo.png");
    image::RgbImage::from_fn(2400, 1200, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])).save(&picture).unwrap();
    alice2.handle(Cmd::Attach { room: room.clone(), path: picture.clone(), reply_to: None }).await.unwrap();
    let (photo_id, media) = loop {
        let found = next_live(&mut alice2_rx, &room_id).await.into_iter().find_map(|i| match i {
            Item::Msg(m) => m.media.clone().map(|media| (m.id, media)),
            _ => None,
        });
        if let Some(found) = found {
            break found;
        }
    };
    assert_eq!((media.kind, media.dims, media.name.as_str()), (MediaKind::Image, Some((2400, 1200)), "holiday photo.png"));
    let longest = |bytes: &[u8]| {
        let image = image::load_from_memory(bytes).unwrap();
        image.width().max(image.height())
    };
    alice2.handle(Cmd::Thumbs(vec![(photo_id.clone(), media.clone())])).await.unwrap();
    let thumb = wait_for(&mut alice2_rx, "thumbnail", |e| match e {
        Ev::Thumb(id, bytes) if id == photo_id => Some(bytes),
        _ => None,
    })
    .await;
    assert_eq!(longest(&thumb), THUMB_PX);
    alice2.handle(Cmd::View(photo_id.clone(), media.clone())).await.unwrap();
    let full = wait_for(&mut alice2_rx, "viewing copy", |e| match e {
        Ev::Viewed(id, bytes) if id == photo_id => Some(bytes),
        _ => None,
    })
    .await;
    assert_eq!(longest(&full), VIEW_PX);
    alice2.handle(Cmd::Fetch(photo_id.clone(), media)).await.unwrap();
    let saved = wait_for(&mut alice2_rx, "downloaded file", |e| match e {
        Ev::Fetched(id, path) if id == photo_id => path,
        _ => None,
    })
    .await;
    assert!(saved.starts_with(root.join("alice2/media")) && saved.ends_with("holiday photo.png"), "{saved:?}");
    assert_eq!(std::fs::read(&saved).unwrap(), std::fs::read(&picture).unwrap());

    // A chat with no messages, as a bridge creates them before it brings the history in: it has
    // no time, so it sorts last, and it is marked as looked at. Its first message dates it.
    let mut req = matrix_sdk::ruma::api::client::room::create_room::v3::Request::new();
    req.name = Some("Just connected".into());
    req.invite = vec![alice_client.user_id().unwrap().to_owned()];
    let quiet = bob.create_room(req).await.unwrap();
    let quiet_id = quiet.room_id().to_owned();
    alice_client.sync_once(SyncSettings::default()).await.unwrap();
    alice_client.join_room_by_id(&quiet_id).await.unwrap();
    wait_for(&mut alice2_rx, "empty chat looked at", |e| match e {
        Ev::Rooms(r) => r.iter().any(|s| s.id == quiet_id && s.dated && s.ts == 0 && s.preview.is_none()).then_some(()),
        _ => None,
    })
    .await;
    // A cache from an older build gave such a chat the time of a state event; whichever of the
    // startup pass and a row's own refresh gets to it first, it must lose that time.
    alice2.rooms.lock().unwrap().get_mut(&quiet_id).map(|r| (r.ts, r.dated) = (777, false)).unwrap();
    alice2.handle(Cmd::Previews(vec![quiet_id.clone()])).await.unwrap();
    wait_for(&mut alice2_rx, "stale time dropped", |e| match e {
        Ev::Rooms(r) => r.iter().any(|s| s.id == quiet_id && s.dated && s.ts == 0).then_some(()),
        _ => None,
    })
    .await;
    // A chat with messages whose cached time is a guess gets its last message's time.
    alice2.rooms.lock().unwrap().get_mut(&room_id).map(|r| (r.ts, r.dated) = (1, false)).unwrap();
    alice2.handle(Cmd::Previews(vec![room_id.clone()])).await.unwrap();
    wait_for(&mut alice2_rx, "guessed time replaced", |e| match e {
        Ev::Rooms(r) => r.iter().any(|s| s.id == room_id && s.dated && s.ts > 1_600_000_000_000).then_some(()),
        _ => None,
    })
    .await;
    // A chat left while the app was closed disappears from the cache at the next start.
    let gone = matrix_sdk::ruma::room_id!("!left-long-ago:localhost").to_owned();
    let ghost = RoomSummary { id: gone.clone(), ..alice2.rooms.lock().unwrap()[&quiet_id].clone() };
    alice2.rooms.lock().unwrap().insert(gone.clone(), ghost);
    drop_left(alice2.client().unwrap(), &alice2.rooms, &alice2.ev).await;
    assert!(!alice2.rooms.lock().unwrap().contains_key(&gone));
    assert!(alice2.rooms.lock().unwrap().contains_key(&quiet_id) && alice2.rooms.lock().unwrap().contains_key(&room_id));
    wait_for(&mut alice2_rx, "RoomGone for the left chat", |e| matches!(e, Ev::RoomGone(id) if id == gone).then_some(())).await;

    quiet.send(RoomMessageEventContent::text_plain("first!")).await.unwrap();
    wait_for(&mut alice2_rx, "first message dates the chat", |e| match e {
        Ev::Rooms(r) => r.iter().any(|s| s.id == quiet_id && s.ts > 0 && s.preview.as_deref().is_some_and(|p| p.ends_with("first!"))).then_some(()),
        _ => None,
    })
    .await;

    // After a reload the deleted reply and reaction are gone, not "unable to decrypt".
    alice2.handle(Cmd::Open(room.clone())).await.unwrap();
    let items = wait_for(&mut alice2_rx, "history after deletions", |e| match e {
        Ev::History { room, items, .. } if room == room_id => Some(items),
        _ => None,
    })
    .await;
    assert!(!items.iter().any(|i| matches!(i, Item::Msg(m) if m.id == reply_id)));
    assert!(!bodies(&items).iter().any(|b| b == UTD), "{:?}", bodies(&items));

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
    async fn say(room: &Room, text: &str) -> OwnedEventId {
        room.send(RoomMessageEventContent::text_plain(text)).await.unwrap().response.event_id
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
    let question = say(&bob_group, "Who drives?").await;
    let reply = Reply { event_id: question.clone(), enforce_thread: EnforceThread::MaybeThreaded, add_mentions: AddMentions::Yes };
    let answer = RoomMessageEventContent::text_plain("I can, my car fits four");
    carol_group.send(carol_group.make_reply_event(answer.into(), reply).await.unwrap()).await.unwrap();
    for who in [&carol_group, &group] {
        who.send(ReactionEventContent::new(Annotation::new(question.clone(), "👍".into()))).await.unwrap();
    }

    let mut req = matrix_sdk::ruma::api::client::room::create_room::v3::Request::new();
    req.name = Some("Mum".into());
    req.invite = vec![bob.user_id().unwrap().to_owned()];
    let bridged = alice.create_room(req).await.unwrap();
    let bridge = serde_json::json!({ "protocol": { "id": "whatsapp", "displayname": "WhatsApp" } });
    bridged.send_state_event_raw("m.bridge", "fixture", bridge).await.unwrap();
    let bob_bridged = join(&bob, bridged.room_id()).await;
    say(&bob_bridged, "Call me when you land").await;
    say(&bob_bridged, "And take a jacket, it is cold here").await;

    // Media, plain and encrypted: a photo, a document, and a photo with a caption.
    let photo = root_photo(&out);
    for (room, path) in [(&bob_bridged, &photo), (&bob_group, &photo), (&carol_group, &out.join("avatar.png"))] {
        attach(room, path, None).await.unwrap();
    }
    let notes = out.join("packing list.txt");
    std::fs::write(&notes, "tent, stove, map\n").unwrap();
    attach(&carol_group, &notes, None).await.unwrap();

    // A bridged one-to-one chat as Beeper has them: no name, the other person and a bridge bot
    // that the room lists as a service member. Also: letters outside the basic fonts, two lines.
    let mut req = matrix_sdk::ruma::api::client::room::create_room::v3::Request::new();
    req.invite = vec![bob.user_id().unwrap().to_owned(), carol.user_id().unwrap().to_owned()];
    let direct = alice.create_room(req).await.unwrap();
    let bot = serde_json::json!({ "service_members": [carol.user_id().unwrap()] });
    direct.send_state_event_raw("io.element.functional_members", "", bot).await.unwrap();
    let bridge = serde_json::json!({ "protocol": { "id": "signal", "displayname": "Signal" }, "com.beeper.room_type.v2": "dm" });
    direct.send_state_event_raw("m.bridge", "fixture", bridge).await.unwrap();
    let (bob_direct, _) = (join(&bob, direct.room_id()).await, join(&carol, direct.room_id()).await);
    say(&bob_direct, "już ide XDD\nzaraz będę, 𝑺𝒉𝒖 🍮🐰 też").await;

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

#[test]
fn archive_rule_matches_beeper() {
    let room = |ts, order, unread, pinned| RoomSummary {
        id: matrix_sdk::ruma::room_id!("!r:example.org").to_owned(),
        name: String::new(),
        preview: None,
        ts,
        network: None,
        unread,
        dm: false,
        pinned,
        low_priority: false,
        muted: false,
        marked_unread: false,
        archived: false,
        order,
        dated: true,
    };
    let done = |updated_ts, at_order| Done { updated_ts, at_order };
    let now = 10_000;
    assert!(!is_done(&done(None, None), &room(100, 0, 0, false), now));
    // Server order: archived until a newer message arrives.
    assert!(is_done(&done(Some(500), Some(7)), &room(100, 7, 0, false), now));
    assert!(!is_done(&done(Some(500), Some(7)), &room(900, 8, 0, false), now));
    // Order unknown on our side: trust the mark.
    assert!(is_done(&done(Some(500), Some(7)), &room(100, 0, 3, false), now));
    // No order in the mark: timestamps, and an unread chat is never done.
    assert!(is_done(&done(Some(500), None), &room(100, 0, 0, false), now));
    assert!(!is_done(&done(Some(500), None), &room(600, 0, 0, false), now));
    assert!(!is_done(&done(Some(500), None), &room(100, 0, 1, false), now));
    assert!(!is_done(&done(Some(20_000), None), &room(100, 0, 0, false), now));
    // Pinned chats stay in the inbox.
    assert!(!is_done(&done(Some(500), Some(7)), &room(100, 7, 0, true), now));
}

#[test]
fn file_names_stay_in_their_directory() {
    assert_eq!(safe_name("../../etc/passwd"), "_.._etc_passwd");
    assert_eq!(safe_name("holiday photo.png"), "holiday photo.png");
    assert_eq!(safe_name(".."), "file");
    assert_eq!(safe_name("$abc:beeper.com"), "_abc_beeper.com");
}

#[test]
fn notifications_follow_the_push_rules() {
    let msg = |sender: &str, own, notify| {
        Item::Msg(Msg {
            id: "$e".into(),
            sender: sender.into(),
            from: matrix_sdk::ruma::user_id!("@a:example.org").to_owned(),
            body: "hello".into(),
            own,
            ts: 1,
            order: 0,
            reply_to: None,
            deleted: false,
            edited: false,
            media: None,
            notify,
            reactions: Vec::new(),
        })
    };
    let chat = |dm| RoomSummary {
        id: matrix_sdk::ruma::room_id!("!r:example.org").to_owned(),
        name: "Trip".into(),
        preview: None,
        ts: 1,
        network: None,
        unread: 1,
        dm,
        pinned: false,
        low_priority: false,
        muted: false,
        marked_unread: false,
        archived: false,
        order: 0,
        dated: true,
    };
    let now = 60_000;
    assert_eq!(notification(&chat(false), &[msg("Ann", false, true)], now).as_deref(), Some("Ann: hello"));
    assert_eq!(notification(&chat(true), &[msg("Ann", false, true)], now).as_deref(), Some("hello"));
    // Muted or mentions-only chats, and one's own messages from another device.
    assert_eq!(notification(&chat(false), &[msg("Ann", false, false)], now), None);
    assert_eq!(notification(&chat(false), &[msg("Me", true, true)], now), None);
    // The newest that qualifies, past ones that do not.
    assert_eq!(notification(&chat(false), &[msg("Ann", false, true), msg("Me", true, true)], now).as_deref(), Some("Ann: hello"));
    // History a bridge brings in later is not news.
    assert_eq!(notification(&chat(false), &[msg("Ann", false, true)], now + 3_600_000), None);
}

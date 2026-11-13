//! End-to-end check of the backend against a real homeserver with JWT login.
//! Run: MATRIX_TEST_HS=http://127.0.0.1:8008 MATRIX_TEST_JWT_SECRET=... cargo test -- --ignored

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac as _};
use matrix_sdk::ruma::events::room::message::ReplacementMetadata;
use sha2::Sha256;

use matrix_sdk::ruma::events::{reaction::ReactionEventContent, relation::Annotation};

use super::*;

/// A suffix no other test in this run or an earlier one uses: tests running side by side start in
/// the same millisecond, and two of them creating the same user fails with "User ID already taken".
fn tag() -> String {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    format!("{ms}x{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

fn jwt(secret: &str, sub: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::json!({ "sub": sub }).to_string());
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{header}.{payload}").as_bytes());
    let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    format!("{header}.{payload}.{sig}")
}

/// What a UI holds: the events coming in and the rows of the open conversation, which every
/// change has to reach in order, whatever else is being waited for.
struct Seat {
    rx: mpsc::Receiver<Ev>,
    rows: Vec<Msg>,
}

impl Seat {
    async fn next(&mut self, what: &str, deadline: tokio::time::Instant) -> Option<Ev> {
        let ev = tokio::time::timeout_at(deadline, self.rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}; rows: {:?}", bodies(&self.rows)))
            .expect("event channel closed");
        match ev {
            Ev::Timeline { diffs, .. } => {
                for diff in diffs {
                    apply(&mut self.rows, diff);
                }
                None
            }
            Ev::Error(e) => {
                eprintln!("backend error while waiting for {what}: {e}");
                Some(Ev::Error(e))
            }
            ev => Some(ev),
        }
    }
}

fn state(dir: PathBuf, hs: &str) -> (State, Seat) {
    std::fs::create_dir_all(&dir).unwrap();
    let (ev, rx) = mpsc::channel(CHANNEL_CAP);
    let st = State {
        dir,
        homeserver: hs.to_owned(),
        beeper: true,
        http: http_client().unwrap(),
        ev,
        client: None,
        login_request: None,
        open: None,
        rooms: Rooms::default(),
        fetches: Arc::new(tokio::sync::Semaphore::new(ROW_FETCHES)),
        notif: None,
        incognito: Arc::new(AtomicBool::new(true)),
        tasks: Vec::new(),
    };
    (st, Seat { rx, rows: Vec::new() })
}

async fn wait_for<T>(seat: &mut Seat, what: &str, mut pick: impl FnMut(Ev) -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(v) = seat.next(what, deadline).await.and_then(&mut pick) {
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

/// Waits until `done` holds for the rows of the open conversation.
async fn until(seat: &mut Seat, what: &str, done: impl Fn(&[Msg]) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while !done(&seat.rows) {
        seat.next(what, deadline).await;
    }
}

fn bodies(msgs: &[Msg]) -> Vec<String> {
    msgs.iter().filter(|m| m.kind == Kind::Message).map(|m| m.body.clone()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local homeserver with JWT login, see module docs"]
async fn login_sync_decrypt_send_recover() {
    let hs = std::env::var("MATRIX_TEST_HS").expect("MATRIX_TEST_HS");
    let secret = std::env::var("MATRIX_TEST_JWT_SECRET").expect("MATRIX_TEST_JWT_SECRET");
    let tag = tag();
    let root = std::env::temp_dir().join(format!("beeper-gpui-test-{tag}"));
    let (alice_name, bob_name) = (format!("alice{tag}"), format!("bob{tag}"));

    // Alice goes through the production login path.
    let (mut alice, mut alice_rx) = state(root.join("alice"), &hs);
    alice.login_jwt(jwt(&secret, &alice_name)).await.unwrap();
    wait_for(&mut alice_rx, "Ready", |e| matches!(e, Ev::Ready { .. }).then_some(())).await;
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

    // List path: the decrypted message arrives through the sync loop and becomes the preview, which
    // reads the edit.
    wait_for(&mut alice_rx, "preview of the new message", |e| match e {
        Ev::Rooms(r) => r.iter().any(|s| s.id == room_id && s.preview.as_deref() == Some("hello from bob (edited)") && s.unread > 0).then_some(()),
        _ => None,
    })
    .await;

    // Conversation: opening it gives the message, decrypted and as its edit left it.
    alice.handle(Cmd::Open(room_id.clone())).await.unwrap();
    until(&mut alice_rx, "the edited message", |rows| {
        rows.iter().any(|m| m.id == sent.response.event_id.as_str() && m.body == "hello from bob (edited)" && m.edited && !m.own && !m.sender.is_empty())
    })
    .await;
    assert!(!bodies(&alice_rx.rows).iter().any(|b| b == UTD), "{:?}", bodies(&alice_rx.rows));
    assert!(alice_rx.rows.iter().any(|m| m.kind == Kind::Day), "a day row heads the messages");

    // Send path: the message shows at once, becomes an event, and Bob receives it decrypted.
    alice.handle(Cmd::Send { room: room_id.clone(), body: "hi bob".into(), reply_to: None, edit: None }).await.unwrap();
    until(&mut alice_rx, "the sent message as an event", |rows| {
        rows.iter().any(|m| m.body == "hi bob" && m.own && m.id.starts_with('$') && m.delivery == Delivery::Sent)
    })
    .await;
    let mut got = false;
    for _ in 0..20 {
        let resp = bob.sync_once(SyncSettings::default().timeout(Duration::from_secs(2))).await.unwrap();
        if let Some(update) = resp.rooms.joined.get(&room_id) {
            got |= bodies(&to_msgs(&bob_room, &update.timeline.events).await.0).contains(&"hi bob".to_owned());
        }
        if got {
            break;
        }
    }
    assert!(got, "bob never saw alice's message");

    // Bob reads it: his receipt lands on a row at or under the message.
    let hi_id = alice_rx.rows.iter().rfind(|m| m.body == "hi bob").unwrap().id.clone();
    bob_room.send_single_receipt(create_receipt::v3::ReceiptType::Read, ReceiptThread::Unthreaded, EventId::parse(&hi_id).unwrap()).await.unwrap();
    let bob_id = bob.user_id().unwrap().to_owned();
    until(&mut alice_rx, "bob's read receipt", |rows| rows.iter().skip_while(|m| m.id != hi_id).any(|m| m.seen.contains(&bob_id))).await;

    // Bob starts writing: the open conversation says who.
    bob_room.typing_notice(true).await.unwrap();
    let typing = wait_for(&mut alice_rx, "bob typing", |e| match e {
        Ev::Typing { room, names } if room == room_id && !names.is_empty() => Some(names),
        _ => None,
    })
    .await;
    assert_eq!(typing.len(), 1, "{typing:?}");
    bob_room.typing_notice(false).await.unwrap();
    wait_for(&mut alice_rx, "bob stopped typing", |e| matches!(e, Ev::Typing { names, .. } if names.is_empty()).then_some(())).await;

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
    until(&mut alice2_rx, "locked history", |rows| bodies(rows).iter().any(|b| b == UTD)).await;

    let other = matrix_sdk_crypto::secret_storage::SecretStorageKey::new().to_base58();
    let wrong = alice2.handle(Cmd::Recover(other)).await.unwrap_err().to_string();
    assert!(wrong.contains("does not match"), "{wrong}");
    let garbage = alice2.handle(Cmd::Recover("not a key".into())).await.unwrap_err().to_string();
    assert!(garbage.contains("not a recovery code"), "{garbage}");

    // Grouped and padded the way recovery codes are displayed.
    let spaced: String = key.chars().collect::<Vec<_>>().chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ");
    alice2.handle(Cmd::Recover(format!("  {spaced}\n"))).await.unwrap();
    wait_for(&mut alice2_rx, "NeedRecovery(false)", |e| matches!(e, Ev::NeedRecovery(false)).then_some(())).await;

    // With the keys' backup unlocked, the open conversation reads itself.
    until(&mut alice2_rx, "history on device 2", |rows| {
        let read = bodies(rows);
        read.contains(&"hello from bob (edited)".to_owned()) && read.contains(&"hi bob".to_owned()) && !read.iter().any(|b| b == UTD)
    })
    .await;

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

    // React, reply, edit, delete: each shows on the rows of the open conversation.
    let room = room_id.clone();
    let reacted = |rows: &[Msg]| rows.iter().find(|m| m.id == ping.as_str()).is_some_and(|m| m.reactions.iter().any(|r| r.key == "👍" && r.own && r.count == 1));
    until(&mut alice2_rx, "the new message", |rows| rows.iter().any(|m| m.id == ping.as_str())).await;
    alice2.handle(Cmd::React { room: room.clone(), target: ping.to_string(), key: "👍".into() }).await.unwrap();
    until(&mut alice2_rx, "the reaction", reacted).await;
    alice2.handle(Cmd::Send { room: room.clone(), body: "yes".into(), reply_to: Some(ping.to_string()), edit: None }).await.unwrap();
    let replied = |m: &&Msg| m.body == "yes" && m.own && m.id.starts_with('$') && m.reply.as_ref().is_some_and(|q| q.id == ping.as_str() && q.text == "are you there?");
    until(&mut alice2_rx, "the reply with its quote", |rows| rows.iter().any(|m| replied(&m))).await;
    let reply_id = alice2_rx.rows.iter().find(replied).unwrap().id.clone();
    alice2.handle(Cmd::Send { room: room.clone(), body: "yes!".into(), reply_to: None, edit: Some(reply_id.clone()) }).await.unwrap();
    until(&mut alice2_rx, "the edit", |rows| rows.iter().any(|m| m.id == reply_id && m.body == "yes!" && m.edited)).await;
    // The chat list's preview follows an edit and a deletion of the message it shows.
    let rooms = alice2.rooms.clone();
    let preview_is = |want: String| {
        let (rooms, room) = (rooms.clone(), room.clone());
        async move {
            for _ in 0..100 {
                if rooms.lock().unwrap().get(&room).and_then(|r| r.preview.clone()) == Some(want.clone()) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("preview never read {want:?}: {:?}", rooms.lock().unwrap().get(&room).map(|r| r.preview.clone()));
        }
    };
    preview_is("You: yes!".into()).await;
    // The same reaction again takes it back.
    alice2.handle(Cmd::React { room: room.clone(), target: ping.to_string(), key: "👍".into() }).await.unwrap();
    until(&mut alice2_rx, "the reaction gone", |rows| !reacted(rows)).await;
    alice2.handle(Cmd::Delete { room: room.clone(), target: reply_id.clone() }).await.unwrap();
    until(&mut alice2_rx, "the deletion", |rows| rows.iter().any(|m| m.id == reply_id && m.deleted)).await;
    preview_is(format!("You: {DELETED}")).await;
    // Media: a picture sent to an encrypted room comes back with its shape, a thumbnail and a
    // viewing copy no larger than their limits, and downloads byte for byte.
    let picture = root.join("holiday photo.png");
    image::RgbImage::from_fn(2400, 1200, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])).save(&picture).unwrap();
    alice2.handle(Cmd::Attach { room: room.clone(), path: picture.clone(), reply_to: None }).await.unwrap();
    until(&mut alice2_rx, "the picture", |rows| rows.iter().any(|m| m.media.is_some() && m.id.starts_with('$'))).await;
    let (photo_id, media) = alice2_rx.rows.iter().find_map(|m| m.media.clone().map(|media| (m.id.clone(), media))).unwrap();
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

    // After a reload the deleted reply is still a deleted message, not "unable to decrypt".
    alice2.handle(Cmd::Close).await.unwrap();
    alice2.handle(Cmd::Open(room.clone())).await.unwrap();
    until(&mut alice2_rx, "the conversation again", |rows| rows.iter().any(|m| m.id == reply_id)).await;
    assert!(alice2_rx.rows.iter().any(|m| m.id == reply_id && m.deleted));
    assert!(!bodies(&alice2_rx.rows).iter().any(|b| b == UTD), "{:?}", bodies(&alice2_rx.rows));

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
    let tag = tag();
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

    // One person with a picture, for the faces beside a group's messages.
    let picture = matrix_sdk::ruma::OwnedMxcUri::from(upload["content_uri"].as_str().unwrap());
    carol.account().set_avatar_url(Some(&picture)).await.unwrap();

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
    // The user's last word, which the other side has read.
    let last = say(&dm, "See you there").await;
    // Formatted messages, shaped like what bridges send: a markdown-ish `body` next to the HTML.
    let html = |body: &str, formatted: &str| RoomMessageEventContent::text_html(body, formatted);
    bob_dm.send(html("**Bold** and _italic_ and ~~gone~~, see [the map](https://example.com/map)", "<strong>Bold</strong> and <em>italic</em> and <del>gone</del>, see <a href=\"https://example.com/map\">the map</a>")).await.unwrap();
    bob_dm.send(html("- tent\n- stove\n\n1. pack\n2. drive", "<ul><li>tent</li><li>stove</li></ul><ol><li>pack</li><li>drive</li></ol>")).await.unwrap();
    bob_dm.send(html("```\nlet a = 1;\nlet b = a + 1;\n```", "<pre><code>let a = 1;\nlet b = a + 1;\n</code></pre>")).await.unwrap();
    bob_dm.send(html("> Quoted line\n\nAnd my answer", "<blockquote><p>Quoted line</p></blockquote><p>And my answer</p>")).await.unwrap();
    bob_dm.send(html("> Shared your story\n\n[https://www.instagram.com/stories/mapfriend/3000000000000000000/](https://www.instagram.com/stories/mapfriend/3000000000000000000/?reel_id=3000000000000000000)", "<blockquote>\n<p>Shared your story</p>\n</blockquote>\n<p><a href=\"https://www.instagram.com/stories/mapfriend/3000000000000000000/?reel_id=3000000000000000000\">https://www.instagram.com/stories/mapfriend/3000000000000000000/</a></p>")).await.unwrap();
    // Bob syncs only after his last message here: messages he sends after a sync reached Alice's
    // device with the room key at a later index, which left his first two locked (seen, not
    // understood).
    bob.sync_once(SyncSettings::default()).await.unwrap();
    bob_dm.send_single_receipt(create_receipt::v3::ReceiptType::Read, ReceiptThread::Unthreaded, last).await.unwrap();

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
    // Mentions, as Matrix writes them: a link in the HTML form plus `m.mentions`.
    let me = alice.user_id().unwrap();
    let asks = RoomMessageEventContent::text_html(
        format!("@{} can you bring the tent?", me.localpart()),
        format!("<a href=\"https://matrix.to/#/{me}\">{}</a> can you bring the tent?", me.localpart()),
    );
    bob_group.send(asks.add_mentions(Mentions::with_user_ids([me.to_owned()]))).await.unwrap();
    carol_group.send(RoomMessageEventContent::text_plain("@room we leave at 9 sharp").add_mentions(Mentions::with_room_mention())).await.unwrap();
    // A poll with votes from two people, as bridges send them (the `org.matrix.msc3381` names),
    // and a shared place.
    let text = |text: &str| serde_json::json!({ "org.matrix.msc1767.text": text });
    let answer = |id: &str, text: &str| serde_json::json!({ "id": id, "org.matrix.msc1767.text": text });
    let poll = serde_json::json!({
        "org.matrix.msc3381.poll.start": {
            "question": text("Which day do we leave?"),
            "kind": "org.matrix.msc3381.poll.disclosed",
            "max_selections": 1,
            "answers": [answer("fri", "Friday evening"), answer("sat", "Saturday morning"), answer("sun", "Sunday")],
        },
        "org.matrix.msc1767.text": "Which day do we leave?",
    });
    let poll_id = bob_group.send_raw("org.matrix.msc3381.poll.start", poll).await.unwrap().response.event_id;
    for (who, answer) in [(&carol_group, "sat"), (&group, "fri")] {
        let vote = serde_json::json!({
            "org.matrix.msc3381.poll.response": { "answers": [answer] },
            "m.relates_to": { "rel_type": "m.reference", "event_id": poll_id },
        });
        who.send_raw("org.matrix.msc3381.poll.response", vote).await.unwrap();
    }
    let place = serde_json::json!({ "msgtype": "m.location", "body": "Lakeside cabin", "geo_uri": "geo:52.52008,13.40495" });
    carol_group.send_raw("m.room.message", place).await.unwrap();
    // A link with the preview a bridge attaches to it.
    let preview = serde_json::json!({
        "msgtype": "m.text",
        "body": "The cabin: https://example.org/cabins/lakeside, looks good?",
        "com.beeper.linkpreviews": [{
            "matched_url": "https://example.org/cabins/lakeside",
            "og:title": "Lakeside cabin for six",
            "og:description": "Two bedrooms, a wood stove and a jetty. Twenty minutes from the station by bus.",
            "og:site_name": "Example Cabins",
            "og:image": upload["content_uri"],
            "og:image:width": 512,
            "og:image:height": 512,
        }],
    });
    bob_group.send_raw("m.room.message", preview).await.unwrap();
    // Lines about the chat itself: someone comes and goes, the description changes.
    let dave = login(format!("dave{tag}")).await;
    group.invite_user_by_id(dave.user_id().unwrap()).await.unwrap();
    let dave_group = join(&dave, group.room_id()).await;
    dave_group.leave().await.unwrap();
    group.set_room_topic("Cabin by the lake").await.unwrap();
    group.set_room_topic("Cabin by the lake, 14 to 16 June").await.unwrap();

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

/// A user with a password, made through Synapse's shared-secret registration
/// (`registration_shared_secret` in homeserver.yaml), the one way this test server lets one in.
async fn register_with_password(hs: &str, secret: &str, name: &str, password: &str) {
    let http = http_client().unwrap();
    let url = format!("{hs}/_synapse/admin/v1/register");
    let nonce: serde_json::Value = serde_json::from_slice(&http.get(&url).send().await.unwrap().bytes().await.unwrap()).unwrap();
    let nonce = nonce["nonce"].as_str().unwrap();
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{nonce}\0{name}\0{password}\0notadmin").as_bytes());
    let mac: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let body = serde_json::json!({ "nonce": nonce, "username": name, "password": password, "admin": false, "mac": mac });
    let resp = http.post(&url).body(body.to_string()).send().await.unwrap();
    assert!(resp.status().is_success(), "registration: {}", resp.text().await.unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local Synapse; also MATRIX_TEST_REGISTRATION_SECRET, see register_with_password"]
async fn password_login_restore_sign_out() {
    let hs = std::env::var("MATRIX_TEST_HS").expect("MATRIX_TEST_HS");
    let secret = std::env::var("MATRIX_TEST_REGISTRATION_SECRET").expect("MATRIX_TEST_REGISTRATION_SECRET");
    let tag = tag();
    let root = std::env::temp_dir().join(format!("beeper-gpui-password-{tag}"));
    let (name, password) = (format!("carol{tag}"), format!("pw-{tag}-not-for-production"));
    register_with_password(&hs, &secret, &name, &password).await;
    let login = |password: &str| Cmd::PasswordLogin { server: hs.clone(), user: name.clone(), password: password.to_owned() };

    // The state starts out pointed at Beeper, as the app does.
    let (mut st, mut rx) = state(root.clone(), beeper::HOMESERVER_URL);
    let wrong = st.handle(login("not the password")).await.unwrap_err().to_string();
    assert_eq!(wrong, "Wrong user name or password.");
    let insecure = st.handle(Cmd::PasswordLogin { server: "http://example.org".into(), user: name.clone(), password: password.clone() });
    assert_eq!(insecure.await.unwrap_err().to_string(), "This address is not encrypted. Use https.");
    let nowhere = st.handle(Cmd::PasswordLogin { server: "nowhere.invalid".into(), user: name.clone(), password: password.clone() });
    assert_eq!(nowhere.await.unwrap_err().to_string(), "No Matrix server found at nowhere.invalid.");

    st.handle(login(&password)).await.unwrap();
    let beeper = wait_for(&mut rx, "Ready", |e| match e {
        Ev::Ready { beeper, .. } => Some(beeper),
        _ => None,
    })
    .await;
    assert!(!beeper);
    let saved = std::fs::read_to_string(root.join("session.json")).unwrap();
    assert!(!saved.contains(&password));
    let saved: SavedSession = serde_json::from_str(&saved).unwrap();
    assert_eq!(saved.homeserver.as_deref().map(|h| h.trim_end_matches('/')), Some(hs.trim_end_matches('/')));
    // The token of another homeserver never goes to Beeper's account API.
    assert!(st.api().is_err());
    assert_eq!(st.handle(Cmd::Networks).await.unwrap_err().to_string(), "Networks are a Beeper feature");

    // A restart finds the account on its own server.
    for task in st.tasks.drain(..) {
        task.abort();
    }
    drop((st, rx));
    let (mut st, mut rx) = state(root.clone(), beeper::HOMESERVER_URL);
    assert!(st.restore().await.unwrap());
    let user = wait_for(&mut rx, "Ready after restart", |e| match e {
        Ev::Ready { user, beeper: false } => Some(user),
        _ => None,
    })
    .await;
    assert_eq!(user.localpart(), name);
    assert_eq!(st.homeserver.trim_end_matches('/'), hs.trim_end_matches('/'));
    st.client().unwrap().whoami().await.unwrap();

    st.handle(Cmd::SignOut).await.unwrap();
    wait_for(&mut rx, "NeedLogin", |e| matches!(e, Ev::NeedLogin).then_some(())).await;
    assert!(!root.join("session.json").exists());
    assert!(st.beeper);
    assert_eq!(st.homeserver, beeper_homeserver());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn servers_are_read_from_what_was_typed() {
    assert_eq!(server_from_input("matrix.org"), Ok("matrix.org".into()));
    assert_eq!(server_from_input("  matrix.org \n"), Ok("matrix.org".into()));
    assert_eq!(server_from_input("https://x.y"), Ok("https://x.y".into()));
    assert_eq!(server_from_input("HTTPS://X.Y/"), Ok("HTTPS://X.Y/".into()));
    assert_eq!(server_from_input("@a:b.c"), Ok("b.c".into()));
    assert_eq!(server_from_input(" @alice:example.org:8448 "), Ok("example.org:8448".into()));
    assert_eq!(server_from_input("http://127.0.0.1:8008"), Ok("http://127.0.0.1:8008".into()));
    assert_eq!(server_from_input("http://localhost:8008/"), Ok("http://localhost:8008/".into()));
    assert_eq!(server_from_input("http://[::1]:8008"), Ok("http://[::1]:8008".into()));
    assert_eq!(server_from_input("localhost:8008"), Ok("http://localhost:8008".into()));
    assert_eq!(server_from_input("@a:127.0.0.1:8008"), Ok("http://127.0.0.1:8008".into()));
    for refused in ["http://matrix.org", "http://127.0.0.1.evil.org", "http://localhost.evil.org:80", "http://evil.org/localhost"] {
        assert_eq!(server_from_input(refused), Err("This address is not encrypted. Use https."), "{refused}");
    }
    for empty in ["", "   ", "@alice", "@alice:", "https://"] {
        assert!(server_from_input(empty).is_err(), "{empty:?}");
    }
    assert!(server_from_input("matrix .org").is_err());
    assert!(server_from_input("ftp://matrix.org").is_err());
}

#[test]
fn sessions_without_a_homeserver_are_beeper() {
    let old = r#"{"session":{"user_id":"@a:beeper.com","device_id":"D","access_token":"t"}}"#;
    let saved: SavedSession = serde_json::from_str(old).unwrap();
    assert_eq!(saved.homeserver, None);
    let again = serde_json::to_value(&saved).unwrap();
    assert!(again.get("homeserver").is_none());
}

#[test]
fn archive_rule_matches_beeper() {
    let room = |ts, order, unread, pinned| RoomSummary {
        id: matrix_sdk::ruma::room_id!("!r:example.org").to_owned(),
        name: String::new(),
        preview: None,
        preview_id: None,
        ts,
        network: None,
        unread,
        mentions: 0,
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
        let from = matrix_sdk::ruma::user_id!("@a:example.org").to_owned();
        Msg { sender: sender.into(), body: "hello".into(), own, ts: 1, notify, ..Msg::bare("$e".into(), Kind::Message, from) }
    };
    let now = 60_000;
    let said = |items: &[Msg]| notes(items, now).into_iter().map(|n| format!("{}: {}", n.sender, n.text)).collect::<Vec<_>>();
    assert_eq!(said(&[msg("Ann", false, true)]), ["Ann: hello"]);
    // Muted or mentions-only chats, and one's own messages from another device.
    assert!(said(&[msg("Ann", false, false)]).is_empty());
    assert!(said(&[msg("Me", true, true)]).is_empty());
    // Those that qualify, oldest first, past the ones that do not.
    assert_eq!(said(&[msg("Ann", false, true), msg("Me", true, true), msg("Bob", false, true)]), ["Ann: hello", "Bob: hello"]);
    // A burst keeps its last few.
    let burst: Vec<Msg> = (0..10).map(|_| msg("Ann", false, true)).collect();
    assert_eq!(said(&burst).len(), MAX_NOTES);
    // History a bridge brings in later is not news.
    assert!(notes(&[msg("Ann", false, true)], now + 3_600_000).is_empty());
}

#[test]
fn mentions_are_found_in_the_body() {
    let html = r#"Liquid chc\u{119} <a href="https://matrix.to/#/@facebookgo_1:beeper.local">Piotr Iwan</a> i <a href="https://matrix.to/#/%40me%3Abeeper.com?via=beeper.com"><b>Jan &amp; Ola</b></a>, <a href="https://example.org">link</a>"#;
    let linked = linked_users(html);
    assert_eq!(linked, [("@facebookgo_1:beeper.local".to_owned(), "Piotr Iwan".to_owned()), ("@me:beeper.com".to_owned(), "Jan & Ola".to_owned())]);

    let body = "Liquid chc\u{119} @Piotr Iwan, Piotr Iwanowski nie";
    let marked: Vec<&str> = mention_ranges(body, &["Piotr Iwan"], false).into_iter().map(|r| &body[r]).collect();
    assert_eq!(marked, ["@Piotr Iwan"], "the @ belongs to the mention; a longer word is not one");

    let body = "@wszyscy mail a@b.pl @ @Ann";
    let marked: Vec<&str> = mention_ranges(body, &["Ann"], true).into_iter().map(|r| &body[r]).collect();
    assert_eq!(marked, ["@wszyscy", "@Ann"]);
    assert!(mention_ranges(body, &[], false).is_empty());
}

#[test]
fn unread_mentions_are_counted_among_the_unread() {
    let msg = |own, mentioned| {
        let from = matrix_sdk::ruma::user_id!("@a:example.org").to_owned();
        Msg { sender: "Ann".into(), own, ts: 1, notify: true, mentioned, ..Msg::bare("$e".into(), Kind::Message, from) }
    };
    // Two unread: the mention before them was read already.
    assert_eq!(unread_mentions(0, 2, &[msg(false, true), msg(false, false), msg(false, true)]), 1);
    // More unread than this batch holds: the earlier count stands.
    assert_eq!(unread_mentions(1, 5, &[msg(false, true), msg(true, false)]), 2);
    // Read: nothing left.
    assert_eq!(unread_mentions(3, 0, &[msg(false, true)]), 0);
}

#[test]
fn avatars_become_upright_squares() {
    let encode = |w, h| {
        let mut out = Vec::new();
        image::DynamicImage::new_rgb8(w, h).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    };
    let size = |bytes: &[u8]| {
        let picture = image::load_from_memory(bytes).unwrap();
        (picture.width(), picture.height())
    };
    assert_eq!(size(&square(encode(300, 120))), (96, 96));
    assert_eq!(size(&square(encode(40, 90))), (40, 40));
    let small = encode(64, 64);
    assert_eq!(square(small.clone()), small, "already square: untouched");
    assert_eq!(square(b"not a picture".to_vec()), b"not a picture");
}

#[test]
fn errors_lose_their_urls_and_retries_slow_down() {
    let error = "Message not sent: error sending request for url (https://example.org/_matrix/client/v3/sync?filter=%7B%22a%22%7D&since=s1_2): connection reset";
    assert_eq!(plain(&error), "Message not sent: error sending request: connection reset");
    assert_eq!(plain(&"no address here"), "no address here");
    assert_eq!(plain(&"cut off for url (https://example.org"), "cut off");
    let waits: Vec<u64> = (1..=7).map(|failed| retry_wait(failed).as_secs()).collect();
    assert_eq!(waits, [5, 10, 20, 40, 60, 60, 60]);
}

#[test]
fn a_quote_says_what_its_message_holds() {
    let from = OwnedUserId::try_from("@bob:localhost").unwrap();
    let mut m = Msg::bare("$a".to_owned(), Kind::Message, from);
    (m.sender, m.body, m.own) = ("Bob".to_owned(), "Who drives?".to_owned(), true);
    let q = m.quote();
    assert_eq!((q.id.as_str(), q.state, q.sender.as_str(), q.own, q.text.as_str()), ("$a", QuoteState::Ready, "Bob", true, "Who drives?"));
    assert!(!q.deleted && !q.poll && !q.place && q.media.is_none());
    m.place = Place::parse("geo:52.52008,13.40495", "Cabin");
    assert!(m.place.is_some());
    let q = m.quote();
    assert!(q.place);
    assert_eq!(q.text, "Cabin");
    (m.body, m.deleted, m.place) = (DELETED.to_owned(), true, None);
    assert!(m.quote().deleted);
    let unknown = Quote::unknown("$b".to_owned(), QuoteState::Loading);
    assert_eq!((unknown.state, unknown.sender.as_str()), (QuoteState::Loading, ""));
}

#[test]
fn link_previews_are_read_from_the_message() {
    let content = serde_json::json!({
        "body": "look https://www.example.org/a?b=1",
        "com.beeper.linkpreviews": [
            { "matched_url": "javascript:alert(1)", "og:title": "Not a web address" },
            {
                "matched_url": "https://www.example.org/a?b=1",
                "og:title": "  A title  ",
                "og:description": "About it",
                "og:image": "mxc://example.org/abc",
                "og:image:width": "1200",
                "og:image:height": 630,
            },
        ],
    });
    let link = link_preview(&content).expect("the second preview is usable");
    assert_eq!((link.url.as_str(), link.title.as_str(), link.description.as_str()), ("https://www.example.org/a?b=1", "A title", "About it"));
    assert_eq!(link.site, "example.org", "the host stands in for a missing site name");
    assert_eq!(link.image.expect("a picture").dims, Some((1200, 630)));

    assert!(link_preview(&serde_json::json!({ "body": "no previews" })).is_none());
    let bare = serde_json::json!({ "com.beeper.linkpreviews": [{ "matched_url": "https://example.org" }] });
    assert!(link_preview(&bare).is_none(), "nothing to show");
    let long = serde_json::json!({ "com.beeper.linkpreviews": [{ "matched_url": "https://example.org", "og:title": "x".repeat(5000) }] });
    assert_eq!(link_preview(&long).unwrap().title.len(), LINK_TEXT_MAX);
}

#[test]
fn timeline_changes_keep_the_rows_and_tell_the_list() {
    let row = |id: &str| Msg::bare(id.into(), Kind::Message, matrix_sdk::ruma::user_id!("@a:example.org").to_owned());
    let ids = |rows: &[Msg]| rows.iter().map(|m| m.id.clone()).collect::<Vec<_>>().join("");
    let mut rows = Vec::new();
    assert_eq!(apply(&mut rows, Diff::Reset { values: [row("b"), row("c")].into_iter().collect() }), None);
    assert_eq!(apply(&mut rows, Diff::PushFront { value: row("a") }), Some((0..0, 1)));
    assert_eq!(apply(&mut rows, Diff::PushBack { value: row("e") }), Some((3..3, 1)));
    assert_eq!(apply(&mut rows, Diff::Insert { index: 3, value: row("d") }), Some((3..3, 1)));
    assert_eq!(apply(&mut rows, Diff::Set { index: 1, value: row("B") }), Some((1..2, 1)));
    assert_eq!(apply(&mut rows, Diff::Append { values: [row("f"), row("g")].into_iter().collect() }), Some((5..5, 2)));
    assert_eq!(ids(&rows), "aBcdefg");
    assert_eq!(apply(&mut rows, Diff::Remove { index: 2 }), Some((2..3, 0)));
    assert_eq!(apply(&mut rows, Diff::PopFront), Some((0..1, 0)));
    assert_eq!(apply(&mut rows, Diff::PopBack), Some((4..5, 0)));
    assert_eq!(apply(&mut rows, Diff::Truncate { length: 3 }), Some((3..4, 0)));
    assert_eq!(ids(&rows), "Bde");
    // A change that does not fit the rows asks for a redraw instead of panicking.
    assert_eq!(apply(&mut rows, Diff::Set { index: 9, value: row("x") }), None);
    assert_eq!(apply(&mut rows, Diff::Remove { index: 3 }), None);
    assert_eq!(ids(&rows), "Bde");
    assert_eq!(apply(&mut rows, Diff::Clear), None);
    assert!(rows.is_empty() && apply(&mut rows, Diff::PopBack).is_none());
}

/// A hop to a local homeserver that holds every chunk a client sends for `delay`, so that each
/// request costs about one round trip of a phone's connection.
fn slow_hop(homeserver: &str, delay: Duration) -> String {
    use std::io::{Read as _, Write as _};
    use std::net::{Shutdown, TcpListener, TcpStream};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let upstream = homeserver.trim_start_matches("http://").to_owned();
    std::thread::spawn(move || {
        for inbound in listener.incoming().flatten() {
            let Ok(outbound) = TcpStream::connect(&upstream) else { continue };
            let (mut from_client, mut to_server) = (inbound.try_clone().unwrap(), outbound.try_clone().unwrap());
            std::thread::spawn(move || {
                let mut buf = vec![0; 64 * 1024];
                while let Ok(n @ 1..) = from_client.read(&mut buf) {
                    std::thread::sleep(delay);
                    if to_server.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
                let _ = to_server.shutdown(Shutdown::Write);
            });
            let (mut from_server, mut to_client) = (outbound, inbound);
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut from_server, &mut to_client);
                let _ = to_client.shutdown(Shutdown::Write);
            });
        }
    });
    format!("http://{addr}")
}

/// On a phone's connection every request takes a while: what the open conversation looks up for
/// the rows it has (the messages that replies quote, room keys) must not hold back the next rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local homeserver with JWT login, see module docs"]
async fn rows_arrive_while_quoted_messages_are_looked_up() {
    let hs = std::env::var("MATRIX_TEST_HS").expect("MATRIX_TEST_HS");
    let secret = std::env::var("MATRIX_TEST_JWT_SECRET").expect("MATRIX_TEST_JWT_SECRET");
    let tag = tag();
    let root = std::env::temp_dir().join(format!("beeper-gpui-slow-{tag}"));
    const REPLIES: usize = 30;
    let rtt = Duration::from_millis(200);

    let (mut alice, mut seat) = state(root.join("alice"), &slow_hop(&hs, rtt));
    alice.login_jwt(jwt(&secret, &format!("alice{tag}"))).await.unwrap();
    wait_for(&mut seat, "Ready", |e| matches!(e, Ev::Ready { .. }).then_some(())).await;
    let bob = Client::builder().homeserver_url(&hs).sqlite_store(root.join("bob"), None).build().await.unwrap();
    let mut data = JsonObject::new();
    data.insert("token".into(), jwt(&secret, &format!("bob{tag}")).into());
    bob.matrix_auth().login_custom("org.matrix.login.jwt", data).unwrap().await.unwrap();

    // Old messages, then replies to each of them: a chat opened at its end quotes messages it has
    // not loaded.
    let mut req = matrix_sdk::ruma::api::client::room::create_room::v3::Request::new();
    req.invite = vec![alice.client().unwrap().user_id().unwrap().to_owned()];
    let room = bob.create_room(req).await.unwrap();
    let room_id = room.room_id().to_owned();
    let mut old = Vec::new();
    for i in 0..REPLIES {
        old.push(room.send(RoomMessageEventContent::text_plain(format!("old {i}"))).await.unwrap().response.event_id);
    }
    for i in 0..40 {
        room.send(RoomMessageEventContent::text_plain(format!("between {i}"))).await.unwrap();
    }
    for (i, quoted) in old.into_iter().enumerate() {
        let mut reply = RoomMessageEventContent::text_plain(format!("reply {i}"));
        reply.relates_to = Some(matrix_sdk::ruma::events::room::message::Relation::Reply(matrix_sdk::ruma::events::relation::Reply::new(
            matrix_sdk::ruma::events::relation::InReplyTo::new(quoted),
        )));
        room.send(reply).await.unwrap();
    }
    alice.client().unwrap().join_room_by_id(&room_id).await.unwrap();
    wait_for(&mut seat, "the chat in the list", |e| match e {
        Ev::Rooms(r) => r.iter().any(|s| s.id == room_id).then_some(()),
        _ => None,
    })
    .await;

    alice.handle(Cmd::Open(room_id.clone())).await.unwrap();
    until(&mut seat, "the first rows", |rows| bodies(rows).iter().any(|b| b.starts_with("reply"))).await;
    let sent = tokio::time::Instant::now();
    room.send(RoomMessageEventContent::text_plain("fresh")).await.unwrap();
    until(&mut seat, "the new message", |rows| bodies(rows).iter().any(|b| b == "fresh")).await;
    let late = sent.elapsed();
    let replies = bodies(&seat.rows).iter().filter(|b| b.starts_with("reply")).count();
    eprintln!("new message on screen after {late:?}, with {replies} replies on screen");
    // A sync answer is a round trip or two; one lookup per quoted message is thirty. The margin is
    // for a homeserver busy with the other tests running alongside.
    assert!(late < rtt * 8, "the new message took {late:?}");
}

/// The SDK empties its store of events when it upgrades it, and builds before the timeline kept
/// none: a chat with nothing new since then must not wait for a sync that never mentions it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local homeserver with JWT login, see module docs"]
async fn a_chat_opens_at_once_after_the_store_lost_its_events() {
    let hs = std::env::var("MATRIX_TEST_HS").expect("MATRIX_TEST_HS");
    let secret = std::env::var("MATRIX_TEST_JWT_SECRET").expect("MATRIX_TEST_JWT_SECRET");
    let tag = tag();
    let root = std::env::temp_dir().join(format!("beeper-gpui-emptied-{tag}"));

    let (mut alice, mut seat) = state(root.join("alice"), &hs);
    alice.login_jwt(jwt(&secret, &format!("alice{tag}"))).await.unwrap();
    wait_for(&mut seat, "Ready", |e| matches!(e, Ev::Ready { .. }).then_some(())).await;
    let bob = Client::builder().homeserver_url(&hs).sqlite_store(root.join("bob"), None).build().await.unwrap();
    let mut data = JsonObject::new();
    data.insert("token".into(), jwt(&secret, &format!("bob{tag}")).into());
    bob.matrix_auth().login_custom("org.matrix.login.jwt", data).unwrap().await.unwrap();
    let mut req = matrix_sdk::ruma::api::client::room::create_room::v3::Request::new();
    req.invite = vec![alice.client().unwrap().user_id().unwrap().to_owned()];
    let room = bob.create_room(req).await.unwrap();
    let room_id = room.room_id().to_owned();
    for i in 0..5 {
        room.send(RoomMessageEventContent::text_plain(format!("hello {i}"))).await.unwrap();
    }
    alice.client().unwrap().join_room_by_id(&room_id).await.unwrap();
    wait_for(&mut seat, "the chat in the list", |e| match e {
        Ev::Rooms(r) => r.iter().any(|s| s.id == room_id && s.preview.is_some()).then_some(()),
        _ => None,
    })
    .await;

    // The app stops, and comes back to a store without events.
    for task in alice.tasks.drain(..) {
        task.abort();
    }
    drop((alice, seat));
    for file in std::fs::read_dir(root.join("alice/store")).unwrap().flatten() {
        if file.file_name().to_string_lossy().starts_with("matrix-sdk-event-cache") {
            std::fs::remove_file(file.path()).unwrap();
        }
    }
    let (mut alice, mut seat) = state(root.join("alice"), &hs);
    assert!(alice.restore().await.unwrap());
    wait_for(&mut seat, "Ready", |e| matches!(e, Ev::Ready { .. }).then_some(())).await;

    let opened = tokio::time::Instant::now();
    alice.handle(Cmd::Open(room_id.clone())).await.unwrap();
    until(&mut seat, "the messages", |rows| bodies(rows).iter().any(|b| b == "hello 4")).await;
    let took = opened.elapsed();
    eprintln!("messages on screen after {took:?}");
    // The SDK waits up to 3 s for a sync to tell it where the chat's history starts.
    assert!(took < Duration::from_secs(2), "the messages took {took:?}");
}

//! Devices and verification against the local Synapse: two devices of one account compare emoji,
//! and another device is signed out behind the server's user-interactive auth.

use matrix_sdk::ruma::OwnedDeviceId;

use super::*;
use crate::backend::verify::{Auth, CrossSigning, Proof, Trust, Verify};

async fn next_verify(seat: &mut Seat, what: &str) -> Verify {
    wait_for(seat, what, |e| match e {
        Ev::Verify(v) => Some(v),
        _ => None,
    })
    .await
}

/// The next step that is not a wait.
async fn settled(seat: &mut Seat, what: &str) -> Verify {
    loop {
        match next_verify(seat, what).await {
            Verify::Waiting | Verify::Confirmed => {}
            v => return v,
        }
    }
}

/// B asks, A accepts, and both reach the emoji, which must be the same seven.
async fn to_emoji(a: &mut State, a_rx: &mut Seat, b: &mut State, b_rx: &mut Seat) -> Vec<(String, String)> {
    b.handle(Cmd::VerifyStart).await.unwrap();
    assert_eq!(next_verify(b_rx, "B waiting").await, Verify::Waiting);
    match settled(a_rx, "A asked").await {
        Verify::Incoming { who } => assert!(who.starts_with("Your device"), "{who}"),
        other => panic!("A got {other:?}"),
    }
    a.handle(Cmd::VerifyAccept).await.unwrap();
    let (Verify::Emoji(on_a), Verify::Emoji(on_b)) = (settled(a_rx, "emoji on A").await, settled(b_rx, "emoji on B").await) else { panic!("no emoji") };
    assert_eq!(on_a.len(), 7);
    assert!(on_a.iter().all(|(symbol, name)| !symbol.is_empty() && !name.is_empty()));
    assert_eq!(on_a, on_b);
    on_a
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local homeserver with JWT login, see module docs"]
async fn two_devices_compare_emoji() {
    let hs = std::env::var("MATRIX_TEST_HS").expect("MATRIX_TEST_HS");
    let secret = std::env::var("MATRIX_TEST_JWT_SECRET").expect("MATRIX_TEST_JWT_SECRET");
    let tag = tag();
    let root = std::env::temp_dir().join(format!("beeper-gpui-verify-{tag}"));
    let name = format!("dana{tag}");

    let (mut a, mut a_rx) = state(root.join("a"), &hs);
    a.login_jwt(jwt(&secret, &name)).await.unwrap();
    wait_for(&mut a_rx, "A ready", |e| matches!(e, Ev::Ready { .. }).then_some(())).await;
    a.client().unwrap().encryption().bootstrap_cross_signing_if_needed(None).await.unwrap();

    let (mut b, mut b_rx) = state(root.join("b"), &hs);
    b.login_jwt(jwt(&secret, &name)).await.unwrap();
    wait_for(&mut b_rx, "B ready", |e| matches!(e, Ev::Ready { .. }).then_some(())).await;
    let b_id = b.client().unwrap().device_id().unwrap().to_string();

    let before = verify::devices(b.client().unwrap()).await.unwrap();
    assert!(!before.this_verified);
    assert_eq!(before.cross_signing, CrossSigning::Elsewhere);
    assert_eq!(before.list.len(), 2);
    assert!(before.list[0].this && before.list[0].id == b_id);
    assert_eq!(before.list[1].trust, Trust::Verified, "A signed itself when it set up cross-signing");
    // The SDK drops a request from a device whose keys it has not fetched yet. Listing the devices
    // fetches them, as the user opening Settings on A would; sync alone gets there a moment later.
    let seen_by_a = verify::devices(a.client().unwrap()).await.unwrap();
    assert_eq!(seen_by_a.list.iter().find(|d| d.id == b_id).map(|d| d.trust), Some(Trust::Unverified));

    // Saying the emoji differ verifies nothing, on either side.
    to_emoji(&mut a, &mut a_rx, &mut b, &mut b_rx).await;
    a.handle(Cmd::VerifyMatch(false)).await.unwrap();
    let mismatch = Verify::Failed("The emoji did not match, so nothing was verified.".into());
    assert_eq!(settled(&mut a_rx, "A ended").await, mismatch);
    assert_eq!(settled(&mut b_rx, "B ended").await, mismatch);
    assert!(!verify::devices(b.client().unwrap()).await.unwrap().this_verified);

    // Matching emoji verify B.
    to_emoji(&mut a, &mut a_rx, &mut b, &mut b_rx).await;
    a.handle(Cmd::VerifyMatch(true)).await.unwrap();
    b.handle(Cmd::VerifyMatch(true)).await.unwrap();
    assert_eq!(settled(&mut a_rx, "A done").await, Verify::Done);
    assert_eq!(settled(&mut b_rx, "B done").await, Verify::Done);
    let after = wait_for(&mut b_rx, "B verified", |e| match e {
        Ev::Devices(d) if d.this_verified => Some(d),
        _ => None,
    })
    .await;
    assert!(after.list.iter().all(|d| d.trust == Trust::Verified), "{:?}", after.list);
    let on_a = verify::devices(a.client().unwrap()).await.unwrap();
    assert_eq!(on_a.cross_signing, CrossSigning::Here);
    assert_eq!(on_a.list.iter().find(|d| d.id == b_id).map(|d| d.trust), Some(Trust::Verified));

    // Renaming reaches the list.
    b.handle(Cmd::RenameDevice("  Kitchen tablet ".into())).await.unwrap();
    // Other refreshes may come first: the one after a change in trust is not ordered with it.
    wait_for(&mut b_rx, "renamed", |e| match e {
        Ev::Devices(d) => (d.list[0].name == "Kitchen tablet").then_some(()),
        _ => None,
    })
    .await;
    b.handle(Cmd::RenameDevice(" ".into())).await.unwrap();
    wait_for(&mut b_rx, "empty name refused", |e| matches!(e, Ev::DevicesFailed(_)).then_some(())).await;

    // An account signed in by JWT has no password; stock Synapse then offers no way to confirm
    // (Beeper's Synapse adds a JWT stage, which this server lacks).
    a.handle(Cmd::SignOutDevice { device: b_id.clone(), proof: Proof::Ask }).await.unwrap();
    let step = wait_for(&mut a_rx, "sign-out step", |e| match e {
        Ev::SignOutStep(s) => Some(s),
        _ => None,
    })
    .await;
    assert_eq!(step, Ok(Some(Auth::Unsupported)));

    for st in [&mut a, &mut b] {
        st.verify.stop();
        for task in st.tasks.drain(..) {
            task.abort();
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local Synapse; also MATRIX_TEST_REGISTRATION_SECRET, see register_with_password"]
async fn another_device_is_signed_out_with_the_password() {
    let hs = std::env::var("MATRIX_TEST_HS").expect("MATRIX_TEST_HS");
    let secret = std::env::var("MATRIX_TEST_REGISTRATION_SECRET").expect("MATRIX_TEST_REGISTRATION_SECRET");
    let tag = tag();
    let root = std::env::temp_dir().join(format!("beeper-gpui-signout-{tag}"));
    let (name, password) = (format!("erin{tag}"), format!("pw-{tag}-not-for-production"));
    register_with_password(&hs, &secret, &name, &password).await;

    let (mut st, mut rx) = state(root.clone(), beeper::HOMESERVER_URL);
    st.handle(Cmd::PasswordLogin { server: hs.clone(), user: name.clone(), password: password.clone() }).await.unwrap();
    wait_for(&mut rx, "Ready", |e| matches!(e, Ev::Ready { .. }).then_some(())).await;
    let other = Client::builder().homeserver_url(&hs).sqlite_store(root.join("other"), None).build().await.unwrap();
    other.matrix_auth().login_username(&name, &password).await.unwrap();
    let other_id = other.device_id().unwrap().to_string();
    let this_id = st.client().unwrap().device_id().unwrap().to_string();

    let mut step = async |st: &mut State, device: &str, proof: Proof| {
        st.handle(Cmd::SignOutDevice { device: device.to_owned(), proof }).await.unwrap();
        wait_for(&mut rx, "sign-out step", |e| match e {
            Ev::SignOutStep(s) => Some(s),
            _ => None,
        })
        .await
    };
    assert_eq!(step(&mut st, &this_id, Proof::Ask).await, Err("Sign this device out from Settings, under Account".into()));
    assert_eq!(step(&mut st, &other_id, Proof::Ask).await, Ok(Some(Auth::Password)));
    assert_eq!(step(&mut st, &other_id, Proof::Password("not the password".into())).await, Err("Wrong password.".into()));
    assert_eq!(step(&mut st, &other_id, Proof::Password(password.clone())).await, Ok(None));

    // Shared-secret registration leaves a device of its own, so only these two are looked for.
    let left: Vec<String> = verify::devices(st.client().unwrap()).await.unwrap().list.into_iter().map(|d| d.id).collect();
    assert!(left.contains(&this_id) && !left.contains(&other_id), "{left:?}");
    assert!(other.whoami().await.is_err(), "the other device's token still works");

    st.verify.stop();
    for task in st.tasks.drain(..) {
        task.abort();
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The other side for a check of the screens in the GUI sandbox, on an account of its own.
/// Writes `$GUI_VERIFY_DIR/session.json` for a new device of a new account (use it as the
/// sandbox's FIXTURE), then stands in for the user's other device for `GUI_VERIFY_SECS`: it
/// accepts the sandbox's request and confirms the emoji (written to `emoji.txt`), and after that
/// asks the sandbox's device to verify, so the incoming sheet can be seen.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives the GUI sandbox check; needs GUI_VERIFY_DIR and the local homeserver"]
async fn gui_verify_helper() {
    let hs = std::env::var("MATRIX_TEST_HS").expect("MATRIX_TEST_HS");
    let secret = std::env::var("MATRIX_TEST_JWT_SECRET").expect("MATRIX_TEST_JWT_SECRET");
    let dir = PathBuf::from(std::env::var("GUI_VERIFY_DIR").expect("GUI_VERIFY_DIR"));
    let secs: u64 = std::env::var("GUI_VERIFY_SECS").map_or(240, |s| s.parse().unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    let name = format!("gui{}", tag());

    let helper = Client::builder().homeserver_url(&hs).sqlite_store(dir.join("helper"), None).build().await.unwrap();
    let mut data = JsonObject::new();
    data.insert("token".into(), jwt(&secret, &name).into());
    helper.matrix_auth().login_custom("org.matrix.login.jwt", data).unwrap().initial_device_display_name("Helper laptop").await.unwrap();
    helper.encryption().bootstrap_cross_signing_if_needed(None).await.unwrap();

    // The sandbox's device: logged in over plain HTTP so that no keys are uploaded for it here.
    let http = http_client().unwrap();
    let body = serde_json::json!({ "type": "org.matrix.login.jwt", "token": jwt(&secret, &name), "initial_device_display_name": "Unisend" });
    let login = http.post(format!("{hs}/_matrix/client/v3/login")).body(body.to_string()).send().await.unwrap().bytes().await.unwrap();
    let login: serde_json::Value = serde_json::from_slice(&login).unwrap();
    let session = serde_json::json!({ "session": { "user_id": login["user_id"], "device_id": login["device_id"], "access_token": login["access_token"] } });
    write_private(&dir.join("session.json"), session.to_string().as_bytes()).unwrap();
    let sandbox_device = OwnedDeviceId::from(login["device_id"].as_str().unwrap());

    let emoji_file = dir.join("emoji.txt");
    helper.add_event_handler(move |e: matrix_sdk::ruma::events::key::verification::request::ToDeviceKeyVerificationRequestEvent, client: Client| {
        let emoji_file = emoji_file.clone();
        async move {
            let Some(request) = client.encryption().get_verification_request(&e.sender, &e.content.transaction_id).await else { return };
            // Handlers run inside sync: one that waits for the next sync waits forever.
            tokio::spawn(async move {
                let mut changes = request.changes();
                request.accept().await.unwrap();
                while let Some(state) = changes.next().await {
                    if let matrix_sdk::encryption::verification::VerificationRequestState::Transitioned { verification } = state {
                        let sas = verification.sas().unwrap();
                        sas.accept().await.unwrap();
                        let mut sas_changes = sas.changes();
                        while let Some(s) = sas_changes.next().await {
                            match s {
                                matrix_sdk::encryption::verification::SasState::KeysExchanged { .. } => {
                                    let list: Vec<String> = sas.emoji().unwrap().iter().map(|e| format!("{} {}", e.symbol, e.description)).collect();
                                    std::fs::write(&emoji_file, list.join("\n")).unwrap();
                                    sas.confirm().await.unwrap();
                                }
                                matrix_sdk::encryption::verification::SasState::Done { .. } | matrix_sdk::encryption::verification::SasState::Cancelled(_) => {
                                    break;
                                }
                                _ => {}
                            }
                        }
                        break;
                    }
                }
            });
        }
    });
    let sync = tokio::spawn({
        let helper = helper.clone();
        async move { helper.sync(SyncSettings::default()).await }
    });
    eprintln!("ready: {}", dir.join("session.json").display());
    // Once the sandbox's device is verified, ask it to verify again, so the incoming sheet shows.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut asked = false;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(5)).await;
        if asked {
            continue;
        }
        let own = helper.user_id().unwrap().to_owned();
        let _ = helper.encryption().request_user_identity(&own).await;
        if let Ok(Some(device)) = helper.encryption().get_device(&own, &sandbox_device).await
            && device.is_cross_signed_by_owner()
            && std::fs::read_to_string(dir.join("ask")).is_ok()
        {
            device.request_verification().await.unwrap();
            asked = true;
            eprintln!("asked the sandbox's device");
        }
    }
    sync.abort();
}

/// Listing the devices queries the account's keys, after which the SDK sets the verification state
/// again; the watcher must not take that for a change and list them again, forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local homeserver with JWT login, see module docs"]
async fn device_list_does_not_refresh_itself() {
    let hs = std::env::var("MATRIX_TEST_HS").expect("MATRIX_TEST_HS");
    let secret = std::env::var("MATRIX_TEST_JWT_SECRET").expect("MATRIX_TEST_JWT_SECRET");
    let tag = tag();
    let root = std::env::temp_dir().join(format!("beeper-gpui-devloop-{tag}"));
    let (mut a, mut a_rx) = state(root.clone(), &hs);
    a.login_jwt(jwt(&secret, &format!("erin{tag}"))).await.unwrap();
    wait_for(&mut a_rx, "ready", |e| matches!(e, Ev::Ready { .. }).then_some(())).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    let mut lists = 0;
    while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, a_rx.rx.recv()).await {
        lists += usize::from(matches!(ev, Ev::Devices(_)));
    }
    assert!(lists <= 2, "{lists} device lists in 4 s");
    let _ = std::fs::remove_dir_all(root);
}

//! 1.1: whether a device is awake, wake requests, and Extend's Tings per Team (test_plan 11–15).
//! Awake is information plus the wake flow, never a gate.

mod common;

use common::*;
use extend_protocol::DeviceOs;
use serde_json::{Value, json};
use uuid::Uuid;

async fn owner_view(env: &Env, token: &str, id: &str) -> Value {
    api(env, "GET", &format!("/api/v1/devices/{id}"), token, None, None)
        .await
        .1["data"]
        .clone()
}

async fn wake_row(env: &Env, wake_id: &str) -> (String, Option<String>, Option<String>, Option<String>) {
    sqlx::query_as(
        "SELECT state, end_reason, ting_delivery, answer_ting FROM extend.wake_requests WHERE wake_id = $1::uuid",
    )
    .bind(wake_id)
    .fetch_one(&env.pool)
    .await
    .unwrap()
}

// ───────────── 11: reporting awake ─────────────

#[tokio::test]
async fn awake_reports_update_the_physical_device_in_order() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &[]).await;
    let a = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (d2, cred2) = pair_another(&env, &cred, &bob, Some("acme"), &[]).await;
    let b = App::connect(&env, &cred2, hello(DeviceOs::Android, "1.1.0")).await;
    let run = Uuid::new_v4();
    a.send(awake_frame(false, Some("screen_off"), None, run, 5));
    eventually("alice reading screen off", || async {
        owner_view(&env, &alice, &d).await["sleep_state"] == "screen_off"
    })
    .await;
    // One instance: bob's pair reads the same.
    let v = owner_view(&env, &bob, &d2).await;
    assert_eq!(
        (v["awake"].as_bool(), v["sleep_state"].as_str()),
        (Some(false), Some("screen_off"))
    );
    assert!(v["awake_changed_at"].is_string());
    // A lower seq in the same run is ignored; a duplicate changes nothing.
    b.send(awake_frame(true, None, Some(true), run, 4));
    b.send(awake_frame(false, Some("screen_off"), None, run, 5));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(owner_view(&env, &alice, &d).await["awake"], false);
    // A new run is applied whatever its seq.
    b.send(awake_frame(false, Some("locked"), None, Uuid::new_v4(), 1));
    eventually("a new run applied", || async {
        owner_view(&env, &alice, &d).await["sleep_state"] == "locked"
    })
    .await;
    // bob's pair reconnects while alice's is connected: the state isn't reset.
    drop(b);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let _b = App::connect(&env, &cred2, hello(DeviceOs::Android, "1.1.0")).await;
    assert_eq!(owner_view(&env, &alice, &d).await["awake"], false);
    // Every socket gone: offline reads null, with the last state seen.
    drop(a);
    drop(_b);
    eventually("offline", || async {
        owner_view(&env, &alice, &d).await["online"] == false
    })
    .await;
    let v = owner_view(&env, &alice, &d).await;
    assert!(v.get("awake").is_none());
    assert_eq!(v["last_sleep_state"], "locked");
    // The first socket back resets it to unknown.
    let _a = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let v = owner_view(&env, &alice, &d).await;
    assert!(v.get("awake").is_none(), "{v}");
}

// ───────────── 12–14: wake requests ─────────────

#[tokio::test]
async fn asking_to_wake_a_device() {
    let env = start().await;
    let ting = env.state.local_ting.clone().unwrap();
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let sous = login(&env, "si:sous").await;
    let scout = login(&env, "si:scout").await;
    let (d, cred) = pair(
        &env,
        &alice,
        Some("acme"),
        DeviceOs::Android,
        "Pixel",
        &["si:chef", "si:sous"],
    )
    .await;
    grant(&env, &alice, &d, "si:scout", "globex").await;
    // Offline: accepted, device_notice offline.
    let (s, w) = wake(&env, &chef, "acme", &d, "Need the OTP screen").await;
    assert_eq!(s, 201, "{w}");
    assert_eq!(
        (w["data"]["device_notice"].as_str(), w["data"]["state"].as_str()),
        (Some("offline"), Some("open"))
    );
    let wid = w["data"]["wake_id"].as_str().unwrap().to_owned();
    // The Carbon Ting: to alice, in acme, as chef, key wake:{id}:1.
    let t = ting.sent_of("device.wake_requested").await;
    assert_eq!(t.len(), 1);
    assert_eq!(
        (t[0]["actor"].as_str(), t[0]["for"].as_str(), t[0]["org_id"].as_str()),
        (Some("si:chef"), Some("c:alice"), Some("acme"))
    );
    assert_eq!(t[0]["key"], format!("wake:{wid}:1"));
    assert!(
        t[0]["data"]["summary"]
            .as_str()
            .unwrap()
            .contains("si:chef asks you to wake Pixel")
    );
    // Checks: silicon_only, no access, reason bounds.
    let (s, _) = wake(&env, &alice, "acme", &d, "x").await;
    assert_eq!(s, 403);
    let (s, _) = wake(&env, &chef, "globex", &d, "x").await;
    assert_eq!(s, 404, "chef has no grant in globex");
    let (s, _) = wake(&env, &sous, "acme", &d, "   ").await;
    assert_eq!(s, 422);
    // Asking again within 5 minutes: 429 with its own request only.
    let (s, e) = wake(&env, &chef, "acme", &d, "again").await;
    assert_eq!((s, e["data"]["code"].as_str()), (429, Some("rate_limited")));
    assert_eq!(e["data"]["details"]["wake_request"]["wake_id"], wid.as_str());
    // After 5 minutes: refreshed (200), asks 2.
    sqlx::query(
        "UPDATE extend.wake_requests SET last_asked_at = now() - interval '6 minutes' WHERE wake_id = $1::uuid",
    )
    .bind(&wid)
    .execute(&env.pool)
    .await
    .unwrap();
    let (s, r) = wake(&env, &chef, "acme", &d, "still need it").await;
    assert_eq!(
        (s, r["data"]["asks"].as_i64(), r["data"]["reason"].as_str()),
        (200, Some(2), Some("still need it"))
    );
    // Collapse: another asker on the same pair and Team within 15 minutes is covered.
    let (s, r) = wake(&env, &sous, "acme", &d, "me too").await;
    assert_eq!(s, 201);
    assert_eq!(
        (r["data"]["ting"].as_str(), r["data"]["ting_covered_by"].as_str()),
        (Some("covered"), Some(wid.as_str()))
    );
    // scout asks through the same pair in globex: a Ting of its own, in globex, as scout.
    let (s, _) = wake(&env, &scout, "globex", &d, "globex needs it").await;
    assert_eq!(s, 201);
    let last = ting.sent_of("device.wake_requested").await.last().unwrap().clone();
    assert_eq!(
        (last["actor"].as_str(), last["org_id"].as_str()),
        (Some("si:scout"), Some("globex"))
    );
    // The owner sees every Team's request on the pair; a Silicon only its own.
    let (_, all) = api(
        &env,
        "GET",
        &format!("/api/v1/devices/{d}/wake-requests?state=open"),
        &alice,
        None,
        None,
    )
    .await;
    assert_eq!(all["data"]["items"].as_array().unwrap().len(), 3);
    let (_, own) = api(
        &env,
        "GET",
        &format!("/api/v1/devices/{d}/wake-requests"),
        &scout,
        Some("globex"),
        None,
    )
    .await;
    assert_eq!(own["data"]["items"].as_array().unwrap().len(), 1);
    // The app connects: it gets the open requests; a 1.1 app on an awake device refuses asks.
    let app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let frames = app.of("wake_request");
    assert_eq!(frames.len(), 3, "{frames:?}");
    assert!(frames.iter().all(|f| f["side"].as_str().is_some_and(|s| s.len() == 16)));
    app.send(awake_frame(true, None, None, Uuid::new_v4(), 1));
    // That wake (input unknown) resolves every open request on the device, each with its own Ting.
    eventually("every request woken", || async {
        wake_row(&env, &wid).await.0 == "woken"
    })
    .await;
    eventually("woken Tings", || async {
        ting.sent_of("device.woken").await.len() == 3
    })
    .await;
    let woken = ting.sent_of("device.woken").await;
    let scout_t = woken.iter().find(|t| t["for"] == "si:scout").unwrap();
    assert_eq!(scout_t["org_id"], "globex");
    assert_eq!(scout_t["data"]["now"], "free");
    assert!(
        scout_t["data"]["next"]
            .as_str()
            .unwrap()
            .starts_with("extend --team globex session new")
    );
    // Sent by scout's own Carbon (the owner of its pair); the body names no one.
    assert!(!scout_t["data"].to_string().contains("si:chef") && !scout_t["data"].to_string().contains("c:alice"));
    let (s, e) = wake(&env, &chef, "acme", &d, "one more").await;
    assert_eq!(
        (s, e["data"]["code"].as_str()),
        (409, Some("conflict")),
        "already awake: {e}"
    );
    assert!(e["data"]["hint"].as_str().unwrap().contains("session new"));
    // Idempotent replay of an ask.
    app.send(awake_frame(false, Some("screen_off"), None, Uuid::new_v4(), 1));
    eventually("asleep again", || async {
        owner_view(&env, &alice, &d).await["awake"] == false
    })
    .await;
    let ask = |key: &'static str| {
        let (env, chef, d) = (&env, chef.clone(), d.clone());
        async move {
            let r = reqwest::Client::new()
                .post(format!("{}/api/v1/devices/{d}/wake-requests", env.base))
                .bearer_auth(&chef)
                .header("x-org-id", "acme")
                .header("idempotency-key", key)
                .json(&json!({"type": "wake_request", "data": {"reason": "after woken"}}))
                .send()
                .await
                .unwrap();
            (
                r.status().as_u16(),
                r.headers().get("idempotency-replayed").is_some(),
                r.json::<Value>().await.unwrap(),
            )
        }
    };
    let (s1, replay1, w1) = ask("wake-idem-0001").await;
    let (s2, replay2, w2) = ask("wake-idem-0001").await;
    assert_eq!(
        (s1, s2, replay1, replay2),
        (201, 201, false, true),
        "allowed at once after woken, and replayed"
    );
    assert_eq!(w1["data"]["wake_id"], w2["data"]["wake_id"]);
    assert_eq!(w1["data"]["device_notice"], "sent");
}

#[tokio::test]
async fn wake_rules_mute_holders_limits_and_redaction() {
    let env = start().await;
    let ting = env.state.local_ting.clone().unwrap();
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let chef = login(&env, "si:chef").await;
    let sous = login(&env, "si:sous").await;
    let scout = login(&env, "si:scout").await;
    let (d, cred) = pair(
        &env,
        &alice,
        Some("acme"),
        DeviceOs::Android,
        "Pixel",
        &["si:chef", "si:sous"],
    )
    .await;
    grant(&env, &alice, &d, "si:scout", "globex").await;
    let a = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (d2, cred2) = pair_another(&env, &cred, &bob, Some("acme"), &["si:chef"]).await;
    let b = App::connect(&env, &cred2, hello(DeviceOs::Android, "1.1.0")).await;
    a.send(awake_frame(false, Some("screen_off"), None, Uuid::new_v4(), 1));
    eventually("asleep", || async {
        owner_view(&env, &alice, &d).await["awake"] == false
    })
    .await;
    // Muted for the pair, then for one Silicon across Teams.
    let (s, _) = api(
        &env,
        "PUT",
        &format!("/api/v1/devices/{d}/wake-settings"),
        &alice,
        None,
        Some(json!({"type": "wake_settings", "data": {"muted": true}})),
    )
    .await;
    assert_eq!(s, 200);
    let (s, e) = wake(&env, &chef, "acme", &d, "x").await;
    assert_eq!(s, 409);
    assert!(
        e["data"]["message"]
            .as_str()
            .unwrap()
            .contains("c:alice has turned off wake requests for Pixel"),
        "{e}"
    );
    let _ = api(
        &env,
        "PUT",
        &format!("/api/v1/devices/{d}/wake-settings"),
        &alice,
        None,
        Some(json!({"type": "wake_settings", "data": {"muted": false}})),
    )
    .await;
    let (s, view) = api(
        &env,
        "PUT",
        &format!("/api/v1/devices/{d}/wake-settings"),
        &alice,
        None,
        Some(json!({"type": "wake_settings", "data": {"muted": true, "silicon_id": "si:scout"}})),
    )
    .await;
    assert_eq!(
        (s, view["data"]["silicons_muted"][0]["team"].as_str()),
        (200, Some("globex"))
    );
    assert_eq!(wake(&env, &scout, "globex", &d, "x").await.0, 409);
    let (s, _) = api(
        &env,
        "PUT",
        &format!("/api/v1/devices/{d}/wake-settings"),
        &alice,
        None,
        Some(json!({"type": "wake_settings", "data": {"muted": true, "silicon_id": "si:nobody"}})),
    )
    .await;
    assert_eq!(s, 422);
    // The device sounds once per 15 minutes across pairs.
    let (_, w1) = wake(&env, &chef, "acme", &d, "first").await;
    let f1 = a.wait("wake_request", |f| f["wake_id"] == w1["data"]["wake_id"]).await;
    assert_eq!(
        (f1["alert"].as_bool(), f1["silicon_id"].as_str()),
        (Some(true), Some("si:chef"))
    );
    // Through the second pair while open through the first: 409 naming its own.
    let (s, e) = wake(&env, &chef, "acme", &d2, "second pair").await;
    assert_eq!(s, 409);
    assert!(
        e["data"]["message"]
            .as_str()
            .unwrap()
            .contains(w1["data"]["wake_id"].as_str().unwrap())
    );
    let (_, w2) = wake(&env, &sous, "acme", &d, "second asker").await;
    let f2 = a.wait("wake_request", |f| f["wake_id"] == w2["data"]["wake_id"]).await;
    assert_eq!(f2["alert"], false);
    // Another side's session: the device's frames lose the Silicon and reason; the side differs.
    grant(&env, &bob, &d2, "si:chef", "acme").await;
    let _ = api(
        &env,
        "DELETE",
        &format!(
            "/api/v1/devices/{d}/wake-requests/{}",
            w1["data"]["wake_id"].as_str().unwrap()
        ),
        &chef,
        Some("acme"),
        None,
    )
    .await;
    a.clear();
    let (s, sess) = session(&env, &chef, "acme", &d2).await;
    assert_eq!(s, 201, "{sess}");
    let started = b.wait("session_started", |_| true).await;
    let redacted = a.wait("wake_request", |f| f["wake_id"] == w2["data"]["wake_id"]).await;
    assert!(
        redacted.get("silicon_id").is_none() && redacted.get("reason").is_none(),
        "{redacted}"
    );
    assert_ne!(redacted["side"], started["side"]);
    assert_eq!(f2["side"], redacted["side"], "the request's side stays");
    // A same-side ask carries the session's side tag (its cancelled request was 5+ minutes ago).
    sqlx::query(
        "UPDATE extend.wake_requests SET last_asked_at = now() - interval '6 minutes' WHERE from_id = 'si:chef'",
    )
    .execute(&env.pool)
    .await
    .unwrap();
    let (s, own) = wake(&env, &chef, "acme", &d2, "the holder may ask").await;
    assert_eq!(s, 201, "{own}");
    let f = b.wait("wake_request", |f| f["wake_id"] == own["data"]["wake_id"]).await;
    assert_eq!(f["side"], started["side"]);
    // Another side can't ask while it's held.
    let (s, e) = wake(&env, &scout, "globex", &d, "x").await;
    let _ = api(
        &env,
        "PUT",
        &format!("/api/v1/devices/{d}/wake-settings"),
        &alice,
        None,
        Some(json!({"type": "wake_settings", "data": {"muted": false, "silicon_id": "si:scout"}})),
    )
    .await;
    let (s2, e2) = wake(&env, &scout, "globex", &d, "x").await;
    assert_eq!(
        (s, s2, e2["data"]["code"].as_str()),
        (409, 409, Some("device_in_use")),
        "{e} {e2}"
    );
    assert!(!e2.to_string().contains("si:chef"));
    // The session ends: frames follow again.
    a.clear();
    let _ = api(
        &env,
        "POST",
        &format!("/api/v1/sessions/{}/end", sess["data"]["session_id"].as_str().unwrap()),
        &chef,
        Some("acme"),
        None,
    )
    .await;
    let back = a.wait("wake_request", |f| f["wake_id"] == w2["data"]["wake_id"]).await;
    assert_eq!(back["silicon_id"], "si:sous");
    // The 7th Carbon Ting in an hour is deferred: the owner sees deferred, the Silicon pending.
    sqlx::query("UPDATE extend.wake_requests SET ting_sent_at = now() - interval '5 minutes', ting_delivery = 'delivered' WHERE to_id = 'c:alice'")
        .execute(&env.pool)
        .await
        .unwrap();
    for i in 0..5 {
        sqlx::query(
            "INSERT INTO extend.wake_requests (wake_id, device_id, instance_id, team, from_id, to_id, reason, expires_at, state,
                 wake_detectable, device_notice, ting_delivery, ting_sent_at)
             SELECT gen_random_uuid(), device_id, instance_id, 'acme', $2, 'c:alice', 'old', now(), 'expired', true, 'sent', 'delivered', now() - interval '10 minutes'
             FROM extend.devices WHERE device_id = $1",
        )
        .bind(&d)
        .bind(format!("si:old{i}"))
        .execute(&env.pool)
        .await
        .unwrap();
    }
    let before = ting.sent_of("device.wake_requested").await.len();
    sqlx::query("UPDATE extend.wake_requests SET ting_sent_at = now() - interval '20 minutes' WHERE ting_sent_at > now() - interval '15 minutes'")
        .execute(&env.pool)
        .await
        .unwrap();
    let (s, deferred) = wake(&env, &scout, "globex", &d, "seventh").await;
    assert_eq!(s, 201, "never refused because of other Teams: {deferred}");
    assert_eq!(
        deferred["data"]["ting"], "pending",
        "a Silicon sees deferred as pending"
    );
    assert_eq!(ting.sent_of("device.wake_requested").await.len(), before);
    let (_, owner) = api(
        &env,
        "GET",
        &format!("/api/v1/devices/{d}/wake-requests?state=open"),
        &alice,
        None,
        None,
    )
    .await;
    let mine = owner["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["wake_id"] == deferred["data"]["wake_id"])
        .unwrap()
        .clone();
    assert_eq!(mine["ting"], "deferred");
    // The window frees: the scheduler sends it.
    sqlx::query(
        "UPDATE extend.wake_requests SET ting_sent_at = now() - interval '2 hours' WHERE ting_sent_at IS NOT NULL",
    )
    .execute(&env.pool)
    .await
    .unwrap();
    extend_service::scheduler::wake_upkeep(&env.state, &extend_service::db::World::production())
        .await
        .unwrap();
    assert_eq!(ting.sent_of("device.wake_requested").await.len(), before + 1);
}

#[tokio::test]
async fn answering_ending_and_no_gate() {
    let env = start().await;
    let ting = env.state.local_ting.clone().unwrap();
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let chef = login(&env, "si:chef").await;
    let sous = login(&env, "si:sous").await;
    let scout = login(&env, "si:scout").await;
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &["si:sous"]).await;
    grant(&env, &alice, &d, "si:scout", "globex").await;
    let a = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (d2, _cred2) = pair_another(&env, &cred, &bob, Some("acme"), &["si:chef"]).await;
    a.send(awake_frame(false, Some("locked"), None, Uuid::new_v4(), 1));
    eventually("locked", || async {
        owner_view(&env, &alice, &d).await["awake"] == false
    })
    .await;
    let (_, ws) = wake(&env, &sous, "acme", &d, "a").await;
    let (_, wg) = wake(&env, &scout, "globex", &d, "b").await;
    let (_, wc) = wake(&env, &chef, "acme", &d2, "c").await;
    // input_seen false resolves nothing; it is logged on the pairs with open requests.
    a.send(awake_frame(true, None, Some(false), Uuid::new_v4(), 1));
    eventually("woke_without_input logged", || async {
        activity(&env, &d).await.iter().any(|x| x.0 == "woke_without_input")
    })
    .await;
    assert_eq!(wake_row(&env, ws["data"]["wake_id"].as_str().unwrap()).await.0, "open");
    a.send(awake_frame(false, Some("locked"), None, Uuid::new_v4(), 1));
    // Decline: only alice's pair; the Silicons told, naming alice.
    let (s, dec) = api(
        &env,
        "POST",
        &format!("/api/v1/devices/{d}/wake-requests/answer"),
        &alice,
        None,
        Some(json!({"type": "wake_answer", "data": {"answer": "declined", "wake_ids": [ws["data"]["wake_id"]]}})),
    )
    .await;
    assert_eq!(s, 200, "{dec}");
    assert_eq!(dec["data"]["ended"].as_array().unwrap().len(), 1);
    assert_eq!(
        wake_row(&env, ws["data"]["wake_id"].as_str().unwrap()).await.0,
        "declined"
    );
    let declined = ting.sent_of("device.wake_declined").await;
    assert_eq!(
        (declined[0]["for"].as_str(), declined[0]["actor"].as_str()),
        (Some("si:sous"), Some("c:alice"))
    );
    assert!(
        declined[0]["data"]["summary"]
            .as_str()
            .unwrap()
            .contains("c:alice turned down")
    );
    // A wake id not on the pair: 422.
    let (s, _) = api(
        &env,
        "POST",
        &format!("/api/v1/devices/{d}/wake-requests/answer"),
        &alice,
        None,
        Some(json!({"type": "wake_answer", "data": {"answer": "declined", "wake_ids": [wc["data"]["wake_id"]]}})),
    )
    .await;
    assert_eq!(s, 422);
    // "It's awake": every open request on the device, both pairs and both Teams, named no one.
    let (s, woke) = api(
        &env,
        "POST",
        &format!("/api/v1/devices/{d}/wake-requests/answer"),
        &alice,
        None,
        Some(json!({"type": "wake_answer", "data": {"answer": "woken"}})),
    )
    .await;
    assert_eq!(s, 200, "{woke}");
    assert_eq!(
        woke["data"]["ended"].as_array().unwrap().len(),
        1,
        "only alice's own pair's are listed"
    );
    for w in [&wg, &wc] {
        let row = wake_row(&env, w["data"]["wake_id"].as_str().unwrap()).await;
        assert_eq!(
            (row.0.as_str(), row.1.as_deref()),
            ("woken", Some("confirmed_by_carbon"))
        );
    }
    let woken = ting.sent_of("device.woken").await;
    let chef_t = woken.iter().find(|t| t["for"] == "si:chef").unwrap();
    assert_eq!(chef_t["data"]["woken_by"], "carbon");
    assert_eq!(chef_t["actor"], "c:bob", "on bob's pair: bob's held login");
    let scout_t = woken.iter().find(|t| t["for"] == "si:scout").unwrap();
    assert_eq!(
        (scout_t["actor"].as_str(), scout_t["org_id"].as_str()),
        (Some("c:alice"), Some("globex"))
    );
    assert!(!chef_t.to_string().contains("c:alice"));
    let log2 = activity(&env, &d2).await;
    let confirmed = log2.iter().find(|x| x.0 == "wake_confirmed").unwrap();
    assert_eq!(
        confirmed.1, "extend",
        "another Carbon's pair doesn't name the answering Carbon"
    );
    // Nothing open: 409.
    let (s, _) = api(
        &env,
        "POST",
        &format!("/api/v1/devices/{d}/wake-requests/answer"),
        &alice,
        None,
        Some(json!({"type": "wake_answer", "data": {"answer": "woken"}})),
    )
    .await;
    assert_eq!(s, 409);

    // Session start: unknown awake withdraws the starter's request; not awake keeps it. (sous's
    // declined request was less than 5 minutes ago; asking again waits for that.)
    sqlx::query("UPDATE extend.wake_requests SET last_asked_at = now() - interval '6 minutes'")
        .execute(&env.pool)
        .await
        .unwrap();
    let (_, w) = wake(&env, &sous, "acme", &d, "again").await;
    let wid = w["data"]["wake_id"].as_str().unwrap().to_owned();
    let (s, sess) = session(&env, &sous, "acme", &d).await;
    assert_eq!(s, 201, "no gate: a session starts on a device that isn't awake");
    assert_eq!(wake_row(&env, &wid).await.0, "open");
    // No gate: commands run; a failing one carries the wake hint with --team.
    let sid = sess["data"]["session_id"].as_str().unwrap().to_owned();
    a.fail_commands.store(true, std::sync::atomic::Ordering::Relaxed);
    let (s, r) = api(
        &env,
        "POST",
        &format!("/api/v1/sessions/{sid}/commands"),
        &sous,
        Some("acme"),
        Some(json!({"type": "command", "data": {"command": "screenshot", "args": []}})),
    )
    .await;
    assert_eq!(s, 200, "{r}");
    let warnings = r["data"]["warnings"].to_string();
    assert!(
        warnings.contains("isn't awake (locked") && warnings.contains("extend --team acme device wake"),
        "{warnings}"
    );
    let (s, r) = api(
        &env,
        "POST",
        &format!("/api/v1/sessions/{sid}/commands"),
        &sous,
        Some("acme"),
        Some(json!({"type": "command", "data": {"command": "terminal", "args": ["run", "ls"]}})),
    )
    .await;
    assert_eq!(s, 422);
    assert!(r["data"]["hint"].as_str().unwrap().contains("isn't awake"), "{r}");
    let _ = api(
        &env,
        "POST",
        &format!("/api/v1/sessions/{sid}/end"),
        &sous,
        Some("acme"),
        None,
    )
    .await;
    // Cancel, revoke, unpair: each withdraws with its reason.
    let (s, _) = api(
        &env,
        "DELETE",
        &format!("/api/v1/devices/{d}/wake-requests/{wid}"),
        &sous,
        Some("acme"),
        None,
    )
    .await;
    assert_eq!(s, 204);
    assert_eq!(wake_row(&env, &wid).await.1.as_deref(), Some("cancelled"));
    let (s, _) = api(
        &env,
        "DELETE",
        &format!("/api/v1/devices/{d}/wake-requests/{wid}"),
        &sous,
        Some("acme"),
        None,
    )
    .await;
    assert_eq!(s, 409);
    let (_, w) = wake(&env, &scout, "globex", &d, "revoke me").await;
    let _ = api(
        &env,
        "DELETE",
        &format!("/api/v1/devices/{d}/access/si:scout?team=globex"),
        &alice,
        None,
        None,
    )
    .await;
    assert_eq!(
        wake_row(&env, w["data"]["wake_id"].as_str().unwrap())
            .await
            .1
            .as_deref(),
        Some("access_removed")
    );
    let (_, w) = wake(&env, &chef, "acme", &d2, "unpair me").await;
    let _ = api(&env, "DELETE", &format!("/api/v1/devices/{d2}"), &bob, None, None).await;
    assert_eq!(
        wake_row(&env, w["data"]["wake_id"].as_str().unwrap())
            .await
            .1
            .as_deref(),
        Some("device_removed")
    );
    // Expiry (sous cancelled less than 5 minutes ago: aged first).
    sqlx::query("UPDATE extend.wake_requests SET last_asked_at = now() - interval '6 minutes'")
        .execute(&env.pool)
        .await
        .unwrap();
    let (_, w) = wake(&env, &sous, "acme", &d, "expire me").await;
    sqlx::query("UPDATE extend.wake_requests SET expires_at = now() - interval '1 second' WHERE wake_id = $1::uuid")
        .bind(w["data"]["wake_id"].as_str().unwrap())
        .execute(&env.pool)
        .await
        .unwrap();
    extend_service::scheduler::wake_upkeep(&env.state, &extend_service::db::World::production())
        .await
        .unwrap();
    assert_eq!(
        wake_row(&env, w["data"]["wake_id"].as_str().unwrap()).await.0,
        "expired"
    );
}

#[tokio::test]
async fn a_concurrent_ask_never_stays_open_on_an_awake_device() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let sous = login(&env, "si:sous").await;
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &["si:sous"]).await;
    let a = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let run = Uuid::new_v4();
    for i in 0..8u64 {
        a.send(awake_frame(false, Some("screen_off"), None, run, 2 * i + 1));
        eventually("asleep", || async {
            owner_view(&env, &alice, &d).await["awake"] == false
        })
        .await;
        sqlx::query("UPDATE extend.wake_requests SET last_asked_at = now() - interval '10 minutes'")
            .execute(&env.pool)
            .await
            .unwrap();
        let (env_ref, sous_ref, d_ref) = (&env, sous.clone(), d.clone());
        let ask = async move { wake(env_ref, &sous_ref, "acme", &d_ref, "race").await };
        a.send(awake_frame(true, None, Some(true), run, 2 * i + 2));
        let _ = ask.await;
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let open: i64 = sqlx::query_scalar("SELECT count(*) FROM extend.wake_requests WHERE state = 'open'")
            .fetch_one(&env.pool)
            .await
            .unwrap();
        assert_eq!(open, 0, "round {i}");
    }
}

// ───────────── 15: Ting consent, types and retries ─────────────

#[tokio::test]
async fn ting_types_and_registrations_per_team() {
    let env = start().await;
    let ting = env.state.local_ting.clone().unwrap();
    let alice = login(&env, "c:alice").await;
    let scout = login(&env, "si:scout").await;
    let (d, _) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &[]).await;
    // Pairing registered alice in acme; granting in globex registers her there.
    grant(&env, &alice, &d, "si:scout", "globex").await;
    eventually("alice registered in globex", || async {
        ting.registered
            .lock()
            .await
            .iter()
            .any(|(_, t, m)| t == "globex" && m == "c:alice")
    })
    .await;
    // Ting doesn't know wake_requested in globex: recorded, shown with the command.
    ting.set_missing("globex", "device.wake_requested", true);
    let (s, w) = wake(&env, &scout, "globex", &d, "need it").await;
    assert_eq!(s, 201, "{w}");
    let (_, owner) = api(
        &env,
        "GET",
        &format!("/api/v1/devices/{d}/wake-requests"),
        &alice,
        None,
        None,
    )
    .await;
    let err = owner["data"]["items"][0]["ting_last_error"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        err.contains("ting --org globex types register --type extend.device.wake_requested"),
        "{err}"
    );
    let (_, reg) = api(&env, "GET", "/api/v1/ting-registration?team=globex", &alice, None, None).await;
    assert_eq!(reg["data"]["missing_types"], json!(["extend.device.wake_requested"]));
    assert_eq!(reg["data"]["status"], "on");
    // A Carbon who manages Ting types there registers them by opening their settings.
    ting.type_managers
        .lock()
        .unwrap()
        .insert(("globex".into(), "c:alice".into()));
    let (_, reg) = api(&env, "GET", "/api/v1/ting-registration?team=globex", &alice, None, None).await;
    assert_eq!(reg["data"]["missing_types"], json!([]));
    // A later success in that Team clears the record too.
    ting.set_missing("globex", "device.wake_requested", true);
    sqlx::query("INSERT INTO extend.ting_type_status (team, ting_type, missing_since, last_checked_at) VALUES ('globex', 'extend.device.wake_requested', now(), now()) ON CONFLICT DO NOTHING")
        .execute(&env.pool)
        .await
        .unwrap();
    ting.set_missing("globex", "device.wake_requested", false);
    sqlx::query("UPDATE extend.wake_requests SET ting_next_at = now() - interval '1 second'")
        .execute(&env.pool)
        .await
        .unwrap();
    extend_service::scheduler::wake_upkeep(&env.state, &extend_service::db::World::production())
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM extend.ting_type_status")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
    // team=any lists every Team her login reaches.
    let (_, all) = api(&env, "GET", "/api/v1/ting-registration?team=any", &alice, None, None).await;
    let teams: Vec<&str> = all["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["team"].as_str().unwrap())
        .collect();
    assert_eq!(teams, vec!["acme", "globex"]);
    // A Team the login doesn't reach: 403.
    let (s, _) = api(&env, "GET", "/api/v1/ting-registration?team=labs", &alice, None, None).await;
    assert_eq!(s, 403);
}

#[tokio::test]
async fn ting_consent_is_respected() {
    let env = start().await;
    let ting = env.state.local_ting.clone().unwrap();
    ting.require_registration
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let sous = login(&env, "si:sous").await;
    let chef = login(&env, "si:chef").await;
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &["si:sous"]).await;
    let _a = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (d2, _) = pair_another(&env, &cred, &bob, Some("acme"), &["si:chef"]).await;
    let _ = session(&env, &sous, "acme", &d).await;
    // alice turned Extend off in Ting: a refusal after she was registered reads "off".
    eventually("alice registered", || async {
        ting.registered.lock().await.iter().any(|(_, _, m)| m == "c:alice")
    })
    .await;
    ting.registered.lock().await.retain(|(_, _, m)| m != "c:alice");
    let (_, r) = api(
        &env,
        "POST",
        &format!("/api/v1/devices/{d2}/requests"),
        &chef,
        Some("acme"),
        Some(json!({"type": "request", "data": {"reason": "please"}})),
    )
    .await;
    assert_eq!(r["data"]["delivery"], "pending");
    let (_, reg) = api(&env, "GET", "/api/v1/ting-registration?team=acme", &alice, None, None).await;
    assert_eq!(reg["data"]["status"], "off", "{reg}");
    // A second grant in the same Team doesn't re-register her.
    grant(&env, &alice, &d, "si:chef", "acme").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!ting.registered.lock().await.iter().any(|(_, _, m)| m == "c:alice"));
    // "Turn on" does, twice in one process, and the waiting request goes.
    for _ in 0..2 {
        let (s, reg) = api(&env, "PUT", "/api/v1/ting-registration?team=acme", &alice, None, None).await;
        assert_eq!((s, reg["data"]["status"].as_str()), (200, Some("on")), "{reg}");
    }
    let rid = r["data"]["request_id"].as_str().unwrap().to_owned();
    eventually("the request delivered after Turn on", || async {
        ting.sent_of("device.requested")
            .await
            .iter()
            .any(|t| t["key"] == rid.as_str())
    })
    .await;
    // A Carbon never registered in a Team: pending with the sign-in text, never "off".
    let carol = login(&env, "c:carol").await;
    let (_, reg) = api(&env, "GET", "/api/v1/ting-registration?team=globex", &carol, None, None).await;
    assert_eq!(
        (reg["data"]["status"].as_str(), reg["data"]["last_error"].as_str()),
        (Some("pending"), Some("Sign in to Extend for globex"))
    );
}

#[tokio::test]
async fn a_restart_rebuilds_who_is_waited_for_and_1_0_rows_retry_in_their_shape() {
    let state = state_only().await;
    let world = extend_service::db::World::production();
    sqlx::query("INSERT INTO extend.devices (device_id, team, owner_id, name, os, state) VALUES ('0a1b2c3d', 'acme', 'c:alice', 'Pixel', 'android', 'ready')")
        .execute(&state.pool)
        .await
        .unwrap();
    // A row as 1.0.0 inserts it (no 1.1 columns set; the trigger fills the holder columns).
    sqlx::query(
        "INSERT INTO extend.requests (request_id, device_id, team, from_id, to_id, session_id, reason) VALUES ($1, '0a1b2c3d', 'acme', 'si:sous', 'si:chef', 'a3f', 'from 1.0')",
    )
    .bind(Uuid::now_v7())
    .execute(&state.pool)
    .await
    .unwrap();
    extend_service::scheduler::rebuild_waiting(&state, &world).await;
    assert!(
        state
            .waiting_logins
            .read()
            .await
            .contains(&("extend".into(), "si:sous".into()))
    );
    let s = state.iam.login("si:sous", "k", None).await.unwrap();
    state.authorize(&s.access_token, Some("acme"), None).await.unwrap();
    extend_service::scheduler::retry_requests(&state, &world).await.unwrap();
    let ting = state.local_ting.clone().unwrap();
    let sent = ting.sent_of("device.requested").await;
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0]["data"]["end_session"], "extend session end a3f",
        "the exact 1.0 shape"
    );
    assert!(sent[0]["data"].get("routed_to").is_none());
}

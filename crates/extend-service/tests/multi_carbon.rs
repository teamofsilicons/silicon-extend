//! 1.1: devices belong to the Carbons who paired them; several Carbons can pair one device; each
//! Carbon sees only their own side; requests for a device in use go to the holder or to its
//! Carbon; carried devices are recognised across Carbons; computers several Carbons paired rotate
//! their credentials and keep the terminal for the installing Carbon's Silicons; a Carbon's
//! logout ends their side (test_plan 1–9, Carbon decisions 1–3, setup retry).

mod common;

use common::*;
use extend_protocol::model::{SetupStep, StepStatus};
use extend_protocol::{DeviceOs, REQUEST_TO_HIDDEN};
use serde_json::{Value, json};
use uuid::Uuid;

// ───────────── 1–3: user-scoped devices, grants across Teams, views ─────────────

#[tokio::test]
async fn configured_devices_need_an_explicit_binding_in_each_organization() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let (d, _) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &[]).await;
    let (_, other) = api(&env, "GET", "/api/v1/devices", &alice, Some("globex"), None).await;
    assert!(other["data"]["items"].as_array().unwrap().is_empty());
    let (_, current) = api(&env, "GET", "/api/v1/devices", &alice, Some("acme"), None).await;
    assert_eq!(current["data"]["items"][0]["team"], "acme");
    let (_, team) = api(&env, "GET", "/api/v1/devices?scope=team", &bob, Some("acme"), None).await;
    assert_eq!(team["data"]["items"].as_array().unwrap().len(), 1);
    let (status, _) = api(
        &env,
        "DELETE",
        &format!("/api/v1/devices/{d}"),
        &bob,
        Some("acme"),
        None,
    )
    .await;
    assert_eq!(status, 403, "organization discovery does not confer management");
    grant(&env, &alice, &d, "si:scout", "globex").await;
    let (_, imported) = api(&env, "GET", "/api/v1/devices", &alice, Some("globex"), None).await;
    assert_eq!(imported["data"]["items"][0]["device_id"], d);
    assert_eq!(imported["data"]["items"][0]["team"], "globex");
}

#[tokio::test]
async fn grants_and_revocation_are_per_organization() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let scout = login(&env, "si:scout").await;
    let chef = login(&env, "si:chef").await;
    let (d, _) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &[]).await;
    grant(&env, &alice, &d, "si:sous", "acme").await;
    grant(&env, &alice, &d, "si:scout", "globex").await;
    grant(&env, &alice, &d, "si:chef", "acme").await;
    let (_, g) = api(&env, "GET", "/api/v1/devices", &scout, Some("globex"), None).await;
    assert_eq!(g["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(g["data"]["items"][0]["team"], "globex");
    let (_, g) = api(&env, "GET", "/api/v1/devices", &chef, Some("globex"), None).await;
    assert!(g["data"]["items"].as_array().unwrap().is_empty());
    let (s, _) = api(
        &env,
        "GET",
        &format!("/api/v1/devices/{d}"),
        &chef,
        Some("globex"),
        None,
    )
    .await;
    assert_eq!(
        s, 200,
        "organization-wide devices are discoverable without a control grant"
    );
    let (s, _) = api(
        &env,
        "PUT",
        &format!("/api/v1/devices/{d}/access/si:chef"),
        &alice,
        Some("labs"),
        None,
    )
    .await;
    assert_eq!(s, 403);
    let (s, _) = api(
        &env,
        "PUT",
        &format!("/api/v1/devices/{d}/access/si:sous"),
        &alice,
        Some("globex"),
        None,
    )
    .await;
    assert_eq!(s, 422);
    let (_, list) = api(
        &env,
        "GET",
        &format!("/api/v1/devices/{d}/access"),
        &alice,
        Some("acme"),
        None,
    )
    .await;
    let rows = list["data"]["items"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|g| g["team"] == "acme"));
    grant(&env, &alice, &d, "si:chef", "globex").await;
    for team in ["globex", "acme"] {
        let (s, _) = api(
            &env,
            "DELETE",
            &format!("/api/v1/devices/{d}/access/si:chef"),
            &alice,
            Some(team),
            None,
        )
        .await;
        assert_eq!(s, 204);
        let n: i64 =
            sqlx::query_scalar("SELECT count(*) FROM extend.device_access WHERE device_id=$1 AND silicon_id='si:chef'")
                .bind(&d)
                .fetch_one(&env.pool)
                .await
                .unwrap();
        assert_eq!(n, if team == "globex" { 1 } else { 0 });
    }
    let revoked: Vec<_> = activity(&env, &d)
        .await
        .into_iter()
        .filter(|a| a.0 == "access_revoked")
        .collect();
    assert_eq!(revoked.len(), 2);
    assert!(revoked.iter().any(|a| a.3.as_deref() == Some("globex")));
    assert!(revoked.iter().any(|a| a.3.as_deref() == Some("acme")));
}

#[tokio::test]
async fn each_viewer_sees_their_own_side() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let sous = login(&env, "si:sous").await;
    let scout = login(&env, "si:scout").await;
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &["si:sous"]).await;
    grant(&env, &alice, &d, "si:scout", "globex").await;
    let _app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (s, sess) = session(&env, &sous, "acme", &d).await;
    assert_eq!(s, 201, "{sess}");
    let (_, owner) = api(&env, "GET", &format!("/api/v1/devices/{d}"), &alice, None, None).await;
    assert_eq!(owner["data"]["in_use"]["team"], "acme");
    assert_eq!(owner["data"]["in_use"]["silicon_id"], "si:sous");
    assert_eq!(owner["data"]["paired_by_others"], false);
    assert_eq!(owner["data"]["engine_version"], "0.21.15");
    assert_eq!(owner["data"]["agent_device_version"], "0.21.15");
    let (_, other) = api(
        &env,
        "GET",
        &format!("/api/v1/devices/{d}"),
        &scout,
        Some("globex"),
        None,
    )
    .await;
    assert!(other["data"].get("in_use").is_none(), "{other}");
    assert_eq!(other["data"]["in_use_by_other"], true);
    assert!(other["data"].get("paired_by_others").is_none() && other["data"].get("last_used_at").is_none());
    let (_, own) = api(&env, "GET", &format!("/api/v1/devices/{d}"), &sous, Some("acme"), None).await;
    assert_eq!(own["data"]["in_use"]["silicon_id"], "si:sous");
    // Another side's start names nobody.
    let (s, e) = session(&env, &scout, "globex", &d).await;
    assert_eq!((s, e["data"]["code"].as_str()), (409, Some("device_in_use")));
    assert_eq!(e["data"]["details"], json!({"in_use": {"hidden": true}}), "{e}");
    let sid = sess["data"]["session_id"].as_str().unwrap();
    for field in ["message", "hint"] {
        let text = e["data"][field].as_str().unwrap();
        assert!(!text.contains("si:sous"), "{e}");
        // Session IDs can be only three characters; an unrelated request UUID or public device
        // ID can contain the same substring. Check human text tokens and the exact details shape.
        assert!(
            !text.split(|c: char| !c.is_ascii_alphanumeric()).any(|word| word == sid),
            "{e}"
        );
    }
    assert!(
        e["data"]["hint"]
            .as_str()
            .unwrap()
            .contains("extend --team globex request send")
    );
}

// ───────────── 4–6: several Carbons on one device ─────────────

#[tokio::test]
async fn another_carbon_pairs_the_same_device() {
    let env = start_with(extend_service::config::Tuning {
        max_pairs_per_device: 3,
        ..Default::default()
    })
    .await;
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let carol = login(&env, "c:carol").await;
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &[]).await;
    let _app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (d2, cred2) = pair_another(&env, &cred, &bob, Some("acme"), &[]).await;
    assert_ne!(d, d2);
    assert_eq!(instance_of(&env, &d).await, instance_of(&env, &d2).await);
    // D2 is ready at once (copied from D).
    let (_, v2) = api(&env, "GET", &format!("/api/v1/devices/{d2}"), &bob, None, None).await;
    assert_eq!(v2["data"]["state"], "ready", "{v2}");
    assert_eq!(v2["data"]["paired_by_others"], true);
    // D's log says another Carbon paired it, naming no one.
    let log = activity(&env, &d).await;
    let row = log
        .iter()
        .find(|a| a.0 == "another_carbon_paired")
        .expect("another_carbon_paired");
    assert_eq!(row.2, json!({}));
    assert!(!format!("{log:?}").contains("c:bob"));
    let paired = activity(&env, &d2).await;
    assert_eq!(paired[0].2["with_existing_pairs"], true);
    // DeviceSelf: same instance, first_pair only on D.
    let (_, me1) = device_api(&env, "GET", "/api/v1/device", &cred).await;
    let (_, me2) = device_api(&env, "GET", "/api/v1/device", &cred2).await;
    assert_eq!(me1["data"]["instance_id"], me2["data"]["instance_id"]);
    assert_eq!(
        (me1["data"]["first_pair"].as_bool(), me2["data"]["first_pair"].as_bool()),
        (Some(true), Some(false))
    );
    assert!(
        me1["data"].get("hardware_salt").is_none(),
        "no salt for an Android pair"
    );
    // bob again, and alice with a code of her own device: 409, naming only their own.
    let (s, e, _) = pair_another_raw(&env, &cred, &bob, Some("acme"), &[]).await;
    assert_eq!((s, e["data"]["code"].as_str()), (409, Some("conflict")));
    assert!(e["data"]["message"].as_str().unwrap().contains(&d2), "{e}");
    let (s, e, _) = pair_another_raw(&env, &cred, &alice, Some("acme"), &[]).await;
    assert_eq!(s, 409);
    assert!(e["data"]["message"].as_str().unwrap().contains(&d), "{e}");
    // The two refused claims left their codes waiting; a device shows at most 3 at once.
    let (s, _) = device_api(&env, "POST", "/api/v1/device/enrollments", &cred).await;
    assert_eq!(s, 201);
    let (s, e) = device_api(&env, "POST", "/api/v1/device/enrollments", &cred).await;
    assert_eq!((s, e["data"]["code"].as_str()), (429, Some("rate_limited")), "{e}");
    sqlx::query("DELETE FROM extend_global.enrollments WHERE paired_device_id IS NULL")
        .execute(&env.pool)
        .await
        .unwrap();
    // Two codes claimed at once by the same Carbon: one 201, one 409, never a 500.
    let (_, e1) = device_api(&env, "POST", "/api/v1/device/enrollments", &cred).await;
    let (_, e2) = device_api(&env, "POST", "/api/v1/device/enrollments", &cred).await;
    let claim = |code: Value| {
        let (env, carol) = (&env, carol.clone());
        async move {
            api(
                env,
                "POST",
                "/api/v1/pairings",
                &carol,
                Some("globex"),
                Some(json!({"type": "pairing", "data": {"pairing_code": code, "name": "Carol's"}})),
            )
            .await
            .0
        }
    };
    let (a, b) = tokio::join!(
        claim(e1["data"]["pairing_code"].clone()),
        claim(e2["data"]["pairing_code"].clone())
    );
    let mut got = vec![a, b];
    got.sort();
    assert_eq!(got, vec![201, 409]);
    // Three Carbons now: the most this service allows.
    let (s, e) = device_api(&env, "POST", "/api/v1/device/enrollments", &cred).await;
    assert_eq!((s, e["data"]["code"].as_str()), (409, Some("conflict")));
    assert!(
        e["data"]["message"].as_str().unwrap().contains("paired to 3 Carbons"),
        "{e}"
    );
}

#[tokio::test]
async fn pairs_are_independent_and_share_activity() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let sous = login(&env, "si:sous").await;
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &["si:sous"]).await;
    let app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (d2, cred2) = pair_another(&env, &cred, &bob, Some("acme"), &["si:chef"]).await;
    let app2 = App::connect(&env, &cred2, hello(DeviceOs::Android, "1.1.0")).await;
    // bob renames his pair and sets its lifetime; alice's is unchanged.
    let (s, _) = api(
        &env,
        "PATCH",
        &format!("/api/v1/devices/{d2}"),
        &bob,
        None,
        Some(json!({"type": "device", "data": {"name": "Family phone", "pair_ttl_days": 3}})),
    )
    .await;
    assert_eq!(s, 200);
    let (_, v) = api(&env, "GET", &format!("/api/v1/devices/{d}"), &alice, None, None).await;
    assert_eq!(
        (v["data"]["name"].as_str(), v["data"]["pair_ttl_days"].as_i64()),
        (Some("Pixel"), Some(14))
    );
    // A session through D keeps D2 alive too.
    sqlx::query("UPDATE extend.devices SET last_activity_at = now() - interval '2 days' WHERE device_id = $1")
        .bind(&d2)
        .execute(&env.pool)
        .await
        .unwrap();
    let (s, sess) = session(&env, &sous, "acme", &d).await;
    assert_eq!(s, 201, "{sess}");
    let fresh: bool = sqlx::query_scalar(
        "SELECT last_activity_at > now() - interval '1 minute' FROM extend.devices WHERE device_id = $1",
    )
    .bind(&d2)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert!(fresh, "activity through any pair counts for every pair");
    // Removing alice's organization binding ends only its session; both native pairs stay.
    let (s, _) = api(&env, "DELETE", &format!("/api/v1/devices/{d}"), &alice, None, None).await;
    assert_eq!(s, 204);
    app.wait("session_ended", |_| true).await;
    let (_, sv) = api(
        &env,
        "GET",
        &format!("/api/v1/sessions/{}", sess["data"]["session_id"].as_str().unwrap()),
        &alice,
        Some("acme"),
        None,
    )
    .await;
    assert_eq!(sv["data"]["end_reason"], "access_removed");
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(!app2.is_closed(), "bob's pair keeps its connection");
    assert!(app2.of("unpaired").is_empty());
    let grants: i64 = sqlx::query_scalar("SELECT count(*) FROM extend.device_access WHERE device_id = $1")
        .bind(&d2)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(grants, 1);
    assert_eq!(device_api(&env, "GET", "/api/v1/device", &cred2).await.0, 200);
    assert_eq!(device_api(&env, "GET", "/api/v1/device", &cred).await.0, 200);
    assert!(!app.is_closed());
    assert_eq!(device_api(&env, "DELETE", "/api/v1/device", &cred).await.0, 204);
    app.wait("unpaired", |_| true).await;
    assert_eq!(device_api(&env, "GET", "/api/v1/device", &cred).await.0, 401);
}

#[tokio::test]
async fn physical_lock_is_shared_but_only_the_owning_carbon_can_stop_in_the_web_api() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let sous = login(&env, "si:sous").await;
    let chef = login(&env, "si:chef").await;
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &["si:sous"]).await;
    let _app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (d2, cred2) = pair_another(&env, &cred, &bob, Some("acme"), &["si:chef"]).await;
    let _app2 = App::connect(&env, &cred2, hello(DeviceOs::Android, "1.1.0")).await;
    let (_, sess) = session(&env, &sous, "acme", &d).await;
    let sid = sess["data"]["session_id"].as_str().unwrap().to_owned();
    let (s, e) = session(&env, &chef, "acme", &d2).await;
    assert_eq!(s, 409);
    assert!(
        !e.to_string().contains("si:sous") && !e.to_string().contains(&sid),
        "{e}"
    );
    let (_, bv) = api(&env, "GET", &format!("/api/v1/devices/{d2}"), &bob, None, None).await;
    assert_eq!(bv["data"]["in_use_by_other"], true);
    assert!(bv["data"].get("in_use").is_none());
    // A 1.0-shaped lock for D2 while D is held does nothing (the trigger and the unique index).
    sqlx::query("INSERT INTO extend.session_ids (session_id) VALUES ('fff') ON CONFLICT DO NOTHING")
        .execute(&env.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO extend.sessions (session_id, device_id, silicon_id, team, state) VALUES ('fff', $1, 'si:chef', 'acme', 'active')")
        .bind(&d2)
        .execute(&env.pool)
        .await
        .unwrap();
    let locked = sqlx::query(
        "INSERT INTO extend.device_locks (device_id, session_id) VALUES ($1, 'fff') ON CONFLICT DO NOTHING",
    )
    .bind(&d2)
    .execute(&env.pool)
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(locked, 0);
    sqlx::query("UPDATE extend.sessions SET state = 'ended' WHERE session_id = 'fff'")
        .execute(&env.pool)
        .await
        .unwrap();
    // A website owner cannot stop another Carbon's session through an alias.
    let (s, stopped) = api(&env, "POST", &format!("/api/v1/devices/{d2}/stop"), &bob, None, None).await;
    assert_eq!((s, stopped["data"]["code"].as_str()), (409, Some("device_not_in_use")));
    assert!(!stopped.to_string().contains(&sid));
    let (s, _) = api(&env, "POST", &format!("/api/v1/devices/{d}/stop"), &alice, None, None).await;
    assert_eq!(s, 200);
    let (_, sv) = api(
        &env,
        "GET",
        &format!("/api/v1/sessions/{sid}"),
        &sous,
        Some("acme"),
        None,
    )
    .await;
    assert_eq!(sv["data"]["end_reason"], "stopped_by_carbon");
    let log = activity(&env, &d).await;
    let ended = log.iter().find(|a| a.0 == "session_ended").unwrap();
    assert_eq!(ended.1, "c:alice");
    assert!(!format!("{log:?}").contains("c:bob"));
    // Nothing running: 409 device_not_in_use.
    let (s, e) = api(&env, "POST", &format!("/api/v1/devices/{d2}/stop"), &bob, None, None).await;
    assert_eq!((s, e["data"]["code"].as_str()), (409, Some("device_not_in_use")));
}

// ───────────── 7: requests routed to a side ─────────────

#[tokio::test]
async fn requests_go_to_the_holder_or_its_carbon() {
    let env = start().await;
    let ting = env.state.local_ting.clone().unwrap();
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let sous = login(&env, "si:sous").await;
    let chef = login(&env, "si:chef").await;
    let scout = login(&env, "si:scout").await;
    let (d, cred) = pair(
        &env,
        &alice,
        Some("acme"),
        DeviceOs::Android,
        "Family TV",
        &["si:sous", "si:chef"],
    )
    .await;
    grant(&env, &alice, &d, "si:scout", "globex").await;
    let _app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (d2, cred2) = pair_another(&env, &cred, &bob, Some("acme"), &["si:chef"]).await;
    let _app2 = App::connect(&env, &cred2, hello(DeviceOs::Android, "1.1.0")).await;
    let (_, sess) = session(&env, &sous, "acme", &d).await;
    let sid = sess["data"]["session_id"].as_str().unwrap().to_owned();
    let send = |token: String, team: &'static str, device: String, reason: &'static str| {
        let env = &env;
        async move {
            api(
                env,
                "POST",
                &format!("/api/v1/devices/{device}/requests"),
                &token,
                Some(team),
                Some(json!({"type": "request", "data": {"reason": reason}})),
            )
            .await
        }
    };

    // Same side: chef through D in acme asks sous (D, acme): as in 1.0.
    let (s, r) = send(chef.clone(), "acme", d.clone(), "same side").await;
    assert_eq!(s, 201, "{r}");
    assert_eq!(
        (r["data"]["to"].as_str(), r["data"]["routed_to"].as_str()),
        (Some("si:sous"), Some("holder"))
    );
    assert_eq!(r["data"]["session_id"].as_str(), Some(sid.as_str()));

    // chef through bob's pair: routed to alice. They share acme, so the Ting goes as chef.
    let (s, r) = send(chef.clone(), "acme", d2.clone(), "other carbon").await;
    assert_eq!(s, 201, "{r}");
    assert_eq!(r["data"]["to"], REQUEST_TO_HIDDEN);
    assert_eq!(r["data"]["to_hidden"], true);
    assert!(r["data"].get("session_id").is_none());
    let t = ting.sent_of("device.requested").await;
    let last = t.last().unwrap();
    assert_eq!(
        (last["org_id"].as_str(), last["actor"].as_str(), last["for"].as_str()),
        (Some("acme"), Some("si:chef"), Some("c:alice"))
    );
    assert_eq!(
        last["data"]["from"], "si:chef",
        "the Carbon sees the asking Silicon (Carbon decision 2)"
    );
    assert_eq!(
        last["data"]["device_id"].as_str(),
        Some(d.as_str()),
        "alice's own id for the device"
    );
    for gone in ["session_id", "end_session", "team"] {
        assert!(last["data"].get(gone).is_none(), "{gone}: {last}");
    }
    let rid = r["data"]["request_id"].as_str().unwrap().to_owned();
    // bob's view: to hidden. alice's view: on her pair, from chef, with the holder's session.
    let (_, bv) = api(&env, "GET", &format!("/api/v1/devices/{d2}/requests"), &bob, None, None).await;
    let bi = bv["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["request_id"] == rid.as_str())
        .unwrap()
        .clone();
    assert_eq!(
        (bi["to"].as_str(), bi["to_hidden"].as_bool()),
        (Some(REQUEST_TO_HIDDEN), Some(true))
    );
    let (_, av) = api(
        &env,
        "GET",
        &format!("/api/v1/devices/{d}/requests"),
        &alice,
        None,
        None,
    )
    .await;
    let ai = av["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["request_id"] == rid.as_str())
        .unwrap()
        .clone();
    assert_eq!(
        (ai["from"].as_str(), ai["device_id"].as_str(), ai["session_id"].as_str()),
        (Some("si:chef"), Some(d.as_str()), Some(sid.as_str()))
    );
    assert!(
        ai.get("team").is_none(),
        "another Carbon's Silicon: its Team isn't shown"
    );
    // Activity: request_sent on D2 without the holder's session; request_received on D.
    let sent = activity(&env, &d2)
        .await
        .into_iter()
        .find(|a| a.0 == "request_sent")
        .unwrap();
    assert!(sent.4.is_none());
    assert_eq!(sent.2["to"], REQUEST_TO_HIDDEN);
    let got = activity(&env, &d)
        .await
        .into_iter()
        .find(|a| a.0 == "request_received")
        .unwrap();
    assert_eq!(
        (got.2["from"].as_str(), got.4.as_deref()),
        (Some("si:chef"), Some(sid.as_str()))
    );

    // scout (D, globex): the request is delivered to the holder in acme, without cross-org session details.
    let (s, r) = send(scout.clone(), "globex", d.clone(), "own carbon").await;
    assert_eq!(s, 201, "{r}");
    assert_eq!(r["data"]["to"], REQUEST_TO_HIDDEN);
    let last = ting.sent_of("device.requested").await.last().unwrap().clone();
    assert_eq!(
        (last["org_id"].as_str(), last["actor"].as_str(), last["for"].as_str()),
        (Some("acme"), Some("c:alice"), Some("c:alice"))
    );
    assert!(!last["data"]["summary"].as_str().unwrap().contains("si:sous"));
    assert!(last["data"].get("team").is_none());
    let rid = r["data"]["request_id"].as_str().unwrap().to_owned();
    let (_, av) = api(
        &env,
        "GET",
        &format!("/api/v1/devices/{d}/requests"),
        &alice,
        Some("globex"),
        None,
    )
    .await;
    let ai = av["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["request_id"] == rid.as_str())
        .unwrap()
        .clone();
    assert_eq!(
        (ai["from"].as_str(), ai["team"].as_str()),
        (Some("si:scout"), Some("globex"))
    );
    assert!(
        ai.get("session_id").is_none(),
        "source organization cannot see the holder's session"
    );
    assert_eq!(ai["to_hidden"], true);
    let (_, mine) = api(&env, "GET", "/api/v1/requests", &scout, Some("globex"), None).await;
    let mi = &mine["data"]["items"][0];
    assert_eq!((mi["to_hidden"].as_bool(), mi.get("session_id")), (Some(true), None));

    // The 60 s repeat check is per Team: the same reason in globex is a repeat, not in acme.
    let (s2, again) = send(scout.clone(), "globex", d.clone(), "own carbon").await;
    assert_eq!((s2, again["data"]["request_id"].as_str()), (200, Some(rid.as_str())));
}

#[tokio::test]
async fn a_routed_ting_waits_for_the_carbons_login() {
    let env = start().await;
    let ting = env.state.local_ting.clone().unwrap();
    let alice = login(&env, "c:alice").await;
    let carol = login(&env, "c:carol").await;
    let sous = login(&env, "si:sous").await;
    let scout = login(&env, "si:scout").await;
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Family TV", &["si:sous"]).await;
    let _app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (d3, _) = pair_another(&env, &cred, &carol, Some("globex"), &["si:scout"]).await;
    let (_, _) = session(&env, &sous, "acme", &d).await;
    // Extend holds no login for alice now; scout's doesn't reach acme: the Ting waits.
    env.state.auth_cache.forget(&["c:alice".into()]).await;
    let (s, r) = api(
        &env,
        "POST",
        &format!("/api/v1/devices/{d3}/requests"),
        &scout,
        Some("globex"),
        Some(json!({"type": "request", "data": {"reason": "Need the TV"}})),
    )
    .await;
    assert_eq!(s, 201, "{r}");
    assert_eq!(
        (r["data"]["delivery"].as_str(), r["data"]["last_error"].as_str()),
        (Some("pending"), Some("Not delivered yet; it is retried."))
    );
    assert!(
        ting.sent_of("device.requested").await.is_empty(),
        "never as the holder or the asker"
    );
    // alice's next call sends it, from her own login, to herself, in the holder's Team.
    let _ = api(&env, "GET", "/api/v1/devices", &alice, None, None).await;
    let rid = r["data"]["request_id"].as_str().unwrap().to_owned();
    eventually("the routed Ting going at alice's call", || async {
        ting.sent_of("device.requested")
            .await
            .iter()
            .any(|t| t["key"] == rid.as_str())
    })
    .await;
    let t = ting.sent_of("device.requested").await.last().unwrap().clone();
    assert_eq!(
        (t["actor"].as_str(), t["for"].as_str(), t["org_id"].as_str()),
        (Some("c:alice"), Some("c:alice"), Some("acme"))
    );
    assert!(t["data"].get("team").is_none());
    // Ting refusing self-sends: the chain skips it, and it fails with why (and stays listed).
    ting.refuse_self_sends.store(true, std::sync::atomic::Ordering::Relaxed);
    let (_, r2) = api(
        &env,
        "POST",
        &format!("/api/v1/devices/{d3}/requests"),
        &scout,
        Some("globex"),
        Some(json!({"type": "request", "data": {"reason": "Another reason"}})),
    )
    .await;
    let rid2 = r2["data"]["request_id"].as_str().unwrap().to_owned();
    let (_, av) = api(
        &env,
        "GET",
        &format!("/api/v1/devices/{d}/requests"),
        &alice,
        None,
        None,
    )
    .await;
    let ai = av["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["request_id"] == rid2.as_str())
        .unwrap()
        .clone();
    assert_eq!(ai["delivery"], "pending");
    assert!(ai["last_error"].as_str().is_some(), "{ai}");
}

// ───────────── 8: carried devices ─────────────

fn attached(device_id: &str, key: Option<&str>) -> Value {
    let mut v = json!({"type": "attached", "device_id": device_id, "online": true, "capabilities": ["input.remote", "nav.system"],
        "missing": [], "setup": {"state": "complete", "steps": []}});
    if let Some(k) = key {
        v["hardware_key"] = json!(k);
    }
    v
}

async fn attach(env: &Env, token: &str, host: &str, name: &str) -> String {
    let (s, d) = api(
        env,
        "POST",
        &format!("/api/v1/devices/{host}/attachments"),
        token,
        None,
        Some(json!({"type": "attachment", "data": {"os": "tvos", "name": name}})),
    )
    .await;
    assert_eq!(s, 201, "attaching {name}: {d}");
    d["data"]["device_id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn carried_devices_are_recognised_across_carbons() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let chef = login(&env, "si:chef").await;
    let sous = login(&env, "si:sous").await;
    let (ma, cma) = pair(
        &env,
        &alice,
        Some("acme"),
        DeviceOs::Macos,
        "Studio Mac",
        &["si:sous", "si:chef"],
    )
    .await;
    let host_a = App::connect(&env, &cma, hello(DeviceOs::Macos, "1.1.0")).await;
    let (mb, cmb) = pair_another(&env, &cma, &bob, Some("acme"), &[]).await;
    let host_b = App::connect(&env, &cmb, hello(DeviceOs::Macos, "1.1.0")).await;
    // Computer pairs get the world's hardware salt, the same for both.
    let (_, sa) = device_api(&env, "GET", "/api/v1/device", &cma).await;
    let (_, sb) = device_api(&env, "GET", "/api/v1/device", &cmb).await;
    assert!(sa["data"]["hardware_salt"].as_str().is_some_and(|s| s.len() >= 32));
    assert_eq!(sa["data"]["hardware_salt"], sb["data"]["hardware_salt"]);
    let ca = attach(&env, &alice, &ma, "Living room").await;
    let cb = attach(&env, &bob, &mb, "Family TV").await;
    assert_ne!(instance_of(&env, &ca).await, instance_of(&env, &cb).await);
    // A key reported by a socket that doesn't carry the device is ignored.
    host_a.send(attached(&cb, Some("aa11")));
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let key: Option<String> = sqlx::query_scalar("SELECT hardware_key FROM extend.devices WHERE device_id = $1")
        .bind(&cb)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert!(key.is_none());
    // Both hosts report the same key: bob's pair joins alice's instance.
    host_a.send(attached(&ca, Some("aa11")));
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    host_b.send(attached(&cb, Some("aa11")));
    eventually("the two pairs becoming one device", || async {
        instance_of(&env, &ca).await == instance_of(&env, &cb).await
    })
    .await;
    assert!(activity(&env, &cb).await.iter().any(|a| a.0 == "device_linked"));
    // The same Carbon adding it twice: the later one is refused, naming the first.
    let ca2 = attach(&env, &alice, &ma, "Living room again").await;
    host_a.send(attached(&ca2, Some("aa11")));
    eventually("the duplicate refused", || async {
        let (_, v) = api(&env, "GET", &format!("/api/v1/devices/{ca2}/setup"), &alice, None, None).await;
        v["data"]["steps"][0]["key"] == "duplicate_device"
    })
    .await;
    let (_, st) = api(&env, "GET", &format!("/api/v1/devices/{ca2}/setup"), &alice, None, None).await;
    assert!(st["data"]["steps"][0]["error"].as_str().unwrap().contains(&ca), "{st}");
    let (_, v) = api(&env, "GET", &format!("/api/v1/devices/{ca2}"), &alice, None, None).await;
    assert_eq!(v["data"]["state"], "setup");
    // Through another computer (carol's laptop): refused, naming no one; alice's keeps working.
    let carol = login(&env, "c:carol").await;
    let (l, cl) = pair(&env, &carol, Some("globex"), DeviceOs::Macos, "Laptop", &[]).await;
    let laptop = App::connect(&env, &cl, hello(DeviceOs::Macos, "1.1.0")).await;
    let cln = attach(&env, &carol, &l, "TV via laptop").await;
    laptop.send(attached(&cln, Some("aa11")));
    eventually("the other computer's pair refused", || async {
        let (_, v) = api(&env, "GET", &format!("/api/v1/devices/{cln}/setup"), &carol, None, None).await;
        v["data"]["steps"][0]["key"] == "duplicate_device"
    })
    .await;
    let (_, st) = api(&env, "GET", &format!("/api/v1/devices/{cln}/setup"), &carol, None, None).await;
    let text = st["data"]["steps"][0]["error"].as_str().unwrap().to_owned();
    assert!(
        text.contains("through another computer") && !text.contains("c:alice") && !text.contains(&ma),
        "{text}"
    );
    let (_, v) = api(&env, "GET", &format!("/api/v1/devices/{ca}"), &alice, None, None).await;
    assert_eq!(v["data"]["state"], "ready");

    // Lock group: alice's acme Silicon on the Mac; a globex Silicon of alice on the TV is refused;
    // the same Team and Carbon is allowed.
    grant(&env, &alice, &ca, "si:chef", "globex").await;
    grant(&env, &alice, &ca, "si:sous", "acme").await;
    let (s, mac_session) = session(&env, &chef, "acme", &ma).await;
    assert_eq!(s, 201, "{mac_session}");
    let (s, e) = session(&env, &chef, "globex", &ca).await;
    assert_eq!((s, e["data"]["code"].as_str()), (409, Some("device_in_use")), "{e}");
    let (s, tv) = session(&env, &sous, "acme", &ca).await;
    assert_eq!(s, 201, "same Team and Carbon: {tv}");
    let _ = api(
        &env,
        "POST",
        &format!(
            "/api/v1/sessions/{}/end",
            mac_session["data"]["session_id"].as_str().unwrap()
        ),
        &chef,
        Some("acme"),
        None,
    )
    .await;
    // bob's stop of his Mac pair can't end alice's Silicon on alice's TV pair... unless bob paired
    // that TV too (he did: cb is the same device). Unlink it first to test the carried 409.
    sqlx::query("UPDATE extend.devices SET instance_id = gen_random_uuid() WHERE device_id = $1")
        .bind(&cb)
        .execute(&env.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO extend.device_instances (instance_id) SELECT instance_id FROM extend.devices WHERE device_id = $1 ON CONFLICT DO NOTHING").bind(&cb).execute(&env.pool).await.unwrap();
    let (s, e) = api(&env, "POST", &format!("/api/v1/devices/{mb}/stop"), &bob, None, None).await;
    assert_eq!((s, e["data"]["code"].as_str()), (409, Some("conflict")), "{e}");
    assert!(e["data"]["message"].as_str().unwrap().contains("carried by"), "{e}");
    let (_, bv) = api(&env, "GET", &format!("/api/v1/devices/{mb}"), &bob, None, None).await;
    assert_eq!(
        (
            bv["data"]["in_use_by_other"].as_bool(),
            bv["data"]["in_use_by_other_carried"].as_bool()
        ),
        (Some(true), Some(true))
    );
    // The Mac's own Stop, from bob's pair's connection, ends it.
    host_b.send(json!({"type": "stop"}));
    let tv_sid = tv["data"]["session_id"].as_str().unwrap().to_owned();
    eventually("the carried session stopped from the computer", || async {
        let (_, sv) = api(
            &env,
            "GET",
            &format!("/api/v1/sessions/{tv_sid}"),
            &sous,
            Some("acme"),
            None,
        )
        .await;
        sv["data"]["end_reason"] == "stopped_by_carbon"
    })
    .await;
}

// ───────────── 9: computers several Carbons paired ─────────────

#[tokio::test]
async fn shared_computers_rotate_credentials_and_keep_the_terminal_for_the_first_pair() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let chef = login(&env, "si:chef").await;
    let sous = login(&env, "si:sous").await;
    let (m, cm) = pair(&env, &alice, Some("acme"), DeviceOs::Macos, "Family Mac", &["si:chef"]).await;
    let app_a = App::connect(&env, &cm, hello(DeviceOs::Macos, "1.1.0")).await;
    // One Carbon: the terminal works and nothing rotates.
    let (_, sess) = session(&env, &chef, "acme", &m).await;
    let sid = sess["data"]["session_id"].as_str().unwrap().to_owned();
    let (_, v) = api(&env, "GET", &format!("/api/v1/devices/{m}"), &chef, Some("acme"), None).await;
    assert!(
        v["data"]["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("terminal"))
    );
    let _ = api(
        &env,
        "POST",
        &format!("/api/v1/sessions/{sid}/end"),
        &chef,
        Some("acme"),
        None,
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        app_a.of("credential").is_empty(),
        "a computer one Carbon paired is never rotated"
    );

    let (mb, cmb) = pair_another(&env, &cm, &bob, Some("acme"), &["si:sous"]).await;
    let app_b = App::connect(&env, &cmb, hello(DeviceOs::Macos, "1.1.0")).await;
    // Carbon decision 3: bob's Silicons get no terminal; alice's (the first pair's) keep it.
    let (_, vb) = api(&env, "GET", &format!("/api/v1/devices/{mb}"), &sous, Some("acme"), None).await;
    assert!(
        !vb["data"]["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("terminal")),
        "{vb}"
    );
    let missing = vb["data"]["missing"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["capability"] == "terminal")
        .unwrap()
        .clone();
    assert_eq!(missing["reason"], extend_protocol::TERMINAL_NOT_SHARED_REASON);
    let (_, va) = api(&env, "GET", &format!("/api/v1/devices/{m}"), &chef, Some("acme"), None).await;
    assert!(
        va["data"]["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("terminal"))
    );
    // Enforced on commands too.
    let (s, sb) = session(&env, &sous, "acme", &mb).await;
    assert_eq!(s, 201, "{sb}");
    let sbid = sb["data"]["session_id"].as_str().unwrap().to_owned();
    let (s, e) = api(
        &env,
        "POST",
        &format!("/api/v1/sessions/{sbid}/commands"),
        &sous,
        Some("acme"),
        Some(json!({"type": "command", "data": {"command": "terminal", "args": ["run", "whoami"]}})),
    )
    .await;
    assert_eq!(
        (s, e["data"]["code"].as_str()),
        (422, Some("unsupported_on_device")),
        "{e}"
    );
    assert!(
        e["data"]["message"]
            .as_str()
            .unwrap()
            .contains("installed Silicon Extend"),
        "{e}"
    );
    let (s, ok) = api(
        &env,
        "POST",
        &format!("/api/v1/sessions/{sbid}/commands"),
        &sous,
        Some("acme"),
        Some(json!({"type": "command", "data": {"command": "screenshot", "args": []}})),
    )
    .await;
    assert_eq!(s, 200, "screen commands work: {ok}");
    // The session ends: both pairs get a new credential on their own connection.
    app_a.clear();
    app_b.clear();
    let _ = api(
        &env,
        "POST",
        &format!("/api/v1/sessions/{sbid}/end"),
        &sous,
        Some("acme"),
        None,
    )
    .await;
    let new_a = app_a.wait("credential", |_| true).await["device_credential"]
        .as_str()
        .unwrap()
        .to_owned();
    let new_b = app_b.wait("credential", |_| true).await["device_credential"]
        .as_str()
        .unwrap()
        .to_owned();
    // The old ones work until the app confirms.
    assert_eq!(device_api(&env, "GET", "/api/v1/device", &cm).await.0, 200);
    app_a.send(json!({"type": "credential_saved"}));
    eventually("alice's old credential stopping", || async {
        device_api(&env, "GET", "/api/v1/device", &cm).await.0 == 401
    })
    .await;
    assert_eq!(device_api(&env, "GET", "/api/v1/device", &new_a).await.0, 200);
    // bob's pair: a connection with the new credential promotes it.
    assert_eq!(device_api(&env, "GET", "/api/v1/device", &new_b).await.0, 200);
    assert_eq!(device_api(&env, "GET", "/api/v1/device", &cmb).await.0, 401);
    // A second socket for bob's pair while the first answered a ping: logged on bob's pair.
    app_b.send(json!({"type": "pong", "nonce": 1}));
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let _b2 = App::connect(&env, &new_b, hello(DeviceOs::Macos, "1.1.0")).await;
    eventually("connection_replaced logged", || async {
        activity(&env, &mb).await.iter().any(|a| a.0 == "connection_replaced")
    })
    .await;
    assert!(!activity(&env, &m).await.iter().any(|a| a.0 == "connection_replaced"));
}

// ───────────── Carbon decision 1: logout ─────────────

#[tokio::test]
async fn a_carbons_logout_ends_only_their_side() {
    let env = start().await;
    let alice = env.client.login("c:alice").await.unwrap();
    let bob = login(&env, "c:bob").await;
    let sous = login(&env, "si:sous").await;
    let chef = login(&env, "si:chef").await;
    let (d, cred) = pair(
        &env,
        &alice.access_token,
        Some("acme"),
        DeviceOs::Android,
        "Pixel",
        &["si:sous"],
    )
    .await;
    let _app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (other, cred_o) = pair(&env, &bob, Some("acme"), DeviceOs::Android, "Bob's phone", &["si:chef"]).await;
    let _app_o = App::connect(&env, &cred_o, hello(DeviceOs::Android, "1.1.0")).await;
    let (_, s1) = session(&env, &sous, "acme", &d).await;
    let (_, s2) = session(&env, &chef, "acme", &other).await;
    env.client
        .logout(&alice.refresh_token, Some(&alice.access_token))
        .await
        .unwrap();
    let id1 = s1["data"]["session_id"].as_str().unwrap().to_owned();
    let (_, v1) = api(
        &env,
        "GET",
        &format!("/api/v1/sessions/{id1}"),
        &sous,
        Some("acme"),
        None,
    )
    .await;
    assert_eq!(v1["data"]["end_reason"], "access_removed");
    let id2 = s2["data"]["session_id"].as_str().unwrap().to_owned();
    let (_, v2) = api(
        &env,
        "GET",
        &format!("/api/v1/sessions/{id2}"),
        &chef,
        Some("acme"),
        None,
    )
    .await;
    assert_eq!(v2["data"]["state"], "active", "another Carbon's side goes on");
    // The grant stays: the Silicon can start again.
    let (s, _) = session(&env, &sous, "acme", &d).await;
    assert_eq!(s, 201);
}

// ───────────── Contract A: setup retry ─────────────

#[tokio::test]
async fn setup_retry_asks_the_device_to_run_failed_steps_again() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let failed = extend_protocol::model::Setup::from_steps(vec![
        SetupStep {
            key: "accessibility".into(),
            title: "Allow control".into(),
            status: StepStatus::Done,
            help: None,
            error: None,
            input: None,
        },
        SetupStep {
            key: "wireless_debugging".into(),
            title: "Turn on wireless debugging".into(),
            status: StepStatus::Failed,
            help: None,
            error: Some("The phone turned down the pairing. Tap Retry on it.".into()),
            input: None,
        },
    ]);
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &["si:chef"]).await;
    let retry = |token: String, body: Option<Value>| {
        let (env, d) = (&env, d.clone());
        async move {
            api(
                env,
                "POST",
                &format!("/api/v1/devices/{d}/setup/retry"),
                &token,
                None,
                body,
            )
            .await
        }
    };
    // A 1.0 app can't retry from here: 426.
    let old = App::connect(&env, &cred, hello_with(DeviceOs::Android, "1.0.2", failed.clone())).await;
    let (s, e) = retry(alice.clone(), None).await;
    assert_eq!((s, e["data"]["code"].as_str()), (426, Some("upgrade_required")), "{e}");
    assert!(
        e["data"]["message"]
            .as_str()
            .unwrap()
            .contains("runs Silicon Extend 1.0.2"),
        "{e}"
    );
    drop(old);
    // Offline: 409 device_offline.
    eventually("the app going offline", || async {
        !env.state.hub.is_connected(&("extend".into(), d.clone())).await
    })
    .await;
    let (s, e) = retry(alice.clone(), None).await;
    assert_eq!((s, e["data"]["code"].as_str()), (409, Some("device_offline")), "{e}");
    assert!(
        e["data"]["message"]
            .as_str()
            .unwrap()
            .contains("Setup carries on when it reconnects")
    );
    let app = App::connect(&env, &cred, hello_with(DeviceOs::Android, "1.1.0", failed)).await;
    // Silicons can't.
    let (s, e) = retry(chef.clone(), None).await;
    assert_eq!((s, e["data"]["code"].as_str()), (403, Some("carbon_only")));
    // Unknown step: 400 listing the steps; a step that didn't fail: 409.
    let (s, e) = retry(alice.clone(), Some(json!({"step": "nope"}))).await;
    assert_eq!((s, e["data"]["code"].as_str()), (400, Some("invalid_input")));
    assert!(
        e["data"]["message"]
            .as_str()
            .unwrap()
            .contains("accessibility, wireless_debugging"),
        "{e}"
    );
    let (s, e) = retry(alice.clone(), Some(json!({"step": "accessibility"}))).await;
    assert_eq!((s, e["data"]["code"].as_str()), (409, Some("conflict")));
    // Every failed step, with an empty body; then within 5 s: 429.
    let (s, r) = retry(alice.clone(), None).await;
    assert_eq!((s, r["type"].as_str()), (202, Some("setup_retry")), "{r}");
    assert_eq!(r["data"]["retrying"], json!(["wireless_debugging"]));
    let f = app.wait("setup_retry", |_| true).await;
    assert_eq!((f["target"].clone(), f["step"].clone()), (Value::Null, Value::Null));
    let (s, e) = retry(
        alice.clone(),
        Some(json!({"type": "setup_retry", "data": {"step": "wireless_debugging"}})),
    )
    .await;
    assert_eq!((s, e["data"]["code"].as_str()), (429, Some("rate_limited")));
    assert!(e["data"]["details"]["retry_after_s"].as_i64().is_some());
    // Nothing failed any more: 409.
    app.send(json!({"type": "setup_progress", "setup": {"state": "complete", "steps": []}}));
    tokio::time::sleep(std::time::Duration::from_millis(5_100)).await;
    let (s, e) = retry(alice.clone(), None).await;
    assert_eq!(
        (s, e["data"]["message"].as_str()),
        (409, Some("Nothing to retry: no setup step has failed."))
    );
    let _ = Uuid::nil();
}

#[tokio::test]
async fn a_carbons_logout_by_iam_event_ends_their_side() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let sous = login(&env, "si:sous").await;
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &["si:sous"]).await;
    let _app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (_, s) = session(&env, &sous, "acme", &d).await;
    let sid = s["data"]["session_id"].as_str().unwrap().to_owned();
    // IAM: alice logged out everywhere (the dev stand-in revokes her tokens and sends the event).
    let r = reqwest::Client::new()
        .post(format!("{}/dev/iam/members", env.base))
        .json(&json!({"type": "member", "data": {"id": "c:alice", "teams": ["acme", "globex"], "revoke": true}}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    let (_, v) = api(
        &env,
        "GET",
        &format!("/api/v1/sessions/{sid}"),
        &sous,
        Some("acme"),
        None,
    )
    .await;
    assert_eq!(v["data"]["end_reason"], "access_removed");
}

#[tokio::test]
async fn team_silicons_across_every_team() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let (s, v) = api(&env, "GET", "/api/v1/team/silicons?team=any", &alice, None, None).await;
    assert_eq!(s, 200, "{v}");
    let items = v["data"]["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["id"] == "si:scout" && i["team"] == "globex"));
    assert!(items.iter().any(|i| i["id"] == "si:sous" && i["team"] == "acme"));
    let teams: Vec<&str> = v["data"]["teams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["team"].as_str().unwrap())
        .collect();
    assert_eq!(teams, vec!["acme", "globex"]);
    // Without team: 1.0 behaviour, in X-Org-ID.
    let (_, one) = api(&env, "GET", "/api/v1/team/silicons", &alice, Some("globex"), None).await;
    assert!(one["data"].get("teams").is_none());
    assert!(
        one["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["id"] != "si:sous")
    );
}

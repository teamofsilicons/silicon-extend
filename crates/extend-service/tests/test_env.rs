//! 1.1 in test environments: the device limit counts physical devices (a second Carbon's pair of
//! one never counts, nor a carried pair still waiting to be recognised); a carried pair accepted
//! over the limit is removed when it isn't recognised; a clean leaves nothing and forgets which
//! Silicons Ting knew (test_plan 17).

mod common;

use common::*;
use extend_protocol::{DeviceOs, TEST_DEVICE_LIMIT_MESSAGE};
use extend_service::db::World;
use serde_json::json;

async fn count(env: &Env, envid: uuid::Uuid) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(DISTINCT instance_id) FROM {}.devices WHERE removed_at IS NULL AND provisional_until IS NULL",
        World::test(envid).schema
    )))
    .fetch_one(&env.pool)
    .await
    .unwrap()
}

fn attached(device_id: &str, key: &str) -> serde_json::Value {
    json!({"type": "attached", "device_id": device_id, "online": true, "capabilities": ["input.remote"], "missing": [],
        "setup": {"state": "complete", "steps": []}, "hardware_key": key})
}

#[tokio::test]
async fn the_limit_counts_physical_devices() {
    let env = start().await;
    let (envid, secret) = open_test_env(&env).await;
    let alice = login_in(&env, &secret, "c:alice").await;
    let bob = login_in(&env, &secret, "c:bob").await;
    let (mac, mac_cred) = pair_in(&env, &secret, &alice, DeviceOs::Macos, "Mac").await.unwrap();
    let host_a = App::connect(&env, &mac_cred, hello(DeviceOs::Macos, "1.1.0")).await;
    for i in 0..3 {
        pair_in(&env, &secret, &alice, DeviceOs::Android, &format!("Phone {i}"))
            .await
            .unwrap();
    }
    // bob pairs the Mac too ("Pair with another Carbon", into the Mac's environment): no new device.
    let (s, e) = device_api(&env, "POST", "/api/v1/device/enrollments", &mac_cred).await;
    assert_eq!(s, 201, "{e}");
    let (s, d) = api_in(
        &env,
        &secret,
        "POST",
        "/api/v1/pairings",
        Some(&bob),
        Some("acme"),
        Some(json!({"type": "pairing", "data": {"pairing_code": e["data"]["pairing_code"], "name": "bob's Mac"}})),
    )
    .await;
    assert_eq!(s, 201, "{d}");
    assert_eq!(d["data"]["paired_by_others"], true);
    let mac_b = d["data"]["device_id"].as_str().unwrap().to_owned();
    let (_, st) = api_in(
        &env,
        &secret,
        "GET",
        &format!("/api/v1/enrollments/{}", e["data"]["enrollment_id"].as_str().unwrap()),
        Some(&format!(
            "Extend-Enrollment {}",
            e["data"]["enrollment_secret"].as_str().unwrap()
        )),
        None,
        None,
    )
    .await;
    assert_eq!(
        st["data"]["environment"]["paired_devices"], 4,
        "counted after the claim: still 4 devices"
    );
    let host_b = App::connect(
        &env,
        st["data"]["device_credential"].as_str().unwrap(),
        hello(DeviceOs::Macos, "1.1.0"),
    )
    .await;
    assert_eq!(count(&env, envid).await, 4);
    // A production claim of this environment's code is refused (its world).
    let (s, e2) = device_api(&env, "POST", "/api/v1/device/enrollments", &mac_cred).await;
    assert_eq!(s, 201);
    let prod = login(&env, "c:carol").await;
    let (s, _) = api(
        &env,
        "POST",
        "/api/v1/pairings",
        &prod,
        Some("globex"),
        Some(json!({"type": "pairing", "data": {"pairing_code": e2["data"]["pairing_code"], "name": "x"}})),
    )
    .await;
    assert_eq!(
        s, 404,
        "a code made for a test environment doesn't pair into production"
    );
    // alice's Apple TV through her pair of the Mac: the 5th device.
    let attach = |token: String, host: String| {
        let (env, secret) = (&env, secret.clone());
        async move {
            api_in(
                env,
                &secret,
                "POST",
                &format!("/api/v1/devices/{host}/attachments"),
                Some(&token),
                None,
                Some(json!({"type": "attachment", "data": {"os": "tvos", "name": "TV"}})),
            )
            .await
        }
    };
    let (s, tv_a) = attach(alice.clone(), mac.clone()).await;
    assert_eq!(s, 201, "{tv_a}");
    let tv_a = tv_a["data"]["device_id"].as_str().unwrap().to_owned();
    assert_eq!(count(&env, envid).await, 5);
    // A new phone is refused at the limit.
    let (s, e) = pair_in(&env, &secret, &alice, DeviceOs::Android, "Too many")
        .await
        .unwrap_err();
    assert_eq!(
        (s, e["data"]["message"].as_str()),
        (409, Some(TEST_DEVICE_LIMIT_MESSAGE))
    );
    // bob adds the TV alice already carries through the same Mac: accepted provisionally, counts
    // nothing, and once the Mac recognises it, it is the same device.
    let (s, tv_b) = attach(bob.clone(), mac_b.clone()).await;
    assert_eq!(s, 201, "{tv_b}");
    let tv_b = tv_b["data"]["device_id"].as_str().unwrap().to_owned();
    let (_, setup) = api_in(
        &env,
        &secret,
        "GET",
        &format!("/api/v1/devices/{tv_b}/setup"),
        Some(&bob),
        None,
        None,
    )
    .await;
    assert_eq!(setup["data"]["steps"][0]["key"], "recognising");
    assert_eq!(count(&env, envid).await, 5);
    host_a.send(attached(&tv_a, "cc33"));
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    host_b.send(attached(&tv_b, "cc33"));
    let schema = World::test(envid).schema;
    eventually("recognised", || async {
        sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(format!(
            "SELECT a.instance_id = b.instance_id AND b.provisional_until IS NULL FROM {schema}.devices a, {schema}.devices b
             WHERE a.device_id = '{tv_a}' AND b.device_id = '{tv_b}'")))
            .fetch_one(&env.pool)
            .await
            .unwrap()
    })
    .await;
    assert_eq!(count(&env, envid).await, 5);
    // One never recognised is removed after the window, with the limit's message.
    let (s, tv_x) = attach(bob.clone(), mac_b.clone()).await;
    assert_eq!(s, 201, "{tv_x}");
    let tv_x = tv_x["data"]["device_id"].as_str().unwrap().to_owned();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {schema}.devices SET provisional_until = now() - interval '1 second' WHERE device_id = $1"
    )))
    .bind(&tv_x)
    .execute(&env.pool)
    .await
    .unwrap();
    extend_service::scheduler::remove_unlinked(&env.state, &World::test(envid))
        .await
        .unwrap();
    let (reason, details): (Option<String>, serde_json::Value) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT d.removed_reason, a.details FROM {schema}.devices d JOIN {schema}.activity a ON a.device_id = d.device_id AND a.action = 'removed'
         WHERE d.device_id = $1")))
        .bind(&tv_x)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(reason.as_deref(), Some("device_removed"));
    assert_eq!(
        (details["reason"].as_str(), details["message"].as_str()),
        (Some("test_device_limit"), Some(TEST_DEVICE_LIMIT_MESSAGE))
    );
}

#[tokio::test]
async fn a_clean_leaves_nothing_and_forgets_ting_registrations() {
    let env = start().await;
    let ting = env.state.local_ting.clone().unwrap();
    let (envid, secret) = open_test_env(&env).await;
    let alice = login_in(&env, &secret, "c:alice").await;
    let chef = login_in(&env, &secret, "si:chef").await;
    let (d, cred) = pair_in(&env, &secret, &alice, DeviceOs::Android, "Phone")
        .await
        .unwrap();
    let (s, _) = api_in(
        &env,
        &secret,
        "PUT",
        &format!("/api/v1/devices/{d}/access/si:chef?team=acme"),
        Some(&alice),
        None,
        None,
    )
    .await;
    assert_eq!(s, 200);
    let app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    app.send(awake_frame(false, Some("screen_off"), None, uuid::Uuid::new_v4(), 1));
    let (s, _) = api_in(
        &env,
        &secret,
        "POST",
        &format!("/api/v1/devices/{d}/wake-requests"),
        Some(&chef),
        Some("acme"),
        Some(json!({"type": "wake_request", "data": {"reason": "before the clean"}})),
    )
    .await;
    assert_eq!(s, 201);
    let (s, _) = api_in(
        &env,
        &secret,
        "POST",
        "/api/v1/sessions",
        Some(&chef),
        Some("acme"),
        Some(json!({"type": "session", "data": {"device_id": d}})),
    )
    .await;
    assert_eq!(s, 201);
    eventually("chef registered in the environment", || async {
        ting.registered
            .lock()
            .await
            .iter()
            .any(|(e, _, m)| *e == Some(envid) && m == "si:chef")
    })
    .await;
    lifecycle(&env, envid, "clean", 2, 2).await;
    assert!(
        !ting.registered.lock().await.iter().any(|(e, _, _)| *e == Some(envid)),
        "a clean forgets the environment's registrations"
    );
    let schema = World::test(envid).schema;
    for table in [
        "devices",
        "device_instances",
        "wake_requests",
        "device_access",
        "sessions",
        "ting_recipients",
        "membership_checks",
    ] {
        let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {schema}.{table}")))
            .fetch_one(&env.pool)
            .await
            .unwrap();
        assert_eq!(n, 0, "{table} after the clean");
    }
    let salt: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM {schema}.world_settings"
    )))
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(salt, 1, "the world's own settings stay");
}

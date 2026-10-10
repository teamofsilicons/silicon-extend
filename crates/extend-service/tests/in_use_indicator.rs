//! 1.1: the in-use banner. One setting per physical device, shared by every pair of it; a Carbon
//! who paired it changes it through their pair, the device's own app through any of its pair
//! credentials, and a Silicon never. Every live connection of the device re-reads it; a computer
//! carrying the device is sent its `attach` again. Each side logs only its own change.

mod common;

use common::*;
use extend_protocol::DeviceOs;
use serde_json::{Value, json};

/// A call with a device credential and a body.
async fn device_patch(env: &Env, credential: &str, body: Value) -> (u16, Value) {
    device_patch_path(env, credential, "/api/v1/device", body).await
}

async fn device_patch_path(env: &Env, credential: &str, path: &str, body: Value) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .patch(format!("{}{path}", env.base))
        .header("authorization", format!("Extend-Device {credential}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn host_app_can_only_change_a_device_carried_by_its_authenticated_pair() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let (mac, credential) = pair(&env, &alice, DeviceOs::Macos, "Mac", &[]).await;
    let (_, bob_credential) = pair_another(&env, &credential, &bob, &[]).await;
    let host = App::connect(&env, &credential, hello(DeviceOs::Macos, "1.1.0")).await;
    let (s, created) = api(
        &env,
        "POST",
        &format!("/api/v2/devices/{mac}/attachments"),
        &alice,
        Some(json!({"type":"attachment", "data":{"os":"ipados", "name":"iPad"}})),
    )
    .await;
    assert_eq!(s, 201, "{created}");
    let ipad = created["data"]["device_id"].as_str().unwrap();
    let path = format!("/api/v1/device/attachments/{ipad}");
    host.wait("attach", |f| f["device_id"] == ipad).await;
    host.clear();
    let (s, body) = device_patch_path(&env, &credential, &path, banner("hidden")).await;
    assert_eq!(s, 200, "{body}");
    assert_eq!(body["data"]["device_id"], ipad);
    assert_eq!(body["data"]["in_use_indicator"], "hidden");
    assert_eq!(indicator(&env, &alice, ipad).await.0, "hidden");
    assert_eq!(indicator(&env, &alice, &mac).await.0, "shown");
    assert_eq!(
        host.wait("attach", |f| f["device_id"] == ipad).await["in_use_indicator"],
        "hidden"
    );

    for (credential, path) in [
        (bob_credential.as_str(), path.as_str()),
        (credential.as_str(), &format!("/api/v1/device/attachments/{mac}")),
        (credential.as_str(), "/api/v1/device/attachments/00000000"),
    ] {
        let (s, body) = device_patch_path(&env, credential, path, banner("shown")).await;
        assert_eq!(s, 404, "{body}");
        assert_eq!(body["data"]["code"], "device_not_found");
    }
    let (s, _) = device_patch_path(&env, "edc_not_valid", &path, banner("shown")).await;
    assert_eq!(s, 401);
    let (s, _) = device_patch_path(&env, &credential, &path, banner("dimmed")).await;
    assert_eq!(s, 422);
    assert_eq!(indicator(&env, &alice, ipad).await.0, "hidden");
    let (s, body) = api(&env, "DELETE", &format!("/api/v2/devices/{ipad}"), &alice, None).await;
    assert_eq!(s, 204, "{body}");
    // Removing the carried device unpairs it: nothing can change it any more.
    let (s, body) = device_patch_path(&env, &credential, &path, banner("shown")).await;
    assert_eq!(
        (s, body["data"]["code"].as_str()),
        (404, Some("device_not_found")),
        "{body}"
    );
}

fn banner(value: &str) -> Value {
    json!({"type": "device", "data": {"in_use_indicator": value}})
}

async fn indicator(env: &Env, token: &str, device_id: &str) -> (String, i64) {
    let (s, v) = api(env, "GET", &format!("/api/v2/devices/{device_id}"), token, None).await;
    assert_eq!(s, 200, "{v}");
    (
        v["data"]["in_use_indicator"].as_str().unwrap_or("absent").to_owned(),
        v["data"]["version"].as_i64().unwrap(),
    )
}

type ActivityRow = (String, String, Value, Option<String>);

fn banner_rows(log: &[ActivityRow]) -> Vec<(String, String, Value)> {
    log.iter()
        .filter(|a| a.0.starts_with("banner_"))
        .map(|a| (a.0.clone(), a.1.clone(), a.2.clone()))
        .collect()
}

#[tokio::test]
async fn one_setting_per_device_changed_by_its_carbons_or_its_app() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let bob = login(&env, "c:bob").await;
    let chef = login(&env, "si:chef").await;
    let (d, cred) = pair(&env, &alice, DeviceOs::Android, "Pixel", &["si:chef"]).await;
    let app_a = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (d2, cred2) = pair_another(&env, &cred, &bob, &[]).await;
    let app_b = App::connect(&env, &cred2, hello(DeviceOs::Android, "1.1.0")).await;

    // Shown by default, everywhere it is read.
    assert_eq!(indicator(&env, &alice, &d).await.0, "shown");
    assert_eq!(indicator(&env, &chef, &d).await.0, "shown");
    let (_, me) = device_api(&env, "GET", "/api/v1/device", &cred).await;
    assert_eq!(me["data"]["in_use_indicator"], "shown");

    // A Silicon may not change it, even with access.
    let (s, e) = api(
        &env,
        "PATCH",
        &format!("/api/v2/devices/{d}"),
        &chef,
        Some(banner("hidden")),
    )
    .await;
    assert_eq!((s, e["data"]["code"].as_str()), (403, Some("carbon_only")), "{e}");
    // Only "shown" or "hidden".
    let (s, e) = api(
        &env,
        "PATCH",
        &format!("/api/v2/devices/{d}"),
        &alice,
        Some(banner("dimmed")),
    )
    .await;
    assert_eq!((s, e["data"]["code"].as_str()), (422, Some("invalid_input")), "{e}");

    // alice hides it: one setting, so bob's pair and the device app read it hidden too.
    app_a.clear();
    app_b.clear();
    let (_, v2_before) = indicator(&env, &bob, &d2).await;
    let (s, v) = api(
        &env,
        "PATCH",
        &format!("/api/v2/devices/{d}"),
        &alice,
        Some(banner("hidden")),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["data"]["in_use_indicator"], "hidden");
    let (b_value, v2_after) = indicator(&env, &bob, &d2).await;
    assert_eq!(b_value, "hidden");
    assert!(v2_after > v2_before, "bob's pair's ETag changed with it");
    assert_eq!(indicator(&env, &chef, &d).await.0, "hidden");
    let (_, me2) = device_api(&env, "GET", "/api/v1/device", &cred2).await;
    assert_eq!(me2["data"]["in_use_indicator"], "hidden");
    // Every live connection of the device re-reads it.
    app_a.wait("refresh", |_| true).await;
    app_b.wait("refresh", |_| true).await;
    // alice's log says she hid it; bob's says nothing, and never names her.
    assert_eq!(
        banner_rows(&activity(&env, &d).await),
        vec![(
            "banner_hidden".into(),
            uuid("c:alice"),
            json!({"in_use_indicator": "hidden"})
        )]
    );
    let bob_log = activity(&env, &d2).await;
    assert!(banner_rows(&bob_log).is_empty(), "{bob_log:?}");
    assert!(!format!("{bob_log:?}").contains("c:alice") && !format!("{bob_log:?}").contains(&uuid("c:alice")));

    // The same value again changes and logs nothing.
    app_a.clear();
    let (s, _) = api(
        &env,
        "PATCH",
        &format!("/api/v2/devices/{d}"),
        &alice,
        Some(banner("hidden")),
    )
    .await;
    assert_eq!(s, 200);
    assert_eq!(banner_rows(&activity(&env, &d).await).len(), 1);
    // A rename (what a 1.0 website or CLI sends) leaves it alone.
    let (s, v) = api(
        &env,
        "PATCH",
        &format!("/api/v2/devices/{d}"),
        &alice,
        Some(json!({"type": "device", "data": {"name": "Alice's Pixel"}})),
    )
    .await;
    assert_eq!(
        (s, v["data"]["in_use_indicator"].as_str()),
        (200, Some("hidden")),
        "{v}"
    );

    // The device's own app shows it again, with bob's pair credential and a bare body: the
    // device logs it on every pair, naming no Carbon.
    app_a.clear();
    app_b.clear();
    let (s, v) = device_patch(&env, &cred2, json!({"in_use_indicator": "shown"})).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["type"], "device_self");
    assert_eq!(v["data"]["in_use_indicator"], "shown");
    assert_eq!(indicator(&env, &alice, &d).await.0, "shown");
    app_a.wait("refresh", |_| true).await;
    app_b.wait("refresh", |_| true).await;
    for pair in [&d, &d2] {
        let rows = banner_rows(&activity(&env, pair).await);
        let last = rows.last().expect("a banner row");
        assert_eq!(
            (last.0.as_str(), last.1.as_str(), &last.2),
            (
                "banner_shown",
                "extend",
                &json!({"in_use_indicator": "shown", "on_device": true})
            )
        );
    }
    // The envelope works as well; a body without the setting, or with an unknown value, doesn't.
    let (s, v) = device_patch(
        &env,
        &cred,
        json!({"type": "device_self", "data": {"in_use_indicator": "hidden"}}),
    )
    .await;
    assert_eq!(
        (s, v["data"]["in_use_indicator"].as_str()),
        (200, Some("hidden")),
        "{v}"
    );
    let (s, e) = device_patch(&env, &cred, json!({})).await;
    assert_eq!((s, e["data"]["code"].as_str()), (422, Some("invalid_input")), "{e}");
    let (s, e) = device_patch(&env, &cred, json!({"in_use_indicator": "dimmed"})).await;
    assert_eq!((s, e["data"]["code"].as_str()), (422, Some("invalid_input")), "{e}");
    // Only a paired device's credential.
    let (s, _) = device_patch(&env, "edc_not_a_real_credential", json!({"in_use_indicator": "shown"})).await;
    assert_eq!(s, 401);
}

#[tokio::test]
async fn a_carried_devices_banner_reaches_its_computer() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let (mac, cred) = pair(&env, &alice, DeviceOs::Macos, "Studio Mac", &[]).await;
    let host = App::connect(&env, &cred, hello(DeviceOs::Macos, "1.1.0")).await;
    let (s, d) = api(
        &env,
        "POST",
        &format!("/api/v2/devices/{mac}/attachments"),
        &alice,
        Some(json!({"type": "attachment", "data": {"os": "ios", "name": "Alice's iPhone"}})),
    )
    .await;
    assert_eq!(s, 201, "{d}");
    let phone = d["data"]["device_id"].as_str().unwrap().to_owned();
    assert_eq!(d["data"]["in_use_indicator"], "shown");
    let first = host.wait("attach", |f| f["device_id"] == phone.as_str()).await;
    assert_eq!(first["in_use_indicator"], "shown");

    host.clear();
    let (s, v) = api(
        &env,
        "PATCH",
        &format!("/api/v2/devices/{phone}"),
        &alice,
        Some(banner("hidden")),
    )
    .await;
    assert_eq!(
        (s, v["data"]["in_use_indicator"].as_str()),
        (200, Some("hidden")),
        "{v}"
    );
    let again = host.wait("attach", |f| f["device_id"] == phone.as_str()).await;
    assert_eq!(
        (again["in_use_indicator"].as_str(), again["removed"].as_bool()),
        (Some("hidden"), Some(false))
    );
    // The Mac's own banner is its own setting.
    assert_eq!(indicator(&env, &alice, &mac).await.0, "shown");
    assert!(host.of("refresh").is_empty(), "the Mac's own setting didn't change");

    // A computer that reconnects is told again.
    drop(host);
    let host = App::connect(&env, &cred, hello(DeviceOs::Macos, "1.1.0")).await;
    let greeted = host.wait("attach", |f| f["device_id"] == phone.as_str()).await;
    assert_eq!(greeted["in_use_indicator"], "hidden");
}

#[tokio::test]
async fn a_conditional_settings_patch_is_atomic() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let (d, _) = pair(&env, &alice, DeviceOs::Android, "Pixel", &[]).await;
    let (_, version) = indicator(&env, &alice, &d).await;
    let patch = |name: &'static str, indicator: &'static str| {
        reqwest::Client::new()
            .patch(format!("{}/api/v2/devices/{d}", env.base))
            .bearer_auth(&alice)
            .header("if-match", format!("\"{version}\""))
            .json(&json!({"type":"device","data":{"name":name,"in_use_indicator":indicator}}))
            .send()
    };
    let (a, b) = tokio::join!(patch("Hidden phone", "hidden"), patch("Shown phone", "shown"));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.status().is_success(), b.status().is_success());
    let winner = if a.status().is_success() { "hidden" } else { "shown" };
    let loser = if a.status().is_success() { b } else { a };
    assert!(matches!(loser.status().as_u16(), 409 | 412));
    let (_, after) = api(&env, "GET", &format!("/api/v2/devices/{d}"), &alice, None).await;
    assert_eq!(after["data"]["in_use_indicator"], winner);
    assert_eq!(
        after["data"]["name"],
        if winner == "hidden" {
            "Hidden phone"
        } else {
            "Shown phone"
        }
    );
}

//! Organization boundaries and hidden-device ACLs over the real HTTP/SQL/socket stack.
mod common;
use common::*;
use extend_protocol::{DeviceOs, model::EnrollmentCreate};
use serde_json::{Value, json};
use uuid::Uuid;

async fn mutation(env: &Env, method: &str, path: &str, token: &str, org: &str, body: Value, key: Uuid) -> (u16, Value) {
    let response = reqwest::Client::new()
        .request(method.parse().unwrap(), format!("{}{path}", env.base))
        .bearer_auth(token)
        .header("x-org-id", org)
        .header("idempotency-key", key.to_string())
        .json(&body)
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap_or(Value::Null))
}
async fn read(env: &Env, path: &str, token: &str, org: &str) -> (u16, Value) {
    api(env, "GET", path, token, Some(org), None).await
}
async fn visibility(env: &Env, id: &str, token: &str, org: &str, value: &str) {
    let (status, body) = api(
        env,
        "PATCH",
        &format!("/api/v1/devices/{id}"),
        token,
        Some(org),
        Some(json!({"type":"device", "data":{"visibility":value}})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"]["visibility"], value);
    assert_eq!(body["data"]["team"], org);
}
async fn grant_here(env: &Env, id: &str, owner: &str, silicon: &str, org: &str) {
    let (status, body) = api(
        env,
        "PUT",
        &format!("/api/v1/devices/{id}/access/{silicon}"),
        owner,
        Some(org),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn configured_devices_import_privately_and_all_operations_remain_in_the_selected_organization() {
    let env = start().await;
    let alice = login(&env, "c:alice@acme").await;
    let alice_globex = login(&env, "c:alice@globex").await;
    let bob = login(&env, "c:bob").await;
    let carol = login(&env, "c:carol").await;
    let chef = login(&env, "si:chef@acme").await;
    let chef_globex = login(&env, "si:chef@globex").await;
    let enrollment = env
        .client
        .enroll(&EnrollmentCreate {
            os: DeviceOs::Macos,
            os_version: None,
            model: Some("Fixture".into()),
            app_version: "1.1.0".into(),
            engine_version: None,
        })
        .await
        .unwrap();
    let (status, paired) = api(
        &env,
        "POST",
        "/api/v1/pairings",
        &alice,
        Some("acme"),
        Some(json!({"type":"pairing", "data":{"pairing_code":enrollment.pairing_code,"name":"Alice Mac"}})),
    )
    .await;
    assert_eq!(status, 201, "{paired}");
    assert_eq!(paired["data"]["visibility"], "personal");
    let id = paired["data"]["device_id"].as_str().unwrap();
    let credential = credential_of(&env, enrollment.enrollment_id, &enrollment.enrollment_secret).await;
    let _device = App::connect(&env, &credential, hello(DeviceOs::Macos, "1.1.0")).await;
    let path = format!("/api/v1/devices/{id}");
    let before: (String, Uuid) =
        sqlx::query_as("SELECT credential_digest, instance_id FROM extend.devices WHERE device_id=$1")
            .bind(id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    for token in [&bob, &chef] {
        for suffix in ["", "/activity", "/access", "/setup", "/requests", "/wake-requests"] {
            let (code, body) = read(&env, &format!("{path}{suffix}"), token, "acme").await;
            assert_eq!(code, 404, "hidden {suffix}: {body}");
        }
    }
    assert_eq!(
        read(&env, &path, &alice, "globex").await.0,
        403,
        "a login cannot switch orgs by changing a header"
    );
    assert_eq!(read(&env, &path, &alice_globex, "globex").await.0, 404);
    assert!(
        read(&env, "/api/v1/devices?scope=mine", &alice_globex, "globex")
            .await
            .1["data"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let importable = read(&env, "/api/v1/devices/importable", &alice_globex, "globex").await;
    assert_eq!(importable.0, 200, "{}", importable.1);
    assert_eq!(importable.1["data"]["items"][0]["device_id"], id);
    assert!(!importable.1.to_string().contains("acme"));
    assert!(
        read(&env, "/api/v1/devices/importable", &bob, "acme").await.1["data"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let payload = json!({"type":"device_import","data":{}});
    assert_eq!(
        mutation(
            &env,
            "POST",
            &format!("{path}/import"),
            &bob,
            "acme",
            payload.clone(),
            Uuid::new_v4()
        )
        .await
        .0,
        404
    );
    let key = Uuid::new_v4();
    let imported = mutation(
        &env,
        "POST",
        &format!("{path}/import"),
        &alice_globex,
        "globex",
        payload.clone(),
        key,
    )
    .await;
    assert_eq!(imported.0, 201, "{}", imported.1);
    assert_eq!(imported.1["data"]["visibility"], "personal");
    assert_eq!(imported.1["data"]["team"], "globex");
    assert_eq!(
        mutation(
            &env,
            "POST",
            &format!("{path}/import"),
            &alice_globex,
            "globex",
            payload.clone(),
            key
        )
        .await,
        imported
    );
    let after: (String, Uuid) =
        sqlx::query_as("SELECT credential_digest, instance_id FROM extend.devices WHERE device_id=$1")
            .bind(id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(
        before, after,
        "import must not re-pair or duplicate the physical device"
    );
    assert_eq!(read(&env, &path, &carol, "globex").await.0, 404);
    visibility(&env, id, &alice, "acme", "team").await;
    assert_eq!(read(&env, &path, &bob, "acme").await.0, 200);
    assert_eq!(
        read(&env, "/api/v1/devices?scope=team", &bob, "acme").await.1["data"]["items"][0]["device_id"],
        id
    );
    assert_eq!(read(&env, &format!("{path}/access"), &bob, "acme").await.0, 403);
    assert_eq!(
        session(&env, &chef, "acme", id).await.0,
        404,
        "discovery grants no control"
    );
    grant_here(&env, id, &alice, "si:chef", "acme").await;
    let running = session(&env, &chef, "acme", id).await;
    assert_eq!(running.0, 201, "{}", running.1);
    let acme_session = running.1["data"]["session_id"].as_str().unwrap();
    assert_eq!(
        read(
            &env,
            &format!("/api/v1/sessions/{acme_session}"),
            &alice_globex,
            "globex"
        )
        .await
        .0,
        404
    );
    let globex_view = read(&env, &path, &alice_globex, "globex").await.1;
    assert!(globex_view["data"]["in_use"].is_null());
    assert!(!globex_view.to_string().contains(acme_session));
    visibility(&env, id, &alice, "acme", "personal").await;
    assert_eq!(read(&env, &path, &chef, "acme").await.0, 404);
    let ended: String = sqlx::query_scalar("SELECT state FROM extend.sessions WHERE session_id=$1")
        .bind(acme_session)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(ended, "ended");
    visibility(&env, id, &alice_globex, "globex", "team").await;
    grant_here(&env, id, &alice_globex, "si:chef", "globex").await;
    let running = session(&env, &chef_globex, "globex", id).await;
    assert_eq!(running.0, 201, "{}", running.1);
    let globex_session = running.1["data"]["session_id"].as_str().unwrap();
    assert_eq!(
        read(&env, "/api/v1/sessions", &alice, "acme").await.1["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["session_id"] == globex_session)
            .count(),
        0
    );
    assert_ne!(
        api(&env, "POST", &format!("{path}/stop"), &alice, Some("acme"), None)
            .await
            .0,
        200
    );
    let (code, removed) = api(&env, "DELETE", &path, &alice, Some("acme"), None).await;
    assert_eq!(code, 204, "{removed}");
    assert!(read(&env, &path, &alice, "acme").await.1["data"]["removed_at"].is_string());
    assert_eq!(read(&env, &path, &alice_globex, "globex").await.0, 200);
    assert_eq!(device_api(&env, "GET", "/api/v1/device", &credential).await.0, 200);
    let state: String = sqlx::query_scalar("SELECT state FROM extend.sessions WHERE session_id=$1")
        .bind(globex_session)
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(state, "active", "removing one org must not stop another org's session");
    let logout = env.client.login("c:alice@acme").await.unwrap();
    let response = api(
        &env,
        "POST",
        "/api/v1/auth/logout",
        &logout.access_token,
        Some("acme"),
        Some(json!({"type":"logout","data":{"token":logout.refresh_token}})),
    )
    .await;
    assert_eq!(response.0, 204, "{}", response.1);
    assert_eq!(
        read(&env, &path, &alice_globex, "globex").await.0,
        200,
        "logout must preserve another organization login"
    );
    assert_eq!(
        read(
            &env,
            &format!("/api/v1/sessions/{globex_session}"),
            &chef_globex,
            "globex"
        )
        .await
        .0,
        200
    );
    let (_, secret) = open_test_env(&env).await;
    let test_client = silicon_extend_client::Client::builder(&env.base)
        .testing_secret(&secret)
        .connect()
        .await
        .unwrap();
    let test_token = test_client.login("c:alice").await.unwrap().access_token;
    let response = reqwest::Client::new()
        .post(format!("{}{path}/import", env.base))
        .bearer_auth(test_token)
        .header("x-org-id", "acme")
        .header("x-testing-application-secret", secret)
        .header("idempotency-key", Uuid::new_v4().to_string())
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status().as_u16(),
        404,
        "test context must never import a production device"
    );
}

#[tokio::test]
async fn migration_seeds_private_bindings_without_replacing_configurations_and_clean_removes_them() {
    use extend_service::db::{self, World};
    let (url, _) = database("org_migration").await;
    let pool = db::connect(&url).await.unwrap();
    db::migrate_global(&pool).await.unwrap();
    let world = World::test(Uuid::new_v4());
    db::ensure_world_to(&pool, &world, 6).await.unwrap();
    let sql = format!(
        "INSERT INTO {} (device_id, team, owner_id, name, os, credential_digest) VALUES ('aabb1234','acme','c:alice','Mac','macos','original-credential'); INSERT INTO {} (device_id,team,silicon_id,granted_by) VALUES ('aabb1234','globex','si:chef','c:alice')",
        world.t("devices"),
        world.t("device_access")
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql)).execute(&pool).await.unwrap();
    db::ensure_world(&pool, &world).await.unwrap();
    db::ensure_world(&pool, &world).await.unwrap();
    let bindings: Vec<(String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT org_id, visibility FROM {} ORDER BY org_id",
        world.t("device_organizations")
    )))
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        bindings,
        vec![("acme".into(), "personal".into()), ("globex".into(), "personal".into())]
    );
    let credential: String = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT credential_digest FROM {}",
        world.t("devices")
    )))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(credential, "original-credential");
    db::truncate_world(&pool, &world).await.unwrap();
    let left: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM {}",
        world.t("device_organizations")
    )))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, 0);
    pool.close().await;
    let (admin, name) = url.rsplit_once('/').unwrap();
    let admin = db::connect(&format!("{admin}/postgres")).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
async fn startup_applies_private_bindings_to_persisted_sessions_and_notifications() {
    let state = state_only().await;
    // Simulate persisted work from before schema 7, or a process interrupted after hiding.
    sqlx::raw_sql("INSERT INTO extend.devices (device_id,team,owner_id,name,os,credential_digest) VALUES ('aabb1234','acme','c:alice','Mac','macos','retained-native-credential');
        INSERT INTO extend.device_access(device_id,team,silicon_id,granted_by) VALUES ('aabb1234','acme','si:chef','c:alice');
        INSERT INTO extend.session_ids(session_id) VALUES ('abc');
        INSERT INTO extend.sessions(session_id,device_id,silicon_id,team,state) VALUES ('abc','aabb1234','si:chef','acme','active');
        INSERT INTO extend.device_locks(device_id,session_id) VALUES ('aabb1234','abc');
        INSERT INTO extend.wake_requests(wake_id,device_id,instance_id,team,from_id,to_id,reason,expires_at,wake_detectable,device_notice)
          SELECT gen_random_uuid(),device_id,instance_id,'acme','si:chef','c:alice','pending before hiding',now()+interval '30 minutes',true,'offline' FROM extend.devices WHERE device_id='aabb1234';
        INSERT INTO extend.requests(request_id,device_id,team,from_id,to_id,reason)
          VALUES (gen_random_uuid(),'aabb1234','acme','si:chef','si:sous','pending before hiding');")
        .execute(&state.pool).await.unwrap();
    let restarted = extend_service::build(state.cfg.clone()).await.unwrap();
    let ended: (String, Option<String>) =
        sqlx::query_as("SELECT state,end_reason FROM extend.sessions WHERE session_id='abc'")
            .fetch_one(&restarted.pool)
            .await
            .unwrap();
    assert_eq!(ended, ("ended".into(), Some("access_removed".into())));
    let wake: (String, Option<String>) = sqlx::query_as("SELECT state,end_reason FROM extend.wake_requests")
        .fetch_one(&restarted.pool)
        .await
        .unwrap();
    assert_eq!(wake, ("withdrawn".into(), Some("access_removed".into())));
    let delivery: String = sqlx::query_scalar("SELECT delivery FROM extend.requests")
        .fetch_one(&restarted.pool)
        .await
        .unwrap();
    assert_eq!(delivery, "failed");
    let locks: i64 = sqlx::query_scalar("SELECT count(*) FROM extend.device_locks")
        .fetch_one(&restarted.pool)
        .await
        .unwrap();
    assert_eq!(locks, 0);
    let credential: String = sqlx::query_scalar("SELECT credential_digest FROM extend.devices")
        .fetch_one(&restarted.pool)
        .await
        .unwrap();
    assert_eq!(credential, "retained-native-credential");
    let grants: i64 = sqlx::query_scalar("SELECT count(*) FROM extend.device_access")
        .fetch_one(&restarted.pool)
        .await
        .unwrap();
    assert_eq!(
        grants, 1,
        "migration leaves grants dormant until the owner explicitly shares"
    );
}

#[tokio::test]
async fn removing_a_host_includes_an_attachment_committing_while_it_waits_for_the_lock() {
    let env = start().await;
    let alice = login(&env, "c:alice@acme").await;
    let (host, _) = pair(&env, &alice, Some("acme"), DeviceOs::Macos, "Mac", &[]).await;
    let instance = instance_of(&env, &host).await;
    let mut attaching = env.pool.begin().await.unwrap();
    extend_service::domain::lock_instances(&mut attaching, &extend_service::db::World::production(), &[instance])
        .await
        .unwrap();
    sqlx::query("INSERT INTO extend.devices(device_id,team,owner_id,name,os,host_device_id) VALUES ('1a2b3c4d','acme','c:alice','TV','tvos',$1)")
        .bind(&host).execute(&mut *attaching).await.unwrap();
    let base = env.base.clone();
    let removing = tokio::spawn(async move {
        reqwest::Client::new()
            .delete(format!("{base}/api/v1/devices/{host}"))
            .bearer_auth(alice)
            .header("x-org-id", "acme")
            .send()
            .await
            .unwrap()
    });
    eventually("removal waiting for the attachment's host lock", || async {
        sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%device_instances%' AND query LIKE '%FOR NO KEY UPDATE%')")
            .fetch_one(&env.pool).await.unwrap()
    }).await;
    attaching.commit().await.unwrap();
    assert_eq!(removing.await.unwrap().status().as_u16(), 204);
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM extend.device_organizations WHERE org_id='acme' AND removed_at IS NULL",
    )
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(active, 0, "the newly committed attachment must leave with its host");
    let configured: i64 = sqlx::query_scalar("SELECT count(*) FROM extend.devices WHERE removed_at IS NULL")
        .fetch_one(&env.pool)
        .await
        .unwrap();
    assert_eq!(configured, 2, "organization removal preserves physical configuration");
}

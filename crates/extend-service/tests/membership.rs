//! 1.1: ending access on membership only when IAM gives a definite answer (test_plan 10), and
//! files across Teams (test_plan 16).

mod common;

use common::*;
use extend_protocol::DeviceOs;
use extend_service::db::World;
use extend_service::iam::ReaderMode;
use serde_json::json;

async fn grants(env: &Env, device: &str) -> Vec<(String, String)> {
    sqlx::query_as("SELECT silicon_id, team FROM extend.device_access WHERE device_id = $1 ORDER BY team, silicon_id")
        .bind(device)
        .fetch_all(&env.pool)
        .await
        .unwrap()
}

async fn end_reason(env: &Env, _token: &str, _team: &str, sid: &str) -> Option<String> {
    // Membership revocation may also remove the caller's read authority. Inspect the persisted
    // lifecycle result directly instead of reading a different organization's session.
    sqlx::query_scalar("SELECT end_reason FROM extend.sessions WHERE session_id=$1")
        .bind(sid)
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

async fn dev_member(env: &Env, id: &str, teams: &[&str]) {
    let r = reqwest::Client::new()
        .post(format!("{}/dev/iam/members", env.base))
        .json(&json!({"type": "member", "data": {"id": id, "teams": teams}}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.status());
}

#[tokio::test]
async fn leaving_a_team_ends_that_teams_access_only() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let scout = login(&env, "si:scout").await;
    let sous = login(&env, "si:sous").await;
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &["si:sous"]).await;
    grant(&env, &alice, &d, "si:scout", "globex").await;
    grant(&env, &alice, &d, "si:chef", "globex").await;
    let _app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    // scout leaves globex (webhook): its grant there goes and its session ends left_team.
    let (_, s) = session(&env, &scout, "globex", &d).await;
    let sid = s["data"]["session_id"].as_str().unwrap().to_owned();
    dev_member(&env, "si:scout", &[]).await;
    assert_eq!(
        end_reason(&env, &alice, "acme", &sid).await.as_deref(),
        Some("left_team")
    );
    assert!(!grants(&env, &d).await.contains(&("si:scout".into(), "globex".into())));
    // alice leaves globex: her globex grants go, the device and her acme grants stay.
    let chef = login(&env, "si:chef").await;
    let (_, s) = session(&env, &chef, "globex", &d).await;
    let sid = s["data"]["session_id"].as_str().unwrap().to_owned();
    dev_member(&env, "c:alice", &["acme"]).await;
    assert_eq!(
        end_reason(&env, &alice, "acme", &sid).await.as_deref(),
        Some("left_team")
    );
    assert_eq!(grants(&env, &d).await, vec![("si:sous".into(), "acme".into())]);
    let (st, _) = api(&env, "GET", &format!("/api/v1/devices/{d}"), &alice, None, None).await;
    assert_eq!(st, 200, "the device stays paired");
    let (st, _) = session(&env, &sous, "acme", &d).await;
    assert_eq!(st, 201);
}

#[tokio::test]
async fn a_missed_webhook_is_caught_at_the_next_use() {
    // Positive answers are reused for 30 s by default; here each use asks.
    let env = start_with(extend_service::config::Tuning {
        owner_check_cache_s: 0,
        ..Default::default()
    })
    .await;
    let alice = login(&env, "c:alice").await;
    let carol = login(&env, "c:carol").await; // a second reader in globex
    let chef = login(&env, "si:chef").await;
    let scout = login(&env, "si:scout").await;
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &[]).await;
    grant(&env, &alice, &d, "si:chef", "globex").await;
    grant(&env, &alice, &d, "si:scout", "globex").await;
    let _app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let _ = api(&env, "GET", "/api/v1/auth/me", &carol, Some("globex"), None).await;
    let (_, s) = session(&env, &chef, "globex", &d).await;
    let sid = s["data"]["session_id"].as_str().unwrap().to_owned();
    // IAM drops alice from globex; no event arrives.
    env.state
        .local_iam
        .as_ref()
        .unwrap()
        .set_member("c:alice", Some(vec!["acme".into()]))
        .await;
    let (st, e) = session(&env, &scout, "globex", &d).await;
    assert_eq!((st, e["data"]["code"].as_str()), (403, Some("no_access")), "{e}");
    assert!(
        e["data"]["message"]
            .as_str()
            .unwrap()
            .contains("c:alice, who gave you access to Pixel, is no longer an active member of globex")
    );
    assert!(e["data"]["hint"].as_str().unwrap().contains("Ask a Carbon in globex"));
    assert_eq!(
        end_reason(&env, &chef, "globex", &sid).await.as_deref(),
        Some("left_team")
    );
    let state: String = sqlx::query_scalar(
        "SELECT state FROM extend.membership_checks WHERE member_id = 'c:alice' AND team = 'globex'",
    )
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert_eq!(state, "gone");
    // Grant deletion commits before the asynchronous confirmation writes its activity entry.
    eventually("the grants going and their revocation being logged", || async {
        grants(&env, &d).await.is_empty()
            && activity(&env, &d)
                .await
                .iter()
                .any(|a| a.0 == "access_revoked" && a.2["reason"] == "left_team")
    })
    .await;
    assert!(grants(&env, &d).await.is_empty());
    let log = activity(&env, &d).await;
    assert!(
        log.iter()
            .any(|a| a.0 == "access_revoked" && a.2["reason"] == "left_team")
    );
}

#[tokio::test]
async fn unclear_answers_delete_nothing() {
    let env = start().await;
    let iam = env.state.local_iam.clone().unwrap();
    let alice = login(&env, "c:alice").await;
    let carol = login(&env, "c:carol").await;
    let scout = login(&env, "si:scout").await;
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &[]).await;
    grant(&env, &alice, &d, "si:scout", "globex").await;
    let _app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let _ = api(&env, "GET", "/api/v1/auth/me", &carol, Some("globex"), None).await;
    // Strict readers: a Silicon can't read a Carbon's entry, so at-use checks can't tell.
    iam.set_reader_mode(ReaderMode::Strict);
    iam.set_member("c:alice", Some(vec!["acme".into()])).await;
    let (st, s) = session(&env, &scout, "globex", &d).await;
    assert_eq!(st, 201, "unknown: the call goes on: {s}");
    assert_eq!(grants(&env, &d).await.len(), 1);
    let _ = api(
        &env,
        "POST",
        &format!("/api/v1/sessions/{}/end", s["data"]["session_id"].as_str().unwrap()),
        &scout,
        Some("globex"),
        None,
    )
    .await;
    // IAM hiding Carbons from Silicons with a 404: the first use is refused, but a second reader
    // contradicts it, so the grants stay and, for 10 minutes, a Silicon's "gone" counts for nothing.
    iam.set_member("c:alice", Some(vec!["acme".into(), "globex".into()]))
        .await;
    iam.set_reader_mode(ReaderMode::HideCarbonsFromSilicons);
    let (st, _) = session(&env, &scout, "globex", &d).await;
    assert_eq!(st, 403);
    eventually("the contradiction recorded", || async {
        let c: Option<Option<time::OffsetDateTime>> = sqlx::query_scalar(
            "SELECT contradicted_at FROM extend.membership_checks WHERE member_id = 'c:alice' AND team = 'globex'",
        )
        .fetch_optional(&env.pool)
        .await
        .unwrap();
        c.flatten().is_some()
    })
    .await;
    assert_eq!(grants(&env, &d).await.len(), 1, "grants kept");
    let (st, s) = session(&env, &scout, "globex", &d).await;
    assert_eq!(st, 201, "within the window a Silicon's 404 counts as unknown: {s}");
}

#[tokio::test]
async fn the_sweep_needs_two_answers_ten_minutes_apart() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let _carol = login(&env, "c:carol").await;
    let (d, _) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &[]).await;
    grant(&env, &alice, &d, "si:scout", "globex").await;
    let _ = api(&env, "GET", "/api/v1/auth/me", &_carol, Some("globex"), None).await;
    env.state
        .local_iam
        .as_ref()
        .unwrap()
        .set_member("si:scout", Some(vec![]))
        .await;
    let now = time::OffsetDateTime::now_utc();
    extend_service::membership::sweep(&env.state, &World::production(), now)
        .await
        .unwrap();
    assert_eq!(grants(&env, &d).await.len(), 1, "one answer deletes nothing");
    extend_service::membership::sweep(&env.state, &World::production(), now + time::Duration::minutes(11))
        .await
        .unwrap();
    assert!(
        grants(&env, &d).await.is_empty(),
        "two answers 10+ minutes apart delete"
    );
}

#[tokio::test]
async fn a_refused_login_keeps_sessions_on_doubt_and_ends_them_only_when_gone() {
    let env = start().await;
    let iam = env.state.local_iam.clone().unwrap();
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &[]).await;
    grant(&env, &alice, &d, "si:chef", "globex").await;
    let _app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (_, s) = session(&env, &chef, "globex", &d).await;
    let sid = s["data"]["session_id"].as_str().unwrap().to_owned();
    // chef signs in again with only acme selected, then acts in globex: IAM says it is still a
    // member of globex, so nothing ends.
    let acme_only = env.client.login("si:chef@acme").await.unwrap().access_token;
    let _ = api(&env, "GET", "/api/v1/auth/me", &acme_only, Some("acme"), None).await;
    let (st, _) = api(
        &env,
        "GET",
        &format!("/api/v1/sessions/{sid}"),
        &acme_only,
        Some("globex"),
        None,
    )
    .await;
    assert_eq!(st, 403);
    assert_eq!(end_reason(&env, &alice, "acme", &sid).await, None);
    // Nobody can tell (strict readers, no other member of globex signed in): the call is still
    // refused, but neither the existing session nor its grant ends on doubt.
    iam.set_reader_mode(ReaderMode::Strict);
    env.state.auth_cache.forget(&[]).await;
    let _ = api(&env, "GET", "/api/v1/auth/me", &acme_only, Some("acme"), None).await;
    let (st, refusal) = api(
        &env,
        "GET",
        &format!("/api/v1/sessions/{sid}"),
        &acme_only,
        Some("globex"),
        None,
    )
    .await;
    assert_eq!(st, 403);
    assert_eq!(refusal["data"]["code"], "not_a_team_member");
    let (st, current) = api(
        &env,
        "GET",
        &format!("/api/v1/sessions/{sid}"),
        &alice,
        Some("globex"),
        None,
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(current["data"]["state"], "active");
    assert!(current["data"]["end_reason"].is_null());
    assert!(!refusal["data"]["message"].as_str().unwrap().contains("left_team"));
    assert_eq!(grants(&env, &d).await, vec![("si:chef".into(), "globex".into())]);
    // The same live session still accepts its original login, which reaches globex.
    let (st, result) = api(
        &env,
        "POST",
        &format!("/api/v1/sessions/{sid}/commands"),
        &chef,
        Some("globex"),
        Some(json!({"type": "command", "data": {"command": "screenshot", "args": []}})),
    )
    .await;
    assert_eq!(st, 200, "{result}");
    assert_eq!(
        result["data"]["ok"], true,
        "the preserved session must still execute commands: {result}"
    );
    // chef really left globex, and two readers say so: the same session ends and the grant goes.
    iam.set_reader_mode(ReaderMode::Open);
    let both = login(&env, "si:chef").await;
    let carol = login(&env, "c:carol").await;
    let _ = api(&env, "GET", "/api/v1/auth/me", &carol, Some("globex"), None).await;
    let _ = api(&env, "GET", "/api/v1/auth/me", &alice, Some("globex"), None).await;
    iam.set_member("si:chef", Some(vec!["acme".into()])).await;
    env.state.auth_cache.forget(&["si:chef".into()]).await;
    let _ = api(&env, "GET", "/api/v1/auth/me", &both, Some("acme"), None).await;
    let (st, _) = api(
        &env,
        "GET",
        &format!("/api/v1/sessions/{sid}"),
        &both,
        Some("globex"),
        None,
    )
    .await;
    assert_eq!(st, 403);
    assert_eq!(
        end_reason(&env, &alice, "acme", &sid).await.as_deref(),
        Some("left_team")
    );
    eventually("the confirmed Gone deleting the grant", || async {
        grants(&env, &d).await.is_empty()
    })
    .await;
}

#[tokio::test]
async fn an_iam_event_clears_the_owner_check_cache() {
    let env = start().await;
    let iam = env.state.local_iam.clone().unwrap();
    let alice = login(&env, "c:alice").await;
    let chef = login(&env, "si:chef").await;
    let (d, cred) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &[]).await;
    grant(&env, &alice, &d, "si:chef", "globex").await;
    let _app = App::connect(&env, &cred, hello(DeviceOs::Android, "1.1.0")).await;
    let (_, s) = session(&env, &chef, "globex", &d).await;
    let sid = s["data"]["session_id"].as_str().unwrap().to_owned();
    let run = || {
        let (env, chef, sid) = (&env, chef.clone(), sid.clone());
        async move {
            api(
                env,
                "POST",
                &format!("/api/v1/sessions/{sid}/commands"),
                &chef,
                Some("globex"),
                Some(json!({"type": "command", "data": {"command": "screenshot", "args": []}})),
            )
            .await
            .0
        }
    };
    assert_eq!(run().await, 200);
    // IAM drops alice from globex; the positive answer is still cached.
    iam.set_member("c:alice", Some(vec!["acme".into()])).await;
    assert_eq!(run().await, 200, "cached for EXTEND_OWNER_CHECK_CACHE_S");
    // An event naming alice (that changes nothing by itself here) clears it: the next use re-asks.
    extend_service::membership::forget(&env.state, &["c:alice".into()]).await;
    assert_eq!(run().await, 403);
    assert_eq!(
        end_reason(&env, &chef, "globex", &sid).await.as_deref(),
        Some("left_team")
    );
}

// ───────────── 16: files across Teams ─────────────

async fn stored_file(env: &Env, member: &str, team: &str, device: &str, name: &str) -> uuid::Uuid {
    let p = extend_service::iam::Principal {
        member: extend_protocol::model::Member {
            kind: extend_protocol::model::MemberKind::Silicon,
            id: member.into(),
            display_name: None,
        },
        team: Some(team.into()),
        teams: vec![team.into()],
        role: None,
        token: String::new(),
    };
    let stored = env
        .state
        .files
        .store(
            &p,
            extend_service::files::NewFile {
                operation_id: uuid::Uuid::new_v4(),
                name,
                content_type: "text/plain",
                bytes: name.as_bytes().to_vec(),
                owner_carbon: "c:alice",
            },
            None,
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO extend.files (file_id, team, device_id, created_by, shared_with, name, kind, content_type, size_bytes, url)
         VALUES ($1, $2, $3, $4, 'c:alice', $5, 'other', 'text/plain', 3, $6)",
    )
    .bind(stored.file_id)
    .bind(team)
    .bind(device)
    .bind(member)
    .bind(name)
    .bind(&stored.url)
    .execute(&env.pool)
    .await
    .unwrap();
    stored.file_id
}

#[tokio::test]
async fn files_are_listed_and_opened_only_in_the_selected_organization() {
    let env = start().await;
    let alice = login(&env, "c:alice").await;
    let scout = login(&env, "si:scout").await;
    let (d, _) = pair(&env, &alice, Some("acme"), DeviceOs::Android, "Pixel", &[]).await;
    grant(&env, &alice, &d, "si:scout", "globex").await;
    let a = stored_file(&env, "si:chef", "acme", &d, "acme.txt").await;
    let g = stored_file(&env, "si:scout", "globex", &d, "globex.txt").await;
    let (_, list) = api(&env, "GET", "/api/v1/files", &alice, None, None).await;
    let items = list["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{list}");
    assert!(items.iter().all(|f| f["team"] == "acme"));
    // A Silicon sees its own, in its Team.
    let (_, own) = api(&env, "GET", "/api/v1/files", &scout, Some("globex"), None).await;
    assert_eq!(own["data"]["items"].as_array().unwrap().len(), 1);
    // An acme login can open only acme files; foreign organization IDs reveal nothing.
    let acme_only = env.client.login("c:alice@acme").await.unwrap().access_token;
    let (st, _) = api(
        &env,
        "GET",
        &format!("/api/v1/files/{a}/content"),
        &acme_only,
        None,
        None,
    )
    .await;
    assert_eq!(st, 200);
    let (st, e) = api(
        &env,
        "GET",
        &format!("/api/v1/files/{g}/content"),
        &acme_only,
        None,
        None,
    )
    .await;
    assert_eq!((st, e["data"]["code"].as_str()), (404, Some("file_not_found")));
    let (st, _) = api(
        &env,
        "GET",
        &format!("/api/v1/files/{g}/content"),
        &alice,
        Some("globex"),
        None,
    )
    .await;
    assert_eq!(st, 200);
}

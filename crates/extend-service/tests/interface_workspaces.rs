//! A standalone HTTP client, the actual Extend service/database/device WebSocket, and
//! independent Organizations/Contacts HTTP fixtures with mutable upstream state.
mod common;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use extend_protocol::{DeviceOs, model::EnrollmentCreate};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Default)]
struct Upstream {
    contexts: HashMap<String, Value>,
    saved: HashMap<String, Value>,
}
type Peer = Arc<Mutex<Upstream>>;
async fn orgs(State(peer): State<Peer>, h: HeaderMap) -> Response {
    let token = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    match peer.lock().unwrap().contexts.get(token) {
        Some(v) => Json(v.clone()).into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}
fn proof(h: &HeaderMap) {
    assert!(h.get("authorization").unwrap().to_str().unwrap().starts_with("Proof "));
}
async fn fixture() -> (common::Env, Peer) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let peer = Peer::default();
    let service = Router::new()
        .route("/v1/integrations/extend/me", get(orgs))
        .route(
            "/v1/me",
            get(|h: HeaderMap| async move {
                proof(&h);
                Json(json!({}))
            }),
        )
        .route(
            "/v1/contacts",
            get(
                |State(p): State<Peer>, h: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                    proof(&h);
                    let lock = p.lock().unwrap();
                    let mut all: Vec<_> = lock.saved.values().cloned().collect();
                    all.sort_by_key(|c| c["uuid"].to_string());
                    let total = all.len();
                    let items: Vec<_> = all
                        .into_iter()
                        .skip(q["offset"].parse().unwrap())
                        .take(q["limit"].parse().unwrap())
                        .collect();
                    Json(json!({"items":items,"total":total}))
                },
            )
            .merge(post(
                |State(p): State<Peer>, h: HeaderMap, Json(v): Json<Value>| async move {
                    proof(&h);
                    let mut db = p.lock().unwrap();
                    let item = db.saved.get_mut(v["id"].as_str().unwrap()).unwrap();
                    for key in ["name", "trust", "note", "photo_id"] {
                        item[key] = v[key].clone();
                    }
                    Json(item.clone())
                },
            )),
        )
        .route(
            "/v1/contacts/{id}",
            get(
                |State(p): State<Peer>, Path(id): Path<String>, h: HeaderMap| async move {
                    proof(&h);
                    p.lock()
                        .unwrap()
                        .saved
                        .get(&id)
                        .cloned()
                        .map(|v| Json(v).into_response())
                        .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
                },
            ),
        )
        .route(
            "/v1/photos/{id}",
            get(|h: HeaderMap| async move {
                proof(&h);
                &b"\x89PNG\r\n\x1a\nphoto"[..]
            }),
        )
        .with_state(peer.clone());
    tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    (
        common::start_config(|c| {
            c.contacts_url = url.clone();
            c.organizations_url = url;
        })
        .await,
        peer,
    )
}
fn context(uuid: &str, admin: bool, member: bool) -> Value {
    json!({"account_uuid":uuid,"organizations":[{"org_id":"studio","name":"Studio","capabilities":{"owner":admin,"admin":admin}}],
        "groups":if member{vec!["organization:studio","team:studio/design"]}else{vec!["organization:studio"]},
        "share_targets":[{"org_id":"studio","id":"studio/design","type":"team","name":"Design"}]})
}
#[tokio::test]
async fn every_current_team_member_uses_an_organization_device() {
    let (env, peer) = fixture().await;
    let mut actors = Vec::new();
    for (id, admin, member) in [
        ("c:alice", true, false),
        ("c:bob", false, true),
        ("si:chef", false, true),
        ("c:dave", false, false),
    ] {
        let token = common::login(&env, id).await;
        let (_, me) = common::api(&env, "GET", "/api/v2/me", &token, None).await;
        let uuid = me["data"]["uuid"].as_str().unwrap();
        peer.lock()
            .unwrap()
            .contexts
            .insert(token.clone(), context(uuid, admin, member));
        actors.push((token, uuid.to_owned()));
    }
    let admin = &actors[0].0;
    let carbon = &actors[1].0;
    let silicon = &actors[2].0;
    let outsider = &actors[3].0;
    let enroll = env
        .client
        .enroll(&EnrollmentCreate {
            os: DeviceOs::Linux,
            os_version: None,
            model: None,
            app_version: "1.1.0".into(),
            engine_version: None,
        })
        .await
        .unwrap();
    let mut pair = json!({"type":"pairing","data":{"name":"Studio computer","organization_id":"studio","team_ids":["elsewhere"],"pairing_code":enroll.pairing_code}});
    assert_eq!(
        common::api(&env, "POST", "/api/v2/pairings", admin, Some(pair.clone()))
            .await
            .0,
        422
    );
    pair["data"]["team_ids"] = json!(["design"]);
    assert_eq!(
        common::api(&env, "POST", "/api/v2/pairings", carbon, Some(pair.clone()))
            .await
            .0,
        403
    );
    let (status, device) = common::api(&env, "POST", "/api/v2/pairings", admin, Some(pair)).await;
    assert_eq!(status, 201, "{device}");
    let id = device["data"]["device_id"].as_str().unwrap();
    let cred = common::credential_of(&env, enroll.enrollment_id, &enroll.enrollment_secret).await;
    let _device = common::App::connect(&env, &cred, common::hello(DeviceOs::Linux, "1.1.0")).await;
    let path = format!("/api/v2/devices/{id}");
    for (token, count) in [(admin, 1), (carbon, 1), (silicon, 1), (outsider, 0)] {
        let (s, list) = common::api(&env, "GET", "/api/v2/devices?scope=organization", token, None).await;
        assert_eq!(s, 200, "{list}");
        assert_eq!(list["data"]["items"].as_array().unwrap().len(), count);
    }
    assert_eq!(common::api(&env, "GET", &path, outsider, None).await.0, 404);
    let share = format!("{path}/teams");
    let clear = json!({"type":"team_access","data":{"team_ids":[]}});
    assert_eq!(
        common::api(&env, "PUT", &share, carbon, Some(clear.clone())).await.0,
        403
    );
    let start = json!({"type":"session","data":{"device_id":id}});
    let session = common::api(&env, "POST", "/api/v2/sessions", silicon, Some(start.clone())).await;
    assert_eq!(session.0, 201, "{}", session.1);
    let sid = session.1["data"]["session_id"].as_str().unwrap();
    assert_eq!(
        common::api(&env, "POST", &format!("/api/v2/sessions/{sid}/end"), silicon, None)
            .await
            .0,
        200
    );
    let (s, session) = common::api(&env, "POST", "/api/v2/sessions", carbon, Some(start)).await;
    assert_eq!(s, 201, "{session}");
    let sid = session["data"]["session_id"].as_str().unwrap();
    let command = format!("/api/v2/sessions/{sid}/commands");
    let input = json!({"type":"command","data":{"command":"snapshot","args":[]}});
    let (s, reply) = common::api(&env, "POST", &command, carbon, Some(input.clone())).await;
    assert_eq!(s, 200, "{reply}");
    peer.lock()
        .unwrap()
        .contexts
        .insert(carbon.clone(), context(&actors[1].1, false, false));
    assert_eq!(common::api(&env, "GET", &path, carbon, None).await.0, 404);
    assert_eq!(common::api(&env, "POST", &command, carbon, Some(input)).await.0, 404);
    assert_eq!(common::api(&env, "PUT", &share, admin, Some(clear)).await.0, 200);
    assert_eq!(common::api(&env, "GET", &path, silicon, None).await.0, 404);
    peer.lock()
        .unwrap()
        .contexts
        .insert(admin.clone(), context(&actors[0].1, false, false));
    assert_eq!(common::api(&env, "GET", &path, admin, None).await.0, 404);
}
#[tokio::test]
async fn address_book_is_caller_scoped_and_ranks_by_trust() {
    let (env, peer) = fixture().await;
    let alice = common::login(&env, "c:alice").await;
    let bob = common::login(&env, "c:bob").await;
    let photo = Uuid::new_v4().to_string();
    let mut first = String::new();
    for (id, name, trust) in [("si:chef", "Ada kitchen", 2), ("c:bob", "Ada studio", 240)] {
        let token = common::login(&env, id).await;
        let (_, me) = common::api(&env, "GET", "/api/v2/me", &token, None).await;
        let a = &me["data"];
        let uuid = a["uuid"].as_str().unwrap();
        if id == "si:chef" {
            first = uuid.to_owned();
        }
        peer.lock().unwrap().saved.insert(
            uuid.to_owned(),
            json!({"uuid":uuid,"account":a,"name":name,"trust":trust,"note":"keep","photo_id":photo,"blocked":false}),
        );
    }
    let conn = "/api/v2/contacts/connection";
    let toggle = |enabled| Some(json!({"type":"contacts_connection","data":{"enabled":enabled}}));
    assert_eq!(common::api(&env, "GET", "/api/v2/contacts", &alice, None).await.0, 403);
    assert_eq!(common::api(&env, "PUT", conn, &alice, toggle(true)).await.0, 200);
    let (_, people) = common::api(&env, "GET", "/api/v2/people?q=aa", &alice, None).await;
    assert_eq!(people["data"]["items"][0]["trust"], 240);
    assert_eq!(people["data"]["items"][1]["trust"], 2);
    let (_, preview) = common::api(&env, "GET", "/api/v2/people/resolve?id=si:chef", &alice, None).await;
    assert_eq!(preview["data"]["display_name"], "Ada kitchen");
    let path = preview["data"]["contact_photo_url"].as_str().unwrap();
    assert_eq!(common::api(&env, "GET", path, &alice, None).await.0, 200);
    assert_eq!(common::api(&env, "GET", path, &bob, None).await.0, 403);
    let (s, saved) = common::api(
        &env,
        "POST",
        "/api/v2/contacts",
        &alice,
        Some(json!({"type":"contact","data":{"id":first,"name":"Scout","trust":255}})),
    )
    .await;
    assert_eq!(s, 200, "{saved}");
    assert_eq!(peer.lock().unwrap().saved[&first]["note"], "keep");
    assert_eq!(saved["data"]["contact_photo_url"], path);
    assert_eq!(common::api(&env, "PUT", conn, &alice, toggle(false)).await.0, 200);
    assert_eq!(common::api(&env, "GET", path, &alice, None).await.0, 403);
    assert!(peer.lock().unwrap().saved.contains_key(&first));
}

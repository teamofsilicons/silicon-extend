//! Per-caller address book and recipient search. Silicon Contacts owns all saved details.
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use extend_protocol::ErrorCode;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

use super::{Body, ok};
use crate::accounts::Principal;
use crate::error::{AppError, AppResult};
use crate::state::{AppState, Auth, Shared};

async fn connected(state: &AppState, p: &Principal) -> AppResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM extend.contacts_connections WHERE account_uuid=$1)")
            .bind(p.uuid())
            .fetch_one(&state.pool)
            .await?,
    )
}

async fn contact_request(
    state: &AppState,
    p: &Principal,
    path: &[&str],
    query: &[(&str, String)],
    body: Option<&Value>,
) -> AppResult<reqwest::Response> {
    let scope = if body.is_some() {
        "contacts.write"
    } else {
        "contacts.read"
    };
    let proof = state.proofs.for_user(p, "silicon-contacts", &[scope]).await?;
    let mut url = url::Url::parse(&state.cfg.contacts_url).map_err(AppError::internal)?;
    url.path_segments_mut()
        .map_err(|_| AppError::internal("Invalid Contacts origin"))?
        .pop_if_empty()
        .push("v1")
        .extend(path);
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    let mut request = state
        .integration_http
        .request(if body.is_some() { Method::POST } else { Method::GET }, url)
        .header("Authorization", format!("Proof {proof}"));
    if let Some(data) = body {
        request = request.json(data);
    }
    let response = request
        .send()
        .await
        .map_err(|_| AppError::unavailable("Silicon Contacts", "connection failed"))?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let data: Value = response.json().await.unwrap_or(Value::Null);
    let message = data
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("Silicon Contacts could not complete this request.");
    Err(match status.as_u16() {
        400 | 404 | 409 | 422 => AppError::invalid(message).status(status.as_u16()),
        401 | 403 => AppError::new(ErrorCode::NoAccess, "Silicon Contacts denied address-book access."),
        429 => AppError::new(ErrorCode::RateLimited, "Silicon Contacts is busy. Retry shortly."),
        _ => AppError::unavailable("Silicon Contacts", status),
    })
}

async fn upstream(
    state: &AppState,
    p: &Principal,
    path: &[&str],
    q: &[(&str, String)],
    body: Option<&Value>,
) -> AppResult<Value> {
    contact_request(state, p, path, q, body)
        .await?
        .json()
        .await
        .map_err(|_| AppError::unavailable("Silicon Contacts", "invalid response"))
}

async fn require_connection(state: &AppState, p: &Principal) -> AppResult<()> {
    if connected(state, p).await? {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::NoAccess,
            "Connect Silicon Contacts in Extend first.",
        ))
    }
}

#[derive(Deserialize)]
pub struct Connection {
    enabled: bool,
}

pub async fn status(State(state): State<Shared>, auth: Auth) -> AppResult<Response> {
    Ok(ok(
        "contacts_connection",
        json!({"connected":connected(&state,&auth.p).await?}),
    ))
}
pub async fn connect(State(state): State<Shared>, auth: Auth, Body(input): Body<Connection>) -> AppResult<Response> {
    if input.enabled {
        upstream(&state, &auth.p, &["me"], &[], None).await?;
        sqlx::query("INSERT INTO extend.contacts_connections(account_uuid) VALUES($1) ON CONFLICT DO NOTHING")
            .bind(auth.p.uuid())
            .execute(&state.pool)
            .await?;
    } else {
        sqlx::query("DELETE FROM extend.contacts_connections WHERE account_uuid=$1")
            .bind(auth.p.uuid())
            .execute(&state.pool)
            .await?;
    }
    Ok(ok("contacts_connection", json!({"connected":input.enabled})))
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Person {
    uuid: String,
    id: String,
    #[serde(rename = "type")]
    kind: String,
    display_name: String,
    pfp_url: Option<String>,
    contact_photo_url: Option<String>,
    trust: u8,
    blocked: bool,
}
impl Person {
    fn from_account(a: &Value) -> AppResult<Self> {
        let field = |key: &str| a[key].as_str().filter(|s| !s.is_empty());
        let uuid = field("uuid").ok_or_else(|| AppError::unavailable("Silicon Contacts", "missing account UUID"))?;
        let id = field("id").unwrap_or("");
        Ok(Self {
            uuid: uuid.into(),
            id: id.into(),
            kind: field("type")
                .or_else(|| field("kind"))
                .unwrap_or(if id.starts_with("si:") { "silicon" } else { "carbon" })
                .into(),
            display_name: field("display_name").unwrap_or(id).into(),
            pfp_url: field("pfp_url").map(String::from),
            contact_photo_url: None,
            trust: 0,
            blocked: false,
        })
    }
    fn from_contact(c: &Value) -> AppResult<Self> {
        let mut p = Self::from_account(&c["account"])?;
        if let Some(name) = c["name"].as_str().filter(|n| !n.is_empty()) {
            p.display_name = name.into();
        }
        p.trust = c["trust"].as_u64().and_then(|n| u8::try_from(n).ok()).unwrap_or(1);
        p.blocked = c["blocked"].as_bool().unwrap_or(false);
        p.contact_photo_url = c["photo_id"]
            .as_str()
            .and_then(|s| s.parse::<Uuid>().ok())
            .map(|id| format!("/api/v2/contacts/photos/{id}"));
        Ok(p)
    }
}

#[derive(Deserialize)]
pub struct Page {
    offset: Option<u32>,
    limit: Option<u16>,
}
async fn saved_page(state: &AppState, p: &Principal, offset: u32, limit: u16) -> AppResult<(Vec<Person>, u64)> {
    let data = upstream(
        state,
        p,
        &["contacts"],
        &[("limit", limit.to_string()), ("offset", offset.to_string())],
        None,
    )
    .await?;
    let bad = || AppError::unavailable("Silicon Contacts", "invalid page");
    let items = data["items"]
        .as_array()
        .ok_or_else(bad)?
        .iter()
        .map(Person::from_contact)
        .collect::<AppResult<_>>()?;
    Ok((items, data["total"].as_u64().ok_or_else(bad)?))
}
pub async fn list(State(state): State<Shared>, auth: Auth, Query(q): Query<Page>) -> AppResult<Response> {
    require_connection(&state, &auth.p).await?;
    let offset = q.offset.unwrap_or(0);
    let limit = q.limit.unwrap_or(100);
    if !(1..=200).contains(&limit) {
        return Err(AppError::invalid("limit must be between 1 and 200."));
    }
    let (items, total) = saved_page(&state, &auth.p, offset, limit).await?;
    Ok(ok(
        "contacts",
        json!({"items":items,"total":total,"offset":offset,"limit":limit}),
    ))
}

#[derive(Deserialize)]
pub struct Save {
    id: String,
    name: String,
    trust: u8,
}
pub async fn save(State(state): State<Shared>, auth: Auth, Body(input): Body<Save>) -> AppResult<Response> {
    require_connection(&state, &auth.p).await?;
    if input.trust == 0 || input.name.trim().is_empty() || input.name.len() > 200 {
        return Err(AppError::invalid(
            "Choose a name up to 200 bytes and a trust level from 1 to 255.",
        ));
    }
    let account = state.accounts.directory.resolve(&input.id, None).await?;
    if account.uuid == auth.p.uuid() {
        return Err(AppError::invalid("Choose another Carbon or Silicon."));
    }
    let mut data = json!({"id":account.uuid,"name":input.name.trim(),"trust":input.trust});
    match upstream(&state, &auth.p, &["contacts", &account.uuid], &[], None).await {
        Ok(previous) => {
            for key in [
                "note",
                "phone",
                "email",
                "photo_id",
                "birthday_month",
                "birthday_day",
                "birthday_year",
            ] {
                data[key] = previous[key].clone();
            }
        }
        Err(e) if e.1 == Some(404) => (),
        Err(e) => return Err(e),
    }
    let saved = upstream(&state, &auth.p, &["contacts"], &[], Some(&data)).await?;
    Ok(ok("contact", Person::from_contact(&saved)?))
}
pub async fn photo(State(state): State<Shared>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Response> {
    require_connection(&state, &auth.p).await?;
    let r = contact_request(&state, &auth.p, &["photos", &id.to_string()], &[], None).await?;
    let data = r
        .bytes()
        .await
        .map_err(|_| AppError::unavailable("Silicon Contacts", "photo download interrupted"))?;
    if !data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(AppError::unavailable("Silicon Contacts", "invalid photo"));
    }
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "private, no-store"),
        ],
        data,
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct Search {
    q: Option<String>,
    kind: Option<String>,
    limit: Option<u16>,
}
fn matches_text(text: &str, query: &str) -> bool {
    let hay = text.to_lowercase();
    query.split_whitespace().all(|word| {
        let mut chars = hay.chars();
        word.chars().all(|c| chars.any(|h| h == c))
    })
}
async fn exact(state: &AppState, p: &Principal, id: &str, with_contacts: bool) -> AppResult<Person> {
    let row = state.accounts.directory.resolve(id, None).await?;
    if with_contacts {
        match upstream(state, p, &["contacts", &row.uuid], &[], None).await {
            Ok(c) => return Person::from_contact(&c),
            Err(e) if e.1 == Some(404) => (),
            Err(e) => return Err(e),
        }
    }
    // Contacts has directory permission for public names/photos of accounts new to Extend.
    if row.pfp_url.as_deref().unwrap_or("").is_empty() {
        if let Ok(a) = upstream(state, p, &["lookup"], &[("q", row.uuid.clone())], None).await {
            return Person::from_account(&a);
        }
    }
    Ok(Person {
        id: row.id.clone(),
        kind: row.kind,
        uuid: row.uuid,
        display_name: row.display_name.filter(|s| !s.is_empty()).unwrap_or(row.id),
        pfp_url: row.pfp_url,
        contact_photo_url: None,
        trust: 0,
        blocked: false,
    })
}
#[derive(Deserialize)]
pub struct Lookup {
    id: String,
}
pub async fn resolve(State(state): State<Shared>, auth: Auth, Query(q): Query<Lookup>) -> AppResult<Response> {
    let person = exact(&state, &auth.p, &q.id, connected(&state, &auth.p).await?).await?;
    Ok(ok("person", person))
}
pub async fn people(State(state): State<Shared>, auth: Auth, Query(q): Query<Search>) -> AppResult<Response> {
    let term = q.q.unwrap_or_default().trim().to_lowercase();
    let limit = q.limit.unwrap_or(12);
    if term.len() > 200
        || !(1..=100).contains(&limit)
        || !matches!(q.kind.as_deref(), None | Some("silicon" | "carbon"))
    {
        return Err(AppError::invalid(
            "Use a search up to 200 bytes, kind carbon or silicon, and a limit from 1 to 100.",
        ));
    }
    let mut peers = HashMap::new();
    let rows:Vec<(String,String,String,Option<String>,Option<String>)>=sqlx::query_as(
        "SELECT a.uuid,a.id,a.kind,a.display_name,a.pfp_url FROM extend.accounts a WHERE a.status='active' AND a.uuid<>$1 AND
        (a.custodian_uuid=$1 OR a.uuid=(SELECT custodian_uuid FROM extend.accounts WHERE uuid=$1)
        OR EXISTS(SELECT 1 FROM extend.device_access g WHERE (g.silicon_id=a.uuid AND g.granted_by=$1) OR (g.granted_by=a.uuid AND g.silicon_id=$1)))")
        .bind(auth.p.uuid()).fetch_all(&state.pool).await?;
    for (uuid, id, kind, name, pfp_url) in rows {
        peers.insert(
            uuid.clone(),
            Person {
                uuid,
                display_name: name.unwrap_or_else(|| id.clone()),
                id,
                kind,
                pfp_url,
                contact_photo_url: None,
                trust: 0,
                blocked: false,
            },
        );
    }
    let is_connected = connected(&state, &auth.p).await?;
    if is_connected {
        let mut offset = 0u32;
        loop {
            let (batch, total) = saved_page(&state, &auth.p, offset, 200).await?;
            let count = batch.len() as u32;
            for person in batch {
                peers.insert(person.uuid.clone(), person);
            }
            offset = offset.saturating_add(count);
            if u64::from(offset) >= total || count == 0 {
                break;
            }
        }
    }
    if (term.starts_with("c:") || term.starts_with("si:") || Uuid::parse_str(&term).is_ok())
        && !peers
            .values()
            .any(|p| p.id.eq_ignore_ascii_case(&term) || p.uuid == term)
    {
        match exact(&state, &auth.p, &term, is_connected).await {
            Ok(person) => {
                peers.insert(person.uuid.clone(), person);
            }
            Err(e) if e.code() == ErrorCode::InvalidInput => (),
            Err(e) => return Err(e),
        }
    }
    let mut items: Vec<_> = peers
        .into_values()
        .filter(|p| {
            p.uuid != auth.p.uuid()
                && !p.blocked
                && !p.id.is_empty()
                && q.kind.as_ref().is_none_or(|k| &p.kind == k)
                && (p.uuid == term || matches_text(&p.display_name, &term) || matches_text(&p.id, &term))
        })
        .collect();
    items.sort_by_cached_key(|p| (std::cmp::Reverse(p.trust), p.display_name.to_lowercase(), p.id.clone()));
    items.truncate(usize::from(limit));
    Ok(ok("people", json!({"items":items,"contacts_connected":is_connected})))
}

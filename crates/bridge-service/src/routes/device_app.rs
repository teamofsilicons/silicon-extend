//! Endpoints a paired Bridge app uses with its device credential (docs/device-protocol.md).

use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use bridge_protocol::frames::{DeviceFrame, ServiceFrame, close};
use bridge_protocol::model::{DeviceSelf, EndReason, Member, MemberKind, Setup, SetupState, Takeover};
use bridge_protocol::{Capability, ErrorCode, MAX_ARTIFACT_BYTES, ids};
use futures::{SinkExt as _, StreamExt as _};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use super::devices::env_view;
use super::enroll::version_at_least;
use super::{no_content, ok};
use crate::db::World;
use crate::domain::{self, DeviceRow};
use crate::error::{AppError, AppResult};
use crate::hub::AttachedState;
use crate::state::{AppState, DeviceAuth, Shared};

async fn this_device(state: &AppState, auth: &DeviceAuth) -> AppResult<DeviceRow> {
    domain::load_device(state, &auth.world, &auth.device_id)
        .await?
        .ok_or_else(|| AppError::new(ErrorCode::Unauthorized, "This device is no longer paired."))
}

fn owner(d: &DeviceRow) -> Member {
    Member { kind: MemberKind::Carbon, id: d.owner_id.clone(), display_name: None }
}

async fn test_selection(state: &AppState, world: &World) -> Option<crate::iam::TestingSelection> {
    let id = world.environment_id?;
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM bridge_global.test_environments WHERE environment_id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten();
    Some(crate::iam::TestingSelection { environment_id: id, name: name.unwrap_or_else(|| id.to_string()), secret: String::new() })
}

pub async fn me(State(state): State<Shared>, auth: DeviceAuth) -> AppResult<Response> {
    let d = this_device(&state, &auth).await?;
    let view = domain::device_view(&state, &auth.world, &d, domain::Access::Owner, false).await;
    let session = match &d.in_use_session {
        Some(s) => domain::load_session(&state, &auth.world, s).await?,
        None => None,
    };
    let takeover: Option<Takeover> = session.as_ref().filter(|s| s.state == "paused").and_then(|s| s.takeover.clone()).and_then(|t| serde_json::from_value(t).ok());
    let sel = test_selection(&state, &auth.world).await;
    Ok(ok(
        "device_self",
        DeviceSelf {
            device_id: view.device_id,
            name: d.name.clone(),
            owner: owner(&d),
            team: d.team.clone(),
            os: d.os(),
            in_use: view.in_use,
            takeover,
            setup: d.setup(),
            environment: env_view(&state, sel.as_ref(), &auth.world).await,
        },
    ))
}

pub async fn revoke(State(state): State<Shared>, auth: DeviceAuth) -> AppResult<Response> {
    let d = this_device(&state, &auth).await?;
    domain::unpair(&state, &auth.world, &auth.device_id, EndReason::PairRevoked, &owner(&d)).await?;
    Ok(no_content())
}

async fn stop_running(state: &AppState, world: &World, d: &DeviceRow) -> AppResult<()> {
    if let Some(sid) = &d.in_use_session {
        domain::end_session(state, world, sid, EndReason::StoppedByCarbon, &owner(d)).await?;
    }
    Ok(())
}

pub async fn stop(State(state): State<Shared>, auth: DeviceAuth) -> AppResult<Response> {
    let d = this_device(&state, &auth).await?;
    stop_running(&state, &auth.world, &d).await?;
    Ok(no_content())
}

pub async fn upload(State(state): State<Shared>, auth: DeviceAuth, Path(upload_id): Path<Uuid>, headers: HeaderMap, body: Bytes) -> AppResult<Response> {
    let h = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let digest = h("x-content-sha256").ok_or_else(|| AppError::invalid("X-Content-SHA256 is required: the 64 lowercase hex SHA-256 of the bytes."))?;
    let name = h("x-file-name").filter(|n| !n.is_empty() && n.len() <= 255 && !n.contains('/') && !n.contains('\\')).ok_or_else(|| {
        AppError::invalid("X-File-Name is required: a plain file name up to 255 characters, no slashes.")
    })?;
    let content_type = h("content-type").unwrap_or_else(|| "application/octet-stream".into());
    if body.len() as u64 > MAX_ARTIFACT_BYTES {
        return Err(AppError::new(ErrorCode::PayloadTooLarge, "Files are limited to 1 GiB."));
    }
    let actual = ids::hex_lower(&Sha256::digest(&body));
    if actual != digest.to_ascii_lowercase() {
        return Err(AppError::invalid(format!("X-Content-SHA256 {digest} does not match the bytes received ({actual}).")));
    }
    let claimed = sqlx::query(sql!(
        "UPDATE {} SET received = true, name = $3, content_type = $4, size_bytes = $5
         WHERE upload_id = $1 AND device_id = $2 AND NOT received AND expires_at > now()",
        auth.world.t("uploads")
    ))
    .bind(upload_id)
    .bind(&auth.device_id)
    .bind(&name)
    .bind(&content_type)
    .bind(body.len() as i64)
    .execute(&state.pool)
    .await?;
    if claimed.rows_affected() == 0 {
        return Err(AppError::new(ErrorCode::FileNotFound, "That upload id is unknown, already used, expired, or belongs to another device."));
    }
    let dir = state.cfg.data_dir.join("uploads");
    tokio::fs::create_dir_all(&dir).await.map_err(AppError::internal)?;
    tokio::fs::write(dir.join(upload_id.to_string()), &body).await.map_err(AppError::internal)?;
    Ok(axum::http::StatusCode::CREATED.into_response())
}

pub async fn socket(State(state): State<Shared>, auth: DeviceAuth, ws: WebSocketUpgrade) -> AppResult<Response> {
    this_device(&state, &auth).await?;
    Ok(ws.max_message_size(16 << 20).on_upgrade(move |socket| run(state, auth.world, auth.device_id, socket)).into_response())
}

fn text(frame: &ServiceFrame) -> Message {
    Message::Text(serde_json::to_string(frame).unwrap_or_default().into())
}

async fn greet(state: &AppState, world: &World, device_id: &str) -> Vec<ServiceFrame> {
    let mut frames = Vec::new();
    let sel = test_selection(state, world).await;
    frames.push(ServiceFrame::Environment { environment: env_view(state, sel.as_ref(), world).await });
    if let Ok(Some(d)) = domain::load_device(state, world, device_id).await
        && let (Some(sid), Some(si), Some(since)) = (&d.in_use_session, &d.in_use_silicon, d.in_use_since)
            && let Ok(session_id) = sid.parse() {
                frames.push(ServiceFrame::SessionStarted { target: None, session_id, silicon_id: si.clone(), since });
            }
    // Hosted devices: re-announce each, with its running session.
    let hosted: Vec<DeviceRow> = sqlx::query_as(sql!("{} WHERE d.host_device_id = $1 AND d.removed_at IS NULL", domain::device_select(world)))
        .bind(device_id)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();
    for h in hosted {
        let Ok(id) = h.device_id.parse() else { continue };
        frames.push(ServiceFrame::Attach { device_id: id, os: h.os(), name: h.name.clone(), address: h.address.clone(), removed: false });
        if let (Some(sid), Some(si), Some(since)) = (&h.in_use_session, &h.in_use_silicon, h.in_use_since)
            && let Ok(session_id) = sid.parse() {
                frames.push(ServiceFrame::SessionStarted { target: h.device_id.parse().ok(), session_id, silicon_id: si.clone(), since });
            }
    }
    frames
}

async fn run(state: Shared, world: World, device_id: String, socket: WebSocket) {
    let key = (world.schema.clone(), device_id.clone());
    let (conn_id, mut rx) = state.hub.register(key.clone()).await;
    let (mut sink, mut stream) = socket.split();
    let _ = sqlx::query(sql!("UPDATE {} SET last_seen_at = now() WHERE device_id = $1", world.t("devices"))).bind(&device_id).execute(&state.pool).await;
    tracing::info!(world = %world.schema, device_id, "device connected");
    for f in greet(&state, &world, &device_id).await {
        if sink.send(text(&f)).await.is_err() {
            break;
        }
    }
    let mut ping = tokio::time::interval(Duration::from_secs(bridge_protocol::HEARTBEAT_S));
    let mut last_heard = Instant::now();
    let mut last_seen_write = Instant::now();
    let mut nonce = 0u64;
    let close_with = |code: u16, reason: &'static str| Message::Close(Some(CloseFrame { code, reason: reason.into() }));
    loop {
        tokio::select! {
            out = rx.recv() => {
                let Some(frame) = out else { break };
                match frame {
                    ServiceFrame::Superseded => {
                        let _ = sink.send(text(&ServiceFrame::Superseded)).await;
                        let _ = sink.send(close_with(close::SUPERSEDED, "superseded")).await;
                        break;
                    }
                    ServiceFrame::Unpaired { .. } => {
                        let _ = sink.send(text(&frame)).await;
                        let _ = sink.send(close_with(close::UNAUTHORIZED, "unpaired")).await;
                        break;
                    }
                    other => if sink.send(text(&other)).await.is_err() { break },
                }
            }
            msg = stream.next() => {
                let Some(Ok(msg)) = msg else { break };
                last_heard = Instant::now();
                if last_seen_write.elapsed() > Duration::from_secs(10) {
                    last_seen_write = Instant::now();
                    let _ = sqlx::query(sql!("UPDATE {} SET last_seen_at = now() WHERE device_id = $1", world.t("devices"))).bind(&device_id).execute(&state.pool).await;
                }
                match msg {
                    Message::Text(t) => {
                        match serde_json::from_str::<DeviceFrame>(&t) {
                            Ok(frame) => {
                                if let Err(code) = handle(&state, &world, &device_id, &key, frame).await {
                                    let _ = sink.send(close_with(code, "refused")).await;
                                    break;
                                }
                            }
                            Err(e) => tracing::warn!(device_id, error = %e, "unreadable device frame"),
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            _ = ping.tick() => {
                if last_heard.elapsed() > Duration::from_secs(bridge_protocol::OFFLINE_AFTER_S) {
                    tracing::info!(device_id, "device stopped answering pings");
                    break;
                }
                nonce += 1;
                if sink.send(text(&ServiceFrame::Ping { nonce })).await.is_err() { break; }
            }
        }
    }
    if state.hub.unregister(&key, conn_id).await {
        let _ = sqlx::query(sql!("UPDATE {} SET last_seen_at = now() WHERE device_id = $1", world.t("devices"))).bind(&device_id).execute(&state.pool).await;
    }
    tracing::info!(world = %world.schema, device_id, "device disconnected");
}

fn allowed(os: bridge_protocol::DeviceOs, caps: Vec<Capability>) -> Vec<Capability> {
    let full = os.full_capabilities();
    let mut caps: Vec<Capability> = caps.into_iter().filter(|c| full.contains(c)).collect();
    caps.sort();
    caps.dedup();
    caps
}

fn state_of(setup: &Setup) -> &'static str {
    if setup.state == SetupState::Complete || setup.steps.is_empty() { "ready" } else { "setup" }
}

/// Handles one frame from a device. `Err(close_code)` closes the socket.
async fn handle(state: &AppState, world: &World, device_id: &str, key: &(String, String), frame: DeviceFrame) -> Result<(), u16> {
    match frame {
        DeviceFrame::Hello(h) => {
            if !version_at_least(&h.app_version, &state.cfg.device_app_min_version) {
                return Err(close::UPGRADE_REQUIRED);
            }
            let Ok(Some(d)) = domain::load_device(state, world, device_id).await else { return Err(close::UNAUTHORIZED) };
            // The OS was fixed at pairing; a hello can't turn a phone into a TV, except the phone/TV
            // pair, which the same Android app detects at runtime.
            let os = if matches!((d.os(), h.os), (bridge_protocol::DeviceOs::Android, bridge_protocol::DeviceOs::AndroidTv) | (bridge_protocol::DeviceOs::AndroidTv, bridge_protocol::DeviceOs::Android)) { h.os } else { d.os() };
            let caps = allowed(os, h.capabilities);
            let _ = sqlx::query(sql!(
                "UPDATE {} SET os = $2, os_version = $3, model = $4, app_version = $5, agent_device_version = $6, capabilities = $7, missing = $8,
                        setup = $9, state = $10, last_seen_at = now() WHERE device_id = $1",
                world.t("devices")
            ))
            .bind(device_id)
            .bind(os.as_str())
            .bind(&h.os_version)
            .bind(&h.model)
            .bind(&h.app_version)
            .bind(&h.agent_device_version)
            .bind(serde_json::to_value(&caps).unwrap_or_default())
            .bind(serde_json::to_value(&h.missing).unwrap_or_default())
            .bind(serde_json::to_value(&h.setup).unwrap_or_default())
            .bind(state_of(&h.setup))
            .execute(&state.pool)
            .await;
        }
        DeviceFrame::SetupProgress { setup } => {
            let _ = sqlx::query(sql!("UPDATE {} SET setup = $2, state = $3 WHERE device_id = $1", world.t("devices")))
                .bind(device_id)
                .bind(serde_json::to_value(&setup).unwrap_or_default())
                .bind(state_of(&setup))
                .execute(&state.pool)
                .await;
        }
        DeviceFrame::Result(outcome) => state.hub.resolve(key, outcome).await,
        DeviceFrame::Stop { target: Some(t) } => {
            if let Ok(Some(h)) = domain::load_device(state, world, t.as_str()).await {
                if h.host_device_id.as_deref() == Some(device_id) {
                    let _ = stop_running(state, world, &h).await;
                }
            }
        }
        DeviceFrame::TakeoverDone { target: Some(t) } => {
            if let Ok(Some(h)) = domain::load_device(state, world, t.as_str()).await {
                if h.host_device_id.as_deref() == Some(device_id) {
                    if let Some(sid) = &h.in_use_session {
                        let _ = super::sessions::release(state, world, sid, &owner(&h)).await;
                    }
                }
            }
        }
        DeviceFrame::Stop { target: None } => {
            if let Ok(Some(d)) = domain::load_device(state, world, device_id).await {
                let _ = stop_running(state, world, &d).await;
                // A host's Stop also stops what runs on the devices it carries.
                let hosted: Vec<DeviceRow> = sqlx::query_as(sql!("{} WHERE d.host_device_id = $1 AND d.removed_at IS NULL", domain::device_select(world)))
                    .bind(device_id)
                    .fetch_all(&state.pool)
                    .await
                    .unwrap_or_default();
                for h in hosted {
                    let _ = stop_running(state, world, &h).await;
                }
            }
        }
        DeviceFrame::TakeoverDone { target: None } => {
            if let Ok(Some(d)) = domain::load_device(state, world, device_id).await
                && let Some(sid) = &d.in_use_session {
                    let _ = super::sessions::release(state, world, sid, &owner(&d)).await;
                }
        }
        DeviceFrame::Attached(a) => {
            let child = a.device_id.to_string();
            let Ok(Some(d)) = domain::load_device(state, world, &child).await else { return Ok(()) };
            if d.host_device_id.as_deref() != Some(device_id) {
                tracing::warn!(device_id, child, "host reported a device it doesn't carry");
                return Ok(());
            }
            let caps = allowed(d.os(), a.capabilities.clone());
            state.hub.set_attached((world.schema.clone(), child.clone()), AttachedState { online: a.online, capabilities: caps.clone(), missing: a.missing.clone() }).await;
            let _ = sqlx::query(sql!(
                "UPDATE {} SET os_version = COALESCE($2, os_version), model = COALESCE($3, model), capabilities = $4, missing = $5, setup = $6, state = $7,
                        last_seen_at = CASE WHEN $8 THEN now() ELSE last_seen_at END WHERE device_id = $1",
                world.t("devices")
            ))
            .bind(&child)
            .bind(&a.os_version)
            .bind(&a.model)
            .bind(serde_json::to_value(&caps).unwrap_or_default())
            .bind(serde_json::to_value(&a.missing).unwrap_or_default())
            .bind(serde_json::to_value(&a.setup).unwrap_or_default())
            .bind(state_of(&a.setup))
            .bind(a.online)
            .execute(&state.pool)
            .await;
        }
        DeviceFrame::Pong { .. } => {}
    }
    Ok(())
}

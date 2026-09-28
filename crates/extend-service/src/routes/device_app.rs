//! Endpoints a paired Extend app uses with its device credential (docs/device-protocol.md).
//!
//! Since 1.1 an app may hold several pairs (one per Carbon who paired the device), with one
//! connection per pair, each exactly a 1.0 device connection plus the new frames. `stop`,
//! `takeover_done` and `awake` act on the physical device, whichever pair's connection they come
//! on; everything else is about that pair.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use extend_protocol::frames::{AttachedStatus, DeviceFrame, ServiceFrame, close};
use extend_protocol::model::{
    DeviceSelf, DeviceSelfPatch, EndReason, InUse, InUseIndicator, Member, MemberKind, Setup, SetupState, SleepState,
    Takeover,
};
use extend_protocol::{Capability, ErrorCode, MAX_ARTIFACT_BYTES, ids};
use futures::{SinkExt as _, StreamExt as _};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use super::devices::env_view;
use super::enroll::version_at_least;
use super::{no_content, ok};
use crate::db::World;
use crate::domain::{self, DeviceRow, Viewer};
use crate::error::{AppError, AppResult};
use crate::hub::AttachedState;
use crate::state::{AppState, DeviceAuth, Shared};

async fn this_device(state: &AppState, auth: &DeviceAuth) -> AppResult<DeviceRow> {
    domain::load_device(state, &auth.world, &auth.device_id)
        .await?
        .ok_or_else(|| AppError::new(ErrorCode::Unauthorized, "This device is no longer paired."))
}

fn carbon(id: &str) -> Member {
    Member {
        kind: MemberKind::Carbon,
        id: id.to_owned(),
        display_name: None,
    }
}

fn owner(d: &DeviceRow) -> Member {
    carbon(&d.owner_id)
}

async fn test_selection(state: &AppState, world: &World) -> Option<crate::iam::TestingSelection> {
    let id = world.environment_id?;
    let name: Option<String> =
        sqlx::query_scalar("SELECT name FROM extend_global.test_environments WHERE environment_id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();
    Some(crate::iam::TestingSelection {
        environment_id: id,
        name: name.unwrap_or_else(|| id.to_string()),
        secret: String::new(),
    })
}

/// The world's key for `hardware_key` (DeviceSelf.hardware_salt): every computer in the world
/// gets the same one, so equal keys mean the same carried device across computers.
async fn hardware_salt(state: &AppState, world: &World) -> Option<String> {
    sqlx::query_scalar(sql!(
        "SELECT value FROM {} WHERE name = 'hardware_salt'",
        world.t("world_settings")
    ))
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
}

pub async fn me(State(state): State<Shared>, auth: DeviceAuth) -> AppResult<Response> {
    let d = this_device(&state, &auth).await?;
    self_view(&state, &auth.world, &d).await
}

async fn self_view(state: &Shared, world: &World, d: &DeviceRow) -> AppResult<Response> {
    let view = domain::device_view(state, world, d, Viewer::owner(d), false).await;
    // Only a session running through this pair: another Carbon's is theirs to show.
    let session = match d.session_here() {
        Some(s) => domain::load_session(state, world, s).await?,
        None => None,
    };
    let takeover: Option<Takeover> = session
        .as_ref()
        .filter(|s| s.state == "paused")
        .and_then(|s| s.takeover.clone())
        .and_then(|t| serde_json::from_value(t).ok());
    let sel = test_selection(state, world).await;
    Ok(ok(
        "device_self",
        DeviceSelf {
            device_id: view.device_id,
            name: d.name.clone(),
            owner: owner(d),
            team: d.team.clone(),
            os: d.os(),
            in_use: view.in_use.map(|u| InUse { team: None, ..u }),
            takeover,
            setup: d.setup(),
            environment: env_view(state, sel.as_ref(), world).await,
            instance_id: Some(d.instance_id),
            hardware_salt: if d.is_computer() {
                hardware_salt(state, world).await
            } else {
                None
            },
            first_pair: Some(d.first_pair),
            in_use_indicator: d.in_use_indicator(),
        },
    ))
}

/// `PATCH /api/v1/device`: the device's own Extend app changes the device's settings (today only
/// `in_use_indicator`), with any of its pair credentials. The setting belongs to the physical
/// device, so it changes for every pair of it; answers the device as `GET /api/v1/device` would.
pub async fn update(State(state): State<Shared>, auth: DeviceAuth, body: Bytes) -> AppResult<Response> {
    let d = this_device(&state, &auth).await?;
    update_indicator(&state, &auth.world, &d, body).await?;
    me(State(state), auth).await
}

/// A host's native app can change the banner only for a device carried by that exact pair.
pub async fn update_attached(
    State(state): State<Shared>,
    auth: DeviceAuth,
    Path(id): Path<String>,
    body: Bytes,
) -> AppResult<Response> {
    let host = this_device(&state, &auth).await?;
    let value = indicator_patch(body)?;
    let d = domain::load_device(&state, &auth.world, &id)
        .await?
        .filter(|d| d.host_device_id.as_deref() == Some(auth.device_id.as_str()))
        .ok_or_else(|| AppError::new(ErrorCode::DeviceNotFound, "This device is not carried by this pair."))?;
    // Removal and physical-device relinking take these locks too. Re-check authorization after
    // taking them, so a request queued behind removal cannot change another pair's device.
    let mut tx = state.pool.begin().await?;
    domain::lock_instances(&mut tx, &auth.world, &[host.instance_id, d.instance_id]).await?;
    let current_host = domain::load_device_in(&mut *tx, &auth.world, &auth.device_id).await?;
    let current = domain::load_device_in(&mut *tx, &auth.world, &id).await?;
    if current_host.as_ref().is_none_or(|h| h.instance_id != host.instance_id)
        || current.as_ref().is_none_or(|c| {
            c.instance_id != d.instance_id || c.host_device_id.as_deref() != Some(auth.device_id.as_str())
        })
    {
        return Err(AppError::new(
            ErrorCode::DeviceNotFound,
            "This device is no longer carried by this pair.",
        ));
    }
    let changed = domain::update_in_use_indicator(&mut tx, &auth.world, d.instance_id, value, None).await?;
    tx.commit().await?;
    if changed {
        domain::notify_in_use_indicator(
            &state,
            &auth.world,
            d.instance_id,
            value,
            domain::BannerChangedBy::Device,
        )
        .await?;
    }
    let d = domain::load_device(&state, &auth.world, &id)
        .await?
        .ok_or_else(|| AppError::new(ErrorCode::DeviceNotFound, "This device is no longer paired."))?;
    self_view(&state, &auth.world, &d).await
}

async fn update_indicator(state: &Shared, world: &World, d: &DeviceRow, body: Bytes) -> AppResult<()> {
    let value = indicator_patch(body)?;
    domain::set_in_use_indicator(state, world, d.instance_id, value, domain::BannerChangedBy::Device).await?;
    Ok(())
}

fn indicator_patch(body: Bytes) -> AppResult<InUseIndicator> {
    // The usual envelope `{"type": "device_self", "data": {...}}`, or the bare object.
    let v: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| AppError::invalid(format!("The body is not valid JSON: {e}")))?;
    let data = if v.get("type").is_some() && v.get("data").is_some() {
        v["data"].clone()
    } else {
        v
    };
    let patch: DeviceSelfPatch =
        serde_json::from_value(data).map_err(|e| AppError::invalid(format!("The body's data is not valid: {e}")))?;
    let Some(value) = patch.in_use_indicator else {
        return Err(AppError::invalid("Send in_use_indicator: \"shown\" or \"hidden\"."));
    };
    if value == InUseIndicator::Other {
        return Err(AppError::invalid("in_use_indicator is \"shown\" or \"hidden\"."));
    }
    Ok(value)
}

pub async fn revoke(State(state): State<Shared>, auth: DeviceAuth) -> AppResult<Response> {
    let d = this_device(&state, &auth).await?;
    domain::unpair(&state, &auth.world, &auth.device_id, EndReason::PairRevoked, &owner(&d)).await?;
    Ok(no_content())
}

/// "Pair with another Carbon": a pairing code that adds a pair to this physical device, in this
/// pair's world. Only a Carbon can claim it.
pub async fn enrollments_create(
    State(state): State<Shared>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    auth: DeviceAuth,
) -> AppResult<Response> {
    let d = this_device(&state, &auth).await?;
    let client = crate::config::client_ip(addr.ip(), &headers, &state.cfg.trusted_proxies);
    state
        .rate_limit(
            format!("enroll:{client}"),
            60,
            Duration::from_secs(3600),
            "new enrollments from this address",
        )
        .await?;
    let pairs: i64 = sqlx::query_scalar(sql!(
        "SELECT count(*) FROM {} WHERE instance_id = $1 AND removed_at IS NULL",
        auth.world.t("devices")
    ))
    .bind(d.instance_id)
    .fetch_one(&state.pool)
    .await?;
    if pairs >= state.cfg.tuning.max_pairs_per_device {
        return Err(super::devices::max_pairs_error(pairs));
    }
    let waiting: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM extend_global.enrollments WHERE instance_id = $1 AND paired_device_id IS NULL AND code_expires_at > now()",
    )
    .bind(d.instance_id)
    .fetch_one(&state.pool)
    .await?;
    if waiting >= 3 {
        return Err(AppError::new(
            ErrorCode::RateLimited,
            "This device already shows 3 codes for other Carbons; use one of them, or wait until they expire.",
        )
        .hint("Codes expire after 5 minutes.")
        .details(serde_json::json!({"retry_after_s": extend_protocol::PAIRING_CODE_TTL_S})));
    }
    let created = super::enroll::insert_enrollment(
        &state,
        &auth.world,
        super::enroll::NewEnrollment {
            os: d.os(),
            os_version: d.os_version.clone(),
            model: d.model.clone(),
            app_version: d.app_version.clone().unwrap_or_else(|| "1.1.0".into()),
            engine_version: d.engine_version.clone(),
            instance: Some(d.instance_id),
            from_device_id: Some(d.device_id.clone()),
        },
    )
    .await?;
    tracing::info!(world = %auth.world.schema, device_id = d.device_id, "pairing with another Carbon started");
    Ok(super::enroll::created_response(created))
}

/// Ends a session from the device itself. The actor is the Carbon of the pair it runs through, so
/// that pair's log names only its own Carbon.
async fn stop_session(state: &AppState, world: &World, session_id: &str) -> AppResult<()> {
    let Some(s) = domain::load_session(state, world, session_id).await? else {
        return Ok(());
    };
    let actor = match domain::load_device_any(state, world, &s.device_id).await? {
        Some(pair) => owner(&pair),
        None => domain::system_member(),
    };
    domain::end_session(state, world, session_id, EndReason::StoppedByCarbon, &actor).await?;
    Ok(())
}

/// The device's own Stop: the physical device's session, whichever pair it runs through, and the
/// sessions on the devices carried by any pair of it (the contract puts the indicator and the stop
/// for carried devices on the computer).
async fn stop_everything(state: &AppState, world: &World, d: &DeviceRow) -> AppResult<()> {
    if let Some(sid) = &d.in_use_session {
        stop_session(state, world, sid).await?;
    }
    for b in d.carried_busy() {
        stop_session(state, world, &b.session_id).await?;
    }
    Ok(())
}

pub async fn stop(State(state): State<Shared>, auth: DeviceAuth) -> AppResult<Response> {
    let d = this_device(&state, &auth).await?;
    stop_everything(&state, &auth.world, &d).await?;
    Ok(no_content())
}

pub async fn upload(
    State(state): State<Shared>,
    auth: DeviceAuth,
    Path(upload_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    let h = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let digest = h("x-content-sha256")
        .ok_or_else(|| AppError::invalid("X-Content-SHA256 is required: the 64 lowercase hex SHA-256 of the bytes."))?;
    let name = h("x-file-name")
        .filter(|n| !n.is_empty() && n.len() <= 255 && !n.contains('/') && !n.contains('\\'))
        .ok_or_else(|| {
            AppError::invalid("X-File-Name is required: a plain file name up to 255 characters, no slashes.")
        })?;
    let content_type = h("content-type").unwrap_or_else(|| "application/octet-stream".into());
    if body.len() as u64 > MAX_ARTIFACT_BYTES {
        return Err(AppError::new(ErrorCode::PayloadTooLarge, "Files are limited to 1 GiB."));
    }
    let actual = ids::hex_lower(&Sha256::digest(&body));
    if actual != digest.to_ascii_lowercase() {
        return Err(AppError::invalid(format!(
            "X-Content-SHA256 {digest} does not match the bytes received ({actual})."
        )));
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
        return Err(AppError::new(
            ErrorCode::FileNotFound,
            "That upload id is unknown, already used, expired, or belongs to another device.",
        ));
    }
    let dir = state.cfg.data_dir.join("uploads");
    tokio::fs::create_dir_all(&dir).await.map_err(AppError::internal)?;
    tokio::fs::write(dir.join(upload_id.to_string()), &body)
        .await
        .map_err(AppError::internal)?;
    Ok(axum::http::StatusCode::CREATED.into_response())
}

pub async fn socket(State(state): State<Shared>, auth: DeviceAuth, ws: WebSocketUpgrade) -> AppResult<Response> {
    this_device(&state, &auth).await?;
    // The fence guards the handshake only; a live socket must not hold a clean or disable back.
    let DeviceAuth {
        world,
        device_id,
        fence,
    } = auth;
    drop(fence);
    Ok(ws
        .max_message_size(16 << 20)
        .on_upgrade(move |socket| run(state, world, device_id, socket))
        .into_response())
}

/// Why the service dropped a device's socket, when it's because its test environment closed
/// (the pair stays): the close reason the device gets with `close::ENVIRONMENT_UNAVAILABLE`.
async fn closed_environment(state: &AppState, world: &World) -> Option<&'static str> {
    let env = world.environment_id?;
    match state.environment(env).await.ok()??.state.as_str() {
        "disabled" => Some("test environment disabled; still paired, reconnect later"),
        "preparing" => Some("test environment not ready; still paired, reconnect later"),
        _ => None,
    }
}

fn text(frame: &ServiceFrame) -> Message {
    Message::Text(serde_json::to_string(frame).unwrap_or_default().into())
}

async fn session_started(state: &AppState, world: &World, h: &DeviceRow, target: bool) -> Option<ServiceFrame> {
    let (Some(sid), Some(si), Some(since), Some(team)) =
        (&h.in_use_session, &h.in_use_silicon, h.in_use_since, &h.in_use_team)
    else {
        return None;
    };
    if !h.held_here() {
        return None;
    }
    Some(ServiceFrame::SessionStarted {
        target: if target { h.device_id.parse().ok() } else { None },
        session_id: sid.parse().ok()?,
        silicon_id: si.clone(),
        since,
        side: domain::side_of(state, world, h, team).await.ok(),
    })
}

/// What a pair's connection is told when it connects: its environment, the session running
/// through it, the devices it carries with theirs, the open wake requests on them, and a new
/// credential when a rotation is owed. It reconciles only this pair's own devices.
async fn greet(state: &AppState, world: &World, device_id: &str) -> Vec<ServiceFrame> {
    let mut frames = Vec::new();
    let sel = test_selection(state, world).await;
    frames.push(ServiceFrame::Environment {
        environment: env_view(state, sel.as_ref(), world).await,
    });
    if let Ok(Some(d)) = domain::load_device(state, world, device_id).await
        && let Some(f) = session_started(state, world, &d, false).await
    {
        frames.push(f);
    }
    // Reconcile this exact host pair, including removals it missed while offline. A missing
    // attach is not a deletion (the greeting query can fail), so existing apps need an explicit
    // tombstone. Other host pairs' aliases and sessions are never announced on this connection.
    let hosted: Vec<DeviceRow> = sqlx::query_as(sql!("{} WHERE d.host_device_id = $1", domain::device_select(world)))
        .bind(device_id)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();
    for h in hosted {
        if h.device_id.parse::<extend_protocol::DeviceId>().is_err() {
            continue;
        }
        let removed = h.removed_at.is_some();
        frames.push(h.attach_frame(removed));
        if !removed && let Some(f) = session_started(state, world, &h, true).await {
            frames.push(f);
        }
    }
    frames.extend(crate::wake::greeting(state, world, device_id).await);
    frames
}

async fn run(state: Shared, world: World, device_id: String, socket: WebSocket) {
    let key = (world.schema.clone(), device_id.clone());
    let (conn_id, mut rx, replaced_live) = state.hub.register(key.clone()).await;
    let (mut sink, mut stream) = socket.split();
    let _ = sqlx::query(sql!(
        "UPDATE {} SET last_seen_at = now() WHERE device_id = $1",
        world.t("devices")
    ))
    .bind(&device_id)
    .execute(&state.pool)
    .await;
    tracing::info!(world = %world.schema, device_id, "device connected");
    connected(&state, &world, &device_id, replaced_live).await;
    for f in greet(&state, &world, &device_id).await {
        if sink.send(text(&f)).await.is_err() {
            break;
        }
    }
    // A rotation owed while this pair was offline: a new credential, through the hub (so the loop
    // below records which credential this connection was given).
    let owed: bool = sqlx::query_scalar(sql!(
        "SELECT next_credential_digest IS NOT NULL FROM {} WHERE device_id = $1",
        world.t("devices")
    ))
    .bind(&device_id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(false);
    if owed {
        let _ = domain::send_new_credential(&state, &world, &device_id).await;
    }
    let mut ping = tokio::time::interval(Duration::from_secs(extend_protocol::HEARTBEAT_S));
    let mut last_heard = Instant::now();
    let mut last_seen_write = Instant::now();
    let mut nonce = 0u64;
    // The digest of the last credential this connection was given, for `credential_saved`.
    let mut given: Option<String> = None;
    let close_with = |code: u16, reason: &'static str| {
        Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        }))
    };
    loop {
        tokio::select! {
            out = rx.recv() => {
                let Some(frame) = out else {
                    // The service dropped this socket; when that's because the test environment
                    // closed, say so with a code that keeps the pair.
                    if let Some(reason) = closed_environment(&state, &world).await {
                        let _ = sink.send(close_with(close::ENVIRONMENT_UNAVAILABLE, reason)).await;
                    }
                    break;
                };
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
                    ServiceFrame::Credential { ref device_credential } => {
                        // Never logged: the frame carries a live credential.
                        given = Some(ids::secret_digest(device_credential.expose()));
                        if sink.send(text(&frame)).await.is_err() { break }
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
                                let ctx = Conn { key: &key, conn_id, given: given.as_deref() };
                                if let Err(code) = handle(&state, &world, &device_id, &ctx, frame).await {
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
                if last_heard.elapsed() > Duration::from_secs(extend_protocol::OFFLINE_AFTER_S) {
                    tracing::info!(device_id, "device stopped answering pings");
                    break;
                }
                nonce += 1;
                if sink.send(text(&ServiceFrame::Ping { nonce })).await.is_err() { break; }
            }
        }
    }
    if state.hub.unregister(&key, conn_id).await {
        let _ = sqlx::query(sql!(
            "UPDATE {} SET last_seen_at = now() WHERE device_id = $1",
            world.t("devices")
        ))
        .bind(&device_id)
        .execute(&state.pool)
        .await;
    }
    tracing::info!(world = %world.schema, device_id, "device disconnected");
}

/// A pair's socket connected: the device's awake state is unknown again unless another pair's
/// socket of the same device is connected (and so already reported it), and a connection that
/// took over a live one is logged on the pair.
async fn connected(state: &AppState, world: &World, device_id: &str, replaced_live: bool) {
    let Ok(Some(d)) = domain::load_device(state, world, device_id).await else {
        return;
    };
    let siblings: Vec<(String,)> = sqlx::query_as(sql!(
        "SELECT device_id FROM {} WHERE instance_id = $1 AND removed_at IS NULL AND device_id <> $2 AND host_device_id IS NULL",
        world.t("devices")
    ))
    .bind(d.instance_id)
    .bind(device_id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    let mut sibling_connected = false;
    for (s,) in &siblings {
        if state.hub.is_connected(&(world.schema.clone(), s.clone())).await {
            sibling_connected = true;
            break;
        }
    }
    if !sibling_connected {
        let _ = sqlx::query(sql!(
            "UPDATE {} SET awake = NULL WHERE instance_id = $1",
            world.t("device_instances")
        ))
        .bind(d.instance_id)
        .execute(&state.pool)
        .await;
    }
    if replaced_live {
        tracing::warn!(world = %world.schema, device_id, "a new connection took over a pair's live connection");
        domain::log(
            state,
            world,
            device_id,
            &domain::system_member(),
            "connection_replaced",
            None,
            serde_json::json!({"while_in_use_by_other": d.in_use_session.is_some() && !d.held_here()}),
        )
        .await;
    }
}

fn allowed(os: extend_protocol::DeviceOs, caps: Vec<Capability>) -> Vec<Capability> {
    let full = os.full_capabilities();
    let mut caps: Vec<Capability> = caps.into_iter().filter(|c| full.contains(c)).collect();
    caps.sort();
    caps.dedup();
    caps
}

fn state_of(setup: &Setup) -> &'static str {
    if setup.state == SetupState::Complete || setup.steps.is_empty() {
        "ready"
    } else {
        "setup"
    }
}

/// The connection a frame came on.
struct Conn<'a> {
    key: &'a (String, String),
    conn_id: Uuid,
    /// The digest of the last credential sent on it.
    given: Option<&'a str>,
}

/// Handles one frame from a device. `Err(close_code)` closes the socket.
async fn handle(
    state: &Shared,
    world: &World,
    device_id: &str,
    conn: &Conn<'_>,
    frame: DeviceFrame,
) -> Result<(), u16> {
    match frame {
        DeviceFrame::Hello(h) => {
            if !version_at_least(&h.app_version, &state.cfg.device_app_min_version) {
                return Err(close::UPGRADE_REQUIRED);
            }
            let Ok(Some(d)) = domain::load_device(state, world, device_id).await else {
                return Err(close::UNAUTHORIZED);
            };
            state.hub.set_features(conn.key, conn.conn_id, h.features.clone()).await;
            // The OS was fixed at pairing; a hello can't turn a phone into a TV, except the phone/TV
            // pair, which the same Android app detects at runtime.
            let os = if matches!(
                (d.os(), h.os),
                (extend_protocol::DeviceOs::Android, extend_protocol::DeviceOs::AndroidTv)
                    | (extend_protocol::DeviceOs::AndroidTv, extend_protocol::DeviceOs::Android)
            ) {
                h.os
            } else {
                d.os()
            };
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
            .bind(&h.engine_version)
            .bind(serde_json::to_value(&caps).unwrap_or_default())
            .bind(serde_json::to_value(&h.missing).unwrap_or_default())
            .bind(serde_json::to_value(&h.setup).unwrap_or_default())
            .bind(state_of(&h.setup))
            .execute(&state.pool)
            .await;
        }
        DeviceFrame::SetupProgress { setup } => {
            let _ = sqlx::query(sql!(
                "UPDATE {} SET setup = $2, state = $3 WHERE device_id = $1",
                world.t("devices")
            ))
            .bind(device_id)
            .bind(serde_json::to_value(&setup).unwrap_or_default())
            .bind(state_of(&setup))
            .execute(&state.pool)
            .await;
        }
        DeviceFrame::Result(outcome) => state.hub.resolve(conn.key, outcome).await,
        DeviceFrame::Stop { target: Some(t) } => {
            if let Ok(Some(h)) = domain::load_device(state, world, t.as_str()).await
                && h.host_device_id.as_deref() == Some(device_id)
                && let Some(sid) = &h.in_use_session
            {
                let _ = stop_session(state, world, sid).await;
            }
        }
        DeviceFrame::TakeoverDone { target: Some(t) } => {
            if let Ok(Some(h)) = domain::load_device(state, world, t.as_str()).await
                && h.host_device_id.as_deref() == Some(device_id)
                && let Some(sid) = &h.in_use_session
            {
                let _ = super::sessions::release(state, world, sid, &owner(&h)).await;
            }
        }
        DeviceFrame::Stop { target: None } => {
            if let Ok(Some(d)) = domain::load_device(state, world, device_id).await {
                let _ = stop_everything(state, world, &d).await;
            }
        }
        DeviceFrame::TakeoverDone { target: None } => {
            // The physical device's paused session, whichever pair it runs through.
            if let Ok(Some(d)) = domain::load_device(state, world, device_id).await
                && let Some(sid) = &d.in_use_session
            {
                let actor = match d.in_use_carbon.as_deref() {
                    Some(c) => carbon(c),
                    None => owner(&d),
                };
                let _ = super::sessions::release(state, world, sid, &actor).await;
            }
        }
        DeviceFrame::Attached(a) => attached(state, world, device_id, conn, a).await,
        DeviceFrame::Pong { .. } => state.hub.pong(conn.key, conn.conn_id).await,
        DeviceFrame::Awake {
            awake,
            sleep_state,
            input_seen,
            run,
            seq,
        } => {
            if let Ok(Some(d)) = domain::load_device(state, world, device_id).await {
                apply_awake(state, world, d.instance_id, awake, sleep_state, input_seen, run, seq).await;
            }
        }
        DeviceFrame::WakeRequestShown { wake_id, shown, note } => {
            wake_request_shown(state, world, device_id, wake_id, shown, note).await;
        }
        DeviceFrame::CredentialSaved => {
            if let Some(digest) = conn.given {
                let promoted = sqlx::query(sql!(
                    "UPDATE {} SET credential_digest = next_credential_digest, next_credential_digest = NULL
                     WHERE device_id = $1 AND next_credential_digest = $2",
                    world.t("devices")
                ))
                .bind(device_id)
                .bind(digest)
                .execute(&state.pool)
                .await
                .map(|r| r.rows_affected())
                .unwrap_or(0);
                if promoted == 1 {
                    tracing::info!(world = %world.schema, device_id, "the device saved its new credential; the old one no longer works");
                }
            }
        }
    }
    Ok(())
}

/// Applies an `awake` report to a physical device (and, from a host's `attached`, to a carried
/// one). Reports from the app's several connections are ordered by (run, seq): a new run is
/// authoritative; in the same run only a larger seq applies; a report without a run always does.
/// A wake with evidence (input_seen not false) ends every open wake request on the device.
#[allow(clippy::too_many_arguments)]
pub async fn apply_awake(
    state: &Shared,
    world: &World,
    instance: Uuid,
    awake: bool,
    sleep_state: Option<SleepState>,
    input_seen: Option<bool>,
    run: Option<Uuid>,
    seq: Option<u64>,
) {
    let result = async {
        let mut tx = state.pool.begin().await?;
        let row: Option<(Option<bool>, Option<Uuid>, Option<i64>)> = sqlx::query_as(sql!(
            "SELECT awake, awake_run, awake_seq FROM {} WHERE instance_id = $1 FOR NO KEY UPDATE",
            world.t("device_instances")
        ))
        .bind(instance)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((old, old_run, old_seq)) = row else {
            return AppResult::Ok(false);
        };
        let seq = seq.map(|s| i64::try_from(s).unwrap_or(i64::MAX));
        let applies = match (run, old_run) {
            (None, _) => true,
            (Some(r), Some(o)) if r == o => seq.is_some_and(|s| old_seq.is_none_or(|o| s > o)),
            (Some(_), _) => true,
        };
        if !applies {
            return Ok(false);
        }
        sqlx::query(sql!(
            "UPDATE {} SET awake = $2, sleep_state = CASE WHEN $2 THEN NULL ELSE $3 END,
                    awake_run = COALESCE($4, awake_run), awake_seq = CASE WHEN $4 IS NULL THEN awake_seq ELSE $5 END,
                    awake_changed_at = CASE WHEN awake IS DISTINCT FROM $2 THEN now() ELSE awake_changed_at END
             WHERE instance_id = $1",
            world.t("device_instances")
        ))
        .bind(instance)
        .bind(awake)
        .bind(sleep_state.map(SleepState::as_str))
        .bind(run)
        .bind(seq)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let _ = old;
        Ok(true)
    }
    .await;
    match result {
        Ok(true) if awake => {
            let (state, world) = (state.clone(), world.clone());
            tokio::spawn(async move {
                let Some(_fence) = state.world_open(&world).await else {
                    return;
                };
                if input_seen == Some(false) {
                    // Awake without an unlock or real input counts for nothing; the owners of the
                    // pairs with open requests can see it happened.
                    let pairs: Vec<(String,)> = sqlx::query_as(sql!(
                        "SELECT DISTINCT device_id FROM {} WHERE instance_id = $1 AND state = 'open'",
                        world.t("wake_requests")
                    ))
                    .bind(instance)
                    .fetch_all(&state.pool)
                    .await
                    .unwrap_or_default();
                    for (p,) in pairs {
                        domain::log(
                            &state,
                            &world,
                            &p,
                            &domain::system_member(),
                            "woke_without_input",
                            None,
                            serde_json::json!({}),
                        )
                        .await;
                    }
                } else {
                    crate::wake::resolve_woken(&state, &world, instance, crate::wake::WokenHow::Device).await;
                }
            });
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!(world = %world.schema, instance = %instance, error = %e, "recording a device's awake state failed")
        }
    }
}

/// The device says whether it could show a wake request. Only a request on the sending pair's
/// lock group changes.
async fn wake_request_shown(
    state: &AppState,
    world: &World,
    device_id: &str,
    wake_id: Uuid,
    shown: bool,
    note: Option<String>,
) {
    let Ok(Some(d)) = domain::load_device(state, world, device_id).await else {
        return;
    };
    let Ok(group) = domain::lock_group(&state.pool, world, d.instance_id).await else {
        return;
    };
    let note: Option<String> = note.map(|n| n.chars().take(extend_protocol::WAKE_NOTE_MAX_CHARS).collect());
    let _ = sqlx::query(sql!(
        "UPDATE {} SET device_notice = $3, device_notice_note = $4
         WHERE wake_id = $1 AND instance_id = ANY($2) AND state = 'open'",
        world.t("wake_requests")
    ))
    .bind(wake_id)
    .bind(&group.members)
    .bind(if shown { "shown" } else { "not_shown" })
    .bind(&note)
    .execute(&state.pool)
    .await;
}

/// A host's report on a device it carries: its state, whether it is awake, and the keyed hardware
/// id that recognises the same physical device across Carbons' pairs.
async fn attached(state: &Shared, world: &World, device_id: &str, conn: &Conn<'_>, a: AttachedStatus) {
    let child = a.device_id.to_string();
    let Ok(Some(d)) = domain::load_device(state, world, &child).await else {
        return;
    };
    if d.host_device_id.as_deref() != Some(device_id) {
        tracing::warn!(device_id, child, "host reported a device it doesn't carry");
        return;
    }
    let caps = allowed(d.os(), a.capabilities.clone());
    if !state
        .hub
        .set_attached(
            conn.key,
            conn.conn_id,
            (world.schema.clone(), child.clone()),
            AttachedState {
                online: a.online,
                capabilities: caps.clone(),
                missing: a.missing.clone(),
            },
        )
        .await
    {
        return;
    }
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
    if let Some(key) = a.hardware_key.as_deref().filter(|k| !k.is_empty() && k.len() <= 128)
        && let Err(e) = recognise(state, world, &d, key).await
    {
        tracing::error!(world = %world.schema, child, error = %e, "recognising a carried device failed");
    }
    if let Some(awake) = a.awake
        && let Ok(Some(d)) = domain::load_device(state, world, &child).await
    {
        apply_awake(state, world, d.instance_id, awake, a.sleep_state, None, None, None).await;
    }
}

/// Recognises a carried pair `c` from its host's keyed hardware id (docs/device-protocol.md 2):
/// - the same Carbon carries it twice: the later-paired one is refused, naming the other;
/// - another Carbon carries it through a pair of the same computer: the two pairs become one
///   physical device (when neither is in use);
/// - it is carried through another computer: the later-paired one is refused, naming no one, so
///   one Silicon at a time holds across computers.
async fn recognise(state: &Shared, world: &World, c: &DeviceRow, key: &str) -> AppResult<()> {
    let devices = world.t("devices");
    sqlx::query(sql!(
        "UPDATE {devices} SET hardware_key = $2 WHERE device_id = $1 AND hardware_key IS DISTINCT FROM $2"
    ))
    .bind(&c.device_id)
    .bind(key)
    .execute(&state.pool)
    .await?;
    let host_instance: Option<Uuid> =
        sqlx::query_scalar(sql!("SELECT instance_id FROM {devices} WHERE device_id = $1"))
            .bind(&c.host_device_id)
            .fetch_optional(&state.pool)
            .await?;
    let Some(host_instance) = host_instance else {
        return Ok(());
    };
    // The other live carried pairs with the same key, with their host's instance.
    let others: Vec<(String, String, Uuid, Uuid, time::OffsetDateTime)> = sqlx::query_as(sql!(
        "SELECT o.device_id, o.owner_id, o.instance_id, h.instance_id, o.paired_at FROM {devices} o
           JOIN {devices} h ON h.device_id = o.host_device_id
         WHERE o.hardware_key = $1 AND o.device_id <> $2 AND o.removed_at IS NULL AND o.host_device_id IS NOT NULL"
    ))
    .bind(key)
    .bind(&c.device_id)
    .fetch_all(&state.pool)
    .await?;
    let refuse = |other: &str, kind: &str| {
        let mark = if kind == "same_carbon" {
            format!("own:{other}")
        } else {
            "other_computer".to_owned()
        };
        (mark, kind.to_owned())
    };
    let mut verdict: Option<(String, String, String)> = None; // (pair to mark, mark, kind)
    if let Some((o, _, _, _, at)) = others.iter().find(|(_, owner, ..)| *owner == c.owner_id) {
        // The later-paired one is refused.
        let (mark_pair, other) = if c.paired_at > *at {
            (&c.device_id, o)
        } else {
            (o, &c.device_id)
        };
        let (mark, kind) = refuse(other, "same_carbon");
        verdict = Some((mark_pair.clone(), mark, kind));
    } else if let Some((o, _, o_instance, _, _)) = others
        .iter()
        .find(|(_, owner, _, host, _)| *owner != c.owner_id && *host == host_instance)
    {
        if *o_instance != c.instance_id {
            link(state, world, c, o, *o_instance).await?;
        }
    } else if let Some((o, _, _, _, at)) = others.iter().find(|(_, _, _, host, _)| *host != host_instance) {
        let (mark_pair, other) = if c.paired_at > *at {
            (&c.device_id, o)
        } else {
            (o, &c.device_id)
        };
        let (mark, kind) = refuse(other, "other_computer");
        verdict = Some((mark_pair.clone(), mark, kind));
    } else {
        sqlx::query(sql!(
            "UPDATE {devices} SET duplicate = NULL WHERE device_id = $1 AND duplicate IS NOT NULL"
        ))
        .bind(&c.device_id)
        .execute(&state.pool)
        .await?;
    }
    if let Some((pair, mark, kind)) = verdict {
        let changed = sqlx::query(sql!(
            "UPDATE {devices} SET duplicate = $2 WHERE device_id = $1 AND duplicate IS DISTINCT FROM $2"
        ))
        .bind(&pair)
        .bind(&mark)
        .execute(&state.pool)
        .await?
        .rows_affected();
        if pair != c.device_id {
            sqlx::query(sql!(
                "UPDATE {devices} SET duplicate = NULL WHERE device_id = $1 AND duplicate IS NOT NULL"
            ))
            .bind(&c.device_id)
            .execute(&state.pool)
            .await?;
        }
        if changed > 0 {
            domain::log(
                state,
                world,
                &pair,
                &domain::system_member(),
                "duplicate_device",
                None,
                serde_json::json!({"kind": kind}),
            )
            .await;
            tracing::info!(world = %world.schema, device_id = pair, kind, "a carried device was refused as a duplicate");
        }
    }
    Ok(())
}

/// Makes carried pair `c` part of `target`'s physical device, in the lock order, when neither is
/// in use (the next report tries again otherwise).
async fn link(state: &AppState, world: &World, c: &DeviceRow, other: &str, target: Uuid) -> AppResult<()> {
    let mut tx = state.pool.begin().await?;
    domain::lock_instances(&mut tx, world, &[c.instance_id, target]).await?;
    let held: i64 = sqlx::query_scalar(sql!(
        "SELECT count(*) FROM {} WHERE instance_id = ANY($1)",
        world.t("device_locks")
    ))
    .bind(vec![c.instance_id, target])
    .fetch_one(&mut *tx)
    .await?;
    if held > 0 {
        return Ok(());
    }
    // A Carbon has one pair of a device.
    let clash: bool = sqlx::query_scalar(sql!(
        "SELECT EXISTS (SELECT 1 FROM {} WHERE instance_id = $1 AND owner_id = $2 AND removed_at IS NULL)",
        world.t("devices")
    ))
    .bind(target)
    .bind(&c.owner_id)
    .fetch_one(&mut *tx)
    .await?;
    if clash {
        return Ok(());
    }
    sqlx::query(sql!(
        "UPDATE {} SET instance_id = $2, provisional_until = NULL, duplicate = NULL WHERE device_id = $1",
        world.t("devices")
    ))
    .bind(&c.device_id)
    .bind(target)
    .execute(&mut *tx)
    .await?;
    // Its open wake requests follow it (unless the same Silicon already asked on the device).
    sqlx::query(sql!(
        "UPDATE {w} x SET instance_id = $2 WHERE x.device_id = $1 AND x.state = 'open'
           AND NOT EXISTS (SELECT 1 FROM {w} y WHERE y.instance_id = $2 AND y.state = 'open' AND y.team = x.team AND y.from_id = x.from_id)",
        w = world.t("wake_requests")
    ))
    .bind(&c.device_id)
    .bind(target)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let _ = other;
    // It now shares the device's in-use banner setting: tell its computer when that differs.
    if let Ok(Some(n)) = domain::load_device(state, world, &c.device_id).await
        && n.in_use_indicator != c.in_use_indicator
    {
        let _ = state.hub.send(&n.route(world), n.attach_frame(false)).await;
    }
    domain::log(
        state,
        world,
        &c.device_id,
        &domain::system_member(),
        "device_linked",
        None,
        serde_json::json!({}),
    )
    .await;
    tracing::info!(world = %world.schema, device_id = c.device_id, "a carried device was recognised as one another Carbon carries through the same computer");
    Ok(())
}

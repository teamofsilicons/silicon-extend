//! An unpaired Extend app getting a pairing code, following its rotations, and receiving its
//! credential once a Carbon claims the code (docs/device-protocol.md section 1).

use std::net::SocketAddr;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use extend_protocol::frames::EnrollmentFrame;
use extend_protocol::model::{EnrollmentCreate, EnrollmentCreated, EnrollmentState, TestingEnvironment};
use extend_protocol::{DeviceOs, ErrorCode, PAIRING_CODE_TTL_S, PairingCode, ids};
use futures::{SinkExt as _, StreamExt as _};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{Body, created, no_content, ok};
use crate::error::{AppError, AppResult};
use crate::state::{AppState, Sel, Selection, Shared};

pub const APP_OSES: [DeviceOs; 5] = [
    DeviceOs::Android,
    DeviceOs::AndroidTv,
    DeviceOs::Macos,
    DeviceOs::Windows,
    DeviceOs::Linux,
];

/// `1.2.3` style comparison; anything unparsable counts as 0.
pub fn version_at_least(v: &str, min: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split(['.', '-', '+'])
            .take(3)
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parse(v) >= parse(min)
}

/// Starts an enrollment in the world the request selects: an app started with a test
/// environment's app_secret gets a code that pairs it only into that environment (and one started
/// without gets a production code).
pub async fn create(
    State(state): State<Shared>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Sel { world, .. }: Sel,
    Body(input): Body<EnrollmentCreate>,
) -> AppResult<Response> {
    let client = crate::config::client_ip(addr.ip(), &headers, &state.cfg.trusted_proxies);
    state
        .rate_limit(
            format!("enroll:{client}"),
            60,
            Duration::from_secs(3600),
            "new enrollments from this address",
        )
        .await?;
    if !APP_OSES.contains(&input.os) {
        return Err(AppError::invalid(format!(
            "{} devices don't run the Extend app; they pair through a paired Mac or computer.",
            input.os.as_str()
        )));
    }
    if !version_at_least(&input.app_version, &state.cfg.device_app_min_version) {
        return Err(AppError::new(
            ErrorCode::UpgradeRequired,
            format!(
                "Extend app {} is too old; the oldest supported is {}.",
                input.app_version, state.cfg.device_app_min_version
            ),
        )
        .hint("Download the latest Extend app from extend.teamofsilicons.com."));
    }
    let id = Uuid::now_v7();
    let secret = ids::new_secret(ids::ENROLLMENT_SECRET_PREFIX);
    let expires = OffsetDateTime::now_utc() + time::Duration::seconds(PAIRING_CODE_TTL_S);
    let mut code = PairingCode::random();
    for attempt in 0..8 {
        let res = sqlx::query(
            "INSERT INTO extend_global.enrollments
             (enrollment_id, secret_digest, os, os_version, model, app_version, agent_device_version, pairing_code, code_expires_at,
              world_schema, environment_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(id)
        .bind(ids::secret_digest(&secret))
        .bind(input.os.as_str())
        .bind(&input.os_version)
        .bind(&input.model)
        .bind(&input.app_version)
        .bind(&input.agent_device_version)
        .bind(code.as_str())
        .bind(expires)
        .bind(&world.schema)
        .bind(world.environment_id)
        .execute(&state.pool)
        .await;
        match res {
            Ok(_) => break,
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() && attempt < 7 => code = PairingCode::random(),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(created(
        "enrollment",
        EnrollmentCreated {
            enrollment_id: id,
            enrollment_secret: secret,
            pairing_code: code.to_string(),
            code_expires_at: expires,
            rotates_every_s: PAIRING_CODE_TTL_S,
        },
    ))
}

fn enrollment_secret(headers: &HeaderMap) -> AppResult<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Extend-Enrollment "))
        .map(|s| s.trim().to_owned())
        .filter(|s| ids::is_secret(ids::ENROLLMENT_SECRET_PREFIX, s))
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::Unauthorized,
                "Send the enrollment secret as Authorization: Extend-Enrollment <secret>.",
            )
        })
}

#[derive(sqlx::FromRow)]
struct Row {
    pairing_code: String,
    code_expires_at: OffsetDateTime,
    paired_device_id: Option<String>,
    paired_credential: Option<String>,
    paired_environment: Option<serde_json::Value>,
    world_schema: String,
    environment_id: Option<Uuid>,
}

/// Loads an enrollment by its secret. A request that also selects a world (with
/// `X-Testing-Application-Secret`) must select the one the enrollment was started in.
async fn load(state: &AppState, id: Uuid, secret: &str, sel: &Sel) -> AppResult<Row> {
    let row = sqlx::query_as::<_, Row>(
        "UPDATE extend_global.enrollments SET last_seen_at = now() WHERE enrollment_id = $1 AND secret_digest = $2
         RETURNING pairing_code, code_expires_at, paired_device_id, paired_credential, paired_environment, world_schema, environment_id",
    )
    .bind(id)
    .bind(ids::secret_digest(secret))
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| {
        AppError::new(
            ErrorCode::EnrollmentNotFound,
            "This enrollment no longer exists (it was discarded, expired, or already paired, or its test environment was cleaned or removed).",
        )
        .hint("Start a new enrollment with POST /api/v1/enrollments.")
    })?;
    if sel.sel.is_some() && sel.world.schema != row.world_schema {
        return Err(AppError::new(
            ErrorCode::TestingSecretInvalid,
            format!(
                "This enrollment was started in {}, but the X-Testing-Application-Secret header selects {}.",
                world_word(row.environment_id),
                world_word(sel.world.environment_id)
            ),
        )
        .hint("Send the same X-Testing-Application-Secret (or none) you started the enrollment with, or start a new enrollment."));
    }
    Ok(row)
}

fn world_word(environment_id: Option<Uuid>) -> String {
    environment_id.map_or_else(|| "production".into(), |id| format!("test environment {id}"))
}

/// Refuses a claim of a pairing code made in another world (production or a test environment),
/// before the claim runs. The world is fixed when the enrollment starts, so checking first is
/// sound; `enrollments_claimed_in_own_world` in the database backs it up. A code that isn't live
/// is left for the claim to refuse. Only called for a signed-in Carbon (`member`): the answer says
/// a code exists, so nobody else hears it, and it counts against that member's own limit.
pub async fn claim_world_check(state: &AppState, selection: &Selection, body: &[u8], member: &str) -> AppResult<()> {
    let Some(code) = crate::state::claimed_code(body) else {
        return Ok(());
    };
    let made_in: Option<(String, Option<Uuid>)> = sqlx::query_as(
        "SELECT world_schema, environment_id FROM extend_global.enrollments
         WHERE pairing_code = $1 AND code_expires_at > now() AND paired_device_id IS NULL",
    )
    .bind(code.as_str())
    .fetch_optional(&state.pool)
    .await?;
    let Some((schema, _)) = made_in else {
        return Ok(());
    };
    if schema == selection.world.schema {
        return Ok(());
    }
    // Answering "this code exists elsewhere" is a hint about codes, so it counts like a guess, per
    // member (a peer address is shared by everyone behind the load balancer).
    state
        .rate_limit(
            format!("pair-world:{member}"),
            20,
            Duration::from_secs(15 * 60),
            "pairing codes from another environment",
        )
        .await?;
    let (message, hint) = match (&selection.sel, schema == "extend") {
        (None, _) => (
            format!("Pairing code {code} was made by an Extend app that is pairing into a test environment, so it can't pair the device into production."),
            "Select that test environment (send its app_secret in X-Testing-Application-Secret, or use `extend --test <id>`) and enter the code again, or restart pairing on the device without a test environment.".to_owned(),
        ),
        (Some(s), true) => (
            format!("Pairing code {code} was made by an Extend app pairing into production, so it can't pair the device into test environment {}.", s.name),
            format!("Leave testing to pair the device into production, or start the device's pairing from inside test environment {}.", s.name),
        ),
        (Some(s), false) => (
            format!("Pairing code {code} was made by an Extend app pairing into a different test environment, not {}.", s.name),
            "Select the test environment the device's pairing was started in and enter the code again.".to_owned(),
        ),
    };
    Err(AppError::new(ErrorCode::PairingCodeInvalid, message).hint(hint))
}

/// Rotates the code if it has expired; returns the live code.
pub async fn rotate_if_due(state: &AppState, id: Uuid) -> AppResult<Option<(String, OffsetDateTime, bool)>> {
    for _ in 0..8 {
        let code = PairingCode::random();
        let expires = OffsetDateTime::now_utc() + time::Duration::seconds(PAIRING_CODE_TTL_S);
        let res: Result<Option<(String, OffsetDateTime)>, sqlx::Error> = sqlx::query_as(
            "UPDATE extend_global.enrollments SET pairing_code = $2, code_expires_at = $3
             WHERE enrollment_id = $1 AND code_expires_at <= now() AND paired_device_id IS NULL
             RETURNING pairing_code, code_expires_at",
        )
        .bind(id)
        .bind(code.as_str())
        .bind(expires)
        .fetch_optional(&state.pool)
        .await;
        match res {
            Ok(Some((c, e))) => return Ok(Some((c, e, true))),
            Ok(None) => {
                let cur: Option<(String, OffsetDateTime)> = sqlx::query_as(
                    "SELECT pairing_code, code_expires_at FROM extend_global.enrollments WHERE enrollment_id = $1",
                )
                .bind(id)
                .fetch_optional(&state.pool)
                .await?;
                return Ok(cur.map(|(c, e)| (c, e, false)));
            }
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(AppError::internal("could not allocate a unique pairing code"))
}

fn paired_state(row: &Row) -> Option<EnrollmentState> {
    Some(EnrollmentState::Paired {
        device_id: row.paired_device_id.as_ref()?.parse().ok()?,
        device_credential: row.paired_credential.clone()?,
        environment: row
            .paired_environment
            .clone()
            .and_then(|v| serde_json::from_value::<TestingEnvironment>(v).ok()),
    })
}

async fn forget(state: &AppState, id: Uuid) {
    let _ = sqlx::query("DELETE FROM extend_global.enrollments WHERE enrollment_id = $1")
        .bind(id)
        .execute(&state.pool)
        .await;
}

pub async fn get(
    State(state): State<Shared>,
    Path(id): Path<Uuid>,
    sel: Sel,
    headers: HeaderMap,
) -> AppResult<Response> {
    let secret = enrollment_secret(&headers)?;
    let row = load(&state, id, &secret, &sel).await?;
    if let Some(paired) = paired_state(&row) {
        forget(&state, id).await;
        return Ok(ok("enrollment", paired));
    }
    let (code, expires) = match rotate_if_due(&state, id).await? {
        Some((c, e, _)) => (c, e),
        None => (row.pairing_code, row.code_expires_at),
    };
    Ok(ok(
        "enrollment",
        EnrollmentState::Waiting {
            pairing_code: code,
            code_expires_at: expires,
        },
    ))
}

pub async fn discard(
    State(state): State<Shared>,
    Path(id): Path<Uuid>,
    sel: Sel,
    headers: HeaderMap,
) -> AppResult<Response> {
    let secret = enrollment_secret(&headers)?;
    load(&state, id, &secret, &sel).await?;
    forget(&state, id).await;
    Ok(no_content())
}

pub async fn socket(
    State(state): State<Shared>,
    Path(id): Path<Uuid>,
    sel: Sel,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> AppResult<Response> {
    let secret = enrollment_secret(&headers)?;
    let row = load(&state, id, &secret, &sel).await?;
    Ok(ws
        .on_upgrade(move |socket| run_socket(state, id, row, socket))
        .into_response())
}

fn text(frame: &EnrollmentFrame) -> Message {
    Message::Text(serde_json::to_string(frame).unwrap_or_default().into())
}

async fn run_socket(state: Shared, id: Uuid, row: Row, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    if let Some(EnrollmentState::Paired {
        device_id,
        device_credential,
        environment,
    }) = paired_state(&row)
    {
        let _ = sink
            .send(text(&EnrollmentFrame::Paired {
                device_id,
                device_credential,
                environment,
            }))
            .await;
        forget(&state, id).await;
        let _ = sink.close().await;
        return;
    }
    let mut rx = state.hub.register_enrollment(id).await;
    let current = rotate_if_due(&state, id).await.ok().flatten();
    let (code, expires) = current
        .map(|(c, e, _)| (c, e))
        .unwrap_or((row.pairing_code, row.code_expires_at));
    if sink
        .send(text(&EnrollmentFrame::Code {
            pairing_code: code,
            code_expires_at: expires,
        }))
        .await
        .is_err()
    {
        state.hub.unregister_enrollment(id).await;
        return;
    }
    let mut ping = tokio::time::interval(Duration::from_secs(extend_protocol::HEARTBEAT_S));
    let mut nonce = 0u64;
    loop {
        tokio::select! {
            frame = rx.recv() => {
                let Some(frame) = frame else { break };
                let done = matches!(frame, EnrollmentFrame::Paired { .. });
                if sink.send(text(&frame)).await.is_err() { break; }
                if done {
                    forget(&state, id).await;
                    let _ = sink.close().await;
                    break;
                }
            }
            msg = stream.next() => {
                match msg {
                    Some(Ok(Message::Text(_) | Message::Pong(_))) => {
                        let _ = sqlx::query("UPDATE extend_global.enrollments SET last_seen_at = now() WHERE enrollment_id = $1")
                            .bind(id).execute(&state.pool).await;
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
            _ = ping.tick() => {
                nonce += 1;
                if sink.send(text(&EnrollmentFrame::Ping { nonce })).await.is_err() { break; }
            }
        }
    }
    state.hub.unregister_enrollment(id).await;
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions() {
        assert!(super::version_at_least("1.0.0", "1.0.0"));
        assert!(super::version_at_least("1.2.0", "1.0.9"));
        assert!(!super::version_at_least("0.9.9", "1.0.0"));
        assert!(super::version_at_least("1.0.0-beta", "1.0.0"));
    }
}

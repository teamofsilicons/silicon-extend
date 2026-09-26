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
use crate::state::{AppState, Shared};

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

pub async fn create(
    State(state): State<Shared>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Body(input): Body<EnrollmentCreate>,
) -> AppResult<Response> {
    state
        .rate_limit(
            format!("enroll:{}", addr.ip()),
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
             (enrollment_id, secret_digest, os, os_version, model, app_version, agent_device_version, pairing_code, code_expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
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
}

async fn load(state: &AppState, id: Uuid, secret: &str) -> AppResult<Row> {
    sqlx::query_as::<_, Row>(
        "UPDATE extend_global.enrollments SET last_seen_at = now() WHERE enrollment_id = $1 AND secret_digest = $2
         RETURNING pairing_code, code_expires_at, paired_device_id, paired_credential, paired_environment",
    )
    .bind(id)
    .bind(ids::secret_digest(secret))
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| {
        AppError::new(
            ErrorCode::EnrollmentNotFound,
            "This enrollment no longer exists (it was discarded, expired, or already paired).",
        )
        .hint("Start a new enrollment with POST /api/v1/enrollments.")
    })
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

pub async fn get(State(state): State<Shared>, Path(id): Path<Uuid>, headers: HeaderMap) -> AppResult<Response> {
    let secret = enrollment_secret(&headers)?;
    let row = load(&state, id, &secret).await?;
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

pub async fn discard(State(state): State<Shared>, Path(id): Path<Uuid>, headers: HeaderMap) -> AppResult<Response> {
    let secret = enrollment_secret(&headers)?;
    load(&state, id, &secret).await?;
    forget(&state, id).await;
    Ok(no_content())
}

pub async fn socket(
    State(state): State<Shared>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> AppResult<Response> {
    let secret = enrollment_secret(&headers)?;
    let row = load(&state, id, &secret).await?;
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

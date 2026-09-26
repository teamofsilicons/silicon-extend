//! Before pairing: get a pairing code, show it, follow its rotations, and wait for a Carbon to
//! claim it (`docs/device-protocol.md` section 1).

use std::time::Duration;

use bridge_protocol::frames::{DeviceFrame, EnrollmentFrame};
use bridge_protocol::model::{EnrollmentCreate, EnrollmentCreated, EnrollmentState, TestingEnvironment, Timestamp};
use bridge_protocol::DeviceId;
use futures::{SinkExt as _, StreamExt as _};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::service::ServiceClient;
use crate::status::{PairingInfo, Phase, StatusHandle};
use crate::ws::{self, Backoff, ConnectError};

/// What a successful pairing hands the app.
#[derive(Debug, Clone)]
pub struct Paired {
    pub device_id: DeviceId,
    pub device_credential: String,
    pub environment: Option<TestingEnvironment>,
}

#[derive(Debug)]
pub enum EnrollOutcome {
    Paired(Paired),
    UpgradeRequired,
    Shutdown,
}

enum SocketEnd {
    Paired(Paired),
    /// The enrollment is gone or its secret was refused; start a new one.
    Gone,
    /// The socket dropped; the enrollment may still be good.
    Dropped(String),
    Shutdown,
}

/// Runs until paired, the service asks for a newer app, or shutdown.
pub async fn enroll(
    service: &ServiceClient,
    status: &StatusHandle,
    info: &EnrollmentCreate,
    shutdown: &CancellationToken,
) -> EnrollOutcome {
    let mut backoff = Backoff::default();
    status.update(|s| {
        s.phase = Phase::Enrolling;
        s.device = None;
        s.in_use = None;
        s.takeover = None;
        s.attached.clear();
    });
    'enrollment: loop {
        let created = match service.create_enrollment(info).await {
            Ok(c) => c,
            Err(e) if e.is_upgrade_required() => return EnrollOutcome::UpgradeRequired,
            Err(e) => {
                tracing::warn!("couldn't get a pairing code: {e}");
                status.update(|s| {
                    s.pairing = None;
                    s.last_error = Some(e.message.clone());
                });
                if sleep_or_shutdown(backoff.next_delay(), shutdown).await {
                    return EnrollOutcome::Shutdown;
                }
                continue;
            }
        };
        backoff.reset();
        show_code(status, &created.pairing_code, created.code_expires_at);
        tracing::info!("pairing code {} (enrollment {})", created.pairing_code, created.enrollment_id);

        let mut socket_backoff = Backoff::default();
        let mut expires_at = created.code_expires_at;
        loop {
            match enrollment_socket(service, status, &created, &mut expires_at, shutdown).await {
                SocketEnd::Paired(p) => return EnrollOutcome::Paired(p),
                SocketEnd::Shutdown => {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(3),
                        service.discard_enrollment(created.enrollment_id, &created.enrollment_secret),
                    )
                    .await;
                    return EnrollOutcome::Shutdown;
                }
                SocketEnd::Gone => continue 'enrollment,
                SocketEnd::Dropped(why) => {
                    tracing::info!("enrollment socket dropped: {why}");
                    if sleep_or_shutdown(socket_backoff.next_delay(), shutdown).await {
                        return EnrollOutcome::Shutdown;
                    }
                    // Polling catches a pairing that happened while the socket was down.
                    match poll(service, status, created.enrollment_id, &created.enrollment_secret, &mut expires_at).await {
                        Poll::Paired(p) => return EnrollOutcome::Paired(p),
                        Poll::Gone => continue 'enrollment,
                        Poll::Waiting | Poll::Failed => continue,
                    }
                }
            }
        }
    }
}

enum Poll {
    Paired(Paired),
    Waiting,
    Gone,
    Failed,
}

async fn poll(service: &ServiceClient, status: &StatusHandle, id: Uuid, secret: &str, expires_at: &mut Timestamp) -> Poll {
    match service.get_enrollment(id, secret).await {
        Ok(EnrollmentState::Waiting { pairing_code, code_expires_at }) => {
            *expires_at = code_expires_at;
            show_code(status, &pairing_code, code_expires_at);
            Poll::Waiting
        }
        Ok(EnrollmentState::Paired { device_id, device_credential, environment }) => {
            Poll::Paired(Paired { device_id, device_credential, environment })
        }
        Err(e) if e.is_auth() || e.is_gone() => Poll::Gone,
        Err(e) => {
            tracing::info!("couldn't check the enrollment: {e}");
            Poll::Failed
        }
    }
}

async fn enrollment_socket(
    service: &ServiceClient,
    status: &StatusHandle,
    created: &EnrollmentCreated,
    expires_at: &mut Timestamp,
    shutdown: &CancellationToken,
) -> SocketEnd {
    let url = service.ws_url(&format!("api/v1/enrollments/{}/connect", created.enrollment_id));
    let auth = format!("Bridge-Enrollment {}", created.enrollment_secret);
    let socket = tokio::select! {
        r = ws::connect(&url, &auth) => r,
        _ = shutdown.cancelled() => return SocketEnd::Shutdown,
    };
    let socket = match socket {
        Ok(s) => s,
        Err(ConnectError::Http(401 | 403 | 404 | 410)) => return SocketEnd::Gone,
        Err(e) => return SocketEnd::Dropped(e.to_string()),
    };
    status.update(|s| s.last_error = None);
    let (mut sink, mut stream) = socket.split();
    let idle = Duration::from_secs(bridge_protocol::OFFLINE_AFTER_S + 15);
    loop {
        // A code that expired without a rotation frame means frames are being lost: re-read it.
        let until_expiry = (*expires_at - time::OffsetDateTime::now_utc()).unsigned_abs() + Duration::from_secs(10);
        let msg = tokio::select! {
            m = tokio::time::timeout(idle, stream.next()) => m,
            _ = tokio::time::sleep(until_expiry) => {
                match poll(service, status, created.enrollment_id, &created.enrollment_secret, expires_at).await {
                    Poll::Paired(p) => return SocketEnd::Paired(p),
                    Poll::Gone => return SocketEnd::Gone,
                    Poll::Waiting | Poll::Failed => continue,
                }
            }
            _ = shutdown.cancelled() => {
                let _ = sink.send(Message::Close(None)).await;
                return SocketEnd::Shutdown;
            }
        };
        let msg = match msg {
            Err(_) => return SocketEnd::Dropped("no ping from Bridge for a minute".into()),
            Ok(None) => return SocketEnd::Dropped("closed".into()),
            Ok(Some(Err(e))) => return SocketEnd::Dropped(e.to_string()),
            Ok(Some(Ok(m))) => m,
        };
        match msg {
            Message::Text(text) => match serde_json::from_str::<EnrollmentFrame>(&text) {
                Ok(EnrollmentFrame::Code { pairing_code, code_expires_at }) => {
                    *expires_at = code_expires_at;
                    let changed = status.get().pairing.is_none_or(|p| !p.code.eq_ignore_ascii_case(&pairing_code));
                    show_code(status, &pairing_code, code_expires_at);
                    if changed {
                        tracing::info!("pairing code rotated to {}", pairing_code.to_ascii_uppercase());
                    }
                }
                Ok(EnrollmentFrame::Paired { device_id, device_credential, environment }) => {
                    return SocketEnd::Paired(Paired { device_id, device_credential, environment });
                }
                Ok(EnrollmentFrame::Ping { nonce }) => {
                    let pong = serde_json::to_string(&DeviceFrame::Pong { nonce }).expect("pong");
                    if let Err(e) = sink.send(Message::Text(pong.into())).await {
                        return SocketEnd::Dropped(e.to_string());
                    }
                }
                Err(e) => tracing::debug!("ignoring an enrollment frame this version doesn't know: {e}"),
            },
            Message::Close(frame) => {
                let code = frame.as_ref().map(|f| u16::from(f.code));
                if code == Some(bridge_protocol::frames::close::UNAUTHORIZED) {
                    return SocketEnd::Gone;
                }
                return SocketEnd::Dropped(format!("closed by Bridge ({code:?})"));
            }
            _ => {}
        }
    }
}

fn show_code(status: &StatusHandle, code: &str, expires_at: Timestamp) {
    let expires = expires_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default();
    status.update(|s| {
        s.phase = Phase::Enrolling;
        s.pairing = Some(PairingInfo { code: code.to_ascii_uppercase(), expires_at: expires });
    });
}

/// Sleeps; true when shutdown came first.
pub async fn sleep_or_shutdown(d: Duration, shutdown: &CancellationToken) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(d) => false,
        _ = shutdown.cancelled() => true,
    }
}

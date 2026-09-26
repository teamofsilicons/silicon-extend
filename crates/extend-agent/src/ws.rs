//! WebSocket plumbing shared by the enrollment socket and the device socket.

use std::time::Duration;

use extend_protocol::{API_VERSION, API_VERSION_HEADER};
use rand::Rng as _;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use url::Url;

pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Why a connection attempt failed.
#[derive(Debug)]
pub enum ConnectError {
    /// The service answered the upgrade with an HTTP status (401, 404, 426, ...).
    Http(u16),
    Other(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(s) => write!(f, "Extend refused the connection (HTTP {s})"),
            Self::Other(m) => write!(f, "{m}"),
        }
    }
}

/// Opens a WebSocket with `Authorization` set.
pub async fn connect(url: &Url, authorization: &str) -> Result<Socket, ConnectError> {
    let mut req = url.as_str().into_client_request().map_err(|e| ConnectError::Other(e.to_string()))?;
    let headers = req.headers_mut();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(authorization).map_err(|_| ConnectError::Other("credential isn't header-safe".into()))?,
    );
    headers.insert(API_VERSION_HEADER, HeaderValue::from(API_VERSION));
    headers.insert("User-Agent", HeaderValue::from_str(&crate::service::user_agent()).expect("ascii"));
    let fut = tokio_tungstenite::connect_async(req);
    match tokio::time::timeout(Duration::from_secs(20), fut).await {
        Err(_) => Err(ConnectError::Other("timed out connecting to Extend".into())),
        Ok(Err(tungstenite::Error::Http(resp))) => Err(ConnectError::Http(resp.status().as_u16())),
        Ok(Err(e)) => Err(ConnectError::Other(format!("couldn't reach Extend: {e}"))),
        Ok(Ok((socket, _))) => Ok(socket),
    }
}

/// The close code in a close message, if any.
pub fn close_code(msg: &Message) -> Option<u16> {
    match msg {
        Message::Close(Some(frame)) => Some(u16::from(frame.code)),
        _ => None,
    }
}

/// Exponential backoff from 1 s to 60 s with full jitter (`docs/device-protocol.md` section 2).
#[derive(Debug, Clone, Default)]
pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    pub const MIN: Duration = Duration::from_secs(1);
    pub const MAX: Duration = Duration::from_secs(60);

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// The ceiling for the next wait: 1 s, 2 s, 4 s ... capped at 60 s.
    pub fn ceiling(&self) -> Duration {
        let secs = 1u64.checked_shl(self.attempt.min(16)).unwrap_or(u64::MAX);
        Duration::from_secs(secs).clamp(Self::MIN, Self::MAX)
    }

    /// The next wait: uniform in [0, ceiling], never shorter than 100 ms.
    pub fn next_delay(&mut self) -> Duration {
        let ceiling = self.ceiling();
        self.attempt = self.attempt.saturating_add(1);
        let ms = rand::rng().random_range(0..=ceiling.as_millis() as u64);
        Duration::from_millis(ms.max(100))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_to_sixty_seconds_with_jitter() {
        let mut b = Backoff::default();
        let ceilings: Vec<u64> = (0..10)
            .map(|_| {
                let c = b.ceiling();
                let d = b.next_delay();
                assert!(d <= c.max(Duration::from_millis(100)), "{d:?} > {c:?}");
                c.as_secs()
            })
            .collect();
        assert_eq!(ceilings, vec![1, 2, 4, 8, 16, 32, 60, 60, 60, 60]);
        b.reset();
        assert_eq!(b.ceiling(), Duration::from_secs(1));
        for _ in 0..100 {
            b.next_delay();
        }
        assert_eq!(b.ceiling(), Duration::from_secs(60));
    }
}

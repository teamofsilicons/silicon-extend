//! WebSocket client for the TVs' local control sockets (`ws://` and self-signed `wss://`).

use std::io;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::tls::{self, Stream};

pub(crate) type Ws = WebSocketStream<Stream>;

/// Why a socket couldn't be opened. `Unreachable` means nothing answered on that port (try another
/// port or the TV is off); `Refused` means something answered but wouldn't upgrade.
#[derive(Debug)]
pub(crate) enum ConnectError {
    Unreachable(String),
    Refused(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(m) | Self::Refused(m) => f.write_str(m),
        }
    }
}

/// Opens `ws://` or `wss://` (certificate not checked, see `tls`).
pub(crate) async fn connect(url: &str, timeout: Duration) -> Result<Ws, ConnectError> {
    let parsed = url::Url::parse(url)
        .map_err(|e| ConnectError::Refused(format!("bad socket URL {url}: {e}")))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| ConnectError::Refused(format!("no host in {url}")))?
        .to_owned();
    let secure = parsed.scheme() == "wss";
    let port = parsed
        .port_or_known_default()
        .unwrap_or(if secure { 443 } else { 80 });
    let stream = if secure {
        tls::tls_insecure(&host, port, timeout).await
    } else {
        tls::tcp(&host, port, timeout).await.map(Stream::Plain)
    }
    .map_err(|e| ConnectError::Unreachable(format!("{host}:{port}: {e}")))?;
    let request = url
        .into_client_request()
        .map_err(|e| ConnectError::Refused(e.to_string()))?;
    let (ws, _resp) =
        tokio::time::timeout(timeout, tokio_tungstenite::client_async(request, stream))
            .await
            .map_err(|_| {
                ConnectError::Refused(format!(
                    "{host}:{port} didn't finish the WebSocket handshake"
                ))
            })?
            .map_err(|e| {
                ConnectError::Refused(format!("{host}:{port} refused the WebSocket: {e}"))
            })?;
    Ok(ws)
}

pub(crate) async fn send_text(ws: &mut Ws, text: impl Into<String>) -> io::Result<()> {
    ws.send(Message::text(text.into()))
        .await
        .map_err(io::Error::other)
}

/// Next text (or UTF-8 binary) message, answering pings on the way. `Ok(None)` when closed.
pub(crate) async fn next_text(ws: &mut Ws, timeout: Duration) -> io::Result<Option<String>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let msg = tokio::time::timeout_at(deadline, ws.next())
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "no message from the TV in time")
            })?;
        match msg {
            None => return Ok(None),
            Some(Err(e)) => return Err(io::Error::other(e)),
            Some(Ok(Message::Text(t))) => return Ok(Some(t.to_string())),
            Some(Ok(Message::Binary(b))) => {
                if let Ok(s) = String::from_utf8(b.to_vec()) {
                    return Ok(Some(s));
                }
            }
            Some(Ok(Message::Close(_))) => return Ok(None),
            Some(Ok(_)) => {} // ping/pong handled by tungstenite
        }
    }
}

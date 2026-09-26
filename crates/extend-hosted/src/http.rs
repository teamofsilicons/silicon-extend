//! A small HTTP/1.1 (and RTSP/1.0) client: enough for TV REST calls, SSDP descriptions and
//! AirPlay's control channel, which runs HTTP-shaped messages over an encrypted stream.

use std::io;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::common::url_host;
use crate::tls::{self, Stream};

/// One HTTP or RTSP message (request or response) off the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Message {
    /// `HTTP/1.1 200 OK`, `RTSP/1.0 200 OK` or `POST /command RTSP/1.0`.
    pub start_line: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Message {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Status code of a response (0 for a request).
    pub fn status(&self) -> u16 {
        let mut parts = self.start_line.split_whitespace();
        match (parts.next(), parts.next()) {
            (Some(p), Some(code)) if p.contains('/') && !p.starts_with('/') => code.parse().unwrap_or(0),
            _ => 0,
        }
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status())
    }

    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Serialises a request. `Content-Length` is added when there is a body (or for POST/PUT).
pub(crate) fn encode_request(
    method: &str,
    target: &str,
    protocol: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> Vec<u8> {
    let mut out = format!("{method} {target} {protocol}\r\n");
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    let has_len = headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-length"));
    if !has_len && (!body.is_empty() || matches!(method, "POST" | "PUT")) {
        out.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

/// Serialises a response (used to answer AirPlay's event channel).
pub(crate) fn encode_response(
    protocol: &str,
    status: u16,
    reason: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> Vec<u8> {
    let mut out = format!("{protocol} {status} {reason}\r\n");
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

/// Incremental parser: push bytes as they arrive, take whole messages out.
#[derive(Debug, Default)]
pub(crate) struct Parser {
    buf: Vec<u8>,
}

impl Parser {
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// A complete message, if the buffer holds one. With `eof`, a message without a length ends
    /// at the end of the stream.
    pub fn take(&mut self, eof: bool) -> io::Result<Option<Message>> {
        let Some(head_end) = find(&self.buf, b"\r\n\r\n") else {
            if eof && !self.buf.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed mid-headers",
                ));
            }
            return Ok(None);
        };
        let head = String::from_utf8_lossy(&self.buf[..head_end]).into_owned();
        let mut lines = head.split("\r\n");
        let start_line = lines.next().unwrap_or_default().to_owned();
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| {
                l.split_once(':')
                    .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
            })
            .collect();
        let get = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        };
        let body_start = head_end + 4;
        let status = Message {
            start_line: start_line.clone(),
            headers: vec![],
            body: vec![],
        }
        .status();
        let is_request = status == 0;
        let no_body = matches!(status, 100..=199 | 204 | 304);

        let (body, consumed) = if no_body {
            (vec![], body_start)
        } else if get("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
            match dechunk(&self.buf[body_start..])? {
                Some((body, used)) => (body, body_start + used),
                None => return Ok(None),
            }
        } else if let Some(len) = get("content-length") {
            let len: usize = len
                .parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad Content-Length"))?;
            if self.buf.len() < body_start + len {
                return Ok(None);
            }
            (self.buf[body_start..body_start + len].to_vec(), body_start + len)
        } else if is_request {
            (vec![], body_start)
        } else if eof {
            (self.buf[body_start..].to_vec(), self.buf.len())
        } else {
            return Ok(None);
        };
        self.buf.drain(..consumed);
        Ok(Some(Message {
            start_line,
            headers,
            body,
        }))
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Decodes a chunked body; `None` until the final chunk has arrived.
fn dechunk(data: &[u8]) -> io::Result<Option<(Vec<u8>, usize)>> {
    let mut pos = 0;
    let mut out = Vec::new();
    loop {
        let Some(line_end) = find(&data[pos..], b"\r\n") else {
            return Ok(None);
        };
        let size_str = String::from_utf8_lossy(&data[pos..pos + line_end]);
        let size_hex = size_str.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
        pos += line_end + 2;
        if size == 0 {
            // Trailers end with an empty line.
            let Some(end) = find(&data[pos..], b"\r\n") else {
                return Ok(None);
            };
            if end == 0 {
                return Ok(Some((out, pos + 2)));
            }
            let Some(tend) = find(&data[pos..], b"\r\n\r\n") else {
                return Ok(None);
            };
            return Ok(Some((out, pos + tend + 4)));
        }
        if data.len() < pos + size + 2 {
            return Ok(None);
        }
        out.extend_from_slice(&data[pos..pos + size]);
        pos += size + 2;
    }
}

/// Reads one message from a stream.
pub(crate) async fn read_message<S: AsyncRead + Unpin>(stream: &mut S, parser: &mut Parser) -> io::Result<Message> {
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        if let Some(m) = parser.take(false)? {
            return Ok(m);
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return parser
                .take(true)?
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed before a reply"));
        }
        parser.push(&chunk[..n]);
    }
}

/// One request on a fresh connection (`Connection: close`). `https` URLs accept self-signed
/// certificates when `insecure` (TVs) and verify them otherwise.
pub(crate) async fn request(
    method: &str,
    url: &str,
    headers: &[(&str, String)],
    body: &[u8],
    timeout: Duration,
    insecure: bool,
) -> io::Result<Message> {
    let parsed = url::Url::parse(url).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no host"))?
        .to_owned();
    let secure = parsed.scheme() == "https";
    let port = parsed.port_or_known_default().unwrap_or(80);
    let mut target = parsed.path().to_owned();
    if let Some(q) = parsed.query() {
        target.push('?');
        target.push_str(q);
    }
    let work = async {
        let mut stream = match (secure, insecure) {
            (true, true) => tls::tls_insecure(&host, port, timeout).await?,
            (true, false) => tls::tls_verified(&host, port, timeout).await?,
            (false, _) => Stream::Plain(tls::tcp(&host, port, timeout).await?),
        };
        let authority = match parsed.port() {
            Some(p) => format!("{}:{p}", url_host(&host)),
            None => url_host(&host),
        };
        let mut all: Vec<(&str, String)> = vec![
            ("Host", authority),
            ("Connection", "close".into()),
            ("User-Agent", "SiliconExtend/1.0".into()),
        ];
        all.extend(headers.iter().cloned());
        stream
            .write_all(&encode_request(method, &target, "HTTP/1.1", &all, body))
            .await?;
        stream.flush().await?;
        let mut parser = Parser::default();
        read_message(&mut stream, &mut parser).await
    };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, format!("{method} {url} timed out")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_content_length_and_pipelined() {
        let mut p = Parser::default();
        p.push(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhiRTSP/1.0 404 Not Found\r\nCSeq: 3\r\nContent-Length: 0\r\n\r\n");
        let a = p.take(false).unwrap().unwrap();
        assert_eq!((a.status(), a.body.as_slice()), (200, &b"hi"[..]));
        let b = p.take(false).unwrap().unwrap();
        assert_eq!(b.status(), 404);
        assert_eq!(b.header("cseq"), Some("3"));
        assert!(p.take(false).unwrap().is_none());
    }

    #[test]
    fn parses_chunked_and_partial() {
        let mut p = Parser::default();
        p.push(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5\r\n");
        assert!(p.take(false).unwrap().is_none());
        p.push(b"pedia\r\n0\r\n\r\n");
        assert_eq!(p.take(false).unwrap().unwrap().body, b"Wikipedia");
    }

    #[test]
    fn body_until_eof_and_requests() {
        let mut p = Parser::default();
        p.push(b"HTTP/1.0 200 OK\r\n\r\n{\"a\":1}");
        assert!(p.take(false).unwrap().is_none());
        assert_eq!(p.take(true).unwrap().unwrap().json().unwrap()["a"], 1);

        let mut p = Parser::default();
        p.push(b"POST /command RTSP/1.0\r\nCSeq: 1\r\nContent-Length: 0\r\n\r\n");
        let m = p.take(false).unwrap().unwrap();
        assert_eq!(m.status(), 0);
        assert!(m.start_line.starts_with("POST /command"));
    }

    #[test]
    fn encodes() {
        let r = encode_request("POST", "/x", "HTTP/1.1", &[("Host", "a".into())], b"");
        assert_eq!(r, b"POST /x HTTP/1.1\r\nHost: a\r\nContent-Length: 0\r\n\r\n");
        let r = encode_response("RTSP/1.0", 200, "OK", &[("CSeq", "2".into())], b"");
        assert_eq!(r, b"RTSP/1.0 200 OK\r\nCSeq: 2\r\nContent-Length: 0\r\n\r\n");
    }
}

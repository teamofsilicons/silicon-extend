//! AirPlay, for `display show --video|--image` on an Apple TV.
//!
//! The control connection (`_airplay._tcp`, usually port 7000) speaks HTTP/RTSP. We pair
//! transiently (HAP Pair-Setup M1–M4 with the fixed PIN 3939 and the transient flag, which Apple
//! TVs accept when AirPlay access is "Anyone on the same network"), or Pair-Verify with the
//! Companion credentials as a fallback, then encrypt the connection as a HAP session: plaintext
//! split into ≤1024-byte blocks, each sent as `[len u16 LE][ChaCha20-Poly1305(block, aad = len)]`.
//!
//! Videos follow AirPlay 2's URL playback (as pyatv does it): `SETUP` (with our NTP timing port),
//! an encrypted event channel back from the Apple TV, `RECORD`, `POST /play` with the URL, then
//! `/rate?value=1`, with `POST /feedback` every two seconds for as long as it plays. A local file is
//! served from this Mac over HTTP (with byte ranges) on the address the Apple TV can reach.
//! Pictures use `PUT /photo` on the paired connection.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::hap::{self, Credentials, NonceStyle, PairSetup, PairVerify, SessionCipher};
use crate::http::{self, Message, Parser};

const TRANSIENT_PIN: &str = "3939";
const FRAME: usize = 1024;
const TIMEOUT: Duration = Duration::from_secs(8);
const USER_AGENT: &str = "AirPlay/550.10";
const BPLIST: &str = "application/x-apple-binary-plist";

// ───────────── HAP-encrypted stream ─────────────

/// Encrypts plaintext into HAP session blocks.
pub(crate) fn seal_blocks(cipher: &mut SessionCipher, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 18 * (data.len() / FRAME + 1));
    for block in data.chunks(FRAME) {
        let len = (block.len() as u16).to_le_bytes();
        out.extend_from_slice(&len);
        out.extend(cipher.encrypt(block, &len));
    }
    out
}

/// Decrypts whole blocks from `buf`, leaving a partial block in place.
pub(crate) fn open_blocks(cipher: &mut SessionCipher, buf: &mut Vec<u8>) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        if buf.len() < 2 {
            return Ok(out);
        }
        let len = u16::from_le_bytes([buf[0], buf[1]]) as usize;
        if buf.len() < 2 + len + 16 {
            return Ok(out);
        }
        let aad = [buf[0], buf[1]];
        let plain = cipher
            .decrypt(&buf[2..2 + len + 16], &aad)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        out.extend(plain);
        buf.drain(..2 + len + 16);
    }
}

/// A TCP connection that becomes a HAP session once keys are set.
pub(crate) struct HapConn {
    stream: TcpStream,
    cipher: Option<SessionCipher>,
    raw: Vec<u8>,
    parser: Parser,
}

impl HapConn {
    pub fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            cipher: None,
            raw: Vec::new(),
            parser: Parser::default(),
        }
    }

    pub fn encrypt_with(&mut self, out_key: [u8; 32], in_key: [u8; 32]) {
        self.cipher = Some(SessionCipher::new(out_key, in_key, NonceStyle::Counter8));
    }

    pub async fn send(&mut self, data: &[u8]) -> io::Result<()> {
        let bytes = match self.cipher.as_mut() {
            Some(c) => seal_blocks(c, data),
            None => data.to_vec(),
        };
        self.stream.write_all(&bytes).await?;
        self.stream.flush().await
    }

    /// Next HTTP/RTSP message (request or response).
    pub async fn read_message(&mut self, timeout: Duration) -> io::Result<Message> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(m) = self.parser.take(false)? {
                return Ok(m);
            }
            let mut chunk = [0u8; 16 * 1024];
            let n = tokio::time::timeout_at(deadline, self.stream.read(&mut chunk))
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the Apple TV didn't answer in time",
                    )
                })??;
            if n == 0 {
                return self.parser.take(true)?.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the Apple TV closed the AirPlay connection",
                    )
                });
            }
            match self.cipher.as_mut() {
                Some(c) => {
                    self.raw.extend_from_slice(&chunk[..n]);
                    let plain = open_blocks(c, &mut self.raw)?;
                    self.parser.push(&plain);
                }
                None => self.parser.push(&chunk[..n]),
            }
        }
    }

    pub fn local_ip(&self) -> io::Result<IpAddr> {
        Ok(self.stream.local_addr()?.ip())
    }

    pub fn peer_ip(&self) -> io::Result<IpAddr> {
        Ok(self.stream.peer_addr()?.ip())
    }
}

// ───────────── Control connection ─────────────

/// Keys a pairing produced (transient: the SRP session key; verified: the X25519 secret).
enum Keys {
    Transient(Vec<u8>),
    Verified(PairVerify),
}

impl Keys {
    fn derive(&self, salt: &str, out_info: &str, in_info: &str) -> ([u8; 32], [u8; 32]) {
        match self {
            Keys::Transient(k) => (hap::hkdf(salt, out_info, k), hap::hkdf(salt, in_info, k)),
            Keys::Verified(pv) => pv.keys(salt, out_info, in_info).expect("verified"),
        }
    }
}

pub(crate) struct Control {
    conn: HapConn,
    cseq: u32,
    pub session_id: u32,
    dacp_id: String,
    active_remote: u32,
    session_uuid: String,
    keys: Option<Keys>,
    host: String,
}

impl Control {
    pub async fn connect(host: &str, port: u16) -> io::Result<Self> {
        let stream = crate::tls::tcp(host, port, TIMEOUT).await?;
        Ok(Self {
            conn: HapConn::new(stream),
            cseq: 0,
            session_id: rand::random(),
            dacp_id: format!("{:016X}", rand::random::<u64>()),
            active_remote: rand::random(),
            session_uuid: uuid::Uuid::new_v4().to_string().to_uppercase(),
            keys: None,
            host: host.to_owned(),
        })
    }

    fn session_uri(&self) -> String {
        let ip = self
            .conn
            .local_ip()
            .map(|i| i.to_string())
            .unwrap_or_else(|_| "127.0.0.1".into());
        format!("rtsp://{ip}/{}", self.session_id)
    }

    /// One request/response on the control connection.
    pub async fn request(
        &mut self,
        method: &str,
        uri: Option<&str>,
        protocol: &str,
        extra: &[(&str, String)],
        body: Option<(&str, Vec<u8>)>,
    ) -> io::Result<Message> {
        self.cseq += 1;
        let uri = uri.map(str::to_owned).unwrap_or_else(|| self.session_uri());
        let mut headers: Vec<(&str, String)> = vec![
            ("CSeq", self.cseq.to_string()),
            ("DACP-ID", self.dacp_id.clone()),
            ("Active-Remote", self.active_remote.to_string()),
            ("Client-Instance", self.dacp_id.clone()),
            ("User-Agent", USER_AGENT.into()),
        ];
        headers.extend(extra.iter().cloned());
        let payload = match &body {
            Some((ct, b)) => {
                headers.push(("Content-Type", (*ct).to_owned()));
                b.clone()
            }
            None => vec![],
        };
        if !payload.is_empty() || matches!(method, "POST" | "PUT" | "SETUP" | "RECORD") {
            headers.push(("Content-Length", payload.len().to_string()));
        }
        self.conn
            .send(&http::encode_request(
                method, &uri, protocol, &headers, &payload,
            ))
            .await?;
        loop {
            let m = self.conn.read_message(TIMEOUT).await?;
            if m.status() != 0 {
                return Ok(m);
            }
        }
    }

    async fn pairing_post(
        &mut self,
        path: &str,
        hkp: u8,
        body: Vec<u8>,
    ) -> Result<Message, String> {
        let r = self
            .request(
                "POST",
                Some(path),
                "HTTP/1.1",
                &[
                    ("X-Apple-HKP", hkp.to_string()),
                    ("Connection", "keep-alive".into()),
                ],
                Some(("application/octet-stream", body)),
            )
            .await
            .map_err(|e| format!("AirPlay {path}: {e}"))?;
        if !r.is_success() {
            return Err(format!("AirPlay {path} answered {}", r.status()));
        }
        Ok(r)
    }

    /// Transient pairing (no PIN on screen). Works when AirPlay access allows anyone on the network.
    async fn pair_transient(&mut self) -> Result<Keys, String> {
        let mut setup = PairSetup::new(&uuid::Uuid::new_v4().to_string());
        setup.transient = true;
        let _ = self.pairing_post("/pair-pin-start", 4, vec![]).await;
        let m2 = self.pairing_post("/pair-setup", 4, setup.m1()).await?;
        setup.handle_m2(&m2.body).map_err(|e| e.to_string())?;
        let m3 = setup.m3(TRANSIENT_PIN).map_err(|e| e.to_string())?;
        let m4 = self.pairing_post("/pair-setup", 4, m3).await?;
        setup.handle_m4(&m4.body).map_err(|e| e.to_string())?;
        Ok(Keys::Transient(
            setup.session_key().expect("after M4").to_vec(),
        ))
    }

    async fn pair_verified(&mut self, creds: &Credentials) -> Result<Keys, String> {
        let mut pv = PairVerify::default();
        let m2 = self.pairing_post("/pair-verify", 3, pv.m1()).await?;
        let m3 = pv.handle_m2(creds, &m2.body).map_err(|e| e.to_string())?;
        let m4 = self.pairing_post("/pair-verify", 3, m3).await?;
        pv.handle_m4(&m4.body).map_err(|e| e.to_string())?;
        Ok(Keys::Verified(pv))
    }

    /// Pairs (transient first, then the Companion credentials) and encrypts the connection.
    pub async fn authenticate(
        &mut self,
        creds: Option<&Credentials>,
        port: u16,
    ) -> Result<(), String> {
        let keys = match self.pair_transient().await {
            Ok(k) => k,
            Err(transient) => {
                let Some(c) = creds else {
                    return Err(format!(
                        "{transient}. Allow AirPlay from this Mac: on the Apple TV, Settings › AirPlay and HomeKit › Allow Access › Anyone on the Same Network"
                    ));
                };
                // The failed attempt may have left the connection unusable; start clean.
                *self = Control::connect(&self.host.clone(), port)
                    .await
                    .map_err(|e| e.to_string())?;
                self.pair_verified(c).await.map_err(|verified| {
                    format!("AirPlay refused this Mac (transient: {transient}; paired: {verified}). On the Apple TV, set Settings › AirPlay and HomeKit › Allow Access to Anyone on the Same Network")
                })?
            }
        };
        let (out, inp) = keys.derive(
            "Control-Salt",
            "Control-Write-Encryption-Key",
            "Control-Read-Encryption-Key",
        );
        self.conn.encrypt_with(out, inp);
        self.keys = Some(keys);
        Ok(())
    }

    async fn bplist(
        &mut self,
        method: &str,
        uri: Option<&str>,
        protocol: &str,
        body: plist::Value,
    ) -> io::Result<Message> {
        let mut buf = Vec::new();
        plist::to_writer_binary(&mut buf, &body).map_err(io::Error::other)?;
        self.request(method, uri, protocol, &[], Some((BPLIST, buf)))
            .await
    }
}

// ───────────── Bodies ─────────────

fn dict(pairs: Vec<(&str, plist::Value)>) -> plist::Value {
    plist::Value::Dictionary(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

pub(crate) fn setup_body(session_uuid: &str, timing_port: u16, name: &str) -> plist::Value {
    use plist::Value as V;
    dict(vec![
        ("deviceID", V::String("AA:BB:CC:DD:EE:FF".into())),
        ("sessionUUID", V::String(session_uuid.into())),
        ("timingPort", V::Integer((timing_port as u64).into())),
        ("timingProtocol", V::String("NTP".into())),
        ("isMultiSelectAirPlay", V::Boolean(true)),
        ("groupContainsGroupLeader", V::Boolean(false)),
        ("macAddress", V::String("AA:BB:CC:DD:EE:FF".into())),
        ("model", V::String("iPhone14,3".into())),
        ("name", V::String(name.into())),
        ("osBuildVersion", V::String("20F66".into())),
        ("osName", V::String("iPhone OS".into())),
        ("osVersion", V::String("16.5".into())),
        ("senderSupportsRelay", V::Boolean(false)),
        ("sourceVersion", V::String("690.7.1".into())),
        ("statsCollectionEnabled", V::Boolean(false)),
    ])
}

pub(crate) fn play_body(url: &str, uuid: &str) -> plist::Value {
    use plist::Value as V;
    dict(vec![
        ("Content-Location", V::String(url.into())),
        ("Start-Position-Seconds", V::Real(0.0)),
        ("uuid", V::String(uuid.into())),
        ("streamType", V::Integer(1.into())),
        ("mediaType", V::String("file".into())),
        ("mightSupportStorePastisKeyRequests", V::Boolean(true)),
        ("playbackRestrictions", V::Integer(0.into())),
        ("volume", V::Real(1.0)),
        ("rate", V::Real(1.0)),
        ("SenderMACAddress", V::String("AA:BB:CC:DD:EE:FF".into())),
        ("model", V::String("iPhone14,3".into())),
        (
            "clientBundleID",
            V::String("com.teamofsilicons.extend".into()),
        ),
        ("clientProcName", V::String("SiliconExtend".into())),
    ])
}

/// Answer to an AirPlay NTP timing request (32 bytes, big-endian), as pyatv's timing server does.
pub(crate) fn timing_reply(request: &[u8], now_ntp: u64) -> Option<[u8; 32]> {
    if request.len() < 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out[0] = request[0];
    out[1] = 0x53 | 0x80;
    out[2..4].copy_from_slice(&7u16.to_be_bytes());
    // Reference time: the requester's send time.
    out[8..16].copy_from_slice(&request[24..32]);
    let (sec, frac) = ((now_ntp >> 32) as u32, now_ntp as u32);
    out[16..20].copy_from_slice(&sec.to_be_bytes());
    out[20..24].copy_from_slice(&frac.to_be_bytes());
    out[24..28].copy_from_slice(&sec.to_be_bytes());
    out[28..32].copy_from_slice(&frac.to_be_bytes());
    Some(out)
}

fn ntp_now() -> u64 {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() + 0x83AA_7E80;
    let frac = ((d.subsec_micros() as u64) << 32) / 1_000_000;
    (secs << 32) | frac
}

// ───────────── Local media server ─────────────

/// `Range: bytes=a-b` → inclusive (start, end) within `len`.
pub(crate) fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
    let spec = header
        .trim()
        .strip_prefix("bytes=")?
        .split(',')
        .next()?
        .trim();
    let (a, b) = spec.split_once('-')?;
    if len == 0 {
        return None;
    }
    let (start, end) = match (a.trim(), b.trim()) {
        ("", suffix) => {
            let n: u64 = suffix.parse().ok()?;
            (len.saturating_sub(n), len - 1)
        }
        (s, "") => (s.parse().ok()?, len - 1),
        (s, e) => (s.parse().ok()?, e.parse::<u64>().ok()?.min(len - 1)),
    };
    (start <= end && start < len).then_some((start, end))
}

/// Serves one file over HTTP (GET/HEAD with byte ranges) until dropped.
pub(crate) struct MediaServer {
    pub url: String,
    task: JoinHandle<()>,
}

impl Drop for MediaServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) async fn serve_file(path: PathBuf, bind: IpAddr) -> io::Result<MediaServer> {
    let listener = TcpListener::bind(SocketAddr::new(bind, 0)).await?;
    let port = listener.local_addr()?.port();
    let name: String = path
        .file_name()
        .map(|n| {
            n.to_string_lossy()
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || ".-_".contains(c) {
                        c
                    } else {
                        '_'
                    }
                })
                .collect()
        })
        .unwrap_or_else(|| "media".into());
    let url = format!(
        "http://{}:{port}/{name}",
        crate::common::url_host(&bind.to_string())
    );
    let content_type = crate::common::content_type_for(&path).to_owned();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let (path, ct) = (path.clone(), content_type.clone());
            tokio::spawn(async move {
                let mut parser = Parser::default();
                while let Ok(req) = http::read_message(&mut s, &mut parser).await {
                    let head = req.start_line.starts_with("HEAD");
                    let Ok(mut file) = tokio::fs::File::open(&path).await else {
                        return;
                    };
                    let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
                    let range = req.header("range").and_then(|r| parse_range(r, len));
                    let (status, start, end) = match range {
                        Some((a, b)) => ("206 Partial Content", a, b),
                        None => ("200 OK", 0, len.saturating_sub(1)),
                    };
                    let count = if len == 0 { 0 } else { end - start + 1 };
                    let mut h = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nAccept-Ranges: bytes\r\nContent-Length: {count}\r\n"
                    );
                    if range.is_some() {
                        h.push_str(&format!("Content-Range: bytes {start}-{end}/{len}\r\n"));
                    }
                    h.push_str("\r\n");
                    if s.write_all(h.as_bytes()).await.is_err() {
                        return;
                    }
                    if head || count == 0 {
                        continue;
                    }
                    use tokio::io::AsyncSeekExt;
                    if file.seek(io::SeekFrom::Start(start)).await.is_err() {
                        return;
                    }
                    let mut left = count;
                    let mut buf = vec![0u8; 64 * 1024];
                    while left > 0 {
                        let want = (left as usize).min(buf.len());
                        let Ok(n) = file.read(&mut buf[..want]).await else {
                            return;
                        };
                        if n == 0 || s.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                        left -= n as u64;
                    }
                }
            });
        }
    });
    Ok(MediaServer { url, task })
}

// ───────────── Sessions ─────────────

/// Something on the TV screen; stays until [`Showing::stop`] or the media ends.
pub(crate) struct Showing {
    pub what: String,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Showing {
    #[cfg(test)]
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    pub async fn stop(mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        let _ = tokio::time::timeout(Duration::from_secs(3), &mut self.task).await;
        self.task.abort();
    }
}

/// Where the video comes from.
pub(crate) enum Media {
    Url(String),
    File(PathBuf),
}

/// Starts playing a video and keeps the AirPlay session alive in the background.
pub(crate) async fn play_video(
    host: &str,
    port: u16,
    media: Media,
    creds: Option<&Credentials>,
    name: &str,
) -> Result<Showing, String> {
    let mut ctl = Control::connect(host, port)
        .await
        .map_err(|e| format!("Can't reach AirPlay on the Apple TV: {e}"))?;
    ctl.authenticate(creds, port).await?;
    let local = ctl.conn.local_ip().map_err(|e| e.to_string())?;
    let peer = ctl.conn.peer_ip().map_err(|e| e.to_string())?;

    let (server, url) = match media {
        Media::Url(u) => (None, u),
        Media::File(p) => {
            let s = serve_file(p, local)
                .await
                .map_err(|e| format!("Can't serve the video from this Mac: {e}"))?;
            let url = s.url.clone();
            (Some(s), url)
        }
    };

    let timing = UdpSocket::bind(SocketAddr::new(local, 0))
        .await
        .map_err(|e| e.to_string())?;
    let timing_port = timing.local_addr().map_err(|e| e.to_string())?.port();
    let setup_uuid = ctl.session_uuid.clone();
    let setup = ctl
        .bplist(
            "SETUP",
            None,
            "RTSP/1.0",
            setup_body(&setup_uuid, timing_port, name),
        )
        .await
        .map_err(|e| format!("AirPlay SETUP: {e}"))?;
    if !setup.is_success() {
        return Err(format!("AirPlay SETUP was refused ({})", setup.status()));
    }
    let event_port = plist::from_bytes::<plist::Value>(&setup.body)
        .ok()
        .and_then(|v| {
            v.as_dictionary()
                .and_then(|d| d.get("eventPort"))
                .and_then(|p| p.as_unsigned_integer())
        })
        .unwrap_or(0) as u16;
    let events = if event_port != 0 {
        let stream = crate::tls::tcp(&peer.to_string(), event_port, TIMEOUT)
            .await
            .map_err(|e| format!("AirPlay event channel: {e}"))?;
        let mut ev = HapConn::new(stream);
        let (out, inp) = ctl.keys.as_ref().expect("authenticated").derive(
            "Events-Salt",
            "Events-Read-Encryption-Key",
            "Events-Write-Encryption-Key",
        );
        ev.encrypt_with(out, inp);
        Some(ev)
    } else {
        None
    };
    let rec = ctl
        .request("RECORD", None, "RTSP/1.0", &[], None)
        .await
        .map_err(|e| format!("AirPlay RECORD: {e}"))?;
    if !rec.is_success() {
        return Err(format!("AirPlay RECORD was refused ({})", rec.status()));
    }
    let play_uuid = uuid::Uuid::new_v4().to_string();
    let extra = [
        ("X-Apple-ProtocolVersion", "1".to_owned()),
        ("X-Apple-Session-ID", ctl.session_uuid.to_lowercase()),
        ("X-Apple-Stream-ID", "1".to_owned()),
    ];
    let mut body = Vec::new();
    plist::to_writer_binary(&mut body, &play_body(&url, &play_uuid)).map_err(|e| e.to_string())?;
    let play = ctl
        .request(
            "POST",
            Some("/play"),
            "HTTP/1.1",
            &extra,
            Some((BPLIST, body)),
        )
        .await
        .map_err(|e| format!("AirPlay /play: {e}"))?;
    if !play.is_success() {
        return Err(format!(
            "The Apple TV wouldn't play it (HTTP {}): {}",
            play.status(),
            play.text().trim()
        ));
    }
    let _ = ctl
        .bplist(
            "PUT",
            Some("/setProperty?actionAtItemEnd"),
            "RTSP/1.0",
            dict(vec![("value", plist::Value::Integer(0.into()))]),
        )
        .await;
    let _ = ctl
        .request("POST", Some("/rate?value=1.000000"), "RTSP/1.0", &[], None)
        .await;

    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(keep_alive(ctl, events, timing, server, rx, true));
    Ok(Showing {
        what: url,
        stop: Some(tx),
        task,
    })
}

/// Shows a picture (`PUT /photo`) and keeps the connection open so it stays on screen.
pub(crate) async fn show_photo(
    host: &str,
    port: u16,
    bytes: Vec<u8>,
    content_type: &str,
    creds: Option<&Credentials>,
) -> Result<Showing, String> {
    let mut ctl = Control::connect(host, port)
        .await
        .map_err(|e| format!("Can't reach AirPlay on the Apple TV: {e}"))?;
    ctl.authenticate(creds, port).await?;
    let asset = uuid::Uuid::new_v4().to_string().to_uppercase();
    let extra = [
        ("X-Apple-AssetKey", asset.clone()),
        ("X-Apple-Transition", "Dissolve".to_owned()),
        ("X-Apple-Session-ID", ctl.session_uuid.to_lowercase()),
    ];
    let r = ctl
        .request(
            "PUT",
            Some("/photo"),
            "HTTP/1.1",
            &extra,
            Some((content_type, bytes)),
        )
        .await
        .map_err(|e| format!("AirPlay /photo: {e}"))?;
    if !r.is_success() {
        return Err(format!(
            "The Apple TV wouldn't show the picture (HTTP {})",
            r.status()
        ));
    }
    let timing = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| e.to_string())?;
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(keep_alive(ctl, None, timing, None, rx, false));
    Ok(Showing {
        what: format!("photo {asset}"),
        stop: Some(tx),
        task,
    })
}

/// Holds an AirPlay session open: answers timing and event messages, sends feedback, and ends on
/// stop (or, for videos, when the Apple TV reports playback has ended).
async fn keep_alive(
    mut ctl: Control,
    mut events: Option<HapConn>,
    timing: UdpSocket,
    _server: Option<MediaServer>,
    mut stop: oneshot::Receiver<()>,
    video: bool,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    let mut buf = [0u8; 128];
    let mut started = false;
    let mut idle_polls = 0;
    loop {
        let event = async {
            match events.as_mut() {
                Some(ev) => ev.read_message(Duration::from_secs(3600)).await.ok(),
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = &mut stop => break,
            r = timing.recv_from(&mut buf) => {
                if let Ok((n, from)) = r
                    && let Some(reply) = timing_reply(&buf[..n], ntp_now())
                {
                    let _ = timing.send_to(&reply, from).await;
                }
            }
            m = event => {
                match m {
                    Some(req) => {
                        let mut headers: Vec<(&str, String)> = vec![("Audio-Latency", "0".into())];
                        if let Some(c) = req.header("CSeq") {
                            headers.push(("CSeq", c.to_owned()));
                        }
                        let proto = req.start_line.split_whitespace().last().unwrap_or("RTSP/1.0").to_owned();
                        let reply = http::encode_response(&proto, 200, "OK", &headers, b"");
                        if let Some(ev) = events.as_mut() {
                            let _ = ev.send(&reply).await;
                        }
                    }
                    None => events = None,
                }
            }
            _ = tick.tick() => {
                if ctl.request("POST", Some("/feedback"), "RTSP/1.0", &[], None).await.is_err() {
                    break; // the Apple TV dropped the session (someone pressed back or menu)
                }
                if video && let Ok(info) = ctl.request("GET", Some("/playback-info"), "HTTP/1.1", &[], None).await {
                    let playing = plist::from_bytes::<plist::Value>(&info.body)
                        .ok()
                        .and_then(|v| v.as_dictionary().map(|d| d.contains_key("duration")))
                        .unwrap_or(false);
                    if playing {
                        started = true;
                        idle_polls = 0;
                    } else {
                        idle_polls += 1;
                        if (started && idle_polls >= 2) || idle_polls >= 15 {
                            break;
                        }
                    }
                }
            }
        }
    }
    let _ = ctl
        .request("POST", Some("/stop"), "HTTP/1.1", &[], None)
        .await;
    let sid = ctl.session_id.to_string();
    let _ = ctl
        .request("TEARDOWN", None, "RTSP/1.0", &[("Session", sid)], None)
        .await;
}

#[cfg(test)]
pub(crate) mod mock {
    //! A pretend AirPlay receiver: transient pairing, encrypted control connection, SETUP with an
    //! event port, RECORD, /play, /photo, /feedback, /playback-info. Logs `METHOD path`.
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::appletv::hap::accessory::Accessory;

    pub(crate) struct MockReceiver {
        pub port: u16,
        pub log: Arc<Mutex<Vec<String>>>,
        pub played: Arc<Mutex<Vec<String>>>,
        pub photos: Arc<Mutex<Vec<usize>>>,
    }

    pub(crate) async fn start() -> MockReceiver {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ev_l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let event_port = ev_l.local_addr().unwrap().port();
        let rx = MockReceiver {
            port: l.local_addr().unwrap().port(),
            log: Arc::default(),
            played: Arc::default(),
            photos: Arc::default(),
        };
        let (log, played, photos) = (rx.log.clone(), rx.played.clone(), rx.photos.clone());
        let session_key: Arc<Mutex<Option<Vec<u8>>>> = Arc::default();
        let sk2 = session_key.clone();
        tokio::spawn(async move {
            // Event channel: the receiver would push events; we only check it connects encrypted.
            while let Ok((s, _)) = ev_l.accept().await {
                let k = sk2.lock().unwrap().clone().unwrap();
                let mut conn = HapConn::new(s);
                // The receiver writes with Events-Write and reads with Events-Read.
                conn.encrypt_with(
                    hap::hkdf("Events-Salt", "Events-Write-Encryption-Key", &k),
                    hap::hkdf("Events-Salt", "Events-Read-Encryption-Key", &k),
                );
                let req = http::encode_request(
                    "POST",
                    "/command",
                    "RTSP/1.0",
                    &[("CSeq", "1".into())],
                    b"",
                );
                conn.send(&req).await.unwrap();
                let reply = conn.read_message(Duration::from_secs(5)).await.unwrap();
                assert_eq!(reply.status(), 200);
                std::future::pending::<()>().await;
            }
        });
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                let (log, played, photos, sk) = (
                    log.clone(),
                    played.clone(),
                    photos.clone(),
                    session_key.clone(),
                );
                tokio::spawn(async move {
                    let mut acc = Accessory::new(TRANSIENT_PIN);
                    let mut conn = HapConn::new(s);
                    let mut playing_polls = 0;
                    while let Ok(req) = conn.read_message(Duration::from_secs(30)).await {
                        let mut parts = req.start_line.split_whitespace();
                        let (method, path) = (
                            parts.next().unwrap_or("").to_owned(),
                            parts.next().unwrap_or("").to_owned(),
                        );
                        let proto = parts.next().unwrap_or("HTTP/1.1").to_owned();
                        log.lock().unwrap().push(format!("{method} {path}"));
                        let cseq = req.header("CSeq").unwrap_or("0").to_owned();
                        let (status, body): (u16, Vec<u8>) = match (method.as_str(), path.as_str())
                        {
                            ("POST", "/pair-pin-start") => (200, vec![]),
                            ("POST", "/pair-setup") => {
                                let flags = hap::tlv_decode(&req.body)
                                    .unwrap()
                                    .get(hap::tag::FLAGS)
                                    .map(<[u8]>::to_vec);
                                let seq = hap::tlv_decode(&req.body)
                                    .unwrap()
                                    .get(hap::tag::SEQ_NO)
                                    .unwrap()[0];
                                if seq == 1 {
                                    assert_eq!(flags, Some(vec![0x10]), "transient flag");
                                }
                                let reply = acc.setup(&req.body);
                                (200, reply)
                            }
                            ("SETUP", _) => {
                                let v: plist::Value = plist::from_bytes(&req.body).unwrap();
                                assert!(v.as_dictionary().unwrap().contains_key("timingPort"));
                                let mut b = Vec::new();
                                plist::to_writer_binary(
                                    &mut b,
                                    &dict(vec![(
                                        "eventPort",
                                        plist::Value::Integer((event_port as u64).into()),
                                    )]),
                                )
                                .unwrap();
                                (200, b)
                            }
                            ("POST", "/play") => {
                                let v: plist::Value = plist::from_bytes(&req.body).unwrap();
                                let url = v
                                    .as_dictionary()
                                    .unwrap()
                                    .get("Content-Location")
                                    .unwrap()
                                    .as_string()
                                    .unwrap()
                                    .to_owned();
                                played.lock().unwrap().push(url);
                                (200, vec![])
                            }
                            ("PUT", "/photo") => {
                                photos.lock().unwrap().push(req.body.len());
                                (200, vec![])
                            }
                            ("GET", "/playback-info") => {
                                playing_polls += 1;
                                let mut b = Vec::new();
                                let d = if playing_polls < 1000 {
                                    dict(vec![("duration", plist::Value::Real(12.0))])
                                } else {
                                    dict(vec![])
                                };
                                plist::to_writer_binary(&mut b, &d).unwrap();
                                (200, b)
                            }
                            _ => (200, vec![]),
                        };
                        let resp =
                            http::encode_response(&proto, status, "OK", &[("CSeq", cseq)], &body);
                        conn.send(&resp).await.unwrap();
                        // After M4 of a transient pairing, both sides switch to encryption.
                        if path == "/pair-setup"
                            && hap::tlv_decode(&req.body).unwrap().get(hap::tag::SEQ_NO)
                                == Some(&[3][..])
                        {
                            let k = acc.session_key().unwrap().to_vec();
                            *sk.lock().unwrap() = Some(k.clone());
                            conn.encrypt_with(
                                hap::hkdf("Control-Salt", "Control-Read-Encryption-Key", &k),
                                hap::hkdf("Control-Salt", "Control-Write-Encryption-Key", &k),
                            );
                        }
                    }
                });
            }
        });
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_round_trip_and_match_pyatv() {
        let key: [u8; 32] = (0..32).collect::<Vec<u8>>().try_into().unwrap();
        let mut c = SessionCipher::new(key, key, NonceStyle::Counter8);
        let sealed = seal_blocks(&mut c, b"GET / RTSP/1.0");
        // pyatv HAPSession: 2-byte LE length, then the counter-8 ChaCha20 frame (see hap vectors).
        let v: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/hap-vectors.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            hex::encode(&sealed),
            format!("0e00{}", v["chacha_counter8_0"].as_str().unwrap())
        );

        let big: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
        let mut enc = SessionCipher::new(key, [1; 32], NonceStyle::Counter8);
        let mut dec = SessionCipher::new([1; 32], key, NonceStyle::Counter8);
        let mut buf = seal_blocks(&mut enc, &big);
        assert_eq!(buf.len(), 3000 + 3 * 18);
        let tail = buf.split_off(1500);
        let mut got = open_blocks(&mut dec, &mut buf).unwrap();
        buf.extend(tail);
        got.extend(open_blocks(&mut dec, &mut buf).unwrap());
        assert_eq!(got, big);
        assert!(buf.is_empty());
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-1", 10), Some((0, 1)));
        assert_eq!(parse_range("bytes=5-", 10), Some((5, 9)));
        assert_eq!(parse_range("bytes=-3", 10), Some((7, 9)));
        assert_eq!(parse_range("bytes=8-100", 10), Some((8, 9)));
        assert_eq!(parse_range("bytes=11-12", 10), None);
        assert_eq!(parse_range("items=0-1", 10), None);
    }

    #[test]
    fn timing_packets() {
        let mut req = [0u8; 32];
        req[0] = 0x80;
        req[1] = 0xD2;
        req[24..32].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let r = timing_reply(&req, 0x1122_3344_5566_7788).unwrap();
        assert_eq!(r[0], 0x80);
        assert_eq!(r[1], 0xD3);
        assert_eq!(&r[2..4], &[0, 7]);
        assert_eq!(&r[8..16], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            &r[16..24],
            &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]
        );
        assert!(timing_reply(&[0; 4], 0).is_none());
    }

    #[test]
    fn bodies_are_binary_plists() {
        let mut b = Vec::new();
        plist::to_writer_binary(&mut b, &play_body("http://10.0.0.2:5000/a.mp4", "u1")).unwrap();
        assert!(b.starts_with(b"bplist00"));
        let v: plist::Value = plist::from_bytes(&b).unwrap();
        assert_eq!(
            v.as_dictionary()
                .unwrap()
                .get("Content-Location")
                .unwrap()
                .as_string(),
            Some("http://10.0.0.2:5000/a.mp4")
        );
        let s = setup_body("UUID", 5555, "Silicon Extend");
        assert_eq!(
            s.as_dictionary()
                .unwrap()
                .get("timingPort")
                .unwrap()
                .as_unsigned_integer(),
            Some(5555)
        );
    }

    #[tokio::test]
    async fn serves_files_with_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("clip one.mp4");
        std::fs::write(&p, b"0123456789").unwrap();
        let s = serve_file(p, "127.0.0.1".parse().unwrap()).await.unwrap();
        assert!(s.url.ends_with("/clip_one.mp4"), "{}", s.url);
        let r = http::request(
            "GET",
            &s.url,
            &[("Range", "bytes=2-5".into())],
            b"",
            Duration::from_secs(2),
            false,
        )
        .await
        .unwrap();
        assert_eq!(r.status(), 206);
        assert_eq!(r.body, b"2345");
        assert_eq!(r.header("content-type"), Some("video/mp4"));
        let r = http::request("GET", &s.url, &[], b"", Duration::from_secs(2), false)
            .await
            .unwrap();
        assert_eq!(r.body, b"0123456789");
    }

    #[tokio::test]
    async fn plays_and_shows_against_a_mock_receiver() {
        let rx = mock::start().await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("clip.mp4");
        std::fs::write(&file, vec![0u8; 5000]).unwrap();
        let showing = play_video(
            "127.0.0.1",
            rx.port,
            Media::File(file),
            None,
            "Silicon Extend",
        )
        .await
        .unwrap();
        let url = rx.played.lock().unwrap()[0].clone();
        assert!(
            url.starts_with("http://127.0.0.1:") && url.ends_with("/clip.mp4"),
            "{url}"
        );
        // The Apple TV fetches the file from our server.
        let r = http::request(
            "GET",
            &url,
            &[("Range", "bytes=0-9".into())],
            b"",
            Duration::from_secs(2),
            false,
        )
        .await
        .unwrap();
        assert_eq!(r.body.len(), 10);
        tokio::time::sleep(Duration::from_millis(2300)).await; // one feedback tick
        assert!(!showing.is_finished());
        showing.stop().await;
        let log = rx.log.lock().unwrap().clone();
        for want in [
            "POST /pair-pin-start",
            "POST /pair-setup",
            "SETUP",
            "RECORD",
            "POST /play",
            "POST /rate?value=1.000000",
            "POST /feedback",
            "POST /stop",
            "TEARDOWN",
        ] {
            assert!(
                log.iter().any(|l| l.starts_with(want)),
                "missing {want} in {log:?}"
            );
        }

        let showing = show_photo(
            "127.0.0.1",
            rx.port,
            vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3],
            "image/jpeg",
            None,
        )
        .await
        .unwrap();
        assert_eq!(*rx.photos.lock().unwrap(), vec![7]);
        showing.stop().await;
    }
}

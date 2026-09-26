//! The Companion protocol ("Companion Link", `_companion-link._tcp`), which the Apple TV Remote in
//! Control Centre uses. Frames are `[type u8][length u24 BE][payload]`; payloads are OPACK
//! dictionaries. Pairing frames (`PS_*`, `PV_*`) carry HAP TLV8 in `_pd`. After Pair-Verify every
//! non-empty payload is ChaCha20-Poly1305 sealed with the frame header as associated data, keys
//! from HKDF(salt "", info "ClientEncrypt-main" / "ServerEncrypt-main"), 12-byte counter nonces.
//!
//! Requests are `{"_i": name, "_t": 2, "_c": content, "_x": xid}`; the reply is `_t: 3` with the
//! same `_x`, and `_em` when it failed. Events (`_t: 1`) from the Apple TV are skipped.

use std::io;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::hap::{
    self, Credentials, NonceStyle, PairSetup, PairVerify, PairingError, SessionCipher,
};
use super::opack::{self, Value};

pub(crate) mod frame {
    pub const PS_START: u8 = 3;
    pub const PS_NEXT: u8 = 4;
    pub const PV_START: u8 = 5;
    pub const PV_NEXT: u8 = 6;
    pub const U_OPACK: u8 = 7;
    pub const E_OPACK: u8 = 8;
    pub const P_OPACK: u8 = 9;
}

const AUTH_TAG: usize = 16;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// HID button codes (`_hidC`).
pub(crate) mod hid {
    pub const UP: u64 = 1;
    pub const DOWN: u64 = 2;
    pub const LEFT: u64 = 3;
    pub const RIGHT: u64 = 4;
    pub const MENU: u64 = 5;
    pub const SELECT: u64 = 6;
    pub const HOME: u64 = 7;
    pub const VOLUME_UP: u64 = 8;
    pub const VOLUME_DOWN: u64 = 9;
    pub const SLEEP: u64 = 12;
    pub const WAKE: u64 = 13;
    pub const PLAY_PAUSE: u64 = 14;
}

pub(crate) fn header(frame_type: u8, len: usize) -> [u8; 4] {
    let l = (len as u32).to_be_bytes();
    [frame_type, l[1], l[2], l[3]]
}

#[derive(Debug)]
pub(crate) enum CompanionError {
    Io(io::Error),
    Pairing(PairingError),
    /// The Apple TV answered with `_em`.
    Refused(String),
    Protocol(String),
}

impl std::fmt::Display for CompanionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Pairing(e) => write!(f, "{e}"),
            Self::Refused(m) => write!(f, "the Apple TV refused: {m}"),
            Self::Protocol(m) => f.write_str(m),
        }
    }
}

impl From<io::Error> for CompanionError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<PairingError> for CompanionError {
    fn from(e: PairingError) -> Self {
        Self::Pairing(e)
    }
}

/// Who we say we are in `_systemInfo`.
#[derive(Debug, Clone)]
pub(crate) struct Identity {
    pub name: String,
    pub model: String,
    /// A MAC-address-shaped identifier, stable for this Mac.
    pub device_id: String,
    /// A short hex identifier, stable for this Mac.
    pub rp_id: String,
}

pub(crate) struct Companion {
    stream: TcpStream,
    cipher: Option<SessionCipher>,
    buf: Vec<u8>,
    xid: u64,
    pub session_id: Option<u64>,
}

impl Companion {
    pub async fn connect(host: &str, port: u16, timeout: Duration) -> io::Result<Self> {
        let stream = crate::tls::tcp(host, port, timeout).await?;
        Ok(Self {
            stream,
            cipher: None,
            buf: Vec::new(),
            xid: rand::random::<u16>() as u64,
            session_id: None,
        })
    }

    pub async fn send_frame(&mut self, frame_type: u8, payload: &[u8]) -> io::Result<()> {
        let bytes = match self.cipher.as_mut() {
            Some(c) if !payload.is_empty() => {
                let h = header(frame_type, payload.len() + AUTH_TAG);
                [&h[..], &c.encrypt(payload, &h)].concat()
            }
            _ => [&header(frame_type, payload.len())[..], payload].concat(),
        };
        self.stream.write_all(&bytes).await?;
        self.stream.flush().await
    }

    pub async fn read_frame(&mut self, timeout: Duration) -> io::Result<(u8, Vec<u8>)> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.buf.len() >= 4 {
                let len = u32::from_be_bytes([0, self.buf[1], self.buf[2], self.buf[3]]) as usize;
                if self.buf.len() >= 4 + len {
                    let h: [u8; 4] = self.buf[..4].try_into().expect("4 bytes");
                    let payload: Vec<u8> = self.buf[4..4 + len].to_vec();
                    self.buf.drain(..4 + len);
                    let payload = match self.cipher.as_mut() {
                        Some(c) if !payload.is_empty() => c.decrypt(&payload, &h).map_err(|e| {
                            io::Error::new(io::ErrorKind::InvalidData, e.to_string())
                        })?,
                        _ => payload,
                    };
                    return Ok((h[0], payload));
                }
            }
            let mut chunk = [0u8; 8192];
            let n = tokio::time::timeout_at(deadline, self.stream.read(&mut chunk))
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the Apple TV didn't answer in time",
                    )
                })??;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the Apple TV closed the connection",
                ));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    fn next_xid(&mut self) -> u64 {
        self.xid += 1;
        self.xid
    }

    /// Sends a pairing frame and waits for the Apple TV's `PS_Next`/`PV_Next` reply; returns `_pd`.
    async fn auth(
        &mut self,
        frame_type: u8,
        mut fields: Vec<(&str, Value)>,
        timeout: Duration,
    ) -> Result<Vec<u8>, CompanionError> {
        let reply_type = match frame_type {
            frame::PS_START | frame::PS_NEXT => frame::PS_NEXT,
            _ => frame::PV_NEXT,
        };
        let xid = self.next_xid();
        fields.push(("_x", Value::Int(xid)));
        let dict = Value::Dict(
            fields
                .into_iter()
                .map(|(k, v)| (Value::str(k), v))
                .collect(),
        );
        self.send_frame(frame_type, &opack::encode(&dict)).await?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let (t, payload) = self.read_frame(left).await?;
            if t != reply_type {
                continue;
            }
            let v = opack::decode(&payload).map_err(|e| CompanionError::Protocol(e.to_string()))?;
            return v
                .get("_pd")
                .and_then(Value::as_bytes)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| CompanionError::Protocol("pairing reply without _pd".into()));
        }
    }

    /// Pair-Setup step 1: after this the Apple TV shows a PIN.
    pub async fn pair_start(&mut self, setup: &mut PairSetup) -> Result<(), CompanionError> {
        let pd = self
            .auth(
                frame::PS_START,
                vec![("_pd", Value::Bytes(setup.m1())), ("_pwTy", Value::Int(1))],
                REQUEST_TIMEOUT,
            )
            .await?;
        setup.handle_m2(&pd)?;
        Ok(())
    }

    /// Pair-Setup steps 2–3 with the PIN the Carbon read off the screen.
    pub async fn pair_finish(
        &mut self,
        setup: &mut PairSetup,
        pin: &str,
        name: &str,
    ) -> Result<(Credentials, Option<Value>), CompanionError> {
        let m3 = setup.m3(pin)?;
        let pd = self
            .auth(
                frame::PS_NEXT,
                vec![("_pd", Value::Bytes(m3)), ("_pwTy", Value::Int(1))],
                REQUEST_TIMEOUT,
            )
            .await?;
        setup.handle_m4(&pd)?;
        let m5 = setup.m5(Some(name))?;
        let pd = self
            .auth(
                frame::PS_NEXT,
                vec![("_pd", Value::Bytes(m5)), ("_pwTy", Value::Int(1))],
                REQUEST_TIMEOUT,
            )
            .await?;
        Ok(setup.handle_m6(&pd)?)
    }

    /// Pair-Verify with stored credentials, then turns on encryption.
    pub async fn verify(&mut self, creds: &Credentials) -> Result<(), CompanionError> {
        let mut pv = PairVerify::default();
        let pd = self
            .auth(
                frame::PV_START,
                vec![("_pd", Value::Bytes(pv.m1())), ("_auTy", Value::Int(4))],
                REQUEST_TIMEOUT,
            )
            .await?;
        let m3 = pv.handle_m2(creds, &pd)?;
        let pd = self
            .auth(
                frame::PV_NEXT,
                vec![("_pd", Value::Bytes(m3))],
                REQUEST_TIMEOUT,
            )
            .await?;
        pv.handle_m4(&pd)?;
        let (out, inp) = pv
            .keys("", "ClientEncrypt-main", "ServerEncrypt-main")
            .expect("verified");
        self.cipher = Some(SessionCipher::new(out, inp, NonceStyle::Counter12));
        Ok(())
    }

    /// Sends a request and returns the reply's `_c` (an empty dictionary when absent).
    pub async fn request(
        &mut self,
        identifier: &str,
        content: Value,
    ) -> Result<Value, CompanionError> {
        let xid = self.next_xid();
        let msg = Value::dict([
            ("_i", Value::str(identifier)),
            ("_t", Value::Int(2)),
            ("_c", content),
            ("_x", Value::Int(xid)),
        ]);
        self.send_frame(frame::E_OPACK, &opack::encode(&msg))
            .await?;
        let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let (t, payload) = self.read_frame(left).await?;
            if !matches!(t, frame::E_OPACK | frame::U_OPACK | frame::P_OPACK) {
                continue;
            }
            let v = opack::decode(&payload).map_err(|e| CompanionError::Protocol(e.to_string()))?;
            if v.get("_t").and_then(Value::as_u64) != Some(3)
                || v.get("_x").and_then(Value::as_u64) != Some(xid)
            {
                continue; // an event, or a reply to something else
            }
            if let Some(em) = v.get("_em") {
                return Err(CompanionError::Refused(
                    em.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("{em:?}")),
                ));
            }
            return Ok(v.get("_c").cloned().unwrap_or(Value::Dict(vec![])));
        }
    }

    /// Introduces us and opens a remote-control session (what the Remote app does on connect).
    pub async fn start_session(
        &mut self,
        me: &Identity,
        client_id: &[u8],
    ) -> Result<(), CompanionError> {
        self.request(
            "_systemInfo",
            Value::dict([
                ("_bf", Value::Int(0)),
                ("_cf", Value::Int(512)),
                ("_clFl", Value::Int(128)),
                ("_i", Value::str(&me.rp_id)),
                ("_idsID", Value::Bytes(client_id.to_vec())),
                ("_pubID", Value::str(&me.device_id)),
                ("_sf", Value::Int(256)),
                ("_sv", Value::str("170.18")),
                ("model", Value::str(&me.model)),
                ("name", Value::str(&me.name)),
            ]),
        )
        .await?;
        self.request(
            "_touchStart",
            Value::dict([
                ("_height", Value::Float(1000.0)),
                ("_tFl", Value::Int(0)),
                ("_width", Value::Float(1000.0)),
            ]),
        )
        .await?;
        let local = rand::random::<u32>() as u64;
        let reply = self
            .request(
                "_sessionStart",
                Value::dict([
                    ("_srvT", Value::str("com.apple.tvremoteservices")),
                    ("_sid", Value::Int(local)),
                ]),
            )
            .await?;
        let remote = reply.get("_sid").and_then(Value::as_u64).unwrap_or(0);
        self.session_id = Some((remote << 32) | local);
        // Newer tvOS wants a TV Remote session before it answers some queries; older ones refuse it.
        let _ = self
            .request(
                "TVRCSessionStart",
                Value::dict([("ProtocolVersionKey", Value::str("1.2"))]),
            )
            .await;
        Ok(())
    }

    pub async fn hid(&mut self, code: u64, down: bool) -> Result<(), CompanionError> {
        self.request(
            "_hidC",
            Value::dict([
                ("_hBtS", Value::Int(if down { 1 } else { 2 })),
                ("_hidC", Value::Int(code)),
            ]),
        )
        .await?;
        Ok(())
    }

    pub async fn launch(&mut self, bundle_or_url: &str) -> Result<(), CompanionError> {
        let key = if crate::common::is_url_or_scheme(bundle_or_url) {
            "_urlS"
        } else {
            "_bundleID"
        };
        self.request(
            "_launchApp",
            Value::dict([(key, Value::str(bundle_or_url))]),
        )
        .await?;
        Ok(())
    }

    /// Launchable apps as (bundle id, name).
    pub async fn apps(&mut self) -> Result<Vec<(String, String)>, CompanionError> {
        let c = self
            .request("FetchLaunchableApplicationsEvent", Value::Dict(vec![]))
            .await?;
        let Value::Dict(pairs) = c else {
            return Ok(vec![]);
        };
        let mut apps: Vec<(String, String)> = pairs
            .into_iter()
            .filter_map(|(k, v)| {
                Some((
                    k.as_str()?.to_owned(),
                    v.as_str().unwrap_or_default().to_owned(),
                ))
            })
            .collect();
        apps.sort_by_key(|a| a.1.to_lowercase());
        Ok(apps)
    }

    /// 1 asleep, 2 screensaver, 3 awake, 4 idle.
    pub async fn attention_state(&mut self) -> Result<u64, CompanionError> {
        let c = self
            .request("FetchAttentionState", Value::Dict(vec![]))
            .await?;
        c.get("state")
            .and_then(Value::as_u64)
            .ok_or_else(|| CompanionError::Protocol("no state in reply".into()))
    }
}

/// Wraps pairing errors from the setup code path for the Carbon.
pub(crate) fn describe(e: &CompanionError) -> String {
    match e {
        CompanionError::Pairing(hap::PairingError::WrongCode) => "That code didn't match.".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
pub(crate) mod mock {
    //! A pretend Apple TV: HAP pairing (via `hap::accessory`), encrypted frames, and the requests
    //! the driver sends. It logs every request it decrypts.
    use std::sync::{Arc, Mutex};

    use tokio::net::TcpListener;

    use super::*;
    use crate::appletv::hap::accessory::Accessory;

    pub(crate) struct MockAppleTv {
        pub port: u16,
        pub accessory: Arc<Mutex<Accessory>>,
        pub log: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
        /// How many times a PIN was put on screen.
        pub pins_shown: Arc<Mutex<u32>>,
    }

    pub(crate) async fn start(pin: &str) -> MockAppleTv {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tv = MockAppleTv {
            port: l.local_addr().unwrap().port(),
            accessory: Arc::new(Mutex::new(Accessory::new(pin))),
            log: Arc::default(),
            pins_shown: Arc::default(),
        };
        let (acc, log, pins) = (tv.accessory.clone(), tv.log.clone(), tv.pins_shown.clone());
        tokio::spawn(async move {
            loop {
                let Ok((s, _)) = l.accept().await else { return };
                let (acc, log, pins) = (acc.clone(), log.clone(), pins.clone());
                tokio::spawn(async move {
                    let mut c = Companion {
                        stream: s,
                        cipher: None,
                        buf: vec![],
                        xid: 0,
                        session_id: None,
                    };
                    while let Ok((t, payload)) = c.read_frame(Duration::from_secs(30)).await {
                        let Ok(v) = opack::decode(&payload) else {
                            return;
                        };
                        let xid = v.get("_x").cloned().unwrap_or(Value::Int(0));
                        match t {
                            frame::PS_START | frame::PS_NEXT => {
                                if t == frame::PS_START {
                                    *pins.lock().unwrap() += 1;
                                }
                                let reply = acc
                                    .lock()
                                    .unwrap()
                                    .setup(v.get("_pd").unwrap().as_bytes().unwrap());
                                let d = Value::dict([("_pd", Value::Bytes(reply)), ("_x", xid)]);
                                c.send_frame(frame::PS_NEXT, &opack::encode(&d))
                                    .await
                                    .unwrap();
                            }
                            frame::PV_START | frame::PV_NEXT => {
                                let body = v.get("_pd").unwrap().as_bytes().unwrap().to_vec();
                                let is_m3 = hap::tlv_decode(&body).unwrap().get(hap::tag::SEQ_NO)
                                    == Some(&[3][..]);
                                let reply = acc.lock().unwrap().verify(&body);
                                let ok = hap::tlv_decode(&reply)
                                    .unwrap()
                                    .get(hap::tag::ERROR)
                                    .is_none();
                                let d = Value::dict([("_pd", Value::Bytes(reply)), ("_x", xid)]);
                                c.send_frame(frame::PV_NEXT, &opack::encode(&d))
                                    .await
                                    .unwrap();
                                if is_m3 && ok {
                                    let (out, inp) = acc.lock().unwrap().keys(
                                        "",
                                        "ServerEncrypt-main",
                                        "ClientEncrypt-main",
                                    );
                                    c.cipher =
                                        Some(SessionCipher::new(out, inp, NonceStyle::Counter12));
                                }
                            }
                            frame::E_OPACK => {
                                let name =
                                    v.get("_i").and_then(Value::as_str).unwrap_or("").to_owned();
                                let content = v.get("_c").cloned().unwrap_or(Value::Null);
                                log.lock().unwrap().push((name.clone(), content.to_json()));
                                if v.get("_t").and_then(Value::as_u64) != Some(2) {
                                    continue;
                                }
                                // An event first, as real devices interleave them.
                                let ev = Value::dict([
                                    ("_i", Value::str("_iMC")),
                                    ("_t", Value::Int(1)),
                                    ("_c", Value::Dict(vec![])),
                                ]);
                                c.send_frame(frame::E_OPACK, &opack::encode(&ev))
                                    .await
                                    .unwrap();
                                let (key, body) = match name.as_str() {
                                    "_sessionStart" => {
                                        ("_c", Value::dict([("_sid", Value::Int(0x2a))]))
                                    }
                                    "FetchLaunchableApplicationsEvent" => (
                                        "_c",
                                        Value::dict([
                                            ("com.netflix.Netflix", Value::str("Netflix")),
                                            ("com.apple.TVSettings", Value::str("Settings")),
                                            ("com.google.ios.youtube", Value::str("YouTube")),
                                        ]),
                                    ),
                                    "FetchAttentionState" => {
                                        ("_c", Value::dict([("state", Value::Int(3))]))
                                    }
                                    "_launchApp"
                                        if content.get("_bundleID").and_then(Value::as_str)
                                            == Some("com.example.missing") =>
                                    {
                                        ("_em", Value::str("app not installed"))
                                    }
                                    _ => ("_c", Value::Dict(vec![])),
                                };
                                let reply = Value::Dict(vec![
                                    (Value::str(key), body),
                                    (Value::str("_t"), Value::Int(3)),
                                    (Value::str("_x"), xid),
                                ]);
                                c.send_frame(frame::E_OPACK, &opack::encode(&reply))
                                    .await
                                    .unwrap();
                            }
                            _ => {}
                        }
                    }
                });
            }
        });
        tv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_headers() {
        assert_eq!(header(frame::E_OPACK, 0x16), [8, 0, 0, 0x16]);
        assert_eq!(header(frame::PS_START, 0x012345), [3, 0x01, 0x23, 0x45]);
    }

    #[tokio::test]
    async fn pairs_verifies_and_sends_requests() {
        let tv = mock::start("2468").await;
        let mut c = Companion::connect("127.0.0.1", tv.port, Duration::from_secs(2))
            .await
            .unwrap();
        let mut setup = PairSetup::new("4D797FD3-0000-4000-8000-00000000BEEF");
        c.pair_start(&mut setup).await.unwrap();
        assert_eq!(*tv.pins_shown.lock().unwrap(), 1);
        let (creds, info) = c
            .pair_finish(&mut setup, "2468", "Silicon Bridge")
            .await
            .unwrap();
        assert_eq!(
            info.unwrap().get("name").unwrap().as_str(),
            Some("Living Room")
        );

        let mut c = Companion::connect("127.0.0.1", tv.port, Duration::from_secs(2))
            .await
            .unwrap();
        c.verify(&creds).await.unwrap();
        let me = Identity {
            name: "Silicon Bridge".into(),
            model: "Mac".into(),
            device_id: "02:00:00:00:00:01".into(),
            rp_id: "a1b2c3d4e5f6".into(),
        };
        c.start_session(&me, &creds.client_id).await.unwrap();
        assert_eq!(c.session_id.unwrap() >> 32, 0x2a);
        c.hid(hid::SELECT, true).await.unwrap();
        c.hid(hid::SELECT, false).await.unwrap();
        c.launch("com.netflix.Netflix").await.unwrap();
        c.launch("youtube://watch?v=1").await.unwrap();
        let apps = c.apps().await.unwrap();
        assert_eq!(
            apps[0],
            ("com.netflix.Netflix".to_string(), "Netflix".to_string())
        );
        assert_eq!(c.attention_state().await.unwrap(), 3);
        let err = c.launch("com.example.missing").await.unwrap_err();
        assert!(
            matches!(err, CompanionError::Refused(ref m) if m == "app not installed"),
            "{err}"
        );

        let log = tv.log.lock().unwrap().clone();
        let names: Vec<&str> = log.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            &names[..4],
            &[
                "_systemInfo",
                "_touchStart",
                "_sessionStart",
                "TVRCSessionStart"
            ]
        );
        assert!(log.contains(&("_hidC".into(), serde_json::json!({"_hBtS": 1, "_hidC": 6}))));
        assert!(log.contains(&(
            "_launchApp".into(),
            serde_json::json!({"_bundleID": "com.netflix.Netflix"})
        )));
        assert!(log.contains(&(
            "_launchApp".into(),
            serde_json::json!({"_urlS": "youtube://watch?v=1"})
        )));
    }

    /// Pairs, verifies and sends requests to a server made of pyatv's own server-side code
    /// (`tests/fixtures/pyatv_companion_server.py`). Needs a Python with pyatv:
    /// `BRIDGE_HOSTED_PYATV_PYTHON=/path/to/python cargo test -p bridge-hosted -- --ignored interop`.
    #[tokio::test]
    #[ignore = "needs BRIDGE_HOSTED_PYATV_PYTHON pointing at a Python with pyatv installed"]
    async fn interop_with_pyatv_server() {
        use tokio::io::AsyncBufReadExt;
        let python =
            std::env::var("BRIDGE_HOSTED_PYATV_PYTHON").expect("set BRIDGE_HOSTED_PYATV_PYTHON");
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pyatv_companion_server.py");
        let mut child = tokio::process::Command::new(python)
            .arg(script)
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let first = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let port: u16 = first.strip_prefix("PORT ").unwrap().parse().unwrap();

        let mut c = Companion::connect("127.0.0.1", port, Duration::from_secs(2))
            .await
            .unwrap();
        let mut setup = PairSetup::new("4D797FD3-3538-427E-A47B-A32FC6CF3A6A");
        c.pair_start(&mut setup).await.unwrap();
        let (creds, info) = c
            .pair_finish(&mut setup, "1111", "Silicon Bridge")
            .await
            .unwrap();
        assert_eq!(creds.atv_id, b"5D797FD3-3538-427E-A47B-A32FC6CF3A6A");
        assert_eq!(
            info.unwrap().get("model").unwrap().as_str(),
            Some("AppleTV6,2")
        );

        let mut c = Companion::connect("127.0.0.1", port, Duration::from_secs(2))
            .await
            .unwrap();
        c.verify(&creds).await.unwrap();
        let me = Identity {
            name: "Silicon Bridge".into(),
            model: "Mac".into(),
            device_id: "02:00:00:00:00:01".into(),
            rp_id: "a1b2c3d4e5f6".into(),
        };
        c.start_session(&me, &creds.client_id).await.unwrap();
        assert_eq!(c.session_id.unwrap() >> 32, 77);
        c.hid(hid::MENU, true).await.unwrap();
        c.launch("com.netflix.Netflix").await.unwrap();
        let apps = c.apps().await.unwrap();
        assert!(apps.contains(&("com.netflix.Netflix".to_string(), "Netflix".to_string())));

        // What pyatv decrypted and decoded, in order.
        let mut seen = Vec::new();
        while seen.len() < 7 {
            let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            seen.push(v["message"].clone());
        }
        let names: Vec<&str> = seen.iter().map(|m| m["_i"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "_systemInfo",
                "_touchStart",
                "_sessionStart",
                "TVRCSessionStart",
                "_hidC",
                "_launchApp",
                "FetchLaunchableApplicationsEvent"
            ]
        );
        assert_eq!(seen[4]["_c"], serde_json::json!({"_hBtS": 1, "_hidC": 5}));
        assert_eq!(
            seen[5]["_c"],
            serde_json::json!({"_bundleID": "com.netflix.Netflix"})
        );
        assert_eq!(seen[0]["_c"]["_idsID"], hex::encode(&creds.client_id));
    }

    #[tokio::test]
    async fn wrong_pin_is_reported() {
        let tv = mock::start("2468").await;
        let mut c = Companion::connect("127.0.0.1", tv.port, Duration::from_secs(2))
            .await
            .unwrap();
        let mut setup = PairSetup::new("id");
        c.pair_start(&mut setup).await.unwrap();
        let err = c.pair_finish(&mut setup, "0000", "x").await.unwrap_err();
        assert_eq!(describe(&err), "That code didn't match.");
    }
}

//! WebSocket frames between an Extend app and the service (TECHNICAL.md section 7).
//!
//! Every frame is a JSON text message `{"type": ..., "id": ..., "data": ...}`. Files never travel
//! on the socket; a device uploads them over HTTP with the upload ids a `command` carries.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::capability::{Capability, DeviceOs};
use crate::ids::{DeviceCredential, DeviceId, SessionId};
use crate::model::{
    Attachment, EndReason, InUseIndicator, MissingCapability, Setup, SleepState, TestingEnvironment, Timestamp,
};

/// Frames a paired device sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceFrame {
    /// First frame after connecting, and again whenever capabilities change.
    Hello(Hello),
    SetupProgress {
        setup: Setup,
    },
    /// The answer to a `command` with the same id.
    Result(CommandOutcome),
    /// The Carbon tapped Stop. `target` names a device this host carries; absent means this device
    /// (and, on a host, everything it carries).
    Stop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<DeviceId>,
    },
    /// The Carbon tapped Done on a takeover (on this device, or on the carried device `target`).
    TakeoverDone {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<DeviceId>,
    },
    /// A device paired through this host changed state.
    Attached(AttachedStatus),
    Pong {
        nonce: u64,
    },
    /// 1.1: whether this device is awake. Sent right after every hello, on every change, on each
    /// pair's connection. The service applies it when `run` differs from the last one it applied
    /// (a new app process), or `run` is the same and `seq` is larger, or `run` is absent.
    Awake {
        awake: bool,
        /// Why it isn't awake. Absent when awake.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sleep_state: Option<SleepState>,
        /// true: the device saw an unlock or real input with this change; false: it woke with no
        /// such sign (the app sends `awake` again with true at the first unlock or input);
        /// absent: it can't tell. Only a wake that isn't `false` resolves wake requests.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_seen: Option<bool>,
        /// Random for each app process.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run: Option<Uuid>,
        /// Increases across all of the app's connections within one `run`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
    },
    /// 1.1: whether the device could show a wake request (once per `wake_id`).
    WakeRequestShown {
        wake_id: Uuid,
        shown: bool,
        /// Why not, when `shown` is false (at most [`crate::WAKE_NOTE_MAX_CHARS`] characters):
        /// "Notifications are off for Silicon Extend on this phone."
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// 1.1: the app stored the credential the last `credential` frame on this connection gave it;
    /// the old one stops working.
    CredentialSaved,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub app_version: String,
    pub os: DeviceOs,
    #[serde(default)]
    pub os_version: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Version of the device engine. 1.0 apps send it as `agent_device_version`, still read.
    #[serde(default, alias = "agent_device_version")]
    pub engine_version: Option<String>,
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub missing: Vec<MissingCapability>,
    pub setup: Setup,
    /// 1.1: what this app or agent can do beyond 1.0, from [`crate::feature`] (`"setup_retry"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

impl Hello {
    /// Whether the app advertised a feature from [`crate::feature`].
    pub fn supports(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachedStatus {
    pub device_id: DeviceId,
    pub online: bool,
    #[serde(default)]
    pub os_version: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub missing: Vec<MissingCapability>,
    pub setup: Setup,
    /// 1.1: whether the carried device is awake (None: the host can't tell).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awake: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sleep_state: Option<SleepState>,
    /// 1.1: hex(HMAC-SHA256(hardware_salt, driver + ":" + stable hardware id)), with the world's
    /// salt from `GET /api/v1/device`. A pseudonym, not a secret; never the raw id. Absent until
    /// the host has the salt (a 1.0 service sends none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandOutcome {
    pub id: Uuid,
    pub ok: bool,
    #[serde(default)]
    pub output: serde_json::Value,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub error: Option<crate::model::CommandError>,
    /// Files uploaded for this command, each under one of the command's upload ids.
    #[serde(default)]
    pub files: Vec<ProducedFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProducedFile {
    pub upload_id: Uuid,
    pub name: String,
    pub content_type: String,
    pub kind: crate::model::FileKind,
    pub size_bytes: i64,
}

/// Frames the service sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServiceFrame {
    /// Run a command. `target` is set when the command is for a device paired through this host.
    Command(CommandFrame),
    Cancel {
        id: Uuid,
    },
    SessionStarted {
        #[serde(default)]
        target: Option<DeviceId>,
        session_id: SessionId,
        silicon_id: String,
        #[serde(with = "time::serde::rfc3339")]
        since: Timestamp,
        /// 1.1: the session's side tag. While it runs, the app treats every wake request whose
        /// side differs (or is absent) as redacted, before it runs any command of the session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        side: Option<String>,
    },
    SessionEnded {
        #[serde(default)]
        target: Option<DeviceId>,
        session_id: SessionId,
        reason: EndReason,
    },
    Takeover {
        #[serde(default)]
        target: Option<DeviceId>,
        session_id: SessionId,
        reason: String,
        #[serde(with = "time::serde::rfc3339")]
        expires_at: Timestamp,
    },
    TakeoverEnded {
        #[serde(default)]
        target: Option<DeviceId>,
        session_id: SessionId,
    },
    /// The device's name, owner or environment changed; re-read `GET /api/v1/device`.
    Refresh,
    /// A device paired through this host needs setting up (or was removed when `removed`).
    /// 1.1: sent again when the carried device's `in_use_indicator` changes.
    Attach {
        device_id: DeviceId,
        os: DeviceOs,
        name: String,
        #[serde(default)]
        address: Option<String>,
        #[serde(default)]
        removed: bool,
        /// 1.1: whether the carried device shows the in-use badge or banner. Absent (a 1.0
        /// service) means shown.
        #[serde(default)]
        in_use_indicator: InUseIndicator,
    },
    /// A setup code the Carbon entered for an attached device (Apple TV).
    SetupCode {
        device_id: DeviceId,
        code: String,
    },
    Environment {
        environment: Option<TestingEnvironment>,
    },
    /// The pair ended; forget the credential and show the pairing screen.
    Unpaired {
        reason: EndReason,
    },
    /// A newer connection replaced this one; close without reconnecting.
    Superseded,
    Ping {
        nonce: u64,
    },
    /// 1.1: a Silicon asks this device's Carbon to wake it. Sent on the connection of the pair the
    /// request was made through (for a carried device, to its host with `target`, which only
    /// probes it every 5 s and shows nothing). A frame without `silicon_id` and `reason` replaces
    /// what the app holds for that `wake_id`: the device then shows "A Silicon asked to use this
    /// device; its Carbon was told through Ting".
    WakeRequest {
        #[serde(default)]
        target: Option<DeviceId>,
        wake_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        silicon_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The request's side tag (see `session_started.side`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        side: Option<String>,
        /// Sound or vibrate for this one (at most every 15 minutes per device).
        #[serde(default)]
        alert: bool,
        #[serde(with = "time::serde::rfc3339")]
        created_at: Timestamp,
        #[serde(with = "time::serde::rfc3339")]
        expires_at: Timestamp,
    },
    /// 1.1: forget a wake request, whatever the reason.
    WakeRequestEnded {
        #[serde(default)]
        target: Option<DeviceId>,
        wake_id: Uuid,
        reason: WakeEnd,
    },
    /// 1.1, computer pairs only: this pair's new credential. Store it, then answer
    /// `credential_saved`; the old one works until then. Never log it.
    Credential {
        device_credential: DeviceCredential,
    },
    /// 1.1: run the named failed setup step (every failed step when `step` is absent) again now,
    /// then report as usual: `setup_progress`, or `attached` for the carried device `target`.
    /// Sent only to apps whose hello lists [`crate::feature::SETUP_RETRY`].
    SetupRetry {
        #[serde(default)]
        target: Option<DeviceId>,
        #[serde(default)]
        step: Option<String>,
    },
}

open_enum! {
    /// Why a wake request ended, as the device hears it.
    pub enum WakeEnd {
        Woken => "woken",
        Expired => "expired",
        Withdrawn => "withdrawn",
        Declined => "declined",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandFrame {
    pub id: Uuid,
    pub session_id: SessionId,
    #[serde(default)]
    pub target: Option<DeviceId>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// Milliseconds the device has before the service gives up.
    pub timeout_ms: u64,
    /// Upload ids for files this command may produce, used in order.
    pub upload_ids: Vec<Uuid>,
}

/// Frames on an unpaired app's enrollment socket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EnrollmentFrame {
    Code {
        pairing_code: String,
        #[serde(with = "time::serde::rfc3339")]
        code_expires_at: Timestamp,
    },
    Paired {
        device_id: DeviceId,
        device_credential: String,
        #[serde(default)]
        environment: Option<TestingEnvironment>,
    },
    Ping {
        nonce: u64,
    },
}

/// WebSocket close codes.
pub mod close {
    pub const UNAUTHORIZED: u16 = 4401;
    pub const SUPERSEDED: u16 = 4409;
    pub const UPGRADE_REQUIRED: u16 = 4426;
    /// 1.x only: the device's test environment was not open (it was disabled, or its services
    /// were not ready yet). The pair is kept: the app stays paired and reconnects with backoff.
    /// The close reason says which. A 2.0 service has no test environments and never sends it.
    pub const ENVIRONMENT_UNAVAILABLE: u16 = 4503;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let f = ServiceFrame::Command(CommandFrame {
            id: Uuid::nil(),
            session_id: "a3f".parse().unwrap(),
            target: None,
            command: "click".into(),
            args: vec!["@e2".into()],
            attachments: vec![],
            timeout_ms: 30_000,
            upload_ids: vec![],
        });
        let s = serde_json::to_string(&f).unwrap();
        assert!(s.starts_with("{\"type\":\"command\""), "{s}");
        assert_eq!(serde_json::from_str::<ServiceFrame>(&s).unwrap(), f);
        let stop: DeviceFrame = serde_json::from_str(r#"{"type":"stop"}"#).unwrap();
        assert_eq!(stop, DeviceFrame::Stop { target: None });
        assert_eq!(serde_json::to_string(&stop).unwrap(), r#"{"type":"stop"}"#);
        let t: DeviceFrame = serde_json::from_str(r#"{"type":"takeover_done","target":"7c1e09ab"}"#).unwrap();
        assert_eq!(
            t,
            DeviceFrame::TakeoverDone {
                target: Some("7c1e09ab".parse().unwrap())
            }
        );
    }

    use serde_json::{Value, json};
    use time::macros::datetime;

    use crate::model::{SetupState, SetupStep, StepStatus};

    const RUN: &str = "0192f3a4-5b6c-7d8e-9f00-112233445566";
    const WAKE: &str = "0192f3a4-0000-7000-8000-000000000001";

    /// Serializes to exactly `expected`, and `expected` decodes back to the same frame.
    fn exact<T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(frame: &T, expected: Value) {
        assert_eq!(serde_json::to_value(frame).unwrap(), expected);
        assert_eq!(&serde_json::from_value::<T>(expected).unwrap(), frame);
    }

    fn device(json: Value) -> DeviceFrame {
        serde_json::from_value(json).unwrap()
    }

    fn service(json: Value) -> ServiceFrame {
        serde_json::from_value(json).unwrap()
    }

    fn setup() -> Setup {
        Setup::from_steps(vec![SetupStep {
            key: "usb_debugging".into(),
            title: "Turn on USB debugging".into(),
            status: StepStatus::Failed,
            help: None,
            error: Some("The phone refused the connection. Tap Allow on the phone, then Retry.".into()),
            input: None,
        }])
    }

    #[test]
    fn awake_frames() {
        exact(
            &DeviceFrame::Awake {
                awake: false,
                sleep_state: Some(SleepState::ScreenOff),
                input_seen: None,
                run: Some(RUN.parse().unwrap()),
                seq: Some(41),
            },
            json!({"type":"awake","awake":false,"sleep_state":"screen_off","run":RUN,"seq":41}),
        );
        exact(
            &DeviceFrame::Awake {
                awake: true,
                sleep_state: None,
                input_seen: Some(true),
                run: Some(RUN.parse().unwrap()),
                seq: Some(42),
            },
            json!({"type":"awake","awake":true,"input_seen":true,"run":RUN,"seq":42}),
        );
        // No run or seq (an app that can't order its frames), and a state from a newer app.
        assert_eq!(
            device(json!({"type":"awake","awake":false,"sleep_state":"hibernating"})),
            DeviceFrame::Awake {
                awake: false,
                sleep_state: Some(SleepState::Other),
                input_seen: None,
                run: None,
                seq: None
            }
        );
    }

    #[test]
    fn wake_request_shown_and_credential_saved() {
        exact(
            &DeviceFrame::WakeRequestShown {
                wake_id: WAKE.parse().unwrap(),
                shown: false,
                note: Some("Notifications are off for Silicon Extend on this phone.".into()),
            },
            json!({"type":"wake_request_shown","wake_id":WAKE,"shown":false,"note":"Notifications are off for Silicon Extend on this phone."}),
        );
        exact(
            &DeviceFrame::WakeRequestShown {
                wake_id: WAKE.parse().unwrap(),
                shown: true,
                note: None,
            },
            json!({"type":"wake_request_shown","wake_id":WAKE,"shown":true}),
        );
        exact(&DeviceFrame::CredentialSaved, json!({"type":"credential_saved"}));
    }

    #[test]
    fn attached_gains_awake_and_hardware_key() {
        let id: DeviceId = "3a2b0c1d".parse().unwrap();
        let status = AttachedStatus {
            device_id: id.clone(),
            online: true,
            os_version: None,
            model: None,
            capabilities: vec![Capability::InputRemote],
            missing: vec![],
            setup: Setup::complete(),
            awake: Some(false),
            sleep_state: Some(SleepState::Standby),
            hardware_key: Some("5f".repeat(32)),
        };
        exact(
            &DeviceFrame::Attached(status.clone()),
            json!({"type":"attached","device_id":"3a2b0c1d","online":true,"os_version":null,"model":null,
                   "capabilities":["input.remote"],"missing":[],"setup":{"state":"complete","steps":[]},
                   "awake":false,"sleep_state":"standby","hardware_key":"5f".repeat(32)}),
        );
        // A 1.0 host's frame.
        let old = device(
            json!({"type":"attached","device_id":"3a2b0c1d","online":true,"setup":{"state":"complete","steps":[]}}),
        );
        let DeviceFrame::Attached(old) = old else {
            panic!("{old:?}")
        };
        assert_eq!((old.awake, old.sleep_state, old.hardware_key), (None, None, None));
    }

    #[test]
    fn hello_engine_version_and_features() {
        // A 1.0 app's hello (contracts/v1/device/agent.device.socket.json).
        let old = json!({"type":"hello","agent_device_version":"0.13.0","app_version":"1.0.0",
            "capabilities":["screen.read","terminal"],"missing":[],"model":"MacBookPro18,3","os":"macos",
            "os_version":"15.5","setup":{"state":"complete","steps":[]}});
        let DeviceFrame::Hello(hello) = device(old) else {
            panic!()
        };
        assert_eq!(hello.engine_version.as_deref(), Some("0.13.0"));
        assert!(hello.features.is_empty());
        assert!(!hello.supports(crate::feature::SETUP_RETRY));
        let v = serde_json::to_value(DeviceFrame::Hello(hello.clone())).unwrap();
        assert_eq!(v["engine_version"], "0.13.0");
        assert!(
            v.get("agent_device_version").is_none() && v.get("features").is_none(),
            "{v}"
        );

        let new = Hello {
            features: vec![crate::feature::SETUP_RETRY.into(), "something_newer".into()],
            ..hello
        };
        assert!(new.supports("setup_retry"));
        let v = serde_json::to_value(DeviceFrame::Hello(new.clone())).unwrap();
        assert_eq!(v["features"], json!(["setup_retry", "something_newer"]));
        assert_eq!(
            serde_json::from_value::<DeviceFrame>(v).unwrap(),
            DeviceFrame::Hello(new)
        );
    }

    #[test]
    fn wake_request_frames() {
        let created = datetime!(2026-09-27 10:02 UTC);
        let expires = datetime!(2026-09-27 10:32 UTC);
        let full = ServiceFrame::WakeRequest {
            target: None,
            wake_id: WAKE.parse().unwrap(),
            silicon_id: Some("si:chef".into()),
            reason: Some("Check the order screen".into()),
            side: Some("9f2c4b1a0d3e5f67".into()),
            alert: true,
            created_at: created,
            expires_at: expires,
        };
        exact(
            &full,
            json!({"type":"wake_request","target":null,"wake_id":WAKE,"silicon_id":"si:chef",
                   "reason":"Check the order screen","side":"9f2c4b1a0d3e5f67","alert":true,
                   "created_at":"2026-09-27T10:02:00Z","expires_at":"2026-09-27T10:32:00Z"}),
        );
        // Redacted, and for a carried device: no Silicon, no reason.
        exact(
            &ServiceFrame::WakeRequest {
                target: Some("3a2b0c1d".parse().unwrap()),
                wake_id: WAKE.parse().unwrap(),
                silicon_id: None,
                reason: None,
                side: None,
                alert: false,
                created_at: created,
                expires_at: expires,
            },
            json!({"type":"wake_request","target":"3a2b0c1d","wake_id":WAKE,"alert":false,
                   "created_at":"2026-09-27T10:02:00Z","expires_at":"2026-09-27T10:32:00Z"}),
        );
        for end in WakeEnd::ALL {
            exact(
                &ServiceFrame::WakeRequestEnded {
                    target: None,
                    wake_id: WAKE.parse().unwrap(),
                    reason: *end,
                },
                json!({"type":"wake_request_ended","target":null,"wake_id":WAKE,"reason":end.as_str()}),
            );
        }
        assert_eq!(
            service(json!({"type":"wake_request_ended","wake_id":WAKE,"reason":"snoozed"})),
            ServiceFrame::WakeRequestEnded {
                target: None,
                wake_id: WAKE.parse().unwrap(),
                reason: WakeEnd::Other
            }
        );
    }

    #[test]
    fn credential_frame_hides_the_secret_from_debug() {
        let secret = crate::ids::new_secret(crate::ids::DEVICE_CREDENTIAL_PREFIX);
        let frame = ServiceFrame::Credential {
            device_credential: DeviceCredential::new(secret.clone()),
        };
        exact(&frame, json!({"type":"credential","device_credential":secret}));
        assert!(!format!("{frame:?}").contains(&secret[4..]));
    }

    #[test]
    fn session_started_side() {
        let since = datetime!(2026-09-27 10:05 UTC);
        let old = json!({"type":"session_started","target":null,"session_id":"a3f","silicon_id":"si:chef","since":"2026-09-27T10:05:00Z"});
        let frame = service(old.clone());
        let ServiceFrame::SessionStarted { side, .. } = &frame else {
            panic!()
        };
        assert_eq!(side, &None);
        // Without a side it serializes exactly as 1.0 did.
        assert_eq!(serde_json::to_value(&frame).unwrap(), old);
        exact(
            &ServiceFrame::SessionStarted {
                target: None,
                session_id: "a3f".parse().unwrap(),
                silicon_id: "si:chef".into(),
                since,
                side: Some("9f2c4b1a0d3e5f67".into()),
            },
            json!({"type":"session_started","target":null,"session_id":"a3f","silicon_id":"si:chef",
                   "since":"2026-09-27T10:05:00Z","side":"9f2c4b1a0d3e5f67"}),
        );
    }

    #[test]
    fn setup_retry_frame() {
        exact(
            &ServiceFrame::SetupRetry {
                target: None,
                step: None,
            },
            json!({"type":"setup_retry","target":null,"step":null}),
        );
        exact(
            &ServiceFrame::SetupRetry {
                target: Some("3a2b0c1d".parse().unwrap()),
                step: Some("usb_debugging".into()),
            },
            json!({"type":"setup_retry","target":"3a2b0c1d","step":"usb_debugging"}),
        );
        assert_eq!(
            service(json!({"type":"setup_retry"})),
            ServiceFrame::SetupRetry {
                target: None,
                step: None
            }
        );
        let s = setup();
        assert_eq!(s.state, SetupState::NeedsCarbon);
        assert_eq!(
            s.failed().map(|x| x.key.as_str()).collect::<Vec<_>>(),
            ["usb_debugging"]
        );
        assert!(s.step("usb_debugging").is_some() && s.step("nope").is_none());
        // setup_progress is unchanged.
        let p = DeviceFrame::SetupProgress { setup: s };
        assert_eq!(
            serde_json::from_value::<DeviceFrame>(serde_json::to_value(&p).unwrap()).unwrap(),
            p
        );
    }

    #[test]
    fn attach_carries_the_in_use_indicator() {
        let attach = |in_use_indicator| ServiceFrame::Attach {
            device_id: "3a2b0c1d".parse().unwrap(),
            os: DeviceOs::Ios,
            name: "Alice's iPhone".into(),
            address: None,
            removed: false,
            in_use_indicator,
        };
        exact(
            &attach(InUseIndicator::Hidden),
            json!({"type":"attach","device_id":"3a2b0c1d","os":"ios","name":"Alice's iPhone","address":null,
                   "removed":false,"in_use_indicator":"hidden"}),
        );
        exact(
            &attach(InUseIndicator::Shown),
            json!({"type":"attach","device_id":"3a2b0c1d","os":"ios","name":"Alice's iPhone","address":null,
                   "removed":false,"in_use_indicator":"shown"}),
        );
        // A 1.0 service's frame: shown.
        assert_eq!(
            service(json!({"type":"attach","device_id":"3a2b0c1d","os":"ios","name":"Alice's iPhone"})),
            attach(InUseIndicator::Shown)
        );
        // A value from a newer service decodes, and still shows.
        let ServiceFrame::Attach { in_use_indicator, .. } = service(
            json!({"type":"attach","device_id":"3a2b0c1d","os":"ios","name":"Alice's iPhone","in_use_indicator":"dimmed"}),
        ) else {
            panic!()
        };
        assert_eq!(in_use_indicator, InUseIndicator::Other);
        assert!(in_use_indicator.shows());
    }
}

//! A scripted Extend app for end-to-end tests of the CLI and website.
//!
//! `cargo run -p extend-service --example fake_device -- <api_url> <os>`
//! prints `PAIRING_CODE <code>` on stdout, waits to be paired, prints `PAIRED <device_id>`, then
//! answers every command: `screenshot` uploads a small PNG, `is` fails, everything else echoes.
//!
//! Options, from the environment (e2e/cli-e2e.sh sets them through `FAKE_ENV`):
//! - `FAKE_APP_VERSION` (default `1.0.0`): the app version it reports. Any other version is a 1.1
//!   app: its hello lists the `setup_retry` feature.
//! - `FAKE_AWAKE=true|false` with `FAKE_SLEEP_STATE=<screen_off|locked|asleep|standby|other_session>`:
//!   an `awake` frame right after each hello. Without `FAKE_AWAKE` it sends none, like a 1.0 app.
//! - `FAKE_PAIR_ANOTHER=1`: once paired, it starts "Pair with another Carbon" and prints
//!   `PAIRING_CODE_2 <code>`; when that code is claimed it prints `PAIRED_2 <device_id>` and
//!   connects that pair too, as a 1.1 app keeps one connection per pair.
//! - `FAKE_FAILED_STEP=<key>`: its setup has that step failed. On `setup_retry` it reports the step
//!   in progress, then done.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use extend_protocol::frames::{CommandOutcome, DeviceFrame, EnrollmentFrame, Hello, ProducedFile, ServiceFrame};
use extend_protocol::model::{
    CommandError, EnrollmentCreate, EnrollmentCreated, FileKind, Setup, SetupStep, SleepState, StepStatus,
};
use extend_protocol::{DeviceOs, feature};
use futures::{SinkExt as _, StreamExt as _};
use silicon_extend_client::Client;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use uuid::Uuid;

// A 1×1 transparent PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49,
    0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00,
    0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

/// What the fake is, from its arguments and `FAKE_*` options; shared by every pair's connection.
struct Fake {
    client: Client,
    os: DeviceOs,
    app_version: String,
    awake: Option<bool>,
    sleep_state: Option<SleepState>,
    failed_step: Option<String>,
    /// One run per process and one sequence across all connections, as a 1.1 app sends `awake`.
    run: Uuid,
    seq: AtomicU64,
}

impl Fake {
    fn is_1_0(&self) -> bool {
        self.app_version == "1.0.0"
    }

    fn setup(&self, failed: bool) -> Setup {
        match &self.failed_step {
            None => Setup::complete(),
            Some(key) => Setup::from_steps(vec![SetupStep {
                key: key.clone(),
                title: "Fake step".into(),
                status: if failed { StepStatus::Failed } else { StepStatus::Done },
                help: None,
                error: failed.then(|| "The fake step didn't finish. Retry it from the website or the CLI.".into()),
                input: None,
            }]),
        }
    }

    fn hello(&self, failed: bool) -> DeviceFrame {
        DeviceFrame::Hello(Hello {
            app_version: self.app_version.clone(),
            os: self.os,
            os_version: Some("1".into()),
            model: Some("Fake device".into()),
            engine_version: None,
            capabilities: self.os.full_capabilities().to_vec(),
            missing: vec![],
            setup: self.setup(failed),
            features: if self.is_1_0() {
                vec![]
            } else {
                vec![feature::SETUP_RETRY.into()]
            },
        })
    }

    fn awake(&self) -> Option<DeviceFrame> {
        let awake = self.awake?;
        Some(DeviceFrame::Awake {
            awake,
            sleep_state: if awake { None } else { self.sleep_state },
            input_seen: None,
            run: Some(self.run),
            seq: Some(self.seq.fetch_add(1, Ordering::Relaxed) + 1),
        })
    }
}

fn text(frame: &impl serde::Serialize) -> anyhow::Result<Message> {
    Ok(Message::Text(serde_json::to_string(frame)?.into()))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let base = args.next().unwrap_or_else(|| "http://127.0.0.1:8480".into());
    let os: DeviceOs = serde_json::from_value(serde_json::json!(args.next().unwrap_or_else(|| "linux".into())))?;
    // Extend 4 has no test environments; a third argument (a test app secret before 4.0) is ignored.
    let b = Client::builder(&base);
    let opt = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let fake = Arc::new(Fake {
        client: b.connect().await?,
        os,
        app_version: opt("FAKE_APP_VERSION").unwrap_or_else(|| "1.0.0".into()),
        awake: opt("FAKE_AWAKE").map(|v| v != "false" && v != "0"),
        sleep_state: opt("FAKE_SLEEP_STATE").map(|s| SleepState::parse(&s)),
        failed_step: opt("FAKE_FAILED_STEP"),
        run: Uuid::new_v4(),
        seq: AtomicU64::new(0),
    });
    let e = fake
        .client
        .enroll(&EnrollmentCreate {
            os,
            os_version: Some("1".into()),
            model: Some("Fake device".into()),
            app_version: fake.app_version.clone(),
            engine_version: None,
        })
        .await?;
    println!("PAIRING_CODE {}", e.pairing_code);
    let (device_id, credential) = wait_paired(&fake.client, &e, "PAIRING_CODE").await?;
    println!("PAIRED {device_id}");
    if opt("FAKE_PAIR_ANOTHER").is_some_and(|v| v != "0") {
        let e2 = fake.client.pair_enrollment(&credential).await?;
        println!("PAIRING_CODE_2 {}", e2.pairing_code);
        let fake = fake.clone();
        tokio::spawn(async move {
            let run = async {
                let (device_id, credential) = wait_paired(&fake.client, &e2, "PAIRING_CODE_2").await?;
                println!("PAIRED_2 {device_id}");
                serve(&fake, &credential).await
            };
            if let Err(e) = run.await {
                println!("SECOND PAIR FAILED {e}");
            }
        });
    }
    if opt("FAKE_RECONNECT").is_some_and(|v| v != "0") {
        loop {
            let _ = serve(&fake, &credential).await;
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    }
    serve(&fake, &credential).await
}

/// Follows an enrollment's socket until a Carbon claims its code; prints each new code after
/// `label`.
async fn wait_paired(client: &Client, e: &EnrollmentCreated, label: &str) -> anyhow::Result<(String, String)> {
    let mut req = client
        .ws_url(&format!("/api/v1/enrollments/{}/connect", e.enrollment_id))
        .into_client_request()?;
    req.headers_mut().insert(
        "authorization",
        format!("Extend-Enrollment {}", e.enrollment_secret).parse()?,
    );
    let (mut ews, _) = tokio_tungstenite::connect_async(req).await?;
    loop {
        let Some(Ok(Message::Text(t))) = ews.next().await else {
            anyhow::bail!("enrollment socket closed")
        };
        match serde_json::from_str::<EnrollmentFrame>(&t)? {
            EnrollmentFrame::Paired {
                device_id,
                device_credential,
                ..
            } => return Ok((device_id.to_string(), device_credential)),
            EnrollmentFrame::Code { pairing_code, .. } => println!("{label} {pairing_code}"),
            EnrollmentFrame::Ping { nonce } => {
                ews.send(Message::Text(
                    serde_json::json!({"type": "pong", "nonce": nonce}).to_string().into(),
                ))
                .await?;
            }
        }
    }
}

/// One pair's device connection: hello (and awake), then answers until the pair ends.
async fn serve(fake: &Fake, credential: &str) -> anyhow::Result<()> {
    let client = &fake.client;
    let mut req = client.ws_url("/api/v1/device/connect").into_client_request()?;
    req.headers_mut()
        .insert("authorization", format!("Extend-Device {credential}").parse()?);
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await?;
    let mut failed = fake.failed_step.is_some();
    ws.send(text(&fake.hello(failed))?).await?;
    if let Some(awake) = fake.awake() {
        ws.send(text(&awake)?).await?;
    }
    while let Some(msg) = ws.next().await {
        let Ok(Message::Text(t)) = msg else { continue };
        let frame: ServiceFrame = serde_json::from_str(&t)?;
        match frame {
            ServiceFrame::Ping { nonce } => ws.send(text(&DeviceFrame::Pong { nonce })?).await?,
            ServiceFrame::Command(c) => {
                println!("COMMAND {} {}", c.command, c.args.join(" "));
                let mut out = CommandOutcome {
                    id: c.id,
                    ok: true,
                    output: serde_json::json!({"command": c.command, "args": c.args, "attachments": c.attachments}),
                    text: Some(format!("{} {} → ok on the fake device", c.command, c.args.join(" "))),
                    error: None,
                    files: vec![],
                };
                match c.command.as_str() {
                    "screenshot" => {
                        client
                            .upload_artifact(credential, c.upload_ids[0], "screenshot.png", "image/png", PNG.to_vec())
                            .await?;
                        out.files.push(ProducedFile {
                            upload_id: c.upload_ids[0],
                            name: "screenshot.png".into(),
                            content_type: "image/png".into(),
                            kind: FileKind::Screenshot,
                            size_bytes: PNG.len() as i64,
                        });
                    }
                    "is" => {
                        out.ok = false;
                        out.error = Some(CommandError {
                            code: "assertion_failed".into(),
                            message: "The element is not visible.".into(),
                            details: serde_json::Value::Null,
                        });
                    }
                    "wait" => {
                        tokio::time::sleep(Duration::from_millis(
                            c.args.first().and_then(|a| a.parse().ok()).unwrap_or(0),
                        ))
                        .await
                    }
                    _ => {}
                }
                ws.send(text(&DeviceFrame::Result(out))?).await?;
            }
            ServiceFrame::SetupRetry { target: None, step } => {
                println!("SETUP_RETRY {}", step.as_deref().unwrap_or("*"));
                let ours = fake
                    .failed_step
                    .as_deref()
                    .is_some_and(|k| step.as_deref().is_none_or(|s| s == k));
                if failed && ours {
                    let mut setup = fake.setup(true);
                    setup.steps[0].status = StepStatus::InProgress;
                    setup.steps[0].error = None;
                    let setup = Setup::from_steps(setup.steps);
                    ws.send(text(&DeviceFrame::SetupProgress { setup })?).await?;
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    failed = false;
                }
                ws.send(text(&DeviceFrame::SetupProgress {
                    setup: fake.setup(failed),
                })?)
                .await?;
            }
            ServiceFrame::Unpaired { reason } => {
                println!("UNPAIRED {}", reason.as_str());
                break;
            }
            other => println!("FRAME {}", serde_json::to_string(&other)?),
        }
    }
    Ok(())
}

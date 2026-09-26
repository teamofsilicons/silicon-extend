//! A scripted Extend app for end-to-end tests of the CLI and website.
//!
//! `cargo run -p extend-service --example fake_device -- <api_url> <os> [test_app_secret]`
//! prints `PAIRING_CODE <code>` on stdout, waits to be paired, prints `PAIRED <device_id>`, then
//! answers every command: `screenshot` uploads a small PNG, `is` fails, everything else echoes.

use std::time::Duration;

use extend_protocol::DeviceOs;
use extend_protocol::frames::{CommandOutcome, DeviceFrame, EnrollmentFrame, Hello, ProducedFile, ServiceFrame};
use extend_protocol::model::{CommandError, EnrollmentCreate, FileKind, Setup};
use futures::{SinkExt as _, StreamExt as _};
use silicon_extend_client::Client;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

// A 1×1 transparent PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49,
    0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00,
    0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let base = args.next().unwrap_or_else(|| "http://127.0.0.1:8480".into());
    let os: DeviceOs = serde_json::from_value(serde_json::json!(args.next().unwrap_or_else(|| "linux".into())))?;
    let mut b = Client::builder(&base);
    if let Some(s) = args.next() {
        b = b.testing_secret(s);
    }
    let client = b.connect().await?;
    let e = client
        .enroll(&EnrollmentCreate {
            os,
            os_version: Some("1".into()),
            model: Some("Fake device".into()),
            app_version: "1.0.0".into(),
            agent_device_version: None,
        })
        .await?;
    println!("PAIRING_CODE {}", e.pairing_code);
    let mut req = client
        .ws_url(&format!("/api/v1/enrollments/{}/connect", e.enrollment_id))
        .into_client_request()?;
    req.headers_mut().insert(
        "authorization",
        format!("Extend-Enrollment {}", e.enrollment_secret).parse()?,
    );
    let (mut ews, _) = tokio_tungstenite::connect_async(req).await?;
    let (device_id, credential) = loop {
        let Some(Ok(Message::Text(t))) = ews.next().await else {
            anyhow::bail!("enrollment socket closed")
        };
        match serde_json::from_str::<EnrollmentFrame>(&t)? {
            EnrollmentFrame::Paired {
                device_id,
                device_credential,
                ..
            } => break (device_id, device_credential),
            EnrollmentFrame::Code { pairing_code, .. } => println!("PAIRING_CODE {pairing_code}"),
            EnrollmentFrame::Ping { nonce } => {
                ews.send(Message::Text(
                    serde_json::json!({"type": "pong", "nonce": nonce}).to_string().into(),
                ))
                .await?;
            }
        }
    };
    println!("PAIRED {device_id}");
    let mut req = client.ws_url("/api/v1/device/connect").into_client_request()?;
    req.headers_mut()
        .insert("authorization", format!("Extend-Device {credential}").parse()?);
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await?;
    let hello = DeviceFrame::Hello(Hello {
        app_version: "1.0.0".into(),
        os,
        os_version: Some("1".into()),
        model: Some("Fake device".into()),
        agent_device_version: None,
        capabilities: os.full_capabilities().to_vec(),
        missing: vec![],
        setup: Setup::complete(),
    });
    ws.send(Message::Text(serde_json::to_string(&hello)?.into())).await?;
    while let Some(msg) = ws.next().await {
        let Ok(Message::Text(t)) = msg else { continue };
        let frame: ServiceFrame = serde_json::from_str(&t)?;
        match frame {
            ServiceFrame::Ping { nonce } => {
                ws.send(Message::Text(
                    serde_json::to_string(&DeviceFrame::Pong { nonce })?.into(),
                ))
                .await?
            }
            ServiceFrame::Command(c) => {
                println!("COMMAND {} {}", c.command, c.args.join(" "));
                let mut out = CommandOutcome {
                    id: c.id,
                    ok: true,
                    output: serde_json::json!({"command": c.command, "args": c.args}),
                    text: Some(format!("{} {} → ok on the fake device", c.command, c.args.join(" "))),
                    error: None,
                    files: vec![],
                };
                match c.command.as_str() {
                    "screenshot" => {
                        client
                            .upload_artifact(
                                &credential,
                                c.upload_ids[0],
                                "screenshot.png",
                                "image/png",
                                PNG.to_vec(),
                            )
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
                ws.send(Message::Text(serde_json::to_string(&DeviceFrame::Result(out))?.into()))
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

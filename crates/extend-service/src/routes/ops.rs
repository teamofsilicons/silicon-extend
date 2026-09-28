//! Bug reports (emailed through Postmark) and telemetry.

use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use extend_protocol::model::{ReportInput, ReportReceipt};
use uuid::Uuid;

use super::{Body, envelope, hash_json, idempotent, no_content};
use crate::error::{AppError, AppResult};
use crate::state::{AppState, Auth, Shared};

async fn postmark(state: &AppState, report: &ReportInput, id: Uuid, member: &str) -> Result<(), String> {
    let token = state
        .cfg
        .postmark_token
        .as_deref()
        .ok_or("no Postmark token configured")?;
    let body = serde_json::json!({
        "From": "bugs@teamofsilicons.com",
        "To": state.cfg.report_recipients.join(","),
        "Subject": format!("[Silicon Extend] Bug report from {member}"),
        "TextBody": format!(
            "Report {id}\nFrom: {member}\nClient: {}\nPR: {}\n\n{}\n\nContext:\n{}",
            report.client_version,
            report.pr.as_deref().unwrap_or("(none)"),
            report.message,
            serde_json::to_string_pretty(&report.context).unwrap_or_default()
        ),
        "TrackOpens": false,
        "TrackLinks": "None",
        "MessageStream": "outbound",
        "Metadata": {"report_id": id.to_string(), "app": "silicon-extend"},
    });
    let resp = state
        .http
        .post("https://api.postmarkapp.com/email")
        .header("X-Postmark-Server-Token", token)
        .header("Accept", "application/json")
        .json(&body)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    if v.get("ErrorCode").and_then(serde_json::Value::as_i64) == Some(0) {
        Ok(())
    } else {
        Err(v.to_string())
    }
}

pub async fn report(
    State(state): State<Shared>,
    auth: Auth,
    headers: HeaderMap,
    Body(input): Body<ReportInput>,
) -> AppResult<Response> {
    let n = input.message.chars().count();
    if n == 0 || n > 60_000 {
        return Err(AppError::invalid(format!(
            "A report message must be 1–60000 characters; it is {n}."
        )));
    }
    if let Some(pr) = &input.pr
        && !(pr.starts_with("https://") || pr.starts_with("http://"))
    {
        return Err(AppError::invalid(
            "--pr must be a link to the pull request, like https://github.com/teamofsilicons/silicon-extend/pull/12.",
        ));
    }
    let hash = hash_json(&input);
    let st = state.clone();
    let world = auth.world.clone();
    let member = auth.p.id().to_owned();
    idempotent(&state, &auth.world, auth.p.id(), "reports", &headers, &hash, || async move {
        // A replay is not a new report and must not consume the hourly quota.
        st
            .rate_limit(
                format!("report:{}", member),
                10,
                Duration::from_secs(3600),
                "bug reports",
            )
            .await?;
        let id = Uuid::now_v7();
        let notification = if world.is_test() {
            "simulated"
        } else if st.cfg.postmark_token.is_none() {
            tracing::warn!(report_id = %id, "no Postmark token; report stored but not emailed");
            "simulated"
        } else {
            match postmark(&st, &input, id, &member).await {
                Ok(()) => "sent",
                Err(e) => {
                    tracing::warn!(report_id = %id, error = %e, "Postmark failed; retrying in the background");
                    let st2 = st.clone();
                    let input2 = input.clone();
                    let member2 = member.clone();
                    tokio::spawn(async move {
                        for wait in [30u64, 300, 1800] {
                            tokio::time::sleep(Duration::from_secs(wait)).await;
                            if postmark(&st2, &input2, id, &member2).await.is_ok() {
                                break;
                            }
                        }
                    });
                    "queued"
                }
            }
        };
        sqlx::query(sql!(
            "INSERT INTO {} (report_id, member_id, message, pr, client_version, context, notification) VALUES ($1, $2, $3, $4, $5, $6, $7)",
            world.t("reports")
        ))
        .bind(id)
        .bind(&member)
        .bind(&input.message)
        .bind(&input.pr)
        .bind(&input.client_version)
        .bind(&input.context)
        .bind(notification)
        .execute(&st.pool)
        .await?;
        let receipt = ReportReceipt { report_id: id, notification: notification.into(), repository_url: st.cfg.repository_url.clone() };
        Ok((StatusCode::ACCEPTED, "report", serde_json::to_value(receipt).map_err(AppError::internal)?))
    })
    .await
}

const TELEMETRY_FIELDS: &[&str] = &[
    "source",
    "event",
    "step",
    "success",
    "duration_ms",
    "error_code",
    "command",
    "device_os",
    "session_id",
    "request_id",
    "client_version",
];

pub async fn telemetry(
    State(state): State<Shared>,
    auth: Auth,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> AppResult<Response> {
    if crate::telemetry::opted_out(&headers) {
        return Ok(no_content());
    }
    let data: serde_json::Value = super::parse_envelope(&body)?;
    let obj = data
        .as_object()
        .ok_or_else(|| AppError::invalid("telemetry data must be an object"))?;
    if let Some(k) = obj.keys().find(|k| !TELEMETRY_FIELDS.contains(&k.as_str())) {
        return Err(AppError::invalid(format!(
            "telemetry field {k:?} is not accepted; allowed: {}",
            TELEMETRY_FIELDS.join(", ")
        )));
    }
    for required in ["source", "event", "step", "success", "duration_ms"] {
        if !obj.contains_key(required) {
            return Err(AppError::invalid(format!("telemetry needs {required}")));
        }
    }
    tracing::info!(target: "telemetry", member = auth.p.id(), world = %auth.world.schema, event = %data, "telemetry");
    sqlx::query(sql!(
        "INSERT INTO {} (member_id, event) VALUES ($1, $2)",
        auth.world.t("telemetry")
    ))
    .bind(auth.p.id())
    .bind(&data)
    .execute(&state.pool)
    .await?;
    let _ = envelope::<()>;
    Ok(no_content())
}

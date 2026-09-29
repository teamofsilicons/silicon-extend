//! Opt-in semantic single actions; all device I/O still uses the normal authorized session API.

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use silicon_extend_client::ref_actions::{ModelClient, ModelConfig, Observation, Provider, RefError, validate_text};

use crate::{Args, CliError, CommandRequest, CommandResult, Ctx, ErrorCode, R};

fn model_error(e: RefError) -> CliError {
    let code = match e {
        RefError::Invalid(_) => ErrorCode::InvalidInput,
        RefError::Provider(_) => ErrorCode::CommandFailed,
    };
    CliError::new(code, e.to_string()).hint("See `extend act --help`. No selected action was executed.")
}

async fn command(ctx: &mut Ctx, sid: &str, name: &str, args: Vec<String>) -> R<CommandResult> {
    let req = CommandRequest {
        command: name.into(),
        args,
        timeout_ms: ctx.g.timeout,
        self_destruct_minutes: None,
        permanent: false,
        attachments: vec![],
    };
    ctx.call("ref action device command", |c, token, team| {
        let sid = sid.to_owned();
        let req = req.clone();
        async move { c.authed(&token, team.as_deref()).run(&sid, &req).await }
    })
    .await
}

fn snapshot_result(result: CommandResult) -> R<Value> {
    if result.ok {
        return Ok(result.output);
    }
    Err(CliError::new(
        ErrorCode::CommandFailed,
        "Could not acquire the ref snapshot; no action executed.",
    )
    .details(json!({"snapshot":result})))
}

pub(super) async fn run(ctx: &mut Ctx, raw: &[String]) -> R<i32> {
    let args = Args::parse(raw, "act")?;
    args.at_most(1)?;
    let instruction = args.req(0, "a single-action instruction")?;
    let text = args.value("--text");
    if let Some(text) = text.as_deref() {
        validate_text(text).map_err(model_error)?;
    }
    let provider = match args.value("--provider").as_deref().unwrap_or("jev") {
        "jev" => Provider::Jev,
        "llm" => Provider::Llm,
        _ => {
            return Err(CliError::usage(
                "Unknown provider",
                "Use --provider jev or --provider llm.",
            ));
        }
    };
    let fallback = match args.value("--fallback").as_deref() {
        None => false,
        Some("llm") if provider == Provider::Jev => true,
        _ => {
            return Err(CliError::usage(
                "Invalid fallback",
                "Use --provider jev --fallback llm, or omit --fallback.",
            ));
        }
    };
    let threshold = args
        .value("--threshold")
        .unwrap_or_else(|| "0.7".into())
        .parse::<f64>()
        .map_err(|_| CliError::usage("Invalid confidence threshold", "Use --threshold 0.7 (between 0 and 1)."))?;
    let timeout = Duration::from_millis(ctx.g.timeout.unwrap_or(30_000));
    // Validate both provider configurations before reading or sending screen content.
    let model = ModelClient::new(ModelConfig::from_env(provider, threshold, timeout).map_err(model_error)?)
        .map_err(model_error)?;
    let fallback_model = if fallback {
        Some(
            ModelClient::new(ModelConfig::from_env(Provider::Llm, threshold, timeout).map_err(model_error)?)
                .map_err(model_error)?,
        )
    } else {
        None
    };
    let sid = ctx.session_id()?;
    ctx.use_session_team(&sid);
    let started = Instant::now();
    let session = ctx
        .call("ref action session", |c, token, team| {
            let sid = sid.clone();
            async move { c.authed(&token, team.as_deref()).session(&sid).await }
        })
        .await?;
    let commands = session.commands.ok_or_else(|| {
        CliError::new(
            ErrorCode::UnsupportedOnDevice,
            "Session did not expose its supported commands.",
        )
    })?;
    if !commands.iter().any(|c| c == "snapshot") {
        return Err(CliError::new(
            ErrorCode::UnsupportedOnDevice,
            "This device has no ref snapshot mode.",
        ));
    }
    let mut snapshot_args = vec!["-i".into(), "--force-full".into()];
    if let Some(scope) = args.value("--scope") {
        snapshot_args.extend(["-s".into(), scope]);
    }
    let t = Instant::now();
    let snapshot = snapshot_result(command(ctx, &sid, "snapshot", snapshot_args.clone()).await?)?;
    let observation = Observation::from_snapshot(&snapshot, &commands, text.is_some()).map_err(model_error)?;
    let snapshot_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let first = model.choose(&observation, &instruction).await;
    let mut attempts = vec![];
    let decision = match first {
        Ok(decision) if decision.accepted || fallback_model.is_none() => {
            attempts.push(json!(decision));
            decision
        }
        result => {
            match result {
                Ok(decision) => attempts.push(json!(decision)),
                Err(e) if fallback_model.is_none() => return Err(model_error(e)),
                Err(e) => attempts.push(json!({"provider":"jev","error":e.to_string()})),
            }
            let decision = fallback_model
                .as_ref()
                .expect("fallback configured")
                .choose(&observation, &instruction)
                .await
                .map_err(model_error)?;
            attempts.push(json!(decision));
            decision
        }
    };
    let selection_ms = t.elapsed().as_secs_f64() * 1000.0;
    let mut revalidate_ms = 0.0;
    let mut execution_ms = 0.0;
    let mut result = None;
    let mut status = if decision.accepted { "selected" } else { "blocked" };
    if decision.accepted && !args.flag("--dry-run") {
        let t = Instant::now();
        let fresh = snapshot_result(command(ctx, &sid, "snapshot", snapshot_args).await?)?;
        let current = Observation::from_snapshot(&fresh, &commands, text.is_some()).map_err(model_error)?;
        revalidate_ms = t.elapsed().as_secs_f64() * 1000.0;
        if current != observation {
            status = "stale";
        } else {
            let (name, mut argv) = current.command(&decision, text.as_deref()).map_err(model_error)?;
            // Engine snapshots expose a generation; pin it so another snapshot cannot rebind the ref.
            if let Some(generation) = fresh.get("refsGeneration").and_then(Value::as_u64) {
                for arg in &mut argv {
                    if Some(arg.as_str()) == decision.target.as_deref() {
                        *arg = format!("{arg}~s{generation}");
                        break;
                    }
                }
            }
            let t = Instant::now();
            let executed = command(ctx, &sid, &name, argv).await?;
            execution_ms = t.elapsed().as_secs_f64() * 1000.0;
            status = if executed.ok { "executed" } else { "execution_failed" };
            result = Some(executed);
        }
    }
    let ok = matches!(status, "selected" | "executed");
    ctx.emit(
        json!({"experimental":true,"ok":ok,"status":status,"decision":decision,"attempts":attempts,
        "result":result,"timings":{"snapshot_ms":snapshot_ms,"selection_ms":selection_ms,
        "revalidate_ms":revalidate_ms,"execution_ms":execution_ms,"total_ms":started.elapsed().as_secs_f64()*1000.0}}),
        || {
            format!(
                "{status}: {} {} (selection {:.0} ms, total {:.0} ms).{}",
                decision.operation,
                decision.target.as_deref().unwrap_or(""),
                selection_ms,
                started.elapsed().as_secs_f64() * 1000.0,
                if status == "stale" {
                    " Screen changed; rerun the instruction after inspecting it."
                } else {
                    ""
                }
            )
        },
    );
    Ok(if ok { 0 } else { ErrorCode::CommandFailed.exit_code() })
}

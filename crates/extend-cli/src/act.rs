//! Opt-in semantic single actions; all device I/O still uses the normal authorized session API.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use silicon_extend_client::ref_actions::{
    Decision, ModelClient, ModelConfig, Observation, Provider, RefError, RefSelectionRequest, validate_text,
};

use crate::{Args, CliError, CommandRequest, CommandResult, Ctx, ErrorCode, R};

fn normal_ref_fallback() -> Value {
    json!({"mode":"normal_refs","requires_fresh_snapshot":true,
        "snapshot":{"command":"snapshot","args":["-i","--force-full"]},
        "instructions":"Continue in the same session using the normal agent: read a fresh snapshot, choose an observed ref, then run the ordinary device command. These commands do not require Jev."})
}

fn model_error(e: RefError) -> CliError {
    let code = match e {
        RefError::Invalid(_) => ErrorCode::InvalidInput,
        RefError::Provider(_) => ErrorCode::CommandFailed,
    };
    selection_error(CliError::new(code, e.to_string()))
}

fn selection_error(mut error: CliError) -> CliError {
    let fallback_hint = "No selected action was executed. Continue with `extend snapshot -i --force-full` and normal ref commands; Jev is optional.";
    error.hint = Some(match error.hint {
        Some(hint) => format!("{hint} {fallback_hint}"),
        None => fallback_hint.into(),
    });
    let mut details = match *error.details {
        Value::Object(details) => details,
        _ => serde_json::Map::new(),
    };
    details.insert("action_executed".into(), json!(false));
    details.insert("normal_ref_fallback".into(), normal_ref_fallback());
    error.details = Box::new(Value::Object(details));
    error
}

fn validate_managed_decision(decision: Decision, observation: &Observation, threshold: f64) -> R<Decision> {
    let valid_confidence = decision
        .confidence
        .is_some_and(|p| p.is_finite() && (0.0..=1.0).contains(&p));
    let valid_selection = !decision.accepted
        || (decision.confidence.is_some_and(|p| p >= threshold)
            && observation
                .targets
                .get(&decision.operation)
                .is_some_and(|refs| decision.target.as_ref().is_some_and(|target| refs.contains(target))));
    if decision.provider != "jev" || !valid_confidence || !valid_selection {
        return Err(model_error(RefError::Provider(
            "managed Jev returned an invalid decision for this observation or confidence threshold.".into(),
        )));
    }
    Ok(decision)
}

struct Selector {
    model: Option<ModelClient>,
    fallback_model: Option<ModelClient>,
    instruction: String,
    has_text: bool,
    threshold: f64,
    timeout: Duration,
}

impl Selector {
    async fn choose(
        &self,
        ctx: &mut Ctx,
        sid: &str,
        snapshot: &Value,
        observation: &Observation,
    ) -> R<(Decision, Vec<Value>)> {
        let first = if let Some(model) = &self.model {
            model.choose(observation, &self.instruction).await.map_err(model_error)
        } else {
            let request = RefSelectionRequest {
                snapshot: snapshot.clone(),
                instruction: self.instruction.clone(),
                has_text: self.has_text,
                threshold: self.threshold,
            };
            let selection = ctx.call("managed ref selection", |c, token, team| {
                let sid = sid.to_owned();
                let request = request.clone();
                async move { c.authed(&token, team.as_deref()).select_ref(&sid, &request).await }
            });
            match tokio::time::timeout(self.timeout, selection).await {
                Ok(result) => result
                    .map_err(selection_error)
                    .and_then(|decision| validate_managed_decision(decision, observation, self.threshold)),
                Err(_) => Err(model_error(RefError::Provider("managed Jev request timed out.".into()))),
            }
        };
        let mut attempts = vec![];
        let decision = match first {
            Ok(decision) if decision.accepted || self.fallback_model.is_none() => {
                attempts.push(json!(decision));
                decision
            }
            result => {
                match result {
                    Ok(decision) => attempts.push(json!(decision)),
                    Err(e) if self.fallback_model.is_none() => return Err(e),
                    Err(e) => attempts.push(json!({"provider":"jev","error":e.message})),
                }
                let decision = self
                    .fallback_model
                    .as_ref()
                    .expect("fallback configured")
                    .choose(observation, &self.instruction)
                    .await
                    .map_err(model_error)?;
                attempts.push(json!(decision));
                decision
            }
        };
        Ok((decision, attempts))
    }
}

fn changed_fields(before: &Value, after: &Value) -> Vec<String> {
    let keys: BTreeSet<_> = before
        .as_object()
        .into_iter()
        .flat_map(|object| object.keys())
        .chain(after.as_object().into_iter().flat_map(|object| object.keys()))
        .collect();
    keys.into_iter()
        .filter(|key| before.get(*key) != after.get(*key))
        .cloned()
        .collect()
}

/// Report the shape of a change without copying screen text or field values into diagnostics.
fn observation_changes(before: &Observation, after: &Observation) -> Value {
    let added: Vec<_> = after
        .elements
        .keys()
        .filter(|key| !before.elements.contains_key(*key))
        .collect();
    let removed: Vec<_> = before
        .elements
        .keys()
        .filter(|key| !after.elements.contains_key(*key))
        .collect();
    let changed: Vec<_> = before
        .elements
        .iter()
        .filter_map(|(reference, value)| {
            let fields = changed_fields(value, after.elements.get(reference)?);
            (!fields.is_empty()).then(|| json!({"ref":reference,"fields":fields}))
        })
        .collect();
    let operations: BTreeSet<_> = before.targets.keys().chain(after.targets.keys()).collect();
    let changed_operations: Vec<_> = operations
        .into_iter()
        .filter(|key| before.targets.get(*key) != after.targets.get(*key))
        .collect();
    json!({"reason":"observation_changed","context_fields":changed_fields(&before.context,&after.context),
        "added_refs":added,"removed_refs":removed,"changed_refs":changed,
        "changed_operation_targets":changed_operations})
}

fn stale_detail(revalidations: &[Value]) -> String {
    let changes = revalidations.last().map(|round| &round["changes"]);
    let refs: BTreeSet<_> = changes
        .into_iter()
        .flat_map(|changes| {
            changes["changed_refs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| entry["ref"].as_str())
                .chain(
                    changes["added_refs"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str),
                )
                .chain(
                    changes["removed_refs"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str),
                )
        })
        .collect();
    let examples = refs.iter().take(6).copied().collect::<Vec<_>>().join(", ");
    let changed = if refs.is_empty() {
        "Screen context changed.".to_owned()
    } else {
        format!("Changed refs: {examples}{}.", if refs.len() > 6 { ", …" } else { "" })
    };
    format!(
        " Screen kept changing across two selection rounds. {changed} Use --scope to narrow the screen, or continue with a fresh snapshot and normal ref commands. --json includes changed field names."
    )
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
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Err(model_error(RefError::Invalid(
            "Confidence threshold must be between 0 and 1.".into(),
        )));
    }
    let timeout = Duration::from_millis(ctx.g.timeout.unwrap_or(30_000));
    // Validate both provider configurations before reading or sending screen content.
    // A configured personal key always stays on the direct path, including when that key fails.
    let managed = provider == Provider::Jev
        && match std::env::var("TYPESAFE_API_KEY") {
            Ok(key) => key.trim().is_empty(),
            Err(std::env::VarError::NotPresent) => true,
            Err(std::env::VarError::NotUnicode(_)) => false,
        };
    let model = if managed {
        None
    } else {
        Some(
            ModelClient::new(ModelConfig::from_env(provider, threshold, timeout).map_err(model_error)?)
                .map_err(model_error)?,
        )
    };
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
    let mut snapshot = snapshot_result(command(ctx, &sid, "snapshot", snapshot_args.clone()).await?)?;
    let mut observation = Observation::from_snapshot(&snapshot, &commands, text.is_some()).map_err(model_error)?;
    let snapshot_ms = t.elapsed().as_secs_f64() * 1000.0;
    let selector = Selector {
        model,
        fallback_model,
        instruction,
        has_text: text.is_some(),
        threshold,
        timeout,
    };
    let mut attempts = vec![];
    let mut revalidations = vec![];
    let mut selection_ms = 0.0;
    let mut revalidate_ms = 0.0;
    let mut execution_ms = 0.0;
    let mut result = None;
    let mut selection_round = 0;
    let (decision, status) = loop {
        selection_round += 1;
        let t = Instant::now();
        let selected = selector.choose(ctx, &sid, &snapshot, &observation).await;
        selection_ms += t.elapsed().as_secs_f64() * 1000.0;
        let (decision, round_attempts) = selected.map_err(|mut error| {
            error.details["selection_rounds"] = json!(selection_round);
            error.details["failed_selection_round"] = json!(selection_round);
            error.details["attempts"] = json!(attempts);
            error.details["revalidations"] = json!(revalidations);
            error.details["timings"] = json!({"snapshot_ms":snapshot_ms,"selection_ms":selection_ms,
                "revalidate_ms":revalidate_ms,"execution_ms":0.0,"total_ms":started.elapsed().as_secs_f64()*1000.0});
            error
        })?;
        attempts.extend(round_attempts.into_iter().map(|mut attempt| {
            attempt["selection_round"] = json!(selection_round);
            attempt
        }));
        if !decision.accepted {
            break (decision, "blocked");
        }
        if args.flag("--dry-run") {
            break (decision, "selected");
        }
        let t = Instant::now();
        let fresh = snapshot_result(command(ctx, &sid, "snapshot", snapshot_args.clone()).await?)?;
        let current = Observation::from_snapshot(&fresh, &commands, text.is_some()).map_err(model_error)?;
        revalidate_ms += t.elapsed().as_secs_f64() * 1000.0;
        let stable = current == observation;
        revalidations.push(json!({"selection_round":selection_round,"stable":stable,
            "changes":if stable {Value::Null} else {observation_changes(&observation,&current)}}));
        if !stable {
            if selection_round < 2 {
                // A changed layout may have rebound every ref. Select again against the new
                // observation, then require another full match before sending any action.
                snapshot = fresh;
                observation = current;
                continue;
            }
            break (decision, "stale");
        }
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
        let status = if !executed.ok {
            "execution_failed"
        } else if executed.output.get("verified").and_then(Value::as_bool) == Some(false) {
            "executed_unverified"
        } else {
            "executed"
        };
        result = Some(executed);
        break (decision, status);
    };
    let ok = matches!(status, "selected" | "executed" | "executed_unverified");
    ctx.emit(
        json!({"experimental":true,"ok":ok,"status":status,"decision":decision,"attempts":attempts,
        "result":result,"selection_rounds":selection_round,"revalidations":revalidations,"normal_ref_fallback":if matches!(status,"blocked"|"stale") {normal_ref_fallback()} else {Value::Null},
        "timings":{"snapshot_ms":snapshot_ms,"selection_ms":selection_ms,
        "revalidate_ms":revalidate_ms,"execution_ms":execution_ms,"total_ms":started.elapsed().as_secs_f64()*1000.0}}),
        || {
            format!(
                "{status}: {} {} (selection {:.0} ms, total {:.0} ms).{}",
                decision.operation,
                decision.target.as_deref().unwrap_or(""),
                selection_ms,
                started.elapsed().as_secs_f64() * 1000.0,
                if status == "stale" {
                    stale_detail(&revalidations)
                } else if status == "blocked" {
                    " Continue with a fresh snapshot and normal ref commands; Jev is optional.".to_owned()
                } else if status == "executed_unverified" {
                    let warning = result.as_ref().and_then(|result| result.output.get("warning"))
                        .and_then(Value::as_str).map(|warning|format!(" {warning}")).unwrap_or_default();
                    format!("\nThe device accepted the action, but could not verify the resulting value.{warning} Inspect a fresh snapshot or screenshot before retrying; do not repeat the action automatically.")
                } else if status == "execution_failed" {
                    let error = result.as_ref().and_then(|result| result.error.as_ref());
                    let detail = error.map_or_else(
                        || "The device reported failure without error details.".to_owned(),
                        |error| {
                            let lengths = match (error.details.get("expectedLength").and_then(Value::as_u64),
                                error.details.get("actualLength").and_then(Value::as_u64)) {
                                (Some(expected),Some(actual)) => format!(" (expected length {expected}, observed length {actual})"),
                                _ => String::new(),
                            };
                            format!("Device error [{}]: {}{lengths}", error.code, error.message)
                        },
                    );
                    format!("\n{detail}\nThe action may already have changed the device. Inspect a fresh snapshot or read the field before retrying; do not repeat the action automatically.")
                } else {
                    String::new()
                }
            )
        },
    );
    Ok(if ok { 0 } else { ErrorCode::CommandFailed.exit_code() })
}

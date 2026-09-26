//! `replay`, `test` and `batch` for TVs, which have no agent-device to run them.
//!
//! A `.ad` script is one command per line, written the way it is typed after `extend`
//! (`tv-remote press down`, `open YouTube`). `#` comments and `context` lines are skipped,
//! `env NAME="value"` lines define `${NAME}` substitutions, and `wait <ms>` pauses. Each step runs
//! through the driver's own `run`, so a script can only do what the TV can.

use std::collections::HashMap;
use std::time::Duration;

use extend_driver::{Driver, Invocation, Output};
use serde_json::{Value, json};

use crate::common::{failed, invalid, resolve_attachment, sleep_or_cancel};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Step {
    pub line: usize,
    pub command: String,
    pub args: Vec<String>,
}

/// Parses an agent-device `.ad` script.
pub(crate) fn parse_ad(text: &str) -> Result<Vec<Step>, String> {
    let mut env: HashMap<String, String> = HashMap::new();
    let mut steps = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line_no = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let tokens = tokenize(line).map_err(|e| format!("line {line_no}: {e}"))?;
        let Some((first, rest)) = tokens.split_first() else {
            continue;
        };
        match first.as_str() {
            "context" => continue,
            "env" => {
                for kv in rest {
                    let (k, v) = kv
                        .split_once('=')
                        .ok_or_else(|| format!("line {line_no}: env needs NAME=value"))?;
                    env.insert(k.to_owned(), substitute(v, &env));
                }
            }
            _ => steps.push(Step {
                line: line_no,
                command: first.clone(),
                args: rest.iter().map(|t| substitute(t, &env)).collect(),
            }),
        }
    }
    Ok(steps)
}

fn substitute(s: &str, env: &HashMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find('}') {
            Some(end) => {
                let name = &rest[start + 2..start + 2 + end];
                out.push_str(env.get(name).map(String::as_str).unwrap_or(""));
                rest = &rest[start + 2 + end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Shell-like splitting: whitespace separates, `"…"` and `'…'` group, `\` escapes inside double
/// quotes and outside quotes. `key="a b"` stays one token (`key=a b`).
pub(crate) fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(n) => cur.push(n),
                            None => return Err("unfinished escape".into()),
                        },
                        Some(ch) => cur.push(ch),
                        None => return Err("unclosed \"".into()),
                    }
                }
            }
            '\'' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => cur.push(ch),
                        None => return Err("unclosed '".into()),
                    }
                }
            }
            '\\' => {
                in_token = true;
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() => {
                if in_token {
                    tokens.push(std::mem::take(&mut cur));
                    in_token = false;
                }
            }
            c => {
                in_token = true;
                cur.push(c);
            }
        }
    }
    if in_token {
        tokens.push(cur);
    }
    Ok(tokens)
}

/// Turns one `batch` step (`{"command", "input"}`) into command-line tokens.
pub(crate) fn batch_step_args(step: &Value) -> Result<(String, Vec<String>), String> {
    let obj = step
        .as_object()
        .ok_or("each batch step must be an object")?;
    for k in obj.keys() {
        if !matches!(
            k.as_str(),
            "command" | "input" | "runtime" | "args" | "positionals"
        ) {
            return Err(format!("unknown batch step field \"{k}\""));
        }
    }
    let command = obj
        .get("command")
        .and_then(Value::as_str)
        .ok_or("batch step without \"command\"")?
        .to_owned();
    for key in ["args", "positionals"] {
        if let Some(list) = obj
            .get(key)
            .or_else(|| obj.get("input").and_then(|i| i.get(key)))
        {
            let list = list.as_array().ok_or(format!("\"{key}\" must be a list"))?;
            return Ok((command, list.iter().map(value_token).collect()));
        }
    }
    let input = obj.get("input").cloned().unwrap_or(Value::Null);
    let s = |k: &str| input.get(k).map(value_token);
    let mut args = Vec::new();
    match command.as_str() {
        "open" => {
            if let Some(app) = s("app").or_else(|| s("target")) {
                args.push(app);
            }
            if let Some(url) = s("url") {
                args.push(url);
            }
        }
        "close" => args.extend(s("app")),
        "tv-remote" => {
            let action = s("action").unwrap_or_else(|| "press".into());
            args.push(action);
            args.push(s("button").ok_or("tv-remote step needs \"button\"")?);
            if let Some(ms) = s("durationMs").or_else(|| s("duration_ms")) {
                args.push("--duration-ms".into());
                args.push(ms);
            }
        }
        "wait" => args.push(
            s("ms")
                .or_else(|| s("durationMs"))
                .ok_or("wait step needs \"ms\"")?,
        ),
        "apps" => {
            if input.get("all").and_then(Value::as_bool) == Some(true) {
                args.push("--all".into());
            }
        }
        "display" => {
            let action = s("action").unwrap_or_else(|| {
                if input.get("clear").is_some() {
                    "clear".into()
                } else {
                    "show".into()
                }
            });
            args.push(action);
            for k in ["url", "image", "video", "text"] {
                if let Some(v) = s(k) {
                    args.push(format!("--{k}"));
                    args.push(v);
                }
            }
        }
        _ => {}
    }
    Ok((command, args))
}

fn value_token(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Runs `replay`, `test` or `batch` against `driver`.
pub(crate) async fn run(driver: &dyn Driver, inv: &Invocation<'_>) -> Output {
    match inv.command {
        "replay" => replay(driver, inv).await,
        "test" => test(driver, inv).await,
        "batch" => batch(driver, inv).await,
        other => invalid(format!("`{other}` is not a script command")),
    }
}

async fn replay(driver: &dyn Driver, inv: &Invocation<'_>) -> Output {
    let mut path = None;
    let mut from = 1usize;
    let mut keep_session = false;
    let mut it = inv.args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--from" => match it.next().and_then(|v| v.parse().ok()) {
                Some(n) if n >= 1 => from = n,
                _ => return invalid("--from needs a step number (1 or more)"),
            },
            "--plan-digest" => {
                it.next();
            }
            "--keep-session" => keep_session = true,
            "--json" | "-u" | "--update" => {}
            f if f.starts_with("--") => return invalid(format!("replay doesn't take {f} on a TV")),
            p if path.is_none() => path = Some(p.to_owned()),
            p => return invalid(format!("unexpected argument \"{p}\"")),
        }
    }
    let Some(path) = path else {
        return invalid("usage: replay <script.ad>");
    };
    let steps = match load_script(&path, inv) {
        Ok(s) => s,
        Err(o) => return *o,
    };
    let mut steps: Vec<Step> = steps.into_iter().skip(from - 1).collect();
    if keep_session && steps.last().is_some_and(|s| s.command == "close") {
        steps.pop();
    }
    run_steps(driver, inv, &steps).await
}

async fn test(driver: &dyn Driver, inv: &Invocation<'_>) -> Output {
    let mut paths: Vec<String> = Vec::new();
    let mut retries = 0u32;
    let mut it = inv.args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--retries" => match it.next().and_then(|v| v.parse().ok()) {
                Some(n) if n <= 3 => retries = n,
                _ => return invalid("--retries takes 0 to 3"),
            },
            "--json" | "--verbose" => {}
            f if f.starts_with("--") => return invalid(format!("test doesn't take {f} on a TV")),
            p => paths.push(p.to_owned()),
        }
    }
    if paths.is_empty() {
        // Scripts sent without names: run every attachment in order.
        paths = inv
            .attachments
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
    }
    if paths.is_empty() {
        return invalid("usage: test <script.ad>...");
    }
    let mut results = Vec::new();
    let mut failures = 0;
    for p in &paths {
        let steps = match load_script(p, inv) {
            Ok(s) => s,
            Err(o) => return *o,
        };
        let mut attempt = 0;
        let outcome = loop {
            let out = run_steps(driver, inv, &steps).await;
            if out.ok || attempt >= retries || inv.cancel.is_cancelled() {
                break (out, attempt);
            }
            attempt += 1;
        };
        let (out, attempts) = outcome;
        if !out.ok {
            failures += 1;
        }
        results.push(json!({
            "script": p,
            "ok": out.ok,
            "retries": attempts,
            "error": out.error.as_ref().map(|e| e.message.clone()),
        }));
    }
    let text = results
        .iter()
        .map(|r| {
            format!(
                "{} {}",
                if r["ok"] == true { "pass" } else { "fail" },
                r["script"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let output =
        json!({ "results": results, "passed": paths.len() - failures, "failed": failures });
    if failures == 0 {
        Output::ok(output, text)
    } else {
        let mut o = failed(format!(
            "{failures} of {} scripts failed\n{text}",
            paths.len()
        ));
        o.output = output;
        o
    }
}

async fn batch(driver: &dyn Driver, inv: &Invocation<'_>) -> Output {
    let mut raw = None;
    let mut max_steps = 100usize;
    let mut it = inv.args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--steps" => raw = it.next().cloned(),
            "--steps-file" => {
                let Some(p) = it.next() else {
                    return invalid("--steps-file needs a path");
                };
                let Some(path) = resolve_attachment(p, inv.attachments) else {
                    return invalid(format!("steps file {p} wasn't sent with the command"));
                };
                match std::fs::read_to_string(&path) {
                    Ok(t) => raw = Some(t),
                    Err(e) => return invalid(format!("can't read {p}: {e}")),
                }
            }
            "--max-steps" => match it.next().and_then(|v| v.parse().ok()) {
                Some(n) => max_steps = n,
                None => return invalid("--max-steps needs a number"),
            },
            "--on-error" => {
                if it.next().map(String::as_str) != Some("stop") {
                    return invalid("only --on-error stop is supported");
                }
            }
            "--json" => {}
            other => return invalid(format!("batch doesn't take {other}")),
        }
    }
    let Some(raw) = raw else {
        return invalid("usage: batch --steps '<json>' | --steps-file <path>");
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return invalid(format!("--steps is not JSON: {e}")),
    };
    let list = match parsed.as_array() {
        Some(l) => l.clone(),
        None => match parsed.get("steps").and_then(Value::as_array) {
            Some(l) => l.clone(),
            None => return invalid("--steps must be a JSON list of steps"),
        },
    };
    if list.len() > max_steps {
        return invalid(format!(
            "{} steps is more than --max-steps {max_steps}",
            list.len()
        ));
    }
    let mut steps = Vec::new();
    for (i, s) in list.iter().enumerate() {
        match batch_step_args(s) {
            Ok((command, args)) => steps.push(Step {
                line: i + 1,
                command,
                args,
            }),
            Err(e) => return invalid(format!("step {}: {e}", i + 1)),
        }
    }
    run_steps(driver, inv, &steps).await
}

fn load_script(path: &str, inv: &Invocation<'_>) -> Result<Vec<Step>, Box<Output>> {
    let Some(file) = resolve_attachment(path, inv.attachments) else {
        return Err(Box::new(invalid(format!(
            "script {path} wasn't sent with the command"
        ))));
    };
    let text = std::fs::read_to_string(&file)
        .map_err(|e| Box::new(invalid(format!("can't read {path}: {e}"))))?;
    if path.ends_with(".yaml") || path.ends_with(".yml") {
        return Err(Box::new(invalid(
            "Maestro flows need a device with a screen; TVs run .ad scripts only",
        )));
    }
    parse_ad(&text).map_err(|e| Box::new(invalid(format!("{path}: {e}"))))
}

/// Runs steps in order, stopping at the first failure.
pub(crate) async fn run_steps(driver: &dyn Driver, inv: &Invocation<'_>, steps: &[Step]) -> Output {
    let mut done = Vec::new();
    for (n, step) in steps.iter().enumerate() {
        if inv.cancel.is_cancelled() {
            return failed("cancelled");
        }
        let out = if step.command == "wait"
            && step.args.len() == 1
            && step.args[0].parse::<u64>().is_ok()
        {
            let ms: u64 = step.args[0].parse().unwrap_or(0);
            if sleep_or_cancel(inv, Duration::from_millis(ms.min(600_000))).await {
                Output::ok(json!({"waited_ms": ms}), format!("Waited {ms} ms"))
            } else {
                failed("cancelled")
            }
        } else if matches!(step.command.as_str(), "replay" | "test" | "batch") {
            invalid(format!("`{}` can't run inside a script", step.command))
        } else {
            let sub = Invocation {
                id: inv.id,
                session_id: inv.session_id,
                command: &step.command,
                args: &step.args,
                attachments: inv.attachments,
                workdir: inv.workdir,
                timeout: inv.timeout,
                cancel: inv.cancel.clone(),
            };
            driver.run(sub).await
        };
        if !out.ok {
            let message = out
                .error
                .as_ref()
                .map(|e| e.message.clone())
                .unwrap_or_default();
            let code = out
                .error
                .as_ref()
                .map(|e| e.code.clone())
                .unwrap_or_else(|| "command_failed".into());
            let mut o = Output::fail(
                &code,
                format!(
                    "step {} (line {}) `{} {}` failed: {message}",
                    n + 1,
                    step.line,
                    step.command,
                    step.args.join(" ")
                ),
            );
            if let Some(e) = o.error.as_mut() {
                e.details = json!({"step": n + 1, "line": step.line, "command": step.command, "completed": done});
            }
            o.output = json!({"completed": done, "failed_step": n + 1});
            o.files = out.files;
            return o;
        }
        done.push(json!({"step": n + 1, "command": step.command, "output": out.output}));
    }
    let count = done.len();
    Output::ok(
        json!({"steps": done}),
        format!("Ran {count} step{}", if count == 1 { "" } else { "s" }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_like_a_shell() {
        assert_eq!(
            tokenize(r#"open "Agent Device" --x"#).unwrap(),
            vec!["open", "Agent Device", "--x"]
        );
        assert_eq!(
            tokenize(r#"wait "label=\"Form\"" 30000"#).unwrap(),
            vec!["wait", "label=\"Form\"", "30000"]
        );
        assert_eq!(
            tokenize(r#"fill id="field name" 'a b'"#).unwrap(),
            vec!["fill", "id=field name", "a b"]
        );
        assert_eq!(
            tokenize(r#"env APP_URL="""#).unwrap(),
            vec!["env", "APP_URL="]
        );
        assert!(tokenize("open \"x").is_err());
    }

    #[test]
    fn parses_ad_scripts() {
        let script = "# comment\ncontext platform=tv timeout=60000\n\nenv APP=\"YouTube\"\nopen \"${APP}\"\nwait 500\ntv-remote press down\nclose\n";
        let steps = parse_ad(script).unwrap();
        assert_eq!(steps.len(), 4);
        assert_eq!(
            steps[0],
            Step {
                line: 5,
                command: "open".into(),
                args: vec!["YouTube".into()]
            }
        );
        assert_eq!(steps[2].args, vec!["press", "down"]);
        assert_eq!(steps[3].command, "close");
    }

    #[test]
    fn batch_steps_to_args() {
        let (c, a) =
            batch_step_args(&json!({"command": "open", "input": {"app": "YouTube"}})).unwrap();
        assert_eq!((c.as_str(), a), ("open", vec!["YouTube".to_string()]));
        let (_, a) = batch_step_args(&json!({"command": "tv-remote", "input": {"button": "up", "action": "longpress", "durationMs": 900}})).unwrap();
        assert_eq!(a, vec!["longpress", "up", "--duration-ms", "900"]);
        let (_, a) =
            batch_step_args(&json!({"command": "tv-remote", "args": ["press", "home"]})).unwrap();
        assert_eq!(a, vec!["press", "home"]);
        assert!(batch_step_args(&json!({"command": "open", "bogus": 1})).is_err());
    }
}

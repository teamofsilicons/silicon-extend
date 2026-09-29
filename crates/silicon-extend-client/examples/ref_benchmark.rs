//! Paired decision benchmark. Fixtures/captured snapshots are replayed, never executed on devices.
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use silicon_extend_client::ref_actions::{ModelClient, ModelConfig, Observation, Provider, validate_text};

#[derive(Deserialize)]
struct Case {
    id: String,
    provenance: String,
    platform: String,
    instruction: String,
    commands: Vec<String>,
    #[serde(default)]
    text: Option<String>,
    snapshot: Value,
    expected_operation: String,
    expected_target: Option<String>,
}

fn percentile(mut values: Vec<f64>, quantile: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(
        values[((values.len() as f64 * quantile).ceil() as usize)
            .saturating_sub(1)
            .min(values.len() - 1)],
    )
}

fn summary(rows: &[Value], provider: &str) -> Value {
    let selected: Vec<_> = rows.iter().filter(|r| r["provider"] == provider).collect();
    let times: Vec<_> = selected.iter().filter_map(|r| r["wall_ms"].as_f64()).collect();
    let correct: Vec<_> = selected.iter().filter(|r| r["correct"] == true).collect();
    json!({"attempts":selected.len(),"correct":correct.len(),
        "accuracy":if selected.is_empty(){0.0}else{correct.len() as f64/selected.len() as f64},
        "errors":selected.iter().filter(|r|r.get("error").is_some()).count(),
        "abstentions":selected.iter().filter(|r|r["decision"]["accepted"]==false).count(),
        "all_attempts_p50_ms":percentile(times.clone(),0.5),"all_attempts_p95_ms":percentile(times,0.95),
        "correct_only_p50_ms":percentile(correct.iter().filter_map(|r|r["wall_ms"].as_f64()).collect(),0.5)})
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let input = args
        .next()
        .ok_or("Usage: ref_benchmark <cases.json> [--repeats N] [--out report.json] [--validate-only]")?;
    let mut repeats = 3usize;
    let mut out = None;
    let mut validate = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--repeats" => repeats = args.next().ok_or("--repeats needs a value")?.parse()?,
            "--out" => out = Some(args.next().ok_or("--out needs a path")?),
            "--validate-only" => validate = true,
            _ => return Err(format!("Unknown argument {arg}").into()),
        }
    }
    if !(1..=100).contains(&repeats) {
        return Err("Repeats must be 1–100".into());
    }
    let cases: Vec<Case> = serde_json::from_slice(&std::fs::read(&input)?)?;
    if cases.is_empty() {
        return Err("Corpus is empty".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut observations = vec![];
    for case in &cases {
        if !ids.insert(case.id.clone()) {
            return Err("Duplicate case id".into());
        }
        if let Some(text) = &case.text {
            validate_text(text)?;
        }
        let observation = Observation::from_snapshot(&case.snapshot, &case.commands, case.text.is_some())?;
        observation.request(&case.instruction)?;
        if case.expected_operation != "BLOCKED"
            && !observation
                .targets
                .get(&case.expected_operation)
                .is_some_and(|refs| case.expected_target.as_ref().is_some_and(|r| refs.contains(r)))
        {
            return Err(format!("{}: expected action is outside candidate space", case.id).into());
        }
        observations.push(observation);
    }
    if validate {
        println!(
            "{}",
            json!({"validated_cases":cases.len(),"model_requests":0,"device_actions":0,"performance_measured":false})
        );
        return Ok(());
    }
    let jev = ModelClient::new(ModelConfig::from_env(Provider::Jev, 0.7, Duration::from_secs(30))?)?;
    let llm = ModelClient::new(ModelConfig::from_env(Provider::Llm, 0.7, Duration::from_secs(30))?)?;
    // Warmups are recorded separately, including failures, and never mixed into measured rows.
    let mut warmups = vec![];
    for (name, client) in [("jev", &jev), ("llm", &llm)] {
        let start = Instant::now();
        let response = client.choose(&observations[0], &cases[0].instruction).await;
        warmups.push(json!({"provider":name,"wall_ms":start.elapsed().as_secs_f64()*1000.0,
            "response":response.as_ref().ok(),"error":response.as_ref().err().map(ToString::to_string)}));
    }
    let mut rows = vec![];
    for repeat in 0..repeats {
        for (index, (case, observation)) in cases.iter().zip(&observations).enumerate() {
            let order = if (repeat + index) % 2 == 0 {
                [("jev", &jev), ("llm", &llm)]
            } else {
                [("llm", &llm), ("jev", &jev)]
            };
            for (name, client) in order {
                let start = Instant::now();
                let result = client.choose(observation, &case.instruction).await;
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                let mut row = json!({"case":case.id,"platform":case.platform,"provenance":case.provenance,
                    "repeat":repeat,"provider":name,"wall_ms":elapsed,"correct":false});
                match result {
                    Ok(decision) => {
                        row["correct"] = json!(if case.expected_operation == "BLOCKED" {
                            !decision.accepted
                        } else {
                            decision.accepted
                                && decision.operation == case.expected_operation
                                && decision.target == case.expected_target
                        });
                        row["decision"] = json!(decision);
                    }
                    Err(e) => row["error"] = json!(e.to_string()),
                }
                eprintln!(
                    "{} repeat {} {}: {} {:.0}ms",
                    case.id,
                    repeat + 1,
                    name,
                    if row["correct"] == true {
                        "correct"
                    } else {
                        "incorrect/error"
                    },
                    elapsed
                );
                rows.push(row);
            }
        }
    }
    let jev_summary = summary(&rows, "jev");
    let llm_summary = summary(&rows, "llm");
    let mut matched = vec![];
    for pair in rows.as_chunks::<2>().0 {
        if pair.iter().all(|r| r["correct"] == true) {
            let j = pair.iter().find(|r| r["provider"] == "jev").unwrap()["wall_ms"]
                .as_f64()
                .unwrap();
            let l = pair.iter().find(|r| r["provider"] == "llm").unwrap()["wall_ms"]
                .as_f64()
                .unwrap();
            if j > 0.0 {
                matched.push(l / j);
            }
        }
    }
    let valid = rows.iter().all(|r| r.get("error").is_none());
    let report = json!({"kind":"paired_ref_selection","performance_measured":true,"valid_no_provider_errors":valid,
        "boundary":"Model selection only, from identical frozen observations; excludes live capture, device execution, and task completion. No provider-cost estimate.",
        "corpus":input,"repeats":repeats,"jev":jev_summary,"llm":llm_summary,
        "both_correct_pairs":matched.len(),"both_correct_median_speedup":percentile(matched,0.5),
        "warmups":warmups,"rows":rows});
    let report = serde_json::to_string_pretty(&report)?;
    if let Some(out) = out {
        std::fs::write(out, &report)?;
    }
    println!("{report}");
    if !valid {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_and_abstentions_stay_in_the_accuracy_and_latency_denominators() {
        let rows = vec![
            json!({"provider":"jev","correct":true,"wall_ms":100,"decision":{"accepted":true}}),
            json!({"provider":"jev","correct":false,"wall_ms":300,"decision":{"accepted":false}}),
            json!({"provider":"jev","correct":false,"wall_ms":2000,"error":"timeout"}),
            json!({"provider":"llm","correct":true,"wall_ms":500}),
        ];
        let report = summary(&rows, "jev");
        assert_eq!(report["attempts"], 3);
        assert_eq!(report["accuracy"], 1.0 / 3.0);
        assert_eq!(report["errors"], 1);
        assert_eq!(report["abstentions"], 1);
        assert_eq!(report["all_attempts_p50_ms"], 300.0);
        assert_eq!(report["all_attempts_p95_ms"], 2000.0);
        assert_eq!(report["correct_only_p50_ms"], 100.0);
        assert!(summary(&[], "jev")["all_attempts_p50_ms"].is_null());
    }
}

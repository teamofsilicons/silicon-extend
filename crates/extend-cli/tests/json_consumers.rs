//! The repository's own scripts that run `extend --json` read the Team CLI convention: the data
//! itself on stdout, with no `{"ok": …, "data": …}` wrapper (a device command prints its
//! CommandResult, not `{"result": …}`). The CLI dropped that wrapper; this keeps a script from
//! quietly going back to it, which only shows up as a KeyError when someone runs that lane.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Scripts under `dir`, skipping build output and dependencies.
fn scripts(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if !matches!(
                name.as_str(),
                "node_modules" | "target" | "build" | ".gradle" | "dist" | "__pycache__" | ".git"
            ) {
                scripts(&p, out);
            }
        } else if [".py", ".sh", ".mjs", ".ts"].iter().any(|x| name.ends_with(x)) {
            out.push(p);
        }
    }
}

/// Whether a script runs the `extend` CLI binary (not `extend-agent`, `extend-service`, …).
fn runs_the_cli(text: &str) -> bool {
    let bare = |needle: &str| {
        text.match_indices(needle).any(|(i, _)| {
            !text[i + needle.len()..]
                .chars()
                .next()
                .is_some_and(|c| c == '-' || c == '_' || c.is_ascii_alphanumeric())
        })
    };
    bare("target/debug/extend") || text.contains("binary(\"extend\")")
}

/// Lines that read the old `{"ok", "data"}` wrapper from the CLI's output.
fn wrapper_reads(text: &str) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .filter_map(|(n, line)| {
            let l = line.replace('\'', "\"");
            let loads_data = l.find("json.loads(").is_some_and(|i| l[i..].contains(")[\"data\"]"));
            let old_result =
                l.contains("[\"data\"][\"result\"]") || (l.contains("cli(") && l.contains(")[\"result\"]"));
            let jq_data = l.contains("jq_ \"d[\"data\"]");
            (loads_data || old_result || jq_data).then(|| (n + 1, line.trim().to_owned()))
        })
        .collect()
}

#[test]
fn scripts_that_run_extend_read_its_json_unwrapped() {
    let root = repo();
    let mut files = Vec::new();
    for dir in ["e2e", "apps", "scripts"] {
        scripts(&root.join(dir), &mut files);
    }
    let consumers: Vec<_> = files
        .iter()
        .filter_map(|p| Some((p, std::fs::read_to_string(p).ok()?)))
        .filter(|(_, t)| runs_the_cli(t))
        .collect();
    // The lanes known to parse `extend --json`; if this drops, the scan stopped finding them.
    for known in [
        "e2e/cli-e2e.sh",
        "e2e/android-recording-service.py",
        "apps/desktop/linux-e2e/record-service-e2e.py",
        "apps/desktop/macos/text-e2e.py",
    ] {
        assert!(
            consumers.iter().any(|(p, _)| p.ends_with(known)),
            "{known} runs `extend` but the scan didn't find it"
        );
    }
    let mut bad = Vec::new();
    for (p, t) in &consumers {
        for (n, line) in wrapper_reads(t) {
            let rel = p.strip_prefix(&root).unwrap_or(p).display().to_string();
            bad.push(format!("{rel}:{n}: {line}"));
        }
    }
    assert!(
        bad.is_empty(),
        "these lines read `extend --json` through the old {{\"ok\", \"data\"}} wrapper; the CLI prints \
         the data itself (a device command prints its CommandResult), so read it directly:\n{}",
        bad.join("\n")
    );
}

#[test]
fn the_scan_tells_the_cli_from_other_binaries_and_http_envelopes() {
    assert!(runs_the_cli("cmd = [str(ROOT / 'target/debug/extend'), '--json']"));
    assert!(runs_the_cli("EXTEND=\"$ROOT/target/debug/extend\""));
    assert!(runs_the_cli("run([binary(\"extend\"), \"login\"])"));
    assert!(!runs_the_cli("BA=/target/debug/extend-agent"));
    assert!(!runs_the_cli("./target/debug/extend-service serve"));

    assert_eq!(
        wrapper_reads("files = json.loads(out)[\"data\"][\"result\"][\"files\"]").len(),
        1
    );
    assert_eq!(
        wrapper_reads("    return json.loads(result.stdout)['data'] if j else o").len(),
        1
    );
    assert_eq!(wrapper_reads("    response = cli('chef', *command)['result']").len(), 1);
    assert_eq!(
        wrapper_reads("[ \"$(as x iam --json | jq_ 'd[\"data\"][\"app_id\"]')\" = extend ]").len(),
        1
    );
    // An HTTP envelope read from the service directly is fine.
    assert!(wrapper_reads("status, v = http(\"GET\", url)\nnew = v[\"data\"]").is_empty());
    assert!(wrapper_reads("files = json.loads(out)[\"files\"]").is_empty());
}

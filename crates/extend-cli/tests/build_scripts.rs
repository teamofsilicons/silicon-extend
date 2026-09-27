//! Every `cargo … -p <package>` in the release and CI workflows, the Dockerfile and the desktop build
//! scripts names a package this workspace has. A wrong name only fails when that job runs: once a
//! release workflow named a package the workspace didn't have yet, and nothing failed until a tag.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The `name` in each workspace member's `[package]` table.
fn workspace_packages() -> Vec<String> {
    let root = std::fs::read_to_string(repo().join("Cargo.toml")).unwrap();
    let members = root.split("members = [").nth(1).unwrap().split(']').next().unwrap();
    members
        .split(',')
        .map(|m| m.trim().trim_matches('"'))
        .filter(|m| !m.is_empty())
        .map(|m| {
            let manifest = std::fs::read_to_string(repo().join(m).join("Cargo.toml")).unwrap();
            let package = manifest.split("[package]").nth(1).unwrap();
            let line = package.lines().find(|l| l.trim_start().starts_with("name")).unwrap();
            line.split('"').nth(1).unwrap().to_owned()
        })
        .collect()
}

fn build_files() -> Vec<PathBuf> {
    let mut files = vec![repo().join("Dockerfile")];
    for dir in [
        ".github/workflows",
        "apps/desktop/linux",
        "apps/desktop/macos",
        "apps/desktop/windows",
        "apps/desktop/linux-e2e",
    ] {
        if let Ok(entries) = std::fs::read_dir(repo().join(dir)) {
            files.extend(entries.flatten().map(|e| e.path()).filter(|p| {
                matches!(
                    p.extension().and_then(|x| x.to_str()),
                    Some("yml" | "yaml" | "sh" | "ps1")
                )
            }));
        }
    }
    files
}

/// `(file:line, package)` for every `-p`/`--package` argument on a line that runs cargo.
fn named_packages(file: &Path) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(file).unwrap();
    let mut found = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if !line.contains("cargo ") {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        for pair in words.windows(2) {
            if pair[0] == "-p" || pair[0] == "--package" {
                let name = pair[1].trim_matches(|c| c == '"' || c == '\'');
                found.push((format!("{}:{}", file.display(), n + 1), name.to_owned()));
            }
        }
    }
    found
}

#[test]
fn build_scripts_name_only_workspace_packages() {
    let packages = workspace_packages();
    assert!(packages.iter().any(|p| p == "silicon-extend-cli"), "{packages:?}");
    let mut seen = 0;
    for file in build_files() {
        for (at, name) in named_packages(&file) {
            seen += 1;
            assert!(
                packages.contains(&name),
                "{at} builds package {name:?}, which this workspace doesn't have (its packages: {packages:?})"
            );
        }
    }
    // The release workflow, CI and the Dockerfile all build with -p; finding none means the scan broke.
    assert!(seen >= 3, "found only {seen} -p arguments");
}

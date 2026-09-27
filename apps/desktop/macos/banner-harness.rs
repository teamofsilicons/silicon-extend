//! Runs the production tray/banner UI with in-memory status and a fake action sink.
//! No Agent, driver, credential store, service connection, or session is created.
use extend_agent::{
    agent::{AgentHandle, UiAction},
    status::{AgentStatus, StatusHandle},
    ui::{self, Context},
};
use std::{io::Write, path::PathBuf, time::Duration};

fn main() {
    let directory = PathBuf::from(std::env::var_os("EXTEND_BANNER_FIXTURE").expect("fixture directory"));
    // Even a deliberately paired fixture must never register itself at login.
    std::fs::write(
        directory.join("start-at-login.json"),
        br#"{"start_at_login":false,"by":"carbon"}"#,
    )
    .unwrap();
    let status = StatusHandle::new(AgentStatus::default());
    let (actions, mut received) = tokio::sync::mpsc::unbounded_channel();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let handle = AgentHandle {
        status: status.clone(),
        actions,
        shutdown: shutdown.clone(),
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let output = directory.join("actions.jsonl");
    runtime.spawn(async move {
        while let Some(action) = received.recv().await {
            let value = match action {
                UiAction::Stop { target } => serde_json::json!({"action":"stop", "target":target}),
                UiAction::TakeoverDone { target } => serde_json::json!({"action":"takeover_done", "target":target}),
                other => serde_json::json!({"unexpected":format!("{other:?}")}),
            };
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&output)
                .unwrap();
            writeln!(file, "{value}").unwrap();
        }
    });
    let input = directory.join("status.json");
    let quit = directory.join("quit");
    let thread = std::thread::spawn(move || {
        let mut previous = Vec::new();
        while !shutdown.is_cancelled() {
            if quit.exists() {
                shutdown.cancel();
                break;
            }
            if let Ok(bytes) = std::fs::read(&input)
                && bytes != previous
                && let Ok(next) = serde_json::from_slice::<AgentStatus>(&bytes)
            {
                status.update(|current| *current = next);
                previous = bytes;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    ui::run(
        handle,
        runtime.handle().clone(),
        thread,
        Context {
            state_dir: directory,
            download_url: "https://example.invalid/fixture".into(),
        },
    );
}

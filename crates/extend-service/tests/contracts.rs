//! API versioning and consumer-driven contract tests (UNDERSTANDING.md "Versioning" 3–6;
//! TECHNICAL.md section 10), against a real PostgreSQL and the real HTTP and WebSocket stack.
//!
//! - Lifecycle: a deprecated major carries `Deprecation` and `Sunset` headers, is sunset after 7
//!   consecutive days without a request (an injected clock and usage rows stand in for the week),
//!   and then answers `410 api_version_sunset`; negotiation steers clients around it; two majors
//!   are served side by side, each path checking its own pin.
//! - The compatibility matrix (`GET /api/v1/contracts`, `GET /api/v2/contracts`) is built from that
//!   state and configuration.
//! - Replay: every fixture under `contracts/` that a consumer published is sent to a real service.
//!   The device apps' fixtures (the device wire, unchanged in 4.0) must still be accepted, and each
//!   answer must still carry every field the app reads. A 1.x–3.x client's fixtures for the
//!   device wire still replay; its account routes (Silicon IAM sign-in) must get the exact 4.0
//!   answer: `410 api_version_sunset` with the update command. Honeycomb's test-environment
//!   instructions are retired with test environments (`contracts/retired/honeycomb`): 404.
//!   `contracts/README.md` describes the fixture format and the provider states named in `given`.
//!
//! Needs a PostgreSQL the tests can create databases on:
//! `EXTEND_TEST_ADMIN_URL` (default `postgres://extend:extend@127.0.0.1:5440/postgres`).

mod common;

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use common::v2::V2;
use extend_protocol::frames::{DeviceFrame, EnrollmentFrame, ServiceFrame};
use extend_protocol::model::*;
use extend_protocol::{DeviceOs, ErrorCode};
use extend_service::config::{Config, Tuning};
use extend_service::state::Shared;
use extend_service::versions::{self, Clock, Lifecycle, Policy, Registry};
use futures::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use silicon_extend_client::Client;
use time::OffsetDateTime;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A placeholder value for the retired Honeycomb fixtures (nothing accepts it any more).
const HONEYCOMB_TOKEN: &str = "hck_contracts";

// ───────────── Harness ─────────────

async fn database() -> String {
    common::database("contracts").await.0
}

fn config(database_url: String, addr: SocketAddr, device_app_min: &str) -> Config {
    let data = std::env::temp_dir().join(format!("extend_contracts_{}", Uuid::new_v4().simple()));
    let mut cfg = common::config(database_url, addr, data, Tuning::default());
    cfg.device_app_min_version = device_app_min.into();
    cfg
}

struct Svc {
    base: String,
    pool: sqlx::PgPool,
    state: Shared,
    versions: Arc<Registry>,
    http: reqwest::Client,
}

impl Svc {
    async fn start(policy: Policy, clock: Clock, device_app_min: &str) -> Svc {
        let url = database().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = extend_service::build(config(url, addr, device_app_min)).await.unwrap();
        common::seed_accounts(&state);
        let pool = state.pool.clone();
        let versions = Registry::start_with_clock(pool.clone(), policy, clock).await.unwrap();
        tokio::spawn(extend_service::serve_versioned(
            listener,
            state.clone(),
            versions.clone(),
        ));
        Svc {
            base: format!("http://{addr}"),
            pool,
            state,
            versions,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        }
    }

    /// An access token for a test account, signed by the local Silicon Accounts.
    fn token(&self, who: &str) -> String {
        let local = self
            .state
            .accounts
            .local
            .as_deref()
            .expect("the local Silicon Accounts");
        let account = local.ensure(who, common::custodian_of(who)).expect("a test account");
        local.mint(&account, 1800)
    }

    async fn get(&self, path: &str, pin: Option<&str>) -> reqwest::Response {
        let mut r = self.http.get(format!("{}{path}", self.base));
        if let Some(p) = pin {
            r = r.header("Silicon-Extend-API-Version", p);
        }
        r.send().await.unwrap()
    }

    async fn negotiate(&self, supported: &str) -> reqwest::Response {
        self.http
            .get(format!("{}/api/version", self.base))
            .header("Silicon-Extend-Supported-API-Versions", supported)
            .send()
            .await
            .unwrap()
    }
}

fn default_policy() -> Policy {
    Policy::parse(versions::SERVED, |_| None).unwrap()
}

fn policy(served: &[u32], vars: &[(&str, &str)]) -> Policy {
    let vars: HashMap<String, String> = vars.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
    Policy::parse(served, move |name| vars.get(name).cloned()).unwrap()
}

fn real_clock() -> Clock {
    Arc::new(OffsetDateTime::now_utc)
}

/// A clock the test moves.
#[derive(Clone)]
struct ManualClock(Arc<Mutex<OffsetDateTime>>);

impl ManualClock {
    fn at(t: OffsetDateTime) -> Self {
        Self(Arc::new(Mutex::new(t)))
    }
    fn set(&self, t: OffsetDateTime) {
        *self.0.lock().unwrap() = t;
    }
    fn clock(&self) -> Clock {
        let t = self.0.clone();
        Arc::new(move || *t.lock().unwrap())
    }
}

async fn envelope(r: reqwest::Response) -> (u16, Value) {
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

fn header(r: &reqwest::Response, name: &str) -> Option<String> {
    r.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned)
}

/// Waits for the request counter (written in the background) to reach `at_least` for a day.
async fn usage(pool: &sqlx::PgPool, major: i32, day: time::Date, at_least: i64) -> i64 {
    for _ in 0..100 {
        let n: Option<i64> = sqlx::query_scalar(
            "SELECT requests FROM extend_global.api_version_usage WHERE api_version = $1 AND day = $2",
        )
        .bind(major)
        .bind(day)
        .fetch_optional(pool)
        .await
        .unwrap();
        if n.unwrap_or(0) >= at_least {
            return n.unwrap_or(0);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("API version {major} never counted {at_least} requests on {day}");
}

fn tomorrow_midnight(t: OffsetDateTime) -> OffsetDateTime {
    t.date().next_day().unwrap().midnight().assume_utc()
}

// ───────────── Versioning 6: the compatibility matrix ─────────────

#[tokio::test]
async fn the_default_matrix_serves_the_device_wire_on_1_and_accounts_on_2() {
    let svc = Svc::start(default_policy(), real_clock(), "1.0.0").await;
    let (status, m) = envelope(svc.get("/api/v2/contracts", None).await).await;
    assert_eq!(status, 200, "{m}");
    let m = &m["data"];
    assert_eq!(m["supported"], json!([1, 2]));
    assert_eq!(m["current"], 2);
    assert_eq!(
        m["versions"][0]["compatible"],
        json!({"client_crate": ">=1.0.0, <4.0.0", "cli": ">=1.0.0, <4.0.0", "device_app_min": "1.0.0"})
    );
    assert_eq!(
        m["versions"][1]["compatible"],
        json!({"client_crate": ">=4.0.0, <5.0.0", "cli": ">=4.0.0, <5.0.0", "device_app_min": "1.0.0"})
    );
    // A 3.x client still agrees 1 (the device wire); its account calls are told to update.
    let client = Client::connect(&svc.base).await.unwrap();
    assert_eq!(client.api_version(), 1);
    assert_eq!(client.contracts().await.unwrap()["supported"], json!([1, 2]));
}

#[tokio::test]
async fn the_matrix_is_built_from_state_and_configuration() {
    let policy = policy(versions::SERVED, &[("EXTEND_API_V1_CLI", ">=1.2.0, <2.0.0")]);
    let svc = Svc::start(policy, real_clock(), "1.4.0").await;
    let client = Client::connect(&svc.base).await.unwrap();

    let m = client.contracts().await.unwrap();
    assert_eq!(m["supported"], json!([1, 2]));
    assert_eq!(m["current"], 2);
    assert_eq!(m["deprecated"], json!([]));
    assert_eq!(m["service_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(m["sunset_rule"], versions::SUNSET_RULE);
    let versions_list = m["versions"].as_array().unwrap();
    assert_eq!(versions_list.len(), 2, "{m}");
    let v1 = &versions_list[0];
    assert_eq!(v1["api_version"], 1);
    assert_eq!(v1["state"], "current");
    assert!(v1["deprecated_at"].is_null() && v1["sunset_at"].is_null() && v1["sunset_earliest_at"].is_null());
    assert_eq!(
        v1["compatible"],
        json!({"client_crate": ">=1.0.0, <4.0.0", "cli": ">=1.2.0, <2.0.0", "device_app_min": "1.4.0"})
    );

    // The app minimum the matrix states is the one enrollment enforces.
    let enroll = |v: &str| EnrollmentCreate {
        os: DeviceOs::Android,
        os_version: None,
        model: None,
        app_version: v.into(),
        engine_version: None,
    };
    assert_eq!(
        client.enroll(&enroll("1.3.9")).await.unwrap_err().code(),
        ErrorCode::UpgradeRequired
    );
    client.enroll(&enroll("1.4.0")).await.unwrap();

    // Requests are counted per major and day, and upkeep reads the count back into the matrix.
    let today = OffsetDateTime::now_utc().date();
    usage(&svc.pool, 1, today, 3).await;
    svc.versions.upkeep().await.unwrap();
    let m = client.contracts().await.unwrap();
    assert_eq!(m["versions"][0]["last_request_on"], today.to_string());
    // A current major carries no deprecation headers.
    let r = svc.get("/api/v1/contracts", Some("1")).await;
    assert_eq!(r.status(), 200);
    assert!(header(&r, "deprecation").is_none() && header(&r, "sunset").is_none());
}

// ───────────── Versioning 5: deprecation and sunset ─────────────

#[tokio::test]
async fn a_deprecated_major_warns_then_sunsets_after_a_quiet_week() {
    let start = OffsetDateTime::now_utc();
    let clock = ManualClock::at(start);
    // A build that serves 1 and 2 (v2 has no routes here; only its version state matters), with 1
    // deprecated by configuration.
    let svc = Svc::start(
        policy(&[1, 2], &[("EXTEND_DEPRECATED_API_VERSIONS", "1")]),
        clock.clock(),
        "1.0.0",
    )
    .await;
    let v1 = svc.versions.major(1).unwrap();
    assert_eq!(v1.state, Lifecycle::Deprecated);
    let deprecated_at = v1.deprecated_at.unwrap();

    // Every v1 answer says it is deprecated and the soonest it can go: 7 days after today ends.
    let r = svc.get("/api/v1/contracts", Some("1")).await;
    assert_eq!(r.status(), 200);
    assert_eq!(header(&r, "silicon-extend-api-version").as_deref(), Some("1"));
    assert_eq!(
        header(&r, "deprecation"),
        Some(format!("@{}", deprecated_at.unix_timestamp()))
    );
    let earliest = tomorrow_midnight(start) + time::Duration::days(7);
    assert_eq!(header(&r, "sunset"), Some(versions::http_date(earliest)));
    // Errors on a deprecated major carry them too.
    let r = svc.get("/api/v1/device", Some("1")).await;
    assert_eq!(r.status(), 401);
    assert!(header(&r, "deprecation").is_some() && header(&r, "sunset").is_some());

    // Negotiation still agrees 1 with a client that only speaks 1, and says it is deprecated.
    let r = svc.negotiate("1").await;
    assert!(header(&r, "deprecation").is_some());
    let (status, body) = envelope(r).await;
    assert_eq!(status, 200);
    assert_eq!(body["data"]["api_version"], 1);
    assert_eq!(body["data"]["supported"], json!([1, 2]));
    assert_eq!(body["data"]["deprecated"], json!([1]));
    // A client that speaks both gets 2, without deprecation headers.
    let r = svc.negotiate("1, 2").await;
    assert!(header(&r, "deprecation").is_none());
    assert_eq!(envelope(r).await.1["data"]["api_version"], 2);

    // The matrix shows the state.
    let (_, m) = envelope(svc.get("/api/v1/contracts", Some("1")).await).await;
    let m = &m["data"];
    assert_eq!(m["deprecated"], json!([1]));
    assert_eq!(m["current"], 2);
    assert_eq!(m["versions"][0]["state"], "deprecated");
    assert!(m["versions"][0]["deprecated_at"].is_string());
    assert_eq!(
        m["versions"][0]["sunset_earliest_at"],
        earliest.format(&time::format_description::well_known::Rfc3339).unwrap()
    );
    assert_eq!(m["versions"][1]["state"], "current");

    // Today's requests were counted, so a week from now is not yet enough.
    usage(&svc.pool, 1, start.date(), 3).await;
    clock.set(start + time::Duration::days(7));
    assert_eq!(svc.versions.upkeep().await.unwrap(), Vec::<u32>::new());
    assert_eq!(svc.versions.major(1).unwrap().state, Lifecycle::Deprecated);
    // Seven full days after the last day with a request, it is sunset.
    clock.set(earliest + time::Duration::seconds(1));
    assert_eq!(svc.versions.upkeep().await.unwrap(), vec![1]);
    let v1 = svc.versions.major(1).unwrap();
    assert_eq!(v1.state, Lifecycle::Sunset);
    assert_eq!(v1.sunset_at, Some(earliest));

    // A sunset major answers 410 with what happened and what to do.
    let (status, body) = envelope(svc.get("/api/v1/contracts", Some("1")).await).await;
    assert_eq!(status, 410);
    let e = &body["data"];
    assert_eq!(e["code"], "api_version_sunset");
    let message = e["message"].as_str().unwrap();
    assert!(
        message.contains("API version 1 was retired") && message.contains("7 consecutive days without a request"),
        "{message}"
    );
    assert!(
        e["hint"].as_str().unwrap().contains("silicon-apps update extend"),
        "{e}"
    );
    assert_eq!(e["details"]["supported"], json!([2]));
    assert!(!e["request_id"].as_str().unwrap().is_empty());
    // Negotiation: a client that only speaks 1 is told it was retired; one that speaks 2 moves on.
    let (status, body) = envelope(svc.negotiate("1").await).await;
    assert_eq!(status, 410);
    assert_eq!(body["data"]["code"], "api_version_sunset");
    assert!(
        body["data"]["message"]
            .as_str()
            .unwrap()
            .contains("This client speaks only 1"),
        "{body}"
    );
    let (status, body) = envelope(svc.negotiate("1, 2").await).await;
    assert_eq!(status, 200);
    assert_eq!(body["data"]["api_version"], 2);
    assert_eq!(body["data"]["supported"], json!([2]));
    let m = svc.versions.matrix("1.0.0", None);
    assert_eq!(m["versions"][0]["state"], "sunset");
    assert_eq!(m["supported"], json!([2]));

    // The state is shared and durable: a restarted instance, even one whose configuration no
    // longer deprecates 1, keeps it sunset.
    let again = Registry::start_with_clock(svc.pool.clone(), policy(&[1, 2], &[]), clock.clock())
        .await
        .unwrap();
    assert_eq!(again.major(1).unwrap().state, Lifecycle::Sunset);
    assert_eq!(again.negotiable(), vec![2]);
}

#[tokio::test]
async fn the_quiet_week_counts_from_the_last_request_and_configuration_can_undo_a_deprecation() {
    let pool = extend_service::db::connect(&database().await).await.unwrap();
    extend_service::db::migrate_global(&pool).await.unwrap();
    let t0 = time::macros::datetime!(2026-01-10 12:00 UTC);
    let clock = ManualClock::at(t0);
    let registry = Registry::start_with_clock(
        pool.clone(),
        policy(&[1, 2, 3], &[("EXTEND_DEPRECATED_API_VERSIONS", "1, 2")]),
        clock.clock(),
    )
    .await
    .unwrap();
    assert_eq!(registry.deprecated(), vec![1, 2]);
    // v1 had requests on the 12th (and a zero row on the 14th, which doesn't count).
    for (day, n) in [
        (time::macros::date!(2026 - 01 - 12), 5i64),
        (time::macros::date!(2026 - 01 - 14), 0),
    ] {
        sqlx::query("INSERT INTO extend_global.api_version_usage (api_version, day, requests) VALUES (1, $1, $2)")
            .bind(day)
            .bind(n)
            .execute(&pool)
            .await
            .unwrap();
    }
    // Eight days after the deprecation: v2 (no requests) is sunset, v1 (last request on the 12th) isn't.
    clock.set(time::macros::datetime!(2026-01-18 12:00 UTC));
    assert_eq!(registry.upkeep().await.unwrap(), vec![2]);
    assert_eq!(
        registry.major(2).unwrap().sunset_at,
        Some(time::macros::datetime!(2026-01-17 12:00 UTC))
    );
    assert_eq!(registry.major(1).unwrap().state, Lifecycle::Deprecated);
    clock.set(time::macros::datetime!(2026-01-19 23:59:59 UTC));
    assert!(registry.upkeep().await.unwrap().is_empty());
    clock.set(time::macros::datetime!(2026-01-20 00:00 UTC));
    assert_eq!(registry.upkeep().await.unwrap(), vec![1]);
    assert_eq!(
        registry.major(1).unwrap().sunset_at,
        Some(time::macros::datetime!(2026-01-20 00:00 UTC))
    );
    // v3 is untouched.
    assert_eq!(registry.major(3).unwrap().state, Lifecycle::Current);

    // Configuration can take back a deprecation that hasn't led to a sunset yet.
    let pool2 = extend_service::db::connect(&database().await).await.unwrap();
    extend_service::db::migrate_global(&pool2).await.unwrap();
    let deprecated = Registry::start_with_clock(
        pool2.clone(),
        policy(&[1, 2], &[("EXTEND_DEPRECATED_API_VERSIONS", "1")]),
        clock.clock(),
    )
    .await
    .unwrap();
    assert_eq!(deprecated.major(1).unwrap().state, Lifecycle::Deprecated);
    let undone = Registry::start_with_clock(pool2.clone(), policy(&[1, 2], &[]), clock.clock())
        .await
        .unwrap();
    let v1 = undone.major(1).unwrap();
    assert_eq!((v1.state, v1.deprecated_at), (Lifecycle::Current, None));

    // During a rolling deploy, an instance still running the old configuration picks up the new
    // state at its next upkeep instead of undoing it.
    let old = undone;
    let new = Registry::start_with_clock(
        pool2,
        policy(&[1, 2], &[("EXTEND_DEPRECATED_API_VERSIONS", "1")]),
        clock.clock(),
    )
    .await
    .unwrap();
    let deprecated_at = new.major(1).unwrap().deprecated_at;
    old.upkeep().await.unwrap();
    let v1 = old.major(1).unwrap();
    assert_eq!((v1.state, v1.deprecated_at), (Lifecycle::Deprecated, deprecated_at));
    new.upkeep().await.unwrap();
    assert_eq!(new.major(1).unwrap().deprecated_at, deprecated_at);
}

// ───────────── Versioning 3: majors side by side ─────────────

#[tokio::test]
async fn two_majors_are_served_side_by_side_each_path_checking_its_own_pin() {
    use axum::routing::get;
    let pool = extend_service::db::connect(&database().await).await.unwrap();
    extend_service::db::migrate_global(&pool).await.unwrap();
    let start = OffsetDateTime::now_utc();
    let clock = ManualClock::at(start);
    let registry = Registry::start_with_clock(
        pool.clone(),
        policy(&[1, 2], &[("EXTEND_DEPRECATED_API_VERSIONS", "1")]),
        clock.clock(),
    )
    .await
    .unwrap();
    // The shape `routes::router` takes: route tables for each major, behind one version layer.
    let v1 = axum::Router::new().route("/api/v1/echo", get(|| async { "one" }));
    let v2 = axum::Router::new().route("/api/v2/echo", get(|| async { "two" }));
    let app = v1
        .merge(v2)
        .route("/api/version-free", get(|| async { "free" }))
        .layer(axum::middleware::from_fn_with_state(registry.clone(), versions::layer));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    let http = reqwest::Client::new();
    let get = |path: &str, pin: Option<&str>| {
        let mut r = http.get(format!("{base}{path}"));
        if let Some(p) = pin {
            r = r.header("Silicon-Extend-API-Version", p);
        }
        r.send()
    };

    let r = get("/api/v1/echo", Some("1")).await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(header(&r, "silicon-extend-api-version").as_deref(), Some("1"));
    assert!(header(&r, "deprecation").is_some() && header(&r, "sunset").is_some());
    assert_eq!(r.text().await.unwrap(), "one");
    let r = get("/api/v2/echo", Some("2")).await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(header(&r, "silicon-extend-api-version").as_deref(), Some("2"));
    assert!(header(&r, "deprecation").is_none());
    assert_eq!(r.text().await.unwrap(), "two");
    // Unpinned requests work on either.
    assert_eq!(get("/api/v2/echo", None).await.unwrap().status(), 200);
    // A pin that disagrees with its path is refused, per path.
    let (status, body) = envelope(get("/api/v1/echo", Some("2")).await.unwrap()).await;
    assert_eq!(
        (status, body["data"]["code"].as_str()),
        (400, Some("api_version_mismatch"))
    );
    let (status, body) = envelope(get("/api/v2/echo", Some("1")).await.unwrap()).await;
    assert_eq!(
        (status, body["data"]["code"].as_str()),
        (400, Some("api_version_mismatch"))
    );
    // A major this build doesn't serve is named as such, with the ones it does.
    let (status, body) = envelope(get("/api/v3/echo", None).await.unwrap()).await;
    assert_eq!(status, 400);
    assert_eq!(body["data"]["code"], "api_version_unsupported");
    assert_eq!(body["data"]["details"], json!({"requested": 3, "service": [1, 2]}));
    assert!(body["data"]["hint"].as_str().unwrap().contains("GET /api/version"));
    // Unversioned paths pass through, stamped with the newest major.
    let r = get("/api/version-free", Some("7")).await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(header(&r, "silicon-extend-api-version").as_deref(), Some("2"));

    // Each major's requests are counted separately (refused ones aren't).
    assert_eq!(usage(&pool, 1, start.date(), 1).await, 1);
    assert_eq!(usage(&pool, 2, start.date(), 2).await, 2);

    // Once v1 is sunset, v2 keeps answering.
    clock.set(tomorrow_midnight(start) + time::Duration::days(8));
    assert_eq!(registry.upkeep().await.unwrap(), vec![1]);
    let (status, body) = envelope(get("/api/v1/echo", Some("1")).await.unwrap()).await;
    assert_eq!(
        (status, body["data"]["code"].as_str()),
        (410, Some("api_version_sunset"))
    );
    assert_eq!(get("/api/v2/echo", Some("2")).await.unwrap().status(), 200);
}

// ───────────── Versioning 4: replaying consumer fixtures ─────────────

/// The repository's `contracts/`, or `EXTEND_CONTRACTS_DIR` (to replay fixtures kept elsewhere,
/// such as a release's from git).
fn contracts_dir() -> PathBuf {
    match std::env::var_os("EXTEND_CONTRACTS_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts"),
    }
}

/// The frozen fixtures of released client versions under a major (`v1/client-1.0.0`): what a
/// published client still sends, kept after the live fixtures are regenerated.
fn frozen_client_dirs(root: &Path, major: u32) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(root.join(format!("v{major}")))
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.is_dir()
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("client-"))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn load(dir: &Path) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let v: Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()));
        out.push((path.file_name().unwrap().to_string_lossy().into_owned(), v));
    }
    out
}

/// Provider states a fixture can name in `given`, and the placeholders they fill.
const STATES: &[(&str, &[&str])] = &[
    ("refresh_token", &["refresh_token"]),
    ("permission_request", &["permission_id", "permission_code"]),
    ("permission_grant", &[]),
    ("enrollment", &["enrollment_id", "enrollment_secret", "pairing_code"]),
    ("device", &["device_id", "device_credential", "device_version"]),
    ("importable_device", &["device_id"]),
    ("session", &["session_id"]),
    ("takeover", &[]),
    ("file", &["file_id"]),
    ("upload", &["upload_id"]),
    ("host", &["host_id"]),
    ("attached", &["attached_id"]),
    ("carried_session", &["session_id"]),
    ("test_environment", &["testing_secret"]),
    (
        "honeycomb_environment",
        &["environment_id", "org_id", "testing_key", "operation_id"],
    ),
    (
        "paired",
        &[
            "device_id",
            "device_credential",
            "command_id",
            "upload_id",
            "attached_id",
            "wake_id",
        ],
    ),
    ("failed_setup", &["device_id", "device_credential"]),
    ("shared_device", &["shared_device_id"]),
    ("shared_computer", &["shared_host_id"]),
    ("wake_request", &["wake_id"]),
];

/// States a state sets up first.
fn implies(state: &str) -> &'static [&'static str] {
    match state {
        "permission_grant" => &["permission_request"],
        "session" | "upload" | "shared_device" | "wake_request" => &["device"],
        "takeover" | "file" => &["session"],
        "attached" | "shared_computer" => &["host"],
        "carried_session" => &["attached"],
        _ => &[],
    }
}

/// Placeholders every fixture can use.
const ALWAYS: &[&str] = &[
    "carbon_token",
    "other_carbon_token",
    "silicon_token",
    "other_silicon_token",
    "carbon_slt",
    "silicon_id",
    "team",
    "isi",
    "idempotency_key",
    "honeycomb_token",
];

fn placeholders_in(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::String(s) => {
            let mut rest = s.as_str();
            while let Some(i) = rest.find('{') {
                rest = &rest[i + 1..];
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || *c == '_')
                    .collect();
                if !name.is_empty() && rest[name.len()..].starts_with('}') {
                    out.insert(name);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| placeholders_in(x, out)),
        Value::Object(o) => o.values().for_each(|x| placeholders_in(x, out)),
        _ => {}
    }
}

/// Every fixture is readable, names known states and placeholders, and sits under its major; every
/// major this build serves has client fixtures to replay.
#[test]
fn every_fixture_is_well_formed() {
    let root = contracts_dir();
    let mut dirs = vec![(None, root.join("retired/honeycomb"))];
    for &major in versions::SERVED {
        if major == extend_protocol::ACCOUNT_API_VERSION && !root.join(format!("v{major}/client")).exists() {
            // API v2's consumer is the 4.0 client crate, which records its own fixtures here.
            eprintln!("contracts/v{major}/client: no fixtures recorded yet");
            continue;
        }
        assert!(
            !load(&root.join(format!("v{major}/client"))).is_empty(),
            "API version {major} is served but contracts/v{major}/client has no fixtures; record them with \
             `EXTEND_CONTRACTS_WRITE=1 cargo test -p silicon-extend-client --test contract_fixtures`"
        );
        dirs.push((Some(major), root.join(format!("v{major}/client"))));
        dirs.push((Some(major), root.join(format!("v{major}/device"))));
        for frozen in frozen_client_dirs(&root, major) {
            dirs.push((Some(major), frozen));
        }
    }
    let known_states: BTreeSet<&str> = STATES.iter().map(|(s, _)| *s).collect();
    let mut problems = Vec::new();
    for (major, dir) in dirs {
        for (name, f) in load(&dir) {
            let at = format!("{}/{name}", dir.display());
            if f["contract"] != 1 {
                problems.push(format!("{at}: \"contract\" must be 1"));
            }
            for field in ["consumer", "operation"] {
                if !f[field].is_string() {
                    problems.push(format!("{at}: \"{field}\" must be a string"));
                }
            }
            if f["api_version"].as_u64().map(|v| v as u32) != major {
                problems.push(format!(
                    "{at}: \"api_version\" must be {major:?}, the directory it is in"
                ));
            }
            let kind = f["kind"].as_str().unwrap_or("http");
            if !["http", "device_socket", "enrollment_socket"].contains(&kind) {
                problems.push(format!("{at}: unknown kind {kind:?}"));
            }
            let given: Vec<&str> = f["given"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            for g in &given {
                if !known_states.contains(g) {
                    problems.push(format!(
                        "{at}: provider state {g:?} is unknown; add it to STATES and Provider::given in {}",
                        file!()
                    ));
                }
            }
            let mut used = BTreeSet::new();
            placeholders_in(&f["request"], &mut used);
            placeholders_in(&f["sends"], &mut used);
            let mut closure: BTreeSet<&str> = given.iter().copied().collect();
            loop {
                let implied: Vec<&str> = closure.iter().flat_map(|g| implies(g).iter().copied()).collect();
                let before = closure.len();
                closure.extend(implied);
                if closure.len() == before {
                    break;
                }
            }
            let available: BTreeSet<&str> = ALWAYS
                .iter()
                .copied()
                .chain(
                    STATES
                        .iter()
                        .filter(|(s, _)| closure.contains(s))
                        .flat_map(|(_, p)| p.iter().copied()),
                )
                .collect();
            for p in &used {
                if !available.contains(p.as_str()) {
                    problems.push(format!(
                        "{at}: placeholder {{{p}}} is not filled by its given states {given:?}"
                    ));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "Malformed contract fixtures:\n{}",
        problems.join("\n")
    );
}

/// A scripted Extend app for provider states: answers pings and commands like a well-behaved device.
struct Device {
    id: String,
    credential: String,
}

async fn ws_connect(url: &str, headers: &[(String, String)]) -> Ws {
    let mut req = url.into_client_request().unwrap();
    for (k, v) in headers {
        req.headers_mut()
            .insert(http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
    }
    tokio_tungstenite::connect_async(req)
        .await
        .unwrap_or_else(|e| panic!("WebSocket {url}: {e}"))
        .0
}

async fn next_json(ws: &mut Ws) -> Value {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(20), ws.next())
            .await
            .expect("a frame in time")
            .expect("socket open")
            .expect("readable frame");
        if let Message::Text(t) = m {
            return serde_json::from_str(&t).expect("JSON frame");
        }
    }
}

async fn send_json(ws: &mut Ws, v: &Value) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

fn hello(os: DeviceOs) -> Value {
    hello_as(os, "1.0.0", Setup::complete())
}

/// A hello from an app of `app_version`; a 1.1 app lists `setup_retry` in its features.
fn hello_as(os: DeviceOs, app_version: &str, setup: Setup) -> Value {
    let v11 = app_version.starts_with("1.1");
    serde_json::to_value(DeviceFrame::Hello(extend_protocol::frames::Hello {
        app_version: app_version.into(),
        os,
        os_version: Some("15".into()),
        model: Some("Contract".into()),
        engine_version: v11.then(|| "0.21.15".into()),
        capabilities: os.full_capabilities().to_vec(),
        missing: vec![],
        setup,
        features: if v11 {
            vec![extend_protocol::feature::SETUP_RETRY.into()]
        } else {
            vec![]
        },
    }))
    .unwrap()
}

/// A raw API call, answering the status and the parsed body.
async fn call(
    base: &str,
    method: reqwest::Method,
    path: &str,
    authorization: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut r = reqwest::Client::new()
        .request(method, format!("{base}{path}"))
        .header("authorization", authorization);
    if let Some(b) = body {
        r = r.json(&b);
    }
    let resp = r.send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// "Pair with another Carbon": the device with `credential` shows a code, c:bob (`bob_token`)
/// claims it, and the new pair's id and credential come back.
async fn pair_another(
    client: &Client,
    base: &str,
    credential: &str,
    bob_token: &str,
    name: &str,
) -> Result<Device, String> {
    let (status, e) = call(
        base,
        reqwest::Method::POST,
        "/api/v1/device/enrollments",
        &format!("Extend-Device {credential}"),
        None,
    )
    .await;
    if status != 201 {
        return Err(format!("POST /api/v1/device/enrollments answered {status}: {e}"));
    }
    let data = &e["data"];
    let claimed = V2::new(base, bob_token)
        .pair(&PairingClaim {
            pairing_code: data["pairing_code"].as_str().unwrap_or_default().into(),
            name: name.into(),
            visibility: None,
            pair_ttl_days: None,
            silicon_ids: vec!["si:chef".into()],
        })
        .await
        .map_err(|e| format!("c:bob claiming the second pair: {e}"))?;
    let id: Uuid = data["enrollment_id"]
        .as_str()
        .unwrap_or_default()
        .parse()
        .map_err(|e| format!("{e}"))?;
    match client
        .enrollment(id, data["enrollment_secret"].as_str().unwrap_or_default())
        .await
        .map_err(|e| format!("reading the second pair's enrollment: {e}"))?
    {
        EnrollmentState::Paired { device_credential, .. } => Ok(Device {
            id: claimed.device_id.to_string(),
            credential: device_credential,
        }),
        other => Err(format!("the second pair's enrollment isn't paired: {other:?}")),
    }
}

const ARTIFACT: &[u8] = b"\x89PNG device contract fixture";

async fn upload(base: &str, credential: &str, upload_id: &str, bytes: &[u8]) {
    use sha2::Digest as _;
    let r = reqwest::Client::new()
        .put(format!("{base}/api/v1/device/artifacts/{upload_id}"))
        .header("authorization", format!("Extend-Device {credential}"))
        .header("content-type", "image/png")
        .header("x-file-name", "screenshot.png")
        .header(
            "x-content-sha256",
            extend_protocol::ids::hex_lower(&sha2::Sha256::digest(bytes)),
        )
        .body(bytes.to_vec())
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "artifact upload: {}", r.status());
}

/// Pairs a device through the enrollment socket, as the apps do; returns it unconnected.
async fn pair(client: &Client, base: &str, carbon_token: &str, os: DeviceOs, silicons: &[&str]) -> Device {
    let e = client
        .enroll(&EnrollmentCreate {
            os,
            os_version: Some("15".into()),
            model: Some("Contract".into()),
            app_version: "1.0.0".into(),
            engine_version: None,
        })
        .await
        .unwrap();
    let mut ews = ws_connect(
        &client.ws_url(&format!("/api/v1/enrollments/{}/connect", e.enrollment_id)),
        &[(
            "authorization".into(),
            format!("Extend-Enrollment {}", e.enrollment_secret),
        )],
    )
    .await;
    V2::new(base, carbon_token)
        .pair(&PairingClaim {
            pairing_code: e.pairing_code,
            name: format!("Contract {}", os.as_str()),
            visibility: None,
            pair_ttl_days: None,
            silicon_ids: silicons.iter().map(|s| (*s).to_owned()).collect(),
        })
        .await
        .unwrap();
    loop {
        let f: EnrollmentFrame = serde_json::from_value(next_json(&mut ews).await).unwrap();
        if let EnrollmentFrame::Paired {
            device_id,
            device_credential,
            ..
        } = f
        {
            return Device {
                id: device_id.to_string(),
                credential: device_credential,
            };
        }
    }
}

impl Device {
    /// Connects, says `hello`, and answers pings and commands in the background; the sender
    /// injects more frames (an `awake`).
    async fn serve_with(
        &self,
        base: &str,
        client: &Client,
        owner: &V2<'_>,
        hello: Value,
    ) -> (tokio::task::JoinHandle<()>, tokio::sync::mpsc::UnboundedSender<Value>) {
        let mut ws = ws_connect(
            &client.ws_url("/api/v1/device/connect"),
            &[("authorization".into(), format!("Extend-Device {}", self.credential))],
        )
        .await;
        send_json(&mut ws, &hello).await;
        // A successful WebSocket send only queues hello. Wait until the real API exposes its
        // persisted setup before a fixture can start a session or retry a failed step.
        wait_for_device_report(owner, &self.id, &hello).await.unwrap();
        let base = base.to_owned();
        let credential = self.credential.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let task = tokio::spawn(async move {
            loop {
                let m = tokio::select! {
                    out = rx.recv() => {
                        if let Some(v) = out { send_json(&mut ws, &v).await; }
                        continue;
                    }
                    m = ws.next() => m,
                };
                let Some(Ok(m)) = m else { break };
                let Message::Text(t) = m else { continue };
                let Ok(f) = serde_json::from_str::<ServiceFrame>(&t) else {
                    continue;
                };
                match f {
                    ServiceFrame::Ping { nonce } => {
                        send_json(&mut ws, &json!({"type": "pong", "nonce": nonce})).await;
                    }
                    ServiceFrame::Command(c) => {
                        let mut files = vec![];
                        if c.command == "screenshot" {
                            upload(&base, &credential, &c.upload_ids[0].to_string(), ARTIFACT).await;
                            files.push(json!({"upload_id": c.upload_ids[0], "name": "screenshot.png",
                                              "content_type": "image/png", "kind": "screenshot",
                                              "size_bytes": ARTIFACT.len()}));
                        }
                        let answer = json!({"type": "result", "id": c.id, "ok": true, "output": {"echo": c.args},
                                            "text": format!("ran {}", c.command), "error": null, "files": files});
                        send_json(&mut ws, &answer).await;
                    }
                    ServiceFrame::Unpaired { .. } | ServiceFrame::Superseded => break,
                    _ => {}
                }
            }
        });
        (task, tx)
    }
}

/// Sets up the provider states fixtures name, and fills their placeholders.
struct Provider<'a> {
    svc: &'a Svc,
    client: Client,
    vars: HashMap<String, String>,
    done: BTreeSet<String>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// Frames to send on a served device's socket, by device id.
    inject: HashMap<String, tokio::sync::mpsc::UnboundedSender<Value>>,
}

impl Drop for Provider<'_> {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// States only API v1's account routes used (Silicon IAM permissions and refresh tokens, Honeycomb
/// test environments, organization imports). Fixtures naming them are retired: they are replayed
/// without them, for the 4.0 answer.
const RETIRED_STATES: &[&str] = &[
    "refresh_token",
    "permission_request",
    "permission_grant",
    "importable_device",
    "test_environment",
    "honeycomb_environment",
];

impl<'a> Provider<'a> {
    async fn new(svc: &'a Svc) -> Provider<'a> {
        let client = Client::connect(&svc.base).await.unwrap();
        let mut vars = HashMap::new();
        for (var, who) in [
            ("carbon_token", "c:alice"),
            ("other_carbon_token", "c:bob"),
            ("silicon_token", "si:chef"),
            ("other_silicon_token", "si:sous"),
        ] {
            vars.insert(var.into(), svc.token(who));
        }
        for (k, v) in [
            ("carbon_slt", "c:alice"),
            ("silicon_id", "si:chef"),
            ("team", "acme"),
            ("isi", "contract-isi"),
            ("honeycomb_token", HONEYCOMB_TOKEN),
        ] {
            vars.insert(k.into(), v.into());
        }
        Provider {
            svc,
            client,
            vars,
            done: BTreeSet::new(),
            tasks: Vec::new(),
            inject: HashMap::new(),
        }
    }

    fn var(&self, k: &str) -> String {
        self.vars[k].clone()
    }

    /// API v2 as c:alice, who pairs the devices.
    fn carbon(&self) -> V2<'_> {
        V2::new(&self.svc.base, &self.vars["carbon_token"])
    }

    /// API v2 as si:chef.
    fn silicon(&self) -> V2<'_> {
        V2::new(&self.svc.base, &self.vars["silicon_token"])
    }

    async fn device_version(&self, id: &str) -> i64 {
        self.carbon().device(id).await.unwrap().version.unwrap()
    }

    /// Fills every placeholder a retired fixture uses with a value of the right shape: nothing it
    /// names exists, and nothing needs to (the route is gone).
    fn fill_retired(&mut self) {
        for (k, v) in [
            ("refresh_token", "rt_retired"),
            ("permission_id", "00000000-0000-7000-8000-000000000000"),
            ("permission_code", "obc_retired"),
            ("enrollment_id", "00000000-0000-7000-8000-000000000001"),
            ("enrollment_secret", "ens_retired"),
            ("pairing_code", "ABC123"),
            ("device_id", "0000aaaa"),
            ("device_credential", "edc_retired"),
            ("device_version", "1"),
            ("session_id", "abc"),
            ("file_id", "00000000-0000-7000-8000-000000000002"),
            ("upload_id", "00000000-0000-7000-8000-000000000003"),
            ("host_id", "0000bbbb"),
            ("attached_id", "0000cccc"),
            ("testing_secret", "ask_retired"),
            ("environment_id", "00000000-0000-7000-8000-000000000004"),
            ("org_id", "acme"),
            ("testing_key", "abcdefghijklmnopqrstuvwxyz012345"),
            ("operation_id", "00000000-0000-7000-8000-000000000005"),
            ("command_id", "00000000-0000-7000-8000-000000000006"),
            ("wake_id", "00000000-0000-7000-8000-000000000007"),
            ("shared_device_id", "0000dddd"),
            ("shared_host_id", "0000eeee"),
        ] {
            self.vars.entry(k.into()).or_insert_with(|| v.into());
        }
    }

    fn given<'s>(&'s mut self, state: &'s str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 's>> {
        Box::pin(async move {
            if !self.done.insert(state.to_owned()) {
                return;
            }
            let base = self.svc.base.clone();
            match state {
                s if RETIRED_STATES.contains(&s) => {
                    panic!("provider state {s:?} belongs to a retired API v1 route; only retired fixtures name it")
                }
                "enrollment" => {
                    let e = self
                        .client
                        .enroll(&EnrollmentCreate {
                            os: DeviceOs::Android,
                            os_version: None,
                            model: None,
                            app_version: "1.0.0".into(),
                            engine_version: None,
                        })
                        .await
                        .unwrap();
                    self.vars.insert("enrollment_id".into(), e.enrollment_id.to_string());
                    self.vars.insert("enrollment_secret".into(), e.enrollment_secret);
                    self.vars.insert("pairing_code".into(), e.pairing_code);
                }
                "device" => {
                    let d = pair(
                        &self.client,
                        &base,
                        &self.var("carbon_token"),
                        DeviceOs::Android,
                        &["si:chef", "si:sous"],
                    )
                    .await;
                    let (task, inject) = d
                        .serve_with(&base, &self.client, &self.carbon(), hello(DeviceOs::Android))
                        .await;
                    self.tasks.push(task);
                    self.inject.insert(d.id.clone(), inject);
                    let v = self.device_version(&d.id).await;
                    self.vars.insert("device_id".into(), d.id);
                    self.vars.insert("device_credential".into(), d.credential);
                    self.vars.insert("device_version".into(), v.to_string());
                }
                "session" => {
                    self.given("device").await;
                    let s = self
                        .silicon()
                        .start_session(&self.var("device_id").parse().unwrap())
                        .await
                        .unwrap();
                    self.vars.insert("session_id".into(), s.session_id.to_string());
                    // Starting a session changes nothing the If-Match version covers, but re-read it.
                    let v = self.device_version(&self.var("device_id")).await;
                    self.vars.insert("device_version".into(), v.to_string());
                }
                "takeover" => {
                    self.given("session").await;
                    self.silicon()
                        .takeover(&self.var("session_id"), "Please approve Face ID")
                        .await
                        .unwrap();
                }
                "file" => {
                    self.given("session").await;
                    let r = self
                        .silicon()
                        .run(
                            &self.var("session_id"),
                            &CommandRequest {
                                command: "screenshot".into(),
                                args: vec![],
                                timeout_ms: None,
                                self_destruct_minutes: None,
                                permanent: false,
                                attachments: vec![],
                            },
                        )
                        .await
                        .unwrap();
                    self.vars.insert("file_id".into(), r.files[0].file_id.to_string());
                }
                "upload" => {
                    self.given("device").await;
                    let id = Uuid::new_v4();
                    sqlx::query(
                        "INSERT INTO extend.uploads (upload_id, device_id, command_id, expires_at)
                         VALUES ($1, $2, $3, now() + interval '10 minutes')",
                    )
                    .bind(id)
                    .bind(self.var("device_id"))
                    .bind(Uuid::new_v4())
                    .execute(&self.svc.pool)
                    .await
                    .unwrap();
                    self.vars.insert("upload_id".into(), id.to_string());
                }
                "host" => {
                    let d = pair(
                        &self.client,
                        &base,
                        &self.var("carbon_token"),
                        DeviceOs::Macos,
                        &["si:chef"],
                    )
                    .await;
                    let (task, inject) = d
                        .serve_with(&base, &self.client, &self.carbon(), hello(DeviceOs::Macos))
                        .await;
                    self.tasks.push(task);
                    self.inject.insert(d.id.clone(), inject);
                    self.vars.insert("host_credential".into(), d.credential.clone());
                    self.vars.insert("host_id".into(), d.id);
                }
                "shared_device" => {
                    self.given("device").await;
                    let bob = pair_another(
                        &self.client,
                        &base,
                        &self.var("device_credential"),
                        &self.var("other_carbon_token"),
                        "Contract, bob's",
                    )
                    .await
                    .unwrap_or_else(|e| panic!("shared_device: {e}"));
                    self.vars.insert("shared_device_id".into(), bob.id);
                }
                "shared_computer" => {
                    self.given("host").await;
                    let host_id = self.var("host_id");
                    // Both pairs connected by 1.1 apps.
                    if let Some(inject) = self.inject.get(&host_id) {
                        let _ = inject.send(hello_as(DeviceOs::Macos, "1.1.0", Setup::complete()));
                    }
                    let bob = pair_another(
                        &self.client,
                        &base,
                        &self.var("host_credential"),
                        &self.var("other_carbon_token"),
                        "Contract Mac, bob's",
                    )
                    .await
                    .unwrap_or_else(|e| panic!("shared_computer: {e}"));
                    let bob_v2 = V2::new(&base, &self.vars["other_carbon_token"]);
                    let (task, inject) = bob
                        .serve_with(
                            &base,
                            &self.client,
                            &bob_v2,
                            hello_as(DeviceOs::Macos, "1.1.0", Setup::complete()),
                        )
                        .await;
                    self.tasks.push(task);
                    self.inject.insert(bob.id.clone(), inject);
                    self.vars.insert("shared_host_id".into(), bob.id);
                }
                "wake_request" => {
                    self.given("device").await;
                    let id = self.var("device_id");
                    // The phone reports its screen off; si:chef asks its Carbon to wake it.
                    if let Some(inject) = self.inject.get(&id) {
                        let _ = inject.send(json!({"type": "awake", "awake": false, "sleep_state": "screen_off",
                                                   "run": Uuid::new_v4(), "seq": 1}));
                    }
                    let owner = &self.carbon();
                    let idr = id.as_str();
                    eventually("the phone reading not awake", move || async move {
                        owner.device(idr).await.is_ok_and(|d| d.awake == Some(false))
                    })
                    .await
                    .unwrap_or_else(|e| panic!("wake_request: {e}"));
                    let (status, w) = call(
                        &base,
                        reqwest::Method::POST,
                        &format!("/api/v2/devices/{id}/wake-requests"),
                        &format!("Bearer {}", self.var("silicon_token")),
                        Some(json!({"type": "wake_request", "data": {"reason": "Contract: the order screen"}})),
                    )
                    .await;
                    assert_eq!(status, 201, "asking to wake the phone: {w}");
                    self.vars.insert(
                        "wake_id".into(),
                        w["data"]["wake_id"].as_str().unwrap_or_default().into(),
                    );
                }
                "failed_setup" => {
                    let d = pair(
                        &self.client,
                        &base,
                        &self.var("carbon_token"),
                        DeviceOs::Android,
                        &["si:chef"],
                    )
                    .await;
                    let failed = Setup::from_steps(vec![SetupStep {
                        key: "wireless_debugging".into(),
                        title: "Turn on wireless debugging".into(),
                        status: StepStatus::Failed,
                        help: None,
                        error: Some(
                            "The phone turned down the pairing. Open Wireless debugging on it, then tap Retry.".into(),
                        ),
                        input: None,
                    }]);
                    let (task, inject) = d
                        .serve_with(
                            &base,
                            &self.client,
                            &self.carbon(),
                            hello_as(DeviceOs::Android, "1.1.0", failed),
                        )
                        .await;
                    self.tasks.push(task);
                    self.inject.insert(d.id.clone(), inject);
                    self.vars.insert("device_id".into(), d.id);
                    self.vars.insert("device_credential".into(), d.credential);
                }
                "attached" => {
                    self.given("host").await;
                    let d = self
                        .carbon()
                        .attach(
                            &self.var("host_id"),
                            &AttachmentCreate {
                                os: DeviceOs::Tvos,
                                name: "Living room".into(),
                                visibility: None,
                                pair_ttl_days: None,
                                address: None,
                            },
                        )
                        .await
                        .unwrap();
                    self.vars.insert("attached_id".into(), d.device_id.to_string());
                }
                "carried_session" => {
                    self.given("attached").await;
                    let id = self.var("attached_id");
                    self.carbon().grant(&id, "si:chef").await.unwrap();
                    let report = json!({"type":"attached", "device_id":id, "online":true,
                        "capabilities":["input.remote", "nav.system"], "missing":[],
                        "setup":{"state":"complete", "steps":[]}});
                    self.inject[&self.var("host_id")].send(report.clone()).unwrap();
                    wait_for_device_report(&self.carbon(), &id, &report).await.unwrap();
                    let session = self.silicon().start_session(&id.parse().unwrap()).await.unwrap();
                    self.vars.insert("session_id".into(), session.session_id.to_string());
                }
                "paired" => {}
                other => panic!("unknown provider state {other:?}"),
            }
        })
    }

    /// Fills `{placeholders}`; an unknown one is an error naming it.
    fn fill(&self, s: &str) -> Result<String, String> {
        let mut out = String::new();
        let mut rest = s;
        while let Some(i) = rest.find('{') {
            out.push_str(&rest[..i]);
            let after = &rest[i + 1..];
            let name: String = after
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '_')
                .collect();
            if !name.is_empty() && after[name.len()..].starts_with('}') {
                let value = if name == "idempotency_key" {
                    Uuid::new_v4().to_string()
                } else {
                    self.vars
                        .get(&name)
                        .cloned()
                        .ok_or_else(|| format!("no value for {{{name}}} (is a provider state missing from `given`?)"))?
                };
                out.push_str(&value);
                rest = &after[name.len() + 1..];
            } else {
                out.push('{');
                rest = after;
            }
        }
        out.push_str(rest);
        Ok(out)
    }

    fn fill_json(&self, v: &Value) -> Result<Value, String> {
        Ok(match v {
            Value::String(s) => Value::String(self.fill(s)?),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.fill_json(x)).collect::<Result<_, _>>()?),
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, x)| Ok((k.clone(), self.fill_json(x)?)))
                    .collect::<Result<_, String>>()?,
            ),
            other => other.clone(),
        })
    }
}

/// Checks `path` (`/a/*/b`) in `v`: `Required` → the last key is present, `NonNull` → if present,
/// it is not null. A missing or null parent passes: the consumer reads the parent as optional, and
/// its own entry says otherwise when it isn't.
fn check_path(v: &Value, segs: &[&str], non_null: bool) -> Result<(), String> {
    match segs {
        [] => Ok(()),
        ["*", rest @ ..] => match v {
            Value::Array(a) => a.iter().try_for_each(|x| check_path(x, rest, non_null)),
            _ => Ok(()),
        },
        [key] => match v {
            Value::Object(o) => match o.get(*key) {
                None if !non_null => Err(format!("missing \"{key}\"")),
                Some(Value::Null) if non_null => Err(format!("\"{key}\" is null")),
                _ => Ok(()),
            },
            _ => Ok(()),
        },
        [key, rest @ ..] => match v.get(*key) {
            Some(child) if !child.is_null() => check_path(child, rest, non_null),
            _ => Ok(()),
        },
    }
}

fn pointer_segs(p: &str) -> Vec<&str> {
    p.trim_start_matches('/').split('/').collect()
}

fn check_response(spec: &Value, body: &[u8], sent: Option<&Value>) -> Result<(), String> {
    let needs_json = ["required", "non_null", "equals_request", "one_of"].iter().any(|k| {
        spec[*k].as_array().is_some_and(|a| !a.is_empty()) || spec[*k].as_object().is_some_and(|o| !o.is_empty())
    });
    if !needs_json {
        return Ok(());
    }
    let v: Value = serde_json::from_slice(body)
        .map_err(|_| format!("the answer is not JSON: {}", String::from_utf8_lossy(body)))?;
    let data = if v.get("type").is_some() && v.get("data").is_some() {
        &v["data"]
    } else {
        &v
    };
    for (list, non_null) in [("required", false), ("non_null", true)] {
        for p in spec[list].as_array().into_iter().flatten().filter_map(Value::as_str) {
            check_path(data, &pointer_segs(p), non_null).map_err(|e| format!("{p}: {e} in {data}"))?;
        }
    }
    for p in spec["equals_request"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let (got, want) = (data.pointer(p), sent.and_then(|s| s.pointer(p)));
        if got != want {
            return Err(format!("{p} should echo the request's {want:?}, got {got:?}"));
        }
    }
    for (p, allowed) in spec["one_of"].as_object().into_iter().flatten() {
        let got = data.pointer(p).cloned().unwrap_or(Value::Null);
        if !allowed.as_array().is_some_and(|a| a.contains(&got)) {
            return Err(format!("{p} is {got}, not one of {allowed}"));
        }
    }
    Ok(())
}

/// Whether a fixture's path is one 4.0 still serves as before: the device wire installed apps
/// speak, and the version and contract routes.
fn still_served(path: &str) -> bool {
    let p = path.split('?').next().unwrap_or(path);
    p == "/api/version"
        || p == "/api/v1/contracts"
        || p == "/api/v1/device"
        || p.starts_with("/api/v1/device/")
        || p == "/api/v1/enrollments"
        || p.starts_with("/api/v1/enrollments/")
}

/// A fixture of a retired route: it gets the 4.0 answer. An API v1 account route answers `410
/// api_version_sunset` with the update command (or, while it still selects a test environment,
/// `testing_secret_invalid`); Honeycomb's test-environment instructions are gone (404).
async fn replay_retired(p: &mut Provider<'_>, f: &Value) -> Result<(), String> {
    p.fill_retired();
    let req = &f["request"];
    let method =
        reqwest::Method::from_bytes(req["method"].as_str().unwrap_or("GET").as_bytes()).map_err(|e| e.to_string())?;
    let path = p.fill(req["path"].as_str().unwrap_or_default())?;
    let mut r = p.svc.http.request(method.clone(), format!("{}{path}", p.svc.base));
    let mut testing = false;
    for (k, v) in req["headers"].as_object().into_iter().flatten() {
        testing |= k.eq_ignore_ascii_case(extend_protocol::TESTING_SECRET_HEADER);
        r = r.header(k.as_str(), p.fill(v.as_str().unwrap_or_default())?);
    }
    if let Some(b64) = req["body_base64"].as_str() {
        r = r.body(
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| e.to_string())?,
        );
    } else if !req["body"].is_null() {
        r = r.body(serde_json::to_vec(&p.fill_json(&req["body"])?).unwrap());
    }
    let resp = r.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    let e = &body["data"];
    if path.starts_with("/internal/") {
        return if status == 404 {
            Ok(())
        } else {
            Err(format!("{method} {path} should be gone (404), got {status}: {body}"))
        };
    }
    if testing {
        return if status == 401 && e["code"] == "testing_secret_invalid" {
            Ok(())
        } else {
            Err(format!(
                "{method} {path} selects a test environment and should be refused, got {status}: {body}"
            ))
        };
    }
    let retired = path.split('?').next().unwrap_or(&path);
    if status != 410
        || e["code"] != "api_version_sunset"
        || e["hint"] != "silicon-apps update extend"
        || e["details"]["retired"] != retired
        || e["details"]["use_api_version"] != 2
    {
        return Err(format!(
            "{method} {path} should answer 410 api_version_sunset with `silicon-apps update extend`, got {status}: {body}"
        ));
    }
    Ok(())
}

async fn replay_http(p: &mut Provider<'_>, f: &Value) -> Result<(), String> {
    if !still_served(f["request"]["path"].as_str().unwrap_or_default()) {
        return replay_retired(p, f).await;
    }
    for g in f["given"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        p.given(g).await;
    }
    let req = &f["request"];
    let method =
        reqwest::Method::from_bytes(req["method"].as_str().unwrap_or("GET").as_bytes()).map_err(|e| e.to_string())?;
    let path = p.fill(req["path"].as_str().unwrap_or_default())?;
    let mut r = p.svc.http.request(method.clone(), format!("{}{path}", p.svc.base));
    for (k, v) in req["headers"].as_object().into_iter().flatten() {
        r = r.header(k.as_str(), p.fill(v.as_str().unwrap_or_default())?);
    }
    let mut sent = None;
    if let Some(b64) = req["body_base64"].as_str() {
        r = r.body(
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| e.to_string())?,
        );
    } else if !req["body"].is_null() {
        let body = p.fill_json(&req["body"])?;
        r = r.body(serde_json::to_vec(&body).unwrap());
        sent = Some(body);
    }
    let resp = r.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "{method} {path} was refused with {status}: {}",
            String::from_utf8_lossy(&bytes)
        ));
    }
    check_response(&f["response"], &bytes, sent.as_ref()).map_err(|e| format!("{method} {path} answered, but {e}"))?;
    match f["effect"].as_str() {
        None => Ok(()),
        Some("device_enrollment") => device_enrollment_effect(p, &bytes).await,
        Some(other) => Err(format!("unknown effect {other:?}")),
    }
}

/// "Pair with another Carbon": c:bob claims the code the answer gave, and the new pair is a second
/// pair of the same physical device, with its own id.
async fn device_enrollment_effect(p: &mut Provider<'_>, bytes: &[u8]) -> Result<(), String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let data = &v["data"];
    let base = p.svc.base.clone();
    let claimed = V2::new(&base, &p.vars["other_carbon_token"])
        .pair(&PairingClaim {
            pairing_code: data["pairing_code"].as_str().unwrap_or_default().into(),
            name: "Contract, bob's".into(),
            visibility: None,
            pair_ttl_days: None,
            silicon_ids: vec![],
        })
        .await
        .map_err(|e| format!("c:bob claiming the code: {e}"))?;
    let id: Uuid = data["enrollment_id"]
        .as_str()
        .unwrap_or_default()
        .parse()
        .map_err(|e| format!("{e}"))?;
    let bob_credential = match p
        .client
        .enrollment(id, data["enrollment_secret"].as_str().unwrap_or_default())
        .await
        .map_err(|e| e.to_string())?
    {
        EnrollmentState::Paired { device_credential, .. } => device_credential,
        other => return Err(format!("the enrollment isn't paired after the claim: {other:?}")),
    };
    let instance = |cred: String| {
        let base = base.clone();
        async move {
            let (status, me) = call(
                &base,
                reqwest::Method::GET,
                "/api/v1/device",
                &format!("Extend-Device {cred}"),
                None,
            )
            .await;
            (
                status,
                me["data"]["instance_id"].clone(),
                me["data"]["device_id"].clone(),
            )
        }
    };
    let (s1, i1, d1) = instance(p.var("device_credential")).await;
    let (s2, i2, d2) = instance(bob_credential).await;
    if s1 != 200 || s2 != 200 || i1.is_null() || i1 != i2 {
        return Err(format!("the two pairs should be one device: {s1} {i1} / {s2} {i2}"));
    }
    if d1 == d2 || d2 != json!(claimed.device_id.to_string()) {
        return Err(format!("the second pair should have its own id: {d1} / {d2}"));
    }
    let alice = p
        .carbon()
        .device(&p.var("device_id"))
        .await
        .map_err(|e| e.to_string())?;
    if alice.paired_by_others != Some(true) {
        return Err("c:alice's view should say paired_by_others".into());
    }
    Ok(())
}

/// The frames the service sent, checked against what the consumer reads from each type.
fn check_reads(reads: &Value, seen: &[Value]) -> Result<(), String> {
    for frame in seen {
        let kind = frame["type"].as_str().unwrap_or_default();
        for key in reads[kind].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if frame.get(key).is_none_or(Value::is_null) {
                return Err(format!(
                    "the service's {kind:?} frame lacks {key:?}, which the app reads: {frame}"
                ));
            }
        }
    }
    Ok(())
}

async fn replay_enrollment_socket(p: &mut Provider<'_>, f: &Value) -> Result<(), String> {
    p.given("enrollment").await;
    let req = &f["request"];
    let headers: Vec<(String, String)> = req["headers"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| Ok((k.clone(), p.fill(v.as_str().unwrap_or_default())?)))
        .collect::<Result<_, String>>()?;
    let url = p.client.ws_url(&p.fill(req["path"].as_str().unwrap_or_default())?);
    let mut ws = ws_connect(&url, &headers).await;
    let mut seen = vec![next_json(&mut ws).await];
    if seen[0]["type"] != "code" {
        return Err(format!("the first enrollment frame should be code, got {}", seen[0]));
    }
    p.carbon()
        .pair(&PairingClaim {
            pairing_code: p.var("pairing_code"),
            name: "Contract".into(),
            visibility: None,
            pair_ttl_days: None,
            silicon_ids: vec![],
        })
        .await
        .map_err(|e| e.to_string())?;
    loop {
        let frame = next_json(&mut ws).await;
        let done = frame["type"] == "paired";
        seen.push(frame);
        if done {
            break;
        }
    }
    check_reads(&f["reads"], &seen)
}

struct Socket {
    ws: Ws,
    seen: Vec<Value>,
}

impl Socket {
    /// Reads frames (answering pings) until one of `kind` arrives.
    async fn until(&mut self, kind: &str) -> Value {
        loop {
            let f = next_json(&mut self.ws).await;
            self.seen.push(f.clone());
            if f["type"] == "ping" {
                send_json(&mut self.ws, &json!({"type": "pong", "nonce": f["nonce"]})).await;
            }
            if f["type"] == kind {
                return f;
            }
        }
    }
}

async fn eventually<F, Fut>(what: &str, mut check: F) -> Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..60 {
        if check().await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!("{what} never happened"))
}

/// The service may prepend its own carried-device recognition step; every step the app
/// reported must nevertheless be visible, including changed errors/statuses within one state.
async fn reported_setup_is_visible(owner: &V2<'_>, id: &str, frame: &Value) -> bool {
    let expected: Setup = serde_json::from_value(frame["setup"].clone()).expect("reported setup");
    owner.setup(id).await.is_ok_and(|actual| {
        actual.state == expected.state && expected.steps.iter().all(|step| actual.steps.contains(step))
    })
}

/// Online can become true before hello is stored; an offline attachment is already offline
/// before its first report. Neither is a barrier for the setup HTTP requests that follow.
async fn wait_for_device_report(owner: &V2<'_>, id: &str, frame: &Value) -> Result<(), String> {
    eventually("the complete device report becoming visible", move || async move {
        let Ok(device) = owner.device(id).await else {
            return false;
        };
        let online = frame["online"].as_bool().unwrap_or(true);
        if device.online != online
            || frame["awake"].as_bool().is_some_and(|v| device.awake != Some(v))
            || frame["model"]
                .as_str()
                .is_some_and(|v| device.model.as_deref() != Some(v))
            || frame["os_version"]
                .as_str()
                .is_some_and(|v| device.os_version.as_deref() != Some(v))
            || frame["app_version"]
                .as_str()
                .is_some_and(|v| device.app_version.as_deref() != Some(v))
            || frame["engine_version"]
                .as_str()
                .is_some_and(|v| device.engine_version.as_deref() != Some(v))
        {
            return false;
        }
        if frame["type"] == "hello" {
            let setup: Setup = serde_json::from_value(frame["setup"].clone()).expect("hello setup");
            let expected = if setup.state == SetupState::Complete || setup.steps.is_empty() {
                DeviceState::Ready
            } else {
                DeviceState::Setup
            };
            if device.state != expected {
                return false;
            }
        }
        reported_setup_is_visible(owner, id, frame).await
    })
    .await
}

#[tokio::test]
async fn a_provider_waits_for_hello_persistence_before_starting_a_session() {
    let svc = Svc::start(default_policy(), real_clock(), "1.0.0").await;
    let mut p = Provider::new(&svc).await;
    let d = pair(
        &p.client,
        &svc.base,
        &p.var("carbon_token"),
        DeviceOs::Android,
        &["si:chef"],
    )
    .await;
    // Force persistence to take longer than the old 150 ms sleep. WebSocket connection and
    // writes still succeed, and ordinary API reads still see the previous committed row.
    let mut lock = svc.pool.begin().await.unwrap();
    sqlx::query("SELECT device_id FROM extend.devices WHERE device_id = $1 FOR UPDATE")
        .bind(&d.id)
        .fetch_one(&mut *lock)
        .await
        .unwrap();
    let owner = p.carbon();
    let (task, _inject) = {
        let serving = d.serve_with(&svc.base, &p.client, &owner, hello(DeviceOs::Android));
        tokio::pin!(serving);
        assert!(
            tokio::time::timeout(Duration::from_millis(350), serving.as_mut())
                .await
                .is_err(),
            "the provider returned before the device's hello could be stored"
        );
        assert_eq!(p.carbon().device(&d.id).await.unwrap().state, DeviceState::Setup);
        lock.rollback().await.unwrap();
        serving.await
    };
    drop(owner);
    p.tasks.push(task);
    assert_eq!(p.carbon().device(&d.id).await.unwrap().state, DeviceState::Ready);
    let session = p.silicon().start_session(&d.id.parse().unwrap()).await.unwrap();
    p.silicon().end_session(session.session_id.as_ref()).await.unwrap();
}

#[tokio::test]
async fn an_offline_attachment_waits_for_reported_steps_within_the_same_setup_state() {
    let svc = Svc::start(default_policy(), real_clock(), "1.0.0").await;
    let mut p = Provider::new(&svc).await;
    p.given("attached").await;
    let id = p.var("attached_id");
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../contracts/v1/device/agent.device.socket.setup_retry.json"
    ))
    .unwrap();
    // The first report is offline, as the new attachment already is. The second changes its
    // steps but stays needs_carbon and offline, so neither broad status is an acknowledgement.
    for step in [&fixture["sends"][1], &fixture["sends"][2]] {
        let mut frame = p.fill_json(&step["frame"]).unwrap();
        frame["online"] = json!(false);
        let mut lock = svc.pool.begin().await.unwrap();
        sqlx::query("SELECT device_id FROM extend.devices WHERE device_id = $1 FOR UPDATE")
            .bind(&id)
            .fetch_one(&mut *lock)
            .await
            .unwrap();
        p.inject[&p.var("host_id")].send(frame.clone()).unwrap();
        let owner = p.carbon();
        let reported = wait_for_device_report(&owner, &id, &frame);
        tokio::pin!(reported);
        assert!(
            tokio::time::timeout(Duration::from_millis(350), reported.as_mut())
                .await
                .is_err(),
            "the report wait accepted old offline/setup state before its steps were stored"
        );
        lock.rollback().await.unwrap();
        reported.await.unwrap();
        assert!(reported_setup_is_visible(&owner, &id, &frame).await);
    }
}

async fn replay_device_socket(p: &mut Provider<'_>, f: &Value) -> Result<(), String> {
    let os: DeviceOs = serde_json::from_value(f["os"].clone()).map_err(|e| format!("os: {e}"))?;
    let base = p.svc.base.clone();
    let d = pair(&p.client, &base, &p.var("carbon_token"), os, &["si:chef"]).await;
    p.vars.insert("device_id".into(), d.id.clone());
    p.vars.insert("device_credential".into(), d.credential.clone());
    let req = &f["request"];
    let headers: Vec<(String, String)> = req["headers"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| Ok((k.clone(), p.fill(v.as_str().unwrap_or_default())?)))
        .collect::<Result<_, String>>()?;
    let url = p.client.ws_url(&p.fill(req["path"].as_str().unwrap_or_default())?);
    let mut sock = Socket {
        ws: ws_connect(&url, &headers).await,
        seen: vec![],
    };
    let mut session: Option<String> = None;
    let mut retry_target: Option<String> = None;
    for step in f["sends"].as_array().into_iter().flatten() {
        let effect = step["effect"].as_str().unwrap_or("none");
        let raw = &step["frame"];
        // Frames that need ids from the service get them first.
        match effect {
            "result" | "takeover_done" | "stop" if session.is_none() => {
                let s = p
                    .silicon()
                    .start_session(&d.id.parse().unwrap())
                    .await
                    .map_err(|e| format!("starting a session for {effect}: {e}"))?;
                sock.until("session_started").await;
                session = Some(s.session_id.to_string());
            }
            _ => {}
        }
        let mut pending_run = None;
        match effect {
            "result" => {
                let (run_base, token, sid) = (base.clone(), p.var("silicon_token"), session.clone().unwrap());
                pending_run = Some(tokio::spawn(async move {
                    V2::new(&run_base, &token)
                        .run(
                            &sid,
                            &CommandRequest {
                                command: "screenshot".into(),
                                args: vec![],
                                timeout_ms: None,
                                self_destruct_minutes: None,
                                permanent: false,
                                attachments: vec![],
                            },
                        )
                        .await
                }));
                let cmd = sock.until("command").await;
                p.vars
                    .insert("command_id".into(), cmd["id"].as_str().unwrap_or_default().into());
                let upload_id = cmd["upload_ids"][0].as_str().unwrap_or_default().to_owned();
                p.vars.insert("upload_id".into(), upload_id.clone());
                if raw["files"].as_array().is_some_and(|a| !a.is_empty()) {
                    upload(&base, &d.credential, &upload_id, ARTIFACT).await;
                }
            }
            "takeover_done" => {
                p.silicon()
                    .takeover(session.as_ref().unwrap(), "Please approve Face ID")
                    .await
                    .map_err(|e| format!("starting a takeover: {e}"))?;
                sock.until("takeover").await;
            }
            "attached" => {
                let a = p
                    .carbon()
                    .attach(
                        &d.id,
                        &AttachmentCreate {
                            os: DeviceOs::Tvos,
                            name: "Living room".into(),
                            visibility: None,
                            pair_ttl_days: None,
                            address: None,
                        },
                    )
                    .await
                    .map_err(|e| format!("attaching a device through the host: {e}"))?;
                sock.until("attach").await;
                p.vars.insert("attached_id".into(), a.device_id.to_string());
            }
            "wake_request_shown" => {
                // si:chef asks to wake the device, and the device gets the request.
                let (status, w) = call(
                    &base,
                    reqwest::Method::POST,
                    &format!("/api/v2/devices/{}/wake-requests", d.id),
                    &format!("Bearer {}", p.var("silicon_token")),
                    Some(json!({"type": "wake_request", "data": {"reason": "Contract: the order screen"}})),
                )
                .await;
                if status != 201 {
                    return Err(format!("asking to wake the device answered {status}: {w}"));
                }
                let frame = sock.until("wake_request").await;
                p.vars
                    .insert("wake_id".into(), frame["wake_id"].as_str().unwrap_or_default().into());
            }
            "credential_saved" => {
                // A computer two Carbons paired: c:bob pairs it too, with a 1.1 app connected.
                let bob = pair_another(
                    &p.client,
                    &base,
                    &d.credential,
                    &p.var("other_carbon_token"),
                    "Contract Mac, bob's",
                )
                .await?;
                let bob_v2 = V2::new(&base, &p.vars["other_carbon_token"]);
                let (task, _) = bob
                    .serve_with(&base, &p.client, &bob_v2, hello_as(os, "1.1.0", Setup::complete()))
                    .await;
                p.tasks.push(task);
                // A session through the fixture's pair ends, and each pair gets a new credential.
                let s = p
                    .silicon()
                    .start_session(&d.id.parse().unwrap())
                    .await
                    .map_err(|e| format!("starting a session for the rotation: {e}"))?;
                sock.until("session_started").await;
                p.silicon()
                    .end_session(s.session_id.as_ref())
                    .await
                    .map_err(|e| format!("ending the session: {e}"))?;
                let cred = sock.until("credential").await;
                p.vars.insert(
                    "new_credential".into(),
                    cred["device_credential"].as_str().unwrap_or_default().into(),
                );
            }
            "setup_retry" => {
                // c:alice retries the first failed step of the device (or of the carried device the
                // frame names), and the device is told.
                let target = if raw["type"] == "attached" {
                    p.var("attached_id")
                } else {
                    d.id.clone()
                };
                let setup = p
                    .carbon()
                    .setup(&target)
                    .await
                    .map_err(|e| format!("reading the setup: {e}"))?;
                let key = setup
                    .failed()
                    .next()
                    .map(|s| s.key.clone())
                    .ok_or_else(|| format!("{target} has no failed step to retry"))?;
                let (status, r) = call(
                    &base,
                    reqwest::Method::POST,
                    &format!("/api/v2/devices/{target}/setup/retry"),
                    &format!("Bearer {}", p.var("carbon_token")),
                    Some(json!({"step": key})),
                )
                .await;
                if status != 202 || r["data"]["retrying"] != json!([key]) {
                    return Err(format!("the setup retry answered {status}: {r}"));
                }
                let frame = sock.until("setup_retry").await;
                if frame["step"] != json!(key) {
                    return Err(format!("the setup_retry frame names another step: {frame}"));
                }
                retry_target = Some(target);
            }
            "stop_target" => {
                let attached = p.var("attached_id");
                p.carbon()
                    .grant(&attached, "si:chef")
                    .await
                    .map_err(|e| format!("granting access to the attached device: {e}"))?;
                let s = p
                    .silicon()
                    .start_session(&attached.parse().unwrap())
                    .await
                    .map_err(|e| format!("starting a session on the attached device: {e}"))?;
                sock.until("session_started").await;
                p.vars.insert("target_session".into(), s.session_id.to_string());
            }
            _ => {}
        }
        let frame = p.fill_json(raw)?;
        serde_json::from_value::<DeviceFrame>(frame.clone())
            .map_err(|e| format!("the service can no longer read this {effect} frame ({e}): {frame}"))?;
        send_json(&mut sock.ws, &frame).await;
        let owner = &p.carbon();
        let silicon = &p.silicon();
        let device_id = d.id.as_str();
        let frame = &frame;
        match effect {
            "hello" => wait_for_device_report(owner, device_id, frame).await?,
            "awake" => {
                let want = frame["awake"].as_bool();
                let sleep = frame["sleep_state"].as_str().map(str::to_owned);
                let sleep = &sleep;
                eventually("the device reading awake as reported", move || async move {
                    owner.device(device_id).await.is_ok_and(|x| {
                        x.awake == want
                            && sleep
                                .as_deref()
                                .is_none_or(|s| x.sleep_state.map(|v| v.as_str()) == Some(s))
                    })
                })
                .await?;
                if want == Some(true)
                    && frame["input_seen"] != json!(false)
                    && let Some(wake_id) = p.vars.get("wake_id").cloned()
                {
                    let ended = sock.until("wake_request_ended").await;
                    if ended["wake_id"] != json!(wake_id) {
                        return Err(format!("expected wake request {wake_id} to end, got {ended}"));
                    }
                    let base = p.svc.base.clone();
                    let token = p.var("carbon_token");
                    let wid = wake_id.as_str();
                    let (base, token) = (&base, &token);
                    eventually("the wake request reading woken", move || async move {
                        let (_, list) = call(
                            base,
                            reqwest::Method::GET,
                            &format!("/api/v2/devices/{device_id}/wake-requests?state=all"),
                            &format!("Bearer {token}"),
                            None,
                        )
                        .await;
                        list["data"]["items"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|w| w["wake_id"] == wid && w["state"] == "woken"))
                    })
                    .await?;
                }
            }
            "wake_request_shown" => {
                let base = p.svc.base.clone();
                let token = p.var("carbon_token");
                let wid = p.var("wake_id");
                let want = if frame["shown"] == json!(true) {
                    "shown"
                } else {
                    "not_shown"
                };
                let note = frame["note"].clone();
                let (base, token, wid, note) = (&base, &token, &wid, &note);
                eventually("the wake request's device_notice", move || async move {
                    let (_, list) = call(
                        base,
                        reqwest::Method::GET,
                        &format!("/api/v2/devices/{device_id}/wake-requests?state=all"),
                        &format!("Bearer {token}"),
                        None,
                    )
                    .await;
                    list["data"]["items"].as_array().is_some_and(|a| {
                        a.iter().any(|w| {
                            w["wake_id"] == json!(wid)
                                && w["device_notice"] == want
                                && (note.is_null() || w["device_notice_note"] == *note)
                        })
                    })
                })
                .await?
            }
            "credential_saved" => {
                let base = p.svc.base.clone();
                let new = p.var("new_credential");
                let old = d.credential.clone();
                let (base, new) = (&base, &new);
                eventually("the new credential working", move || async move {
                    call(
                        base,
                        reqwest::Method::GET,
                        "/api/v1/device",
                        &format!("Extend-Device {new}"),
                        None,
                    )
                    .await
                    .0 == 200
                })
                .await?;
                let (status, _) = call(
                    base,
                    reqwest::Method::GET,
                    "/api/v1/device",
                    &format!("Extend-Device {old}"),
                    None,
                )
                .await;
                if status != 401 {
                    return Err(format!(
                        "the old credential still answers {status} after credential_saved"
                    ));
                }
            }
            "setup_retry" => {
                let target = retry_target.clone().unwrap_or_else(|| device_id.to_owned());
                let target = target.as_str();
                // Two retry reports can both be needs_carbon while different steps failed or
                // finished. The state alone does not show that this frame was processed.
                eventually("the setup reported after the retry", move || {
                    reported_setup_is_visible(owner, target, frame)
                })
                .await?
            }
            "setup_progress" => {
                eventually("the reported setup progress", move || {
                    reported_setup_is_visible(owner, device_id, frame)
                })
                .await?
            }
            "result" => {
                let r = pending_run
                    .take()
                    .unwrap()
                    .await
                    .unwrap()
                    .map_err(|e| format!("the command the result answered failed: {e}"))?;
                if Some(r.ok) != frame["ok"].as_bool()
                    || r.text.as_deref() != frame["text"].as_str()
                    || r.files.len() != frame["files"].as_array().map_or(0, Vec::len)
                {
                    return Err(format!(
                        "the result frame was not relayed as sent: {frame} became {r:?}"
                    ));
                }
            }
            "takeover_done" => {
                let sid = session.clone().unwrap();
                let sid = sid.as_str();
                eventually("the takeover ending", move || async move {
                    silicon.takeover_status(sid).await.is_ok_and(|t| t.is_none())
                })
                .await?
            }
            "stop" => {
                let sid = session.take().unwrap();
                let sid = sid.as_str();
                eventually("the session ending with stopped_by_carbon", move || async move {
                    silicon
                        .session(sid)
                        .await
                        .is_ok_and(|s| s.end_reason == Some(EndReason::StoppedByCarbon))
                })
                .await?
            }
            "attached" => {
                wait_for_device_report(owner, &p.var("attached_id"), frame).await?;
            }
            "stop_target" => {
                let sid = p.var("target_session");
                let sid = sid.as_str();
                eventually("the attached device's session ending", move || async move {
                    silicon
                        .session(sid)
                        .await
                        .is_ok_and(|s| s.end_reason == Some(EndReason::StoppedByCarbon))
                })
                .await?
            }
            "none" => {}
            other => return Err(format!("unknown effect {other:?}")),
        }
    }
    check_reads(&f["reads"], &sock.seen)
}

async fn replay_dir(svc: &Svc, dir: &Path, problems: &mut Vec<String>) -> usize {
    let mut fixtures = load(dir);
    fixtures.sort_by_key(|(name, f)| (f["sequence"].as_u64().unwrap_or(0), name.clone()));
    // A sequence (Honeycomb's lifecycle) shares one provider; other fixtures get fresh state each.
    let sequenced = fixtures.iter().any(|(_, f)| f["sequence"].is_u64());
    let mut shared = if sequenced {
        Some(Provider::new(svc).await)
    } else {
        None
    };
    for (name, f) in &fixtures {
        let mut fresh = None;
        let p = match shared.as_mut() {
            Some(p) => p,
            None => fresh.insert(Provider::new(svc).await),
        };
        let result = match f["kind"].as_str().unwrap_or("http") {
            "device_socket" => replay_device_socket(p, f).await,
            "enrollment_socket" => replay_enrollment_socket(p, f).await,
            _ => replay_http(p, f).await,
        };
        if let Err(e) = result {
            problems.push(format!(
                "{}/{name} ({} {}): {e}",
                dir.display(),
                f["consumer"].as_str().unwrap_or("?"),
                f["operation"].as_str().unwrap_or("?")
            ));
        }
    }
    fixtures.len()
}

async fn replay(kind: &str) {
    let svc = Svc::start(default_policy(), real_clock(), "1.0.0").await;
    let mut problems = Vec::new();
    let mut count = 0;
    // Every major the service still serves (not sunset) is replayed with its own fixtures.
    for major in svc.versions.negotiable() {
        count += replay_dir(&svc, &contracts_dir().join(format!("v{major}/{kind}")), &mut problems).await;
    }
    assert!(
        count > 0,
        "no {kind} fixtures to replay under {}",
        contracts_dir().display()
    );
    assert!(
        problems.is_empty(),
        "The service no longer accepts {} of {count} {kind} contract fixtures. A published consumer would break:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

/// A published 1.x–3.x client against 4.0: its device-wire requests still work as before; every
/// account route answers 410 with `silicon-apps update extend`.
#[tokio::test]
async fn client_fixtures_replay_or_are_told_to_update() {
    replay("client").await;
}

/// The released 1.0.0 client (fixtures frozen before the live ones were regenerated for 1.1).
#[tokio::test]
async fn client_1_0_0_fixtures_replay_or_are_told_to_update() {
    replay("client-1.0.0").await;
}

#[tokio::test]
async fn client_1_1_0_fixtures_replay_or_are_told_to_update() {
    replay("client-1.1.0").await;
}

#[tokio::test]
async fn client_1_2_0_fixtures_replay_or_are_told_to_update() {
    replay("client-1.2.0").await;
}

/// The retired selection operation is archived separately; ordinary released requests are replayed.
#[tokio::test]
async fn client_1_3_0_ordinary_fixtures_replay_or_are_told_to_update() {
    replay("client-1.3.0").await;
}

/// The released 3.1.1 client, frozen before 4.0 regenerated the live fixtures.
#[tokio::test]
async fn client_3_1_1_fixtures_replay_or_are_told_to_update() {
    replay("client-3.1.1").await;
}

/// Installed device apps: the device wire is unchanged in 4.0, so every fixture still replays.
#[tokio::test]
async fn device_app_fixtures_still_replay() {
    replay("device").await;
}

/// Honeycomb's test-environment lifecycle instructions went with test environments.
#[tokio::test]
async fn honeycomb_lifecycle_instructions_are_gone() {
    let svc = Svc::start(default_policy(), real_clock(), "1.0.0").await;
    let mut problems = Vec::new();
    let n = replay_dir(&svc, &contracts_dir().join("retired/honeycomb"), &mut problems).await;
    assert!(n > 0, "no Honeycomb fixtures");
    assert!(
        problems.is_empty(),
        "Honeycomb's retired lifecycle instructions are still answered:\n{}",
        problems.join("\n")
    );
}

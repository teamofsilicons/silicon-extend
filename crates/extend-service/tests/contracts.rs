//! API versioning and consumer-driven contract tests (UNDERSTANDING.md "Versioning" 3–6;
//! TECHNICAL.md section 10), against a real PostgreSQL and the real HTTP and WebSocket stack.
//!
//! - Lifecycle: a deprecated major carries `Deprecation` and `Sunset` headers, is sunset after 7
//!   consecutive days without a request (an injected clock and usage rows stand in for the week),
//!   and then answers `410 api_version_sunset`; negotiation steers clients around it; two majors
//!   are served side by side, each path checking its own pin.
//! - The compatibility matrix (`GET /api/v1/contracts`) is built from that state and configuration.
//! - Replay: every fixture under `contracts/` that a consumer published (the client crate records
//!   its own; the device apps' and Honeycomb's are derived from their code and
//!   docs/device-protocol.md) is sent to a real service for every API major it still serves. Each
//!   must still be accepted, and each answer must still carry every field that consumer reads.
//!   `contracts/README.md` describes the fixture format and the provider states named in `given`.
//!
//! Needs a PostgreSQL the tests can create databases on:
//! `EXTEND_TEST_ADMIN_URL` (default `postgres://extend:extend@127.0.0.1:5440/postgres`).

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use extend_protocol::frames::{DeviceFrame, EnrollmentFrame, ServiceFrame};
use extend_protocol::model::*;
use extend_protocol::{DeviceOs, ErrorCode};
use extend_service::config::{Config, Environment, FilesMode, IamMode, TingMode};
use extend_service::versions::{self, Clock, Lifecycle, Policy, Registry};
use futures::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use silicon_extend_client::Client;
use sqlx::Connection as _;
use time::OffsetDateTime;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const HONEYCOMB_TOKEN: &str = "hck_contracts";

// ───────────── Harness ─────────────

async fn database() -> String {
    let admin = std::env::var("EXTEND_TEST_ADMIN_URL")
        .unwrap_or_else(|_| "postgres://extend:extend@127.0.0.1:5440/postgres".into());
    let db = format!("extend_contracts_{}", Uuid::new_v4().simple());
    let mut conn = sqlx::PgConnection::connect(&admin)
        .await
        .expect("PostgreSQL for tests (set EXTEND_TEST_ADMIN_URL)");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db}")))
        .execute(&mut conn)
        .await
        .unwrap();
    format!("{}/{db}", admin.rsplit_once('/').unwrap().0)
}

fn config(database_url: String, addr: SocketAddr, device_app_min: &str) -> Config {
    let base = format!("http://{addr}");
    Config {
        environment: Environment::Test,
        bind: addr,
        database_url,
        public_url: base.clone(),
        website_url: "http://localhost:5173".into(),
        docs_url: "http://localhost:5173/docs".into(),
        repository_url: "https://github.com/teamofsilicons/silicon-extend".into(),
        data_dir: std::env::temp_dir().join(format!("extend_contracts_{}", Uuid::new_v4().simple())),
        iam: IamMode::Local,
        iam_public_url: format!("{base}/dev/iam"),
        iam_login_url: format!("{base}/dev/iam/login"),
        webhook_secret: None,
        webhook_previous_secret: None,
        files: FilesMode::Local,
        ting: TingMode::Local,
        honeycomb_service_token: Some(HONEYCOMB_TOKEN.into()),
        postmark_token: None,
        report_recipients: vec!["bugs@example.test".into()],
        device_app_min_version: device_app_min.into(),
        local_members: vec![
            ("c:alice".into(), vec!["acme".into()]),
            ("si:chef".into(), vec!["acme".into()]),
            ("si:sous".into(), vec!["acme".into()]),
        ],
        web_dir: None,
        trusted_proxies: vec![],
    }
}

struct Svc {
    base: String,
    pool: sqlx::PgPool,
    versions: Arc<Registry>,
    http: reqwest::Client,
}

impl Svc {
    async fn start(policy: Policy, clock: Clock, device_app_min: &str) -> Svc {
        let url = database().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = extend_service::build(config(url, addr, device_app_min)).await.unwrap();
        let pool = state.pool.clone();
        let versions = Registry::start_with_clock(pool.clone(), policy, clock).await.unwrap();
        tokio::spawn(extend_service::serve_versioned(listener, state, versions.clone()));
        Svc {
            base: format!("http://{addr}"),
            pool,
            versions,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        }
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
async fn the_matrix_is_built_from_state_and_configuration() {
    let policy = policy(versions::SERVED, &[("EXTEND_API_V1_CLI", ">=1.2.0, <2.0.0")]);
    let svc = Svc::start(policy, real_clock(), "1.4.0").await;
    let client = Client::connect(&svc.base).await.unwrap();

    let m = client.contracts().await.unwrap();
    assert_eq!(m["supported"], json!([1]));
    assert_eq!(m["current"], 1);
    assert_eq!(m["deprecated"], json!([]));
    assert_eq!(m["service_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(m["sunset_rule"], versions::SUNSET_RULE);
    let versions_list = m["versions"].as_array().unwrap();
    assert_eq!(versions_list.len(), 1, "{m}");
    let v1 = &versions_list[0];
    assert_eq!(v1["api_version"], 1);
    assert_eq!(v1["state"], "current");
    assert!(v1["deprecated_at"].is_null() && v1["sunset_at"].is_null() && v1["sunset_earliest_at"].is_null());
    assert_eq!(
        v1["compatible"],
        json!({"client_crate": ">=1.0.0, <2.0.0", "cli": ">=1.2.0, <2.0.0", "device_app_min": "1.4.0"})
    );

    // The app minimum the matrix states is the one enrollment enforces.
    let enroll = |v: &str| EnrollmentCreate {
        os: DeviceOs::Android,
        os_version: None,
        model: None,
        app_version: v.into(),
        agent_device_version: None,
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
    let r = svc.get("/api/v1/iam", Some("1")).await;
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
    let r = svc.get("/api/v1/iam", Some("1")).await;
    assert_eq!(r.status(), 200);
    assert_eq!(header(&r, "silicon-extend-api-version").as_deref(), Some("1"));
    assert_eq!(
        header(&r, "deprecation"),
        Some(format!("@{}", deprecated_at.unix_timestamp()))
    );
    let earliest = tomorrow_midnight(start) + time::Duration::days(7);
    assert_eq!(header(&r, "sunset"), Some(versions::http_date(earliest)));
    // Errors on a deprecated major carry them too.
    let r = svc.get("/api/v1/auth/me", Some("1")).await;
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
    let (status, body) = envelope(svc.get("/api/v1/iam", Some("1")).await).await;
    assert_eq!(status, 410);
    let e = &body["data"];
    assert_eq!(e["code"], "api_version_sunset");
    let message = e["message"].as_str().unwrap();
    assert!(
        message.contains("API version 1 was retired") && message.contains("7 consecutive days without a request"),
        "{message}"
    );
    assert!(
        e["hint"].as_str().unwrap().contains("honeycomb install 'extend'"),
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

fn contracts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts")
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
    ("enrollment", &["enrollment_id", "enrollment_secret", "pairing_code"]),
    ("device", &["device_id", "device_credential", "device_version"]),
    ("session", &["session_id"]),
    ("takeover", &[]),
    ("file", &["file_id"]),
    ("upload", &["upload_id"]),
    ("host", &["host_id"]),
    ("attached", &["attached_id"]),
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
        ],
    ),
];

/// States a state sets up first.
fn implies(state: &str) -> &'static [&'static str] {
    match state {
        "session" | "upload" => &["device"],
        "takeover" | "file" => &["session"],
        "attached" => &["host"],
        _ => &[],
    }
}

/// Placeholders every fixture can use.
const ALWAYS: &[&str] = &[
    "carbon_token",
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
    let mut dirs = vec![(None, root.join("internal/honeycomb"))];
    for &major in versions::SERVED {
        assert!(
            !load(&root.join(format!("v{major}/client"))).is_empty(),
            "API version {major} is served but contracts/v{major}/client has no fixtures; record them with \
             `EXTEND_CONTRACTS_WRITE=1 cargo test -p silicon-extend-client --test contract_fixtures`"
        );
        dirs.push((Some(major), root.join(format!("v{major}/client"))));
        dirs.push((Some(major), root.join(format!("v{major}/device"))));
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
    serde_json::to_value(DeviceFrame::Hello(extend_protocol::frames::Hello {
        app_version: "1.0.0".into(),
        os,
        os_version: Some("15".into()),
        model: Some("Contract".into()),
        agent_device_version: None,
        capabilities: os.full_capabilities().to_vec(),
        missing: vec![],
        setup: Setup::complete(),
    }))
    .unwrap()
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
async fn pair(client: &Client, carbon_token: &str, os: DeviceOs, silicons: &[&str]) -> Device {
    let e = client
        .enroll(&EnrollmentCreate {
            os,
            os_version: Some("15".into()),
            model: Some("Contract".into()),
            app_version: "1.0.0".into(),
            agent_device_version: None,
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
    client
        .authed(carbon_token, Some("acme"))
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
    /// Connects, says hello, and answers pings and commands in the background.
    async fn serve(&self, base: &str, client: &Client, os: DeviceOs) -> tokio::task::JoinHandle<()> {
        let mut ws = ws_connect(
            &client.ws_url("/api/v1/device/connect"),
            &[("authorization".into(), format!("Extend-Device {}", self.credential))],
        )
        .await;
        send_json(&mut ws, &hello(os)).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        let base = base.to_owned();
        let credential = self.credential.clone();
        tokio::spawn(async move {
            while let Some(Ok(m)) = ws.next().await {
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
        })
    }
}

/// Sets up the provider states fixtures name, and fills their placeholders.
struct Provider<'a> {
    svc: &'a Svc,
    client: Client,
    vars: HashMap<String, String>,
    done: BTreeSet<String>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Provider<'_> {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl<'a> Provider<'a> {
    async fn new(svc: &'a Svc) -> Provider<'a> {
        let client = Client::connect(&svc.base).await.unwrap();
        let mut vars = HashMap::new();
        for (var, who) in [
            ("carbon_token", "c:alice"),
            ("silicon_token", "si:chef"),
            ("other_silicon_token", "si:sous"),
        ] {
            vars.insert(var.into(), client.login(who).await.unwrap().access_token);
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
        }
    }

    fn var(&self, k: &str) -> String {
        self.vars[k].clone()
    }

    fn carbon(&self) -> silicon_extend_client::Authed<'_> {
        self.client.authed(&self.vars["carbon_token"], Some("acme"))
    }

    fn silicon(&self) -> silicon_extend_client::Authed<'_> {
        self.client.authed(&self.vars["silicon_token"], Some("acme"))
    }

    async fn device_version(&self, id: &str) -> i64 {
        self.carbon().device(id).await.unwrap().version.unwrap()
    }

    fn given<'s>(&'s mut self, state: &'s str) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 's>> {
        Box::pin(async move {
            if !self.done.insert(state.to_owned()) {
                return;
            }
            let base = self.svc.base.clone();
            match state {
                "refresh_token" => {
                    let s = self.client.login("c:alice").await.unwrap();
                    self.vars.insert("refresh_token".into(), s.refresh_token);
                }
                "enrollment" => {
                    let e = self
                        .client
                        .enroll(&EnrollmentCreate {
                            os: DeviceOs::Android,
                            os_version: None,
                            model: None,
                            app_version: "1.0.0".into(),
                            agent_device_version: None,
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
                        &self.var("carbon_token"),
                        DeviceOs::Android,
                        &["si:chef", "si:sous"],
                    )
                    .await;
                    self.tasks.push(d.serve(&base, &self.client, DeviceOs::Android).await);
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
                    let d = pair(&self.client, &self.var("carbon_token"), DeviceOs::Macos, &["si:chef"]).await;
                    self.tasks.push(d.serve(&base, &self.client, DeviceOs::Macos).await);
                    self.vars.insert("host_id".into(), d.id);
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
                "test_environment" => {
                    let env = Uuid::new_v4();
                    let secret = extend_protocol::ids::new_secret("ask_");
                    let op = Uuid::new_v4();
                    let r = reqwest::Client::new()
                        .put(format!(
                            "{base}/internal/honeycomb/organizations/acme/testing-environments/{env}/operations/{op}"
                        ))
                        .bearer_auth(HONEYCOMB_TOKEN)
                        .json(&json!({
                            "operation_id": op, "environment_id": env, "org_id": "acme", "app_id": "extend",
                            "environment_revision": 1, "generation": 1, "key_version": 1, "action": "prepare",
                            "testing_key": "abcdefghijklmnopqrstuvwxyz012345", "name": "contracts"
                        }))
                        .send()
                        .await
                        .unwrap();
                    assert!(r.status().is_success(), "preparing a test environment: {}", r.status());
                    reqwest::Client::new()
                        .post(format!("{base}/dev/iam/test-apps"))
                        .json(&json!({"type": "test_app", "data": {"secret": secret, "environment_id": env}}))
                        .send()
                        .await
                        .unwrap();
                    self.vars.insert("testing_secret".into(), secret);
                }
                "honeycomb_environment" => {
                    self.vars.insert("environment_id".into(), Uuid::new_v4().to_string());
                    self.vars.insert("org_id".into(), "acme".into());
                    let key: String = Uuid::new_v4().simple().to_string();
                    self.vars.insert("testing_key".into(), key);
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

async fn replay_http(p: &mut Provider<'_>, f: &Value) -> Result<(), String> {
    for g in f["given"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        p.given(g).await;
    }
    if f["given"]
        .as_array()
        .is_some_and(|a| a.iter().any(|g| g == "honeycomb_environment"))
    {
        p.vars.insert("operation_id".into(), Uuid::new_v4().to_string());
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
    check_response(&f["response"], &bytes, sent.as_ref()).map_err(|e| format!("{method} {path} answered, but {e}"))
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

async fn replay_device_socket(p: &mut Provider<'_>, f: &Value) -> Result<(), String> {
    let os: DeviceOs = serde_json::from_value(f["os"].clone()).map_err(|e| format!("os: {e}"))?;
    let base = p.svc.base.clone();
    let d = pair(&p.client, &p.var("carbon_token"), os, &["si:chef"]).await;
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
                let (client, token, sid) = (p.client.clone(), p.var("silicon_token"), session.clone().unwrap());
                pending_run = Some(tokio::spawn(async move {
                    client
                        .authed(&token, Some("acme"))
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
        let owner = p.carbon();
        let silicon = p.silicon();
        let device_id = d.id.as_str();
        let frame = &frame;
        match effect {
            "hello" => {
                // Pairing used another model, so a match means the service stored this hello.
                eventually(
                    "the device showing online with what the hello says",
                    move || async move {
                        owner.device(device_id).await.is_ok_and(|x| {
                            x.online
                                && x.app_version.as_deref() == frame["app_version"].as_str()
                                && x.model.as_deref() == frame["model"].as_str()
                                && x.os_version.as_deref() == frame["os_version"].as_str()
                        })
                    },
                )
                .await?
            }
            "setup_progress" => {
                let want = &frame["setup"]["state"];
                eventually("the setup state changing", move || async move {
                    owner
                        .setup(device_id)
                        .await
                        .is_ok_and(|s| serde_json::to_value(s.state).ok().as_ref() == Some(want))
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
                let id = p.var("attached_id");
                let id = id.as_str();
                let online = frame["online"].as_bool();
                eventually("the attached device reporting its state", move || async move {
                    owner.device(id).await.is_ok_and(|x| Some(x.online) == online)
                })
                .await?
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

#[tokio::test]
async fn client_fixtures_still_replay() {
    replay("client").await;
}

#[tokio::test]
async fn device_app_fixtures_still_replay() {
    replay("device").await;
}

#[tokio::test]
async fn honeycomb_lifecycle_fixtures_still_replay() {
    let svc = Svc::start(default_policy(), real_clock(), "1.0.0").await;
    let mut problems = Vec::new();
    let n = replay_dir(&svc, &contracts_dir().join("internal/honeycomb"), &mut problems).await;
    assert!(n > 0, "no Honeycomb fixtures");
    assert!(
        problems.is_empty(),
        "Honeycomb's lifecycle instructions are no longer accepted as it sends them:\n{}",
        problems.join("\n")
    );
}

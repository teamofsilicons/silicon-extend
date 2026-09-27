//! Pairing, devices, access, activity, and requests between Silicons.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, PoisonError};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use extend_protocol::frames::{EnrollmentFrame, ServiceFrame};
use extend_protocol::model::{
    AccessGrant, ActivityEntry, AttachmentCreate, Delivery, DevicePatch, EndReason, Member, MemberKind, PairingClaim,
    RequestCreate, RequestInfo, TestingEnvironment, Visibility,
};
use extend_protocol::{DeviceId, DeviceOs, ErrorCode, PairingCode, TEST_DEVICE_LIMIT, TEST_DEVICE_LIMIT_MESSAGE, ids};
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{Body, decode_cursor, encode_cursor, hash_json, idempotent, limit, no_content, ok};
use crate::db::World;
use crate::domain::{self, Access, DeviceRow};
use crate::error::{AppError, AppResult};
use crate::state::{AppState, Auth, Shared};
use crate::ting::DeviceRequestTing;

const PAIR_FAILURES: usize = 5;
const PAIR_WINDOW: Duration = Duration::from_secs(600);

fn clean_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    let n = name.chars().count();
    if n == 0 || n > 64 || name.chars().any(char::is_control) {
        return Err(AppError::invalid(format!(
            "A device name must be 1–64 characters with no control characters; got {n} characters."
        )));
    }
    Ok(name.to_owned())
}

fn check_ttl(ttl: Option<i32>) -> AppResult<i32> {
    match ttl {
        None => Ok(14),
        Some(d) if (1..=30).contains(&d) => Ok(d),
        Some(d) => Err(AppError::invalid(format!(
            "A device can stay paired without activity for 1–30 days; got {d}."
        ))),
    }
}

async fn check_silicons(state: &Shared, auth: &Auth, ids_: &[String]) -> AppResult<()> {
    let team = auth.team()?;
    for s in ids_ {
        if ids::member_kind(s) != Some(MemberKind::Silicon) {
            return Err(AppError::invalid(format!(
                "{s:?} is not a Silicon id; Silicon ids look like si:chef."
            )));
        }
        if !state
            .iam
            .member_active(team, s, Some(&auth.p), auth.sel.as_ref())
            .await?
        {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                format!("{s} is not an active Silicon in team {team}."),
            )
            .hint("Check the id in Silicon IAM; only Silicons in the device's team can get access."));
        }
    }
    Ok(())
}

fn test_limit_error() -> AppError {
    AppError::new(ErrorCode::TestDeviceLimit, TEST_DEVICE_LIMIT_MESSAGE)
        .hint("Remove a device from this test environment first, with `extend device rm <device_id> --yes`.")
}

/// A quick refusal before any work when a test environment is already full. Not the guard: two
/// requests can both pass it, so [`begin_device_add`] decides inside the insert's transaction.
async fn test_limit(state: &Shared, world: &World) -> AppResult<()> {
    if !world.is_test() {
        return Ok(());
    }
    let n: i64 = sqlx::query_scalar(sql!(
        "SELECT count(*) FROM {} WHERE removed_at IS NULL",
        world.t("devices")
    ))
    .fetch_one(&state.pool)
    .await?;
    if n >= TEST_DEVICE_LIMIT {
        return Err(test_limit_error());
    }
    Ok(())
}

/// Advisory-lock class for "adding a device to this test environment" (the second key is the
/// world's schema). Two-key advisory locks never collide with the single-key ones in db.rs.
const TEST_DEVICE_LOCK_CLASS: i32 = 7_342_010;

/// Longest a device add waits for its turn in this process before it is refused as busy.
const TEST_ADD_WAIT: Duration = Duration::from_secs(10);

/// Longest the turn's holder waits in the database for other service processes adding to the
/// same environment (PostgreSQL `lock_timeout` for the rest of its transaction).
const TEST_ADD_LOCK_TIMEOUT: &str = "5s";

/// PostgreSQL's "canceling statement due to lock timeout".
const LOCK_NOT_AVAILABLE: &str = "55P03";

/// One turn per test environment (by world schema) for adding devices in this process. Waiting
/// here holds no database connection: only the turn's holder takes one. Waiting on the advisory
/// lock instead would pin a pooled connection per waiting request, so a burst of adds to one test
/// environment could use up the pool every other request (production included) shares.
static TEST_ADD_TURNS: LazyLock<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Default::default);

fn test_add_turn(schema: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut turns = TEST_ADD_TURNS.lock().unwrap_or_else(PoisonError::into_inner);
    // Forget the environments nobody is adding to right now (only this map holds their turn).
    turns.retain(|k, turn| k == schema || Arc::strong_count(turn) > 1);
    turns.entry(schema.to_owned()).or_default().clone()
}

fn test_add_busy(waited: Duration) -> AppError {
    AppError::new(
        ErrorCode::RateLimited,
        format!(
            "This test environment is busy adding other devices. Devices join a test environment one at a \
             time, so it never holds more than {TEST_DEVICE_LIMIT}, and this one waited {:.1} s without getting its turn.",
            waited.as_secs_f64()
        ),
    )
    .hint("Try again in a few seconds.")
    .details(serde_json::json!({"retry_after_s": 2}))
}

/// The transaction that adds a device and, in a test environment, that environment's turn, held
/// until the transaction ends.
struct DeviceAdd {
    tx: sqlx::Transaction<'static, sqlx::Postgres>,
    /// Devices the test environment held before this one (0 in production).
    paired_before: i64,
    turn: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl DeviceAdd {
    async fn commit(self) -> AppResult<()> {
        self.tx.commit().await?;
        drop(self.turn);
        Ok(())
    }
}

/// Starts the transaction that adds a device. In a test environment it also enforces the device
/// limit: adds to one environment take turns (in memory within this process, through an advisory
/// lock across processes) until their transaction ends, and count under the turn, so the count
/// includes every device committed before them. Nothing here, and nothing a caller does before
/// [`DeviceAdd::commit`], may use the pool: the turn's holder must never wait for a connection.
async fn begin_device_add(state: &AppState, world: &World) -> AppResult<DeviceAdd> {
    if !world.is_test() {
        return Ok(DeviceAdd {
            tx: state.pool.begin().await?,
            paired_before: 0,
            turn: None,
        });
    }
    let started = std::time::Instant::now();
    let turn = tokio::time::timeout(TEST_ADD_WAIT, test_add_turn(&world.schema).lock_owned())
        .await
        .map_err(|_| test_add_busy(started.elapsed()))?;
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT set_config('lock_timeout', $1, true)")
        .bind(TEST_ADD_LOCK_TIMEOUT)
        .execute(&mut *tx)
        .await?;
    match sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
        .bind(TEST_DEVICE_LOCK_CLASS)
        .bind(&world.schema)
        .execute(&mut *tx)
        .await
    {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some(LOCK_NOT_AVAILABLE) => {
            return Err(test_add_busy(started.elapsed()));
        }
        Err(e) => return Err(e.into()),
    }
    let n: i64 = sqlx::query_scalar(sql!(
        "SELECT count(*) FROM {} WHERE removed_at IS NULL",
        world.t("devices")
    ))
    .fetch_one(&mut *tx)
    .await?;
    if n >= TEST_DEVICE_LIMIT {
        return Err(test_limit_error());
    }
    Ok(DeviceAdd {
        tx,
        paired_before: n,
        turn: Some(turn),
    })
}

/// What a device and the Carbon hear about the test environment it pairs into.
fn environment_view(s: &crate::iam::TestingSelection, paired_devices: i64) -> TestingEnvironment {
    TestingEnvironment {
        environment_id: s.environment_id,
        name: s.name.clone(),
        state: "ready".into(),
        paired_devices,
        device_limit: TEST_DEVICE_LIMIT,
    }
}

pub async fn env_view(
    state: &AppState,
    auth_sel: Option<&crate::iam::TestingSelection>,
    world: &World,
) -> Option<TestingEnvironment> {
    let s = auth_sel?;
    let n: i64 = sqlx::query_scalar(sql!(
        "SELECT count(*) FROM {} WHERE removed_at IS NULL",
        world.t("devices")
    ))
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);
    Some(environment_view(s, n))
}

async fn insert_device(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    world: &World,
    team: &str,
    owner: &str,
    name: &str,
    os: DeviceOs,
    visibility: Visibility,
    ttl: i32,
    extra: (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ),
) -> AppResult<String> {
    let (os_version, model, app_version, host, credential_digest, address) = extra;
    for _ in 0..8 {
        let id = DeviceId::random();
        let res = sqlx::query(sql!(
            "INSERT INTO {} (device_id, team, owner_id, name, os, os_version, model, app_version, visibility, pair_ttl_days, host_device_id, credential_digest, address)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
            world.t("devices")
        ))
        .bind(id.as_str())
        .bind(team)
        .bind(owner)
        .bind(name)
        .bind(os.as_str())
        .bind(&os_version)
        .bind(&model)
        .bind(&app_version)
        .bind(visibility.as_str())
        .bind(ttl)
        .bind(&host)
        .bind(&credential_digest)
        .bind(&address)
        .execute(&mut **tx)
        .await;
        match res {
            Ok(_) => return Ok(id.to_string()),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(AppError::internal("could not allocate a unique device id"))
}

pub async fn claim(
    State(state): State<Shared>,
    auth: Auth,
    headers: HeaderMap,
    Body(input): Body<PairingClaim>,
) -> AppResult<Response> {
    auth.require_carbon()?;
    let team = auth.team()?.to_owned();
    let limit_key = format!("pair:{}:{}", auth.world.schema, auth.p.id());
    state
        .rate_peek(&limit_key, PAIR_FAILURES, PAIR_WINDOW, "wrong pairing codes")
        .await?;
    let code: PairingCode = input.pairing_code.parse().map_err(|_| {
        AppError::invalid(format!(
            "{:?} is not a pairing code; codes are 6 hexadecimal characters, like 4F9C2A.",
            input.pairing_code
        ))
    })?;
    let name = clean_name(&input.name)?;
    let ttl = check_ttl(input.pair_ttl_days)?;
    check_silicons(&state, &auth, &input.silicon_ids).await?;
    test_limit(&state, &auth.world).await?;
    let world = auth.world.clone();
    let hash = hash_json(&input);
    let sel = auth.sel.clone();
    let p = auth.p.clone();
    let st = state.clone();
    idempotent(&state, &auth.world, auth.p.id(), "pairings", &headers, &hash, || async move {
        // Until commit, everything runs on the add's own transaction: in a test environment this
        // request holds the environment's turn, and others wait for it.
        let mut add = begin_device_add(&st, &world).await?;
        let found: Option<(Uuid, String, Option<String>, Option<String>, String)> = sqlx::query_as(
            "SELECT enrollment_id, os, os_version, model, app_version FROM extend_global.enrollments
             WHERE pairing_code = $1 AND code_expires_at > now() AND paired_device_id IS NULL FOR UPDATE",
        )
        .bind(code.as_str())
        .fetch_optional(&mut *add.tx)
        .await?;
        let Some((enrollment_id, os, os_version, model, app_version)) = found else {
            drop(add);
            st.rate_limit(limit_key.clone(), PAIR_FAILURES + 1, PAIR_WINDOW, "wrong pairing codes").await?;
            return Err(AppError::new(ErrorCode::PairingCodeInvalid, "That pairing code is wrong, expired, or already used.")
                .hint("Codes change every 5 minutes. Enter the one the device shows now."));
        };
        let os: DeviceOs = serde_json::from_value(serde_json::Value::String(os)).map_err(AppError::internal)?;
        let credential = ids::new_secret(ids::DEVICE_CREDENTIAL_PREFIX);
        let device_id = insert_device(
            &mut add.tx,
            &world,
            &team,
            p.id(),
            &name,
            os,
            input.visibility.unwrap_or_default(),
            ttl,
            (os_version, model, Some(app_version), None, Some(ids::secret_digest(&credential)), None),
        )
        .await?;
        for s in &input.silicon_ids {
            sqlx::query(sql!("INSERT INTO {} (device_id, silicon_id, granted_by) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING", world.t("device_access")))
                .bind(&device_id)
                .bind(s)
                .bind(p.id())
                .execute(&mut *add.tx)
                .await?;
        }
        // Counted under the turn: the devices before this one, and this one.
        let environment = sel.as_ref().map(|s| environment_view(s, add.paired_before + 1));
        sqlx::query(
            "UPDATE extend_global.enrollments SET paired_schema = $2, paired_device_id = $3, paired_credential = $4, paired_environment = $5
             WHERE enrollment_id = $1",
        )
        .bind(enrollment_id)
        .bind(&world.schema)
        .bind(&device_id)
        .bind(&credential)
        .bind(environment.as_ref().map(|e| serde_json::to_value(e).unwrap_or_default()))
        .execute(&mut *add.tx)
        .await?;
        // Read back before commit, so the answer after commit needs nothing that can fail: a
        // committed pairing always answers 201.
        let d = domain::load_device_in(&mut *add.tx, &world, &device_id)
            .await?
            .ok_or_else(|| AppError::internal("device vanished while pairing"))?;
        add.commit().await?;
        st.rate_reset(&limit_key).await;
        st.hub
            .send_enrollment(
                enrollment_id,
                EnrollmentFrame::Paired { device_id: device_id.parse().map_err(AppError::internal)?, device_credential: credential, environment },
            )
            .await;
        domain::log(&st, &world, &device_id, &p.member, "paired", None, serde_json::json!({"name": name, "access": input.silicon_ids})).await;
        for s in &input.silicon_ids {
            domain::log(&st, &world, &device_id, &p.member, "access_granted", None, serde_json::json!({"silicon_id": s})).await;
        }
        let view = domain::device_view(&st, &world, &d, Access::Owner, false).await;
        tracing::info!(world = %world.schema, device_id, os = os.as_str(), "device paired");
        Ok((StatusCode::CREATED, "device", serde_json::to_value(view).map_err(AppError::internal)?))
    })
    .await
}

#[derive(Deserialize)]
pub struct ListQuery {
    scope: Option<String>,
    online: Option<bool>,
    os: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
    /// `true` also lists the Carbon's removed devices (scope=mine only). Parsed by hand so a bad
    /// value gets a precise error.
    include_removed: Option<String>,
}

/// Rows read per query while filling a page that the online filter thins out.
const ONLINE_SCAN_BATCH: i64 = 200;

pub async fn list(State(state): State<Shared>, auth: Auth, Query(q): Query<ListQuery>) -> AppResult<Response> {
    let team = auth.team()?.to_owned();
    let scope = q.scope.clone().unwrap_or_else(|| {
        if auth.p.is_silicon() {
            "accessible".into()
        } else {
            "mine".into()
        }
    });
    let lim = limit(q.limit)?;
    let (cond, access) = match (scope.as_str(), auth.p.is_silicon()) {
        ("mine", false) => ("d.owner_id = $2".to_owned(), Access::Owner),
        ("team", false) => (
            "d.owner_id <> $2 AND d.visibility = 'team'".to_owned(),
            Access::TeamViewer,
        ),
        ("accessible", true) => (
            format!(
                "EXISTS (SELECT 1 FROM {} a WHERE a.device_id = d.device_id AND a.silicon_id = $2)",
                auth.world.t("device_access")
            ),
            Access::Silicon,
        ),
        ("mine" | "team", true) => {
            return Err(AppError::invalid(
                "A Silicon lists the devices it has access to: use scope=accessible (the default).",
            ));
        }
        ("accessible", false) => {
            return Err(AppError::invalid(
                "A Carbon lists devices with scope=mine (the default) or scope=team.",
            ));
        }
        (other, _) => {
            return Err(AppError::invalid(format!(
                "scope must be mine, team or accessible; got {other:?}."
            )));
        }
    };
    let include_removed = match q.include_removed.as_deref() {
        None | Some("false") => false,
        Some("true") => true,
        Some(other) => {
            return Err(
                AppError::invalid(format!("include_removed must be true or false; got {other:?}."))
                    .hint("Send include_removed=true with scope=mine to list your removed devices too."),
            );
        }
    };
    if include_removed && access != Access::Owner {
        return Err(AppError::invalid(format!(
            "include_removed=true works only with scope=mine: a Carbon can list the devices they paired after \
             they're removed, to read their activity log. This request lists scope={scope}, which shows paired \
             devices only."
        ))
        .hint(
            "Drop include_removed, or, as the Carbon who paired the devices, send scope=mine&include_removed=true.",
        ));
    }
    let mut sql = format!("{} WHERE d.team = $1 AND {cond}", domain::device_select(&auth.world));
    if !include_removed {
        sql.push_str(" AND d.removed_at IS NULL");
    }
    if let Some(os) = &q.os {
        let os: DeviceOs = serde_json::from_value(serde_json::Value::String(os.clone())).map_err(|_| {
            AppError::invalid(format!("os={os:?} is not an operating system Extend knows."))
                .hint("Use one of android, android_tv, macos, windows, linux, ios, ipados, tvos, samsung_tv, lg_tv.")
        })?;
        sql.push_str(&format!(" AND d.os = '{}'", os.as_str()));
    }
    let after = q.cursor.as_deref().map(decode_cursor).transpose()?;
    // Online is known only to this process (live sockets), not to SQL, so the filter is applied
    // while reading: keep reading in device-id order until the page has `lim` matches plus one
    // more (which says another page exists) or the devices run out. next_cursor is the last
    // returned device, so a page is short only when it is the last.
    let batch = if q.online.is_some() {
        ONLINE_SCAN_BATCH.max(lim + 1)
    } else {
        lim + 1
    };
    let mut scanned_to = after;
    let mut rows: Vec<(DeviceRow, extend_protocol::model::Device)> = Vec::new();
    loop {
        let mut page_sql = sql.clone();
        if scanned_to.is_some() {
            page_sql.push_str(" AND d.device_id > $3");
        }
        page_sql.push_str(&format!(" ORDER BY d.device_id LIMIT {batch}"));
        let mut query = sqlx::query_as::<_, DeviceRow>(sqlx::AssertSqlSafe(page_sql))
            .bind(&team)
            .bind(auth.p.id());
        if let Some(a) = &scanned_to {
            query = query.bind(a);
        }
        let fetched = query.fetch_all(&state.pool).await?;
        let exhausted = (fetched.len() as i64) < batch;
        for r in fetched {
            scanned_to = Some(r.device_id.clone());
            let v = domain::device_view(&state, &auth.world, &r, access, false).await;
            if q.online.is_none_or(|o| o == v.online) {
                rows.push((r, v));
                if rows.len() as i64 > lim {
                    break;
                }
            }
        }
        if exhausted || rows.len() as i64 > lim {
            break;
        }
    }
    let next = (rows.len() as i64 > lim).then(|| {
        rows.truncate(lim as usize);
        encode_cursor(&rows.last().map(|(r, _)| r.device_id.clone()).unwrap_or_default())
    });
    let mut items: Vec<_> = rows.into_iter().map(|(_, v)| v).collect();
    items.sort_by(|a, b| {
        b.online
            .cmp(&a.online)
            .then_with(|| a.removed_at.is_some().cmp(&b.removed_at.is_some()))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(ok("devices", serde_json::json!({"items": items, "next_cursor": next})))
}

fn with_etag(mut resp: Response, version: i64) -> Response {
    if let Ok(v) = HeaderValue::from_str(&format!("\"{version}\"")) {
        resp.headers_mut().insert("etag", v);
    }
    resp
}

pub async fn get(State(state): State<Shared>, auth: Auth, Path(device_id): Path<String>) -> AppResult<Response> {
    // The Carbon who paired a removed device can still read it (removed_at and removed_reason set).
    let (d, access) = domain::readable_device(&state, &auth.world, &device_id, &auth.p).await?;
    let view = domain::device_view(&state, &auth.world, &d, access, access != Access::TeamViewer).await;
    Ok(with_etag(ok("device", view), d.version))
}

fn if_match(headers: &HeaderMap, current: i64) -> AppResult<()> {
    let Some(raw) = headers.get("if-match").and_then(|v| v.to_str().ok()) else {
        return Ok(());
    };
    let v: i64 = raw
        .trim()
        .trim_matches('"')
        .parse()
        .map_err(|_| AppError::invalid("If-Match must be the device version from its ETag, like \"3\"."))?;
    if v != current {
        return Err(AppError::new(
            ErrorCode::VersionConflict,
            format!("The device changed since you read it (you sent version {v}, it is now {current})."),
        ))
        .map_err(|e| e.hint("Read the device again and retry."));
    }
    Ok(())
}

pub async fn update(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    headers: HeaderMap,
    Body(patch): Body<DevicePatch>,
) -> AppResult<Response> {
    let d = domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    if patch.name.is_none() && patch.visibility.is_none() && patch.pair_ttl_days.is_none() {
        return Err(AppError::invalid(
            "Send at least one of name, visibility, pair_ttl_days.",
        ));
    }
    if_match(&headers, d.version)?;
    let name = patch.name.as_deref().map(clean_name).transpose()?;
    let ttl = patch.pair_ttl_days.map(|t| check_ttl(Some(t))).transpose()?;
    let updated = sqlx::query(sql!(
        "UPDATE {} SET name = COALESCE($2, name), visibility = COALESCE($3, visibility), pair_ttl_days = COALESCE($4, pair_ttl_days),
                version = version + 1, last_activity_at = now() WHERE device_id = $1 AND version = $5",
        auth.world.t("devices")
    ))
    .bind(&device_id)
    .bind(&name)
    .bind(patch.visibility.map(|v| v.as_str()))
    .bind(ttl)
    .bind(d.version)
    .execute(&state.pool)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::new(
            ErrorCode::VersionConflict,
            "The device changed while you were updating it.",
        )
        .hint("Read it again and retry."));
    }
    let mut changes = serde_json::Map::new();
    if let Some(n) = &name {
        changes.insert("name".into(), serde_json::json!({"from": d.name, "to": n}));
    }
    if let Some(v) = patch.visibility {
        changes.insert("visibility".into(), serde_json::json!(v.as_str()));
    }
    if let Some(t) = ttl {
        changes.insert("pair_ttl_days".into(), serde_json::json!(t));
    }
    let action = if name.is_some() && changes.len() == 1 {
        "renamed"
    } else {
        "settings_changed"
    };
    domain::log(
        &state,
        &auth.world,
        &device_id,
        &auth.p.member,
        action,
        None,
        serde_json::Value::Object(changes),
    )
    .await;
    let _ = state.hub.send(&d.route(&auth.world), ServiceFrame::Refresh).await;
    let d = domain::load_device(&state, &auth.world, &device_id)
        .await?
        .ok_or_else(|| domain::device_not_found(&device_id))?;
    let view = domain::device_view(&state, &auth.world, &d, Access::Owner, true).await;
    Ok(with_etag(ok("device", view), d.version))
}

pub async fn remove(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let d = domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    if_match(&headers, d.version)?;
    domain::unpair(
        &state,
        &auth.world,
        &device_id,
        EndReason::DeviceRemoved,
        &auth.p.member,
    )
    .await?;
    tracing::info!(world = %auth.world.schema, device_id, "device removed");
    Ok(no_content())
}

pub async fn stop(State(state): State<Shared>, auth: Auth, Path(device_id): Path<String>) -> AppResult<Response> {
    let d = domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    let Some(sid) = d.in_use_session.clone() else {
        return Err(AppError::new(
            ErrorCode::DeviceNotInUse,
            format!("No Silicon is using {} right now.", d.name),
        ));
    };
    let row = domain::end_session(&state, &auth.world, &sid, EndReason::StoppedByCarbon, &auth.p.member)
        .await?
        .ok_or_else(|| AppError::new(ErrorCode::DeviceNotInUse, "The session had already ended."))?;
    Ok(ok("session", row.view()))
}

pub async fn attach(
    State(state): State<Shared>,
    auth: Auth,
    Path(host_id): Path<String>,
    headers: HeaderMap,
    Body(input): Body<AttachmentCreate>,
) -> AppResult<Response> {
    auth.require_carbon()?;
    let host = domain::owned_device(&state, &auth.world, &host_id, &auth.p).await?;
    if !input.os.needs_host() {
        return Err(AppError::invalid(format!(
            "{} devices run the Extend app themselves; pair them with their own pairing code.",
            input.os.as_str()
        ))
        .hint("Install the Extend app on the device and enter the code it shows: `extend device pair <pairing_code> --name <name>`."));
    }
    if !input.os.allowed_hosts().contains(&host.os()) {
        let hosts: Vec<&str> = input.os.allowed_hosts().iter().map(|o| o.as_str()).collect();
        return Err(AppError::invalid(format!(
            "{} devices pair through a {}; {} is a {}.",
            input.os.as_str(),
            hosts.join(" or "),
            host.name,
            host.os().as_str()
        ))
        .hint(format!(
            "Pick one of your paired {} computers as the host; `extend device ls` lists them.",
            hosts.join(" or ")
        )));
    }
    if host.host_device_id.is_some() {
        return Err(AppError::invalid(format!(
            "{} is paired through a computer, so it can't carry other devices.",
            host.name
        ))
        .hint("Attach the device to the computer instead: `extend device attach <host_device_id> …`."));
    }
    if !state.hub.is_connected(&host.key(&auth.world)).await {
        return Err(AppError::new(
            ErrorCode::DeviceOffline,
            format!(
                "{} is offline; it has to be online to set up a device through it.",
                host.name
            ),
        )
        .hint("Open the Extend app on that computer and make sure it's connected."));
    }
    let name = clean_name(&input.name)?;
    let ttl = check_ttl(input.pair_ttl_days)?;
    test_limit(&state, &auth.world).await?;
    let hash = hash_json(&input);
    let world = auth.world.clone();
    let team = auth.team()?.to_owned();
    let p = auth.p.clone();
    let st = state.clone();
    idempotent(
        &state,
        &auth.world,
        auth.p.id(),
        "attachments",
        &headers,
        &hash,
        || async move {
            let mut add = begin_device_add(&st, &world).await?;
            let device_id = insert_device(
                &mut add.tx,
                &world,
                &team,
                p.id(),
                &name,
                input.os,
                input.visibility.unwrap_or_default(),
                ttl,
                (
                    None,
                    None,
                    None,
                    Some(host.device_id.clone()),
                    None,
                    input.address.clone(),
                ),
            )
            .await?;
            // Read back on the add's transaction (see claim), then commit.
            let d = domain::load_device_in(&mut *add.tx, &world, &device_id)
                .await?
                .ok_or_else(|| AppError::internal("attached device vanished"))?;
            add.commit().await?;
            st.hub
                .send(
                    &host.key(&world),
                    ServiceFrame::Attach {
                        device_id: device_id.parse().map_err(AppError::internal)?,
                        os: input.os,
                        name: name.clone(),
                        address: input.address.clone(),
                        removed: false,
                    },
                )
                .await;
            domain::log(
                &st,
                &world,
                &device_id,
                &p.member,
                "paired",
                None,
                serde_json::json!({"name": name, "through": host.device_id}),
            )
            .await;
            let view = domain::device_view(&st, &world, &d, Access::Owner, false).await;
            Ok((
                StatusCode::CREATED,
                "device",
                serde_json::to_value(view).map_err(AppError::internal)?,
            ))
        },
    )
    .await
}

pub async fn team_silicons(State(state): State<Shared>, auth: Auth) -> AppResult<Response> {
    auth.team()?;
    let items = state.iam.team_silicons(&auth.p, auth.sel.as_ref()).await?;
    Ok(ok("team_silicons", serde_json::json!({"items": items})))
}

pub async fn setup(State(state): State<Shared>, auth: Auth, Path(device_id): Path<String>) -> AppResult<Response> {
    let d = domain::owned_readable_device(&state, &auth.world, &device_id, &auth.p).await?;
    Ok(ok("setup", d.setup()))
}

#[derive(Deserialize, serde::Serialize)]
pub struct SetupCode {
    code: String,
}

pub async fn setup_code(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    Body(input): Body<SetupCode>,
) -> AppResult<Response> {
    let d = domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    if d.host_device_id.is_none() {
        return Err(AppError::invalid(format!(
            "{} runs the Extend app itself, so it takes no setup code; only an Apple TV paired through a Mac does.",
            d.name
        ))
        .hint("Finish its setup on the device; watch the steps with `extend device setup <device_id>`."));
    }
    if d.os() != DeviceOs::Tvos {
        return Err(AppError::invalid(format!(
            "{} is a {} device, which takes no setup code; only an Apple TV does.",
            d.name,
            d.os().as_str()
        ))
        .hint("Follow the setup steps for this device instead: `extend device setup <device_id>` lists them."));
    }
    let code = input.code.trim();
    if code.len() != 4 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AppError::invalid(format!(
            "The setup code is the 4 digits the Apple TV shows; got {:?}.",
            input.code
        ))
        .hint("Enter the 4 digits on the TV screen now; if they're gone, restart the setup on the Mac."));
    }
    let sent = state
        .hub
        .send(
            &d.route(&auth.world),
            ServiceFrame::SetupCode {
                device_id: d.device_id.parse().map_err(AppError::internal)?,
                code: code.to_owned(),
            },
        )
        .await;
    if !sent {
        return Err(AppError::new(
            ErrorCode::DeviceOffline,
            format!(
                "The computer {} pairs through is offline, so the code could not reach it.",
                d.name
            ),
        )
        .hint("Open the Extend app on that Mac and make sure it's connected, then enter the code again."));
    }
    Ok(ok("setup", d.setup()))
}

pub async fn access_list(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
) -> AppResult<Response> {
    domain::owned_readable_device(&state, &auth.world, &device_id, &auth.p).await?;
    let rows: Vec<(String, String, String, OffsetDateTime, Option<OffsetDateTime>)> = sqlx::query_as(sql!(
        "SELECT device_id, silicon_id, granted_by, granted_at, last_used_at FROM {} WHERE device_id = $1 ORDER BY granted_at",
        auth.world.t("device_access")
    ))
    .bind(&device_id)
    .fetch_all(&state.pool)
    .await?;
    let items: Vec<AccessGrant> = rows
        .into_iter()
        .filter_map(|(d, s, g, at, used)| {
            Some(AccessGrant {
                device_id: d.parse().ok()?,
                silicon_id: s,
                granted_by: g,
                granted_at: at,
                last_used_at: used,
            })
        })
        .collect();
    Ok(ok("access", serde_json::json!({"items": items})))
}

pub async fn access_grant(
    State(state): State<Shared>,
    auth: Auth,
    Path((device_id, silicon_id)): Path<(String, String)>,
) -> AppResult<Response> {
    domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    check_silicons(&state, &auth, std::slice::from_ref(&silicon_id)).await?;
    let inserted = sqlx::query(sql!(
        "INSERT INTO {} (device_id, silicon_id, granted_by) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        auth.world.t("device_access")
    ))
    .bind(&device_id)
    .bind(&silicon_id)
    .bind(auth.p.id())
    .execute(&state.pool)
    .await?;
    if inserted.rows_affected() > 0 {
        domain::log(
            &state,
            &auth.world,
            &device_id,
            &auth.p.member,
            "access_granted",
            None,
            serde_json::json!({"silicon_id": silicon_id}),
        )
        .await;
    }
    let (d, s, g, at, used): (String, String, String, OffsetDateTime, Option<OffsetDateTime>) = sqlx::query_as(sql!(
        "SELECT device_id, silicon_id, granted_by, granted_at, last_used_at FROM {} WHERE device_id = $1 AND silicon_id = $2",
        auth.world.t("device_access")
    ))
    .bind(&device_id)
    .bind(&silicon_id)
    .fetch_one(&state.pool)
    .await?;
    Ok(ok(
        "access_grant",
        AccessGrant {
            device_id: d.parse().map_err(AppError::internal)?,
            silicon_id: s,
            granted_by: g,
            granted_at: at,
            last_used_at: used,
        },
    ))
}

pub async fn access_revoke(
    State(state): State<Shared>,
    auth: Auth,
    Path((device_id, silicon_id)): Path<(String, String)>,
) -> AppResult<Response> {
    let d = domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    let removed = sqlx::query(sql!(
        "DELETE FROM {} WHERE device_id = $1 AND silicon_id = $2",
        auth.world.t("device_access")
    ))
    .bind(&device_id)
    .bind(&silicon_id)
    .execute(&state.pool)
    .await?;
    if removed.rows_affected() > 0 {
        domain::log(
            &state,
            &auth.world,
            &device_id,
            &auth.p.member,
            "access_revoked",
            None,
            serde_json::json!({"silicon_id": silicon_id}),
        )
        .await;
    }
    if d.in_use_silicon.as_deref() == Some(silicon_id.as_str())
        && let Some(sid) = &d.in_use_session
    {
        domain::end_session(&state, &auth.world, sid, EndReason::AccessRemoved, &auth.p.member).await?;
    }
    Ok(no_content())
}

#[derive(Deserialize)]
pub struct ActivityQuery {
    silicon_id: Option<String>,
    session_id: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    since: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    until: Option<OffsetDateTime>,
    limit: Option<i64>,
    cursor: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ActivityRow {
    id: Uuid,
    at: OffsetDateTime,
    actor_kind: String,
    actor_id: String,
    action: String,
    session_id: Option<String>,
    command: Option<String>,
    args: Option<serde_json::Value>,
    outcome: Option<String>,
    files: serde_json::Value,
    details: Option<serde_json::Value>,
}

pub async fn activity(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    Query(q): Query<ActivityQuery>,
) -> AppResult<Response> {
    // The log outlives the pair: the owner reads it after the device is removed too.
    domain::owned_readable_device(&state, &auth.world, &device_id, &auth.p).await?;
    let lim = limit(q.limit)?;
    let before: Option<Uuid> = q
        .cursor
        .as_deref()
        .map(decode_cursor)
        .transpose()?
        .and_then(|c| c.parse().ok());
    let rows: Vec<ActivityRow> = sqlx::query_as(sql!(
        "SELECT id, at, actor_kind, actor_id, action, session_id, command, args, outcome, files, details FROM {}
         WHERE device_id = $1
           AND ($2::text IS NULL OR actor_id = $2)
           AND ($3::text IS NULL OR session_id = $3)
           AND ($4::timestamptz IS NULL OR at >= $4)
           AND ($5::timestamptz IS NULL OR at <= $5)
           AND ($6::uuid IS NULL OR id < $6)
         ORDER BY id DESC LIMIT $7",
        auth.world.t("activity")
    ))
    .bind(&device_id)
    .bind(&q.silicon_id)
    .bind(&q.session_id)
    .bind(q.since)
    .bind(q.until)
    .bind(before)
    .bind(lim + 1)
    .fetch_all(&state.pool)
    .await?;
    let more = rows.len() as i64 > lim;
    let items: Vec<ActivityEntry> = rows
        .into_iter()
        .take(lim as usize)
        .map(|r| ActivityEntry {
            id: r.id,
            at: r.at,
            actor: Member {
                kind: if r.actor_kind == "silicon" {
                    MemberKind::Silicon
                } else {
                    MemberKind::Carbon
                },
                id: r.actor_id,
                display_name: None,
            },
            action: r.action,
            session_id: r.session_id.and_then(|s| s.parse().ok()),
            command: r.command,
            args: r.args.and_then(|a| serde_json::from_value(a).ok()),
            outcome: r.outcome,
            files: serde_json::from_value(r.files).unwrap_or_default(),
            details: r.details.unwrap_or(serde_json::Value::Null),
        })
        .collect();
    let next = more.then(|| encode_cursor(&items.last().map(|i| i.id.to_string()).unwrap_or_default()));
    Ok(ok("activity", serde_json::json!({"items": items, "next_cursor": next})))
}

#[derive(sqlx::FromRow)]
struct RequestRow {
    request_id: Uuid,
    device_id: String,
    from_id: String,
    to_id: String,
    session_id: Option<String>,
    reason: String,
    created_at: OffsetDateTime,
    delivery: String,
    last_error: Option<String>,
}

impl RequestRow {
    fn view(self) -> Option<RequestInfo> {
        Some(RequestInfo {
            request_id: self.request_id,
            device_id: self.device_id.parse().ok()?,
            from: self.from_id,
            to: self.to_id,
            session_id: self.session_id.and_then(|s| s.parse().ok()),
            reason: self.reason,
            created_at: self.created_at,
            delivery: match self.delivery.as_str() {
                "delivered" => Delivery::Delivered,
                "failed" => Delivery::Failed,
                _ => Delivery::Pending,
            },
            last_error: if self.delivery == "delivered" {
                None
            } else {
                self.last_error
            },
        })
    }
}

const REQUEST_COLUMNS: &str =
    "request_id, device_id, from_id, to_id, session_id, reason, created_at, delivery, last_error";

#[derive(Deserialize)]
pub struct PageQuery {
    limit: Option<i64>,
    cursor: Option<String>,
    direction: Option<String>,
    device_id: Option<String>,
}

async fn request_page(
    state: &Shared,
    world: &World,
    cond: &str,
    binds: Vec<String>,
    q: &PageQuery,
) -> AppResult<Response> {
    let lim = limit(q.limit)?;
    let before: Option<Uuid> = q
        .cursor
        .as_deref()
        .map(decode_cursor)
        .transpose()?
        .and_then(|c| c.parse().ok());
    let n = binds.len();
    let sql = format!(
        "SELECT {REQUEST_COLUMNS} FROM {} WHERE {cond} AND (${}::uuid IS NULL OR request_id < ${}) ORDER BY request_id DESC LIMIT ${}",
        world.t("requests"),
        n + 1,
        n + 1,
        n + 2
    );
    let mut query = sqlx::query_as::<_, RequestRow>(sqlx::AssertSqlSafe(sql.clone()));
    for b in &binds {
        query = query.bind(b);
    }
    let rows = query.bind(before).bind(lim + 1).fetch_all(&state.pool).await?;
    let more = rows.len() as i64 > lim;
    let items: Vec<RequestInfo> = rows
        .into_iter()
        .take(lim as usize)
        .filter_map(RequestRow::view)
        .collect();
    let next = more.then(|| encode_cursor(&items.last().map(|i| i.request_id.to_string()).unwrap_or_default()));
    Ok(ok("requests", serde_json::json!({"items": items, "next_cursor": next})))
}

pub async fn requests_for_device(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    domain::owned_readable_device(&state, &auth.world, &device_id, &auth.p).await?;
    request_page(&state, &auth.world, "device_id = $1", vec![device_id], &q).await
}

pub async fn my_requests(State(state): State<Shared>, auth: Auth, Query(q): Query<PageQuery>) -> AppResult<Response> {
    auth.require_silicon()?;
    let team = auth.team()?.to_owned();
    let dir = q.direction.as_deref().unwrap_or("all");
    let who = match dir {
        "sent" => "from_id = $2",
        "received" => "to_id = $2",
        "all" => "(from_id = $2 OR to_id = $2)",
        other => {
            return Err(AppError::invalid(format!(
                "direction must be sent, received or all; got {other:?}."
            )));
        }
    };
    let mut binds = vec![team, auth.p.id().to_owned()];
    let mut cond = format!("team = $1 AND {who}");
    if let Some(d) = &q.device_id {
        binds.push(d.clone());
        cond.push_str(" AND device_id = $3");
    }
    request_page(&state, &auth.world, &cond, binds, &q).await
}

/// Longest a request reason may be including the whitespace around it, which is kept and
/// delivered as sent (the reason itself is 1–300 characters without it).
const REASON_WITH_WHITESPACE_MAX_CHARS: usize = 1_000;

pub async fn request_send(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    headers: HeaderMap,
    Body(input): Body<RequestCreate>,
) -> AppResult<Response> {
    auth.require_silicon()?;
    let (d, access) = domain::visible_device(&state, &auth.world, &device_id, &auth.p).await?;
    if access != Access::Silicon {
        return Err(AppError::new(
            ErrorCode::NoAccess,
            format!("{} has no access to {}.", auth.p.id(), d.name),
        ));
    }
    // The length is counted without surrounding whitespace, but the reason is stored and
    // delivered exactly as the Silicon wrote it.
    let reason = input.reason.clone();
    let n = reason.trim().chars().count();
    if n == 0 || n > extend_protocol::REASON_MAX_CHARS {
        return Err(AppError::invalid(format!(
            "The reason must be 1–300 characters (not counting spaces and line breaks around it); it is {n}."
        ))
        .hint("Say briefly why you need the device, for example \"Need it for an OTP, 2 minutes\"."));
    }
    let total = reason.chars().count();
    if total > REASON_WITH_WHITESPACE_MAX_CHARS {
        return Err(AppError::invalid(format!(
            "The reason is {total} characters with the spaces and line breaks around it; at most {REASON_WITH_WHITESPACE_MAX_CHARS} are accepted."
        ))
        .hint("Remove the extra whitespace around the reason and send it again."));
    }
    let (Some(holder), Some(session)) = (d.in_use_silicon.clone(), d.in_use_session.clone()) else {
        return Err(AppError::new(
            ErrorCode::DeviceNotInUse,
            format!("No Silicon is using {} right now, so there's nobody to ask.", d.name),
        )
        .hint(format!("Start using it: extend session new {device_id}")));
    };
    if holder == auth.p.id() {
        return Err(AppError::new(
            ErrorCode::Conflict,
            format!("You are already using {} in session {session}.", d.name),
        ));
    }
    // The same reason from the same Silicon for the same device (and the same session on it)
    // within 60 s is a repeat (a retry, or a double send): it returns the existing request and
    // sends nothing. A new reason is a new request and is delivered.
    let recent: Option<RequestRow> = sqlx::query_as(sql!(
        "SELECT {REQUEST_COLUMNS} FROM {} WHERE device_id = $1 AND from_id = $2 AND reason = $3 AND session_id = $4
           AND created_at > now() - interval '60 seconds' ORDER BY created_at DESC LIMIT 1",
        auth.world.t("requests")
    ))
    .bind(&device_id)
    .bind(auth.p.id())
    .bind(&reason)
    .bind(&session)
    .fetch_optional(&state.pool)
    .await?;
    if let Some(r) = recent.and_then(RequestRow::view) {
        return Ok(ok("request", r));
    }
    let hash = hash_json(&input);
    let world = auth.world.clone();
    let st = state.clone();
    let p = auth.p.clone();
    let sel = auth.sel.clone();
    idempotent(&state, &auth.world, auth.p.id(), &format!("requests:{device_id}"), &headers, &hash, || async move {
        let id = Uuid::now_v7();
        sqlx::query(sql!(
            "INSERT INTO {} (request_id, device_id, team, from_id, to_id, session_id, reason) VALUES ($1, $2, $3, $4, $5, $6, $7)",
            world.t("requests")
        ))
        .bind(id)
        .bind(&device_id)
        .bind(&d.team)
        .bind(p.id())
        .bind(&holder)
        .bind(&session)
        .bind(&reason)
        .execute(&st.pool)
        .await?;
        let ting = DeviceRequestTing { request_id: id, device_id: &device_id, device_name: &d.name, from: p.id(), to: &holder, session_id: Some(&session), reason: &reason };
        let delivery = match st.notifier.device_request(&p, &ting, sel.as_ref()).await {
            Ok(()) => "delivered",
            Err(e) => {
                tracing::warn!(request_id = %id, error = %e.0.message, "Ting delivery failed; will retry");
                let why = match &e.0.hint {
                    Some(h) => format!("{} {h}", e.0.message),
                    None => e.0.message.clone(),
                };
                sqlx::query(sql!("UPDATE {} SET attempts = attempts + 1, last_error = $2 WHERE request_id = $1", world.t("requests")))
                    .bind(id)
                    .bind(&why)
                    .execute(&st.pool)
                    .await?;
                "pending"
            }
        };
        if delivery == "delivered" {
            sqlx::query(sql!("UPDATE {} SET delivery = 'delivered', attempts = attempts + 1 WHERE request_id = $1", world.t("requests")))
                .bind(id)
                .execute(&st.pool)
                .await?;
        }
        domain::log(&st, &world, &device_id, &p.member, "request_sent", Some(&session), serde_json::json!({"to": holder, "reason": reason})).await;
        let row: RequestRow = sqlx::query_as(sql!("SELECT {REQUEST_COLUMNS} FROM {} WHERE request_id = $1", world.t("requests")))
            .bind(id)
            .fetch_one(&st.pool)
            .await?;
        Ok((StatusCode::CREATED, "request", serde_json::to_value(row.view()).map_err(AppError::internal)?))
    })
    .await
}

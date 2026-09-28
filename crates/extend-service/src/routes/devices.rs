//! Pairing, devices, access, activity, requests between Silicons, and setup retries.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, PoisonError};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use extend_protocol::frames::{EnrollmentFrame, ServiceFrame};
use extend_protocol::model::{
    AccessGrant, ActivityEntry, AttachmentCreate, Delivery, DeviceSettingsPatch as DevicePatch, DeviceStopped,
    EndReason, InUseIndicator, Member, MemberKind, PairingClaim, RequestCreate, RequestInfo, RequestRoute, RetryResult,
    SetupRetryInput, StepStatus, TeamReach, TeamSilicons, TestingEnvironment,
};
use extend_protocol::{DeviceId, DeviceOs, ErrorCode, PairingCode, TEST_DEVICE_LIMIT, TEST_DEVICE_LIMIT_MESSAGE, ids};
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{Body, decode_cursor, encode_cursor, hash_json, idempotent, limit, no_content, ok};
use crate::db::World;
use crate::delivery::{self, Actor};
use crate::domain::{self, Access, DeviceRow, GrantEnd, RevokeScope, Viewer};
use crate::error::{AppError, AppResult};
use crate::iam::{Principal, TestingSelection};
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

/// Checks that each id is an active Silicon of the principal's Team, read with the principal's own
/// login (a Carbon authorized for that Team).
async fn check_silicons(
    state: &AppState,
    p: &Principal,
    sel: Option<&TestingSelection>,
    ids_: &[String],
) -> AppResult<()> {
    if ids_.is_empty() {
        return Ok(());
    }
    let team = p.team()?;
    for s in ids_ {
        if ids::member_kind(s) != Some(MemberKind::Silicon) {
            return Err(AppError::invalid(format!(
                "{s:?} is not a Silicon id; Silicon ids look like si:chef."
            )));
        }
        if !state.iam.member_active(team, s, Some(p), sel).await? {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                format!("{s} is not an active Silicon in team {team}."),
            )
            .hint("Check the id in Silicon IAM; a Silicon gets access in a Team it belongs to."));
        }
    }
    Ok(())
}

fn test_limit_error() -> AppError {
    AppError::new(ErrorCode::TestDeviceLimit, TEST_DEVICE_LIMIT_MESSAGE)
        .hint("Remove a device from this test environment first, with `extend device rm <device_id> --yes`.")
}

/// How many devices a test environment holds: physical devices, so a second Carbon's pair of a
/// device already there never counts, and neither does a carried pair still waiting to be
/// recognised as one already there.
pub async fn paired_count<'c>(db: impl sqlx::PgExecutor<'c>, world: &World) -> AppResult<i64> {
    Ok(sqlx::query_scalar(sql!(
        "SELECT count(DISTINCT instance_id) FROM {} WHERE removed_at IS NULL AND provisional_until IS NULL",
        world.t("devices")
    ))
    .fetch_one(db)
    .await?)
}

/// A quick refusal before any work when a test environment is already full. Not the guard: two
/// requests can both pass it, so [`begin_device_add`]'s count decides inside the insert's transaction.
async fn test_limit(state: &Shared, world: &World) -> AppResult<()> {
    if !world.is_test() {
        return Ok(());
    }
    if paired_count(&state.pool, world).await? >= TEST_DEVICE_LIMIT {
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
    /// Refuses a new physical device when the test environment is full.
    fn check_limit(&self, world: &World) -> AppResult<()> {
        if world.is_test() && self.paired_before >= TEST_DEVICE_LIMIT {
            return Err(test_limit_error());
        }
        Ok(())
    }
}

/// Starts the transaction that adds a device. In a test environment, adds to one environment take
/// turns (in memory within this process, through an advisory lock across processes) until their
/// transaction ends, and count under the turn, so the count includes every device committed before
/// them; the caller decides with [`DeviceAdd::check_limit`] once it knows whether the add is a new
/// physical device. Nothing here, and nothing a caller does before [`DeviceAdd::commit`], may use
/// the pool: the turn's holder must never wait for a connection.
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
    let n = paired_count(&mut *tx, world).await?;
    Ok(DeviceAdd {
        tx,
        paired_before: n,
        turn: Some(turn),
    })
}

/// What a device and the Carbon hear about the test environment it pairs into.
fn environment_view(s: &TestingSelection, paired_devices: i64) -> TestingEnvironment {
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
    auth_sel: Option<&TestingSelection>,
    world: &World,
) -> Option<TestingEnvironment> {
    let s = auth_sel?;
    let n = paired_count(&state.pool, world).await.unwrap_or(0);
    Some(environment_view(s, n))
}

/// A new pair row. `instance` is the physical device when the pair joins one ("Pair with another
/// Carbon"), `None` for a new device.
struct NewPair<'a> {
    team: &'a str,
    owner: &'a str,
    name: &'a str,
    os: DeviceOs,
    ttl: i32,
    os_version: Option<String>,
    model: Option<String>,
    app_version: Option<String>,
    host: Option<String>,
    credential_digest: Option<String>,
    address: Option<String>,
    instance: Option<Uuid>,
    first_pair: bool,
    provisional_until: Option<OffsetDateTime>,
}

fn already_paired(name: &str, device_id: &str) -> AppError {
    AppError::new(
        ErrorCode::Conflict,
        format!("You already paired this device: it's {name} ({device_id}) in your devices."),
    )
    .hint("Each Carbon pairs a device once. Another Carbon can use this code to pair it to their own account.")
}

/// Inserts a pair, each try inside a savepoint: a device-id collision retries with a new id, and
/// only that. A second live pair of one device for the same Carbon (a double claim racing past the
/// check) is the 409 the check gives.
async fn insert_device(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    world: &World,
    p: NewPair<'_>,
) -> AppResult<String> {
    for _ in 0..8 {
        let id = DeviceId::random();
        sqlx::query("SAVEPOINT add_device").execute(&mut **tx).await?;
        let res = sqlx::query(sql!(
            "INSERT INTO {} (device_id, team, owner_id, name, os, os_version, model, app_version, visibility, pair_ttl_days,
                             host_device_id, credential_digest, address, instance_id, first_pair, provisional_until)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'personal', $9, $10, $11, $12, COALESCE($13, gen_random_uuid()), $14, $15)",
            world.t("devices")
        ))
        .bind(id.as_str())
        .bind(p.team)
        .bind(p.owner)
        .bind(p.name)
        .bind(p.os.as_str())
        .bind(&p.os_version)
        .bind(&p.model)
        .bind(&p.app_version)
        .bind(p.ttl)
        .bind(&p.host)
        .bind(&p.credential_digest)
        .bind(&p.address)
        .bind(p.instance)
        .bind(p.first_pair)
        .bind(p.provisional_until)
        .execute(&mut **tx)
        .await;
        match res {
            Ok(_) => {
                sqlx::query("RELEASE SAVEPOINT add_device").execute(&mut **tx).await?;
                return Ok(id.to_string());
            }
            Err(sqlx::Error::Database(e)) if e.constraint() == Some("devices_pkey") => {
                sqlx::query("ROLLBACK TO SAVEPOINT add_device")
                    .execute(&mut **tx)
                    .await?;
            }
            Err(sqlx::Error::Database(e)) if e.constraint() == Some("devices_one_pair_per_carbon") => {
                sqlx::query("ROLLBACK TO SAVEPOINT add_device")
                    .execute(&mut **tx)
                    .await?;
                let mine: Option<(String, String)> = sqlx::query_as(sql!(
                    "SELECT device_id, name FROM {} WHERE instance_id = $1 AND owner_id = $2 AND removed_at IS NULL",
                    world.t("devices")
                ))
                .bind(p.instance)
                .bind(p.owner)
                .fetch_optional(&mut **tx)
                .await?;
                let (id, name) = mine.unwrap_or_else(|| ("?".into(), "this device".into()));
                return Err(already_paired(&name, &id));
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(AppError::internal("could not allocate a unique device id"))
}

/// The Team a Carbon's claim is made in: X-Org-ID, or the first Team their login reaches. It is
/// informational (see [`DeviceRow::team`]).
fn claim_team(p: &Principal) -> AppResult<String> {
    p.team.clone().or_else(|| p.teams.first().cloned()).ok_or_else(|| {
        AppError::new(
            ErrorCode::NotATeamMember,
            format!("{}'s login reaches no Team.", p.id()),
        )
    })
}

/// Whether a code belongs to a "Pair with another Carbon" enrollment whose device still has a live
/// pair: such a claim adds no device to a test environment.
async fn joins_device(state: &AppState, world: &World, code: &PairingCode) -> AppResult<bool> {
    let instance: Option<Option<Uuid>> = sqlx::query_scalar(
        "SELECT instance_id FROM extend_global.enrollments WHERE pairing_code = $1 AND code_expires_at > now() AND paired_device_id IS NULL",
    )
    .bind(code.as_str())
    .fetch_optional(&state.pool)
    .await?;
    let Some(Some(instance)) = instance else {
        return Ok(false);
    };
    let live: bool = sqlx::query_scalar(sql!(
        "SELECT EXISTS (SELECT 1 FROM {} WHERE instance_id = $1 AND removed_at IS NULL)",
        world.t("devices")
    ))
    .bind(instance)
    .fetch_one(&state.pool)
    .await?;
    Ok(live)
}

pub async fn claim(
    State(state): State<Shared>,
    auth: Auth,
    headers: HeaderMap,
    Body(input): Body<PairingClaim>,
) -> AppResult<Response> {
    auth.require_carbon()?;
    let team = claim_team(&auth.p)?;
    if !input.silicon_ids.is_empty() && auth.p.team.is_none() {
        return Err(AppError::invalid(
            "Say which Team the Silicons you give access to are in: send X-Org-ID (--team <handle>), or give access after pairing.",
        ));
    }
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
    // `visibility` is accepted and ignored: every device is personal.
    check_silicons(&state, &auth.p, auth.sel.as_ref(), &input.silicon_ids).await?;
    let world = auth.world.clone();
    let hash = hash_json(&input);
    let sel = auth.sel.clone();
    let p = auth.p.clone();
    let st = state.clone();
    idempotent(&state, &auth.world, auth.p.id(), "pairings", &headers, &hash, || async move {
        // A successful fifth claim must replay before the now-full environment rejects new
        // devices. New claims still get the fast precheck and the transaction's atomic limit.
        if !joins_device(&st, &world, &code).await? {
            test_limit(&st, &world).await?;
        }
        // Until commit, everything runs on the add's own transaction: in a test environment this
        // request holds the environment's turn, and others wait for it.
        let mut add = begin_device_add(&st, &world).await?;
        let found: Option<(Uuid, String, Option<String>, Option<String>, String, Option<String>, Option<Uuid>)> = sqlx::query_as(
            "SELECT enrollment_id, os, os_version, model, app_version, agent_device_version, instance_id FROM extend_global.enrollments
             WHERE pairing_code = $1 AND code_expires_at > now() AND paired_device_id IS NULL FOR UPDATE",
        )
        .bind(code.as_str())
        .fetch_optional(&mut *add.tx)
        .await?;
        let Some((enrollment_id, os, os_version, model, app_version, engine_version, instance)) = found else {
            drop(add);
            st.rate_limit(limit_key.clone(), PAIR_FAILURES + 1, PAIR_WINDOW, "wrong pairing codes").await?;
            return Err(AppError::new(ErrorCode::PairingCodeInvalid, "That pairing code is wrong, expired, or already used.")
                .hint("Codes change every 5 minutes. Enter the one the device shows now."));
        };
        let os: DeviceOs = serde_json::from_value(serde_json::Value::String(os)).map_err(AppError::internal)?;
        // "Pair with another Carbon": the new pair joins the device's other pairs, unless none is
        // left, in which case it pairs as a new device.
        let mut joined: Option<(Uuid, DeviceRow)> = None;
        if let Some(instance) = instance {
            domain::lock_instances(&mut add.tx, &world, &[instance]).await?;
            let pairs: Vec<DeviceRow> = sqlx::query_as(sql!(
                "{} WHERE d.instance_id = $1 AND d.removed_at IS NULL ORDER BY d.paired_at",
                domain::device_select(&world)
            ))
            .bind(instance)
            .fetch_all(&mut *add.tx)
            .await?;
            if let Some(mine) = pairs.iter().find(|d| d.owner_id == p.id()) {
                return Err(already_paired(&mine.name, &mine.device_id));
            }
            if pairs.len() as i64 >= st.cfg.tuning.max_pairs_per_device {
                return Err(max_pairs_error(pairs.len() as i64));
            }
            if let Some(sibling) = pairs.into_iter().next() {
                joined = Some((instance, sibling));
            }
        }
        if joined.is_none() {
            add.check_limit(&world)?;
        }
        let credential = ids::new_secret(ids::DEVICE_CREDENTIAL_PREFIX);
        let device_id = insert_device(
            &mut add.tx,
            &world,
            NewPair {
                team: &team,
                owner: p.id(),
                name: &name,
                os,
                ttl,
                os_version,
                model,
                app_version: Some(app_version),
                host: None,
                credential_digest: Some(ids::secret_digest(&credential)),
                address: None,
                instance: joined.as_ref().map(|(i, _)| *i),
                first_pair: joined.is_none(),
                provisional_until: None,
            },
        )
        .await?;
        // A new pair of a device already paired is ready at once: the app writes the same state
        // into every pair through each pair's hello, and until then it is the sibling's.
        match &joined {
            Some((_, sibling)) => {
                sqlx::query(sql!(
                    "UPDATE {d} n SET os = s.os, os_version = s.os_version, model = s.model, app_version = s.app_version,
                            agent_device_version = s.agent_device_version, capabilities = s.capabilities, missing = s.missing,
                            setup = s.setup, state = s.state
                     FROM {d} s WHERE n.device_id = $1 AND s.device_id = $2",
                    d = world.t("devices")
                ))
                .bind(&device_id)
                .bind(&sibling.device_id)
                .execute(&mut *add.tx)
                .await?;
            }
            None => {
                sqlx::query(sql!("UPDATE {} SET agent_device_version = $2 WHERE device_id = $1", world.t("devices")))
                    .bind(&device_id)
                    .bind(&engine_version)
                    .execute(&mut *add.tx)
                    .await?;
            }
        }
        for s in &input.silicon_ids {
            sqlx::query(sql!(
                "INSERT INTO {} (device_id, team, silicon_id, granted_by) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
                world.t("device_access")
            ))
            .bind(&device_id)
            .bind(&team)
            .bind(s)
            .bind(p.id())
            .execute(&mut *add.tx)
            .await?;
        }
        // Counted after the insert: a second pair of a device already here adds nothing.
        let environment = match &sel {
            Some(s) => Some(environment_view(s, paired_count(&mut *add.tx, &world).await?)),
            None => None,
        };
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
        let mut details = serde_json::json!({"name": name, "access": input.silicon_ids});
        if joined.is_some() {
            details["with_existing_pairs"] = serde_json::json!(true);
        }
        domain::log(&st, &world, &device_id, &p.member, "paired", None, details).await;
        if let Some((instance, _)) = &joined {
            // The other Carbons learn only that another Carbon paired it, never who.
            let others: Vec<(String,)> = sqlx::query_as(sql!(
                "SELECT device_id FROM {} WHERE instance_id = $1 AND removed_at IS NULL AND device_id <> $2",
                world.t("devices")
            ))
            .bind(instance)
            .bind(&device_id)
            .fetch_all(&st.pool)
            .await?;
            for (other,) in others {
                domain::log(&st, &world, &other, &domain::system_member(), "another_carbon_paired", None, serde_json::json!({})).await;
                let _ = st.hub.send(&(world.schema.clone(), other.clone()), ServiceFrame::Refresh).await;
            }
        }
        for s in &input.silicon_ids {
            domain::log_in(&st, &world, &device_id, &p.member, "access_granted", None, Some(&team), serde_json::json!({"silicon_id": s})).await;
        }
        delivery::register_carbon_if_new(&st, &world, &p, &team);
        let view = domain::device_view(&st, &world, &d, Viewer::owner(&d), false).await;
        tracing::info!(world = %world.schema, device_id, os = os.as_str(), joined = joined.is_some(), "device paired");
        Ok((StatusCode::CREATED, "device", serde_json::to_value(view).map_err(AppError::internal)?))
    })
    .await
}

pub fn max_pairs_error(n: i64) -> AppError {
    AppError::new(
        ErrorCode::Conflict,
        format!("This device is paired to {n} Carbons, the most Extend allows."),
    )
    .hint("One of them can revoke their pair on the device to make room.")
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
    let scope = q.scope.clone().unwrap_or_else(|| {
        if auth.p.is_silicon() {
            "accessible".into()
        } else {
            "mine".into()
        }
    });
    let lim = limit(q.limit)?;
    let (cond, access, team) = match (scope.as_str(), auth.p.is_silicon()) {
        // A Carbon's devices, whichever Team is selected: devices belong to the Carbons who paired them.
        ("mine", false) => ("d.owner_id = $1".to_owned(), Access::Owner, String::new()),
        // Devices are never visible to Team colleagues any more; kept (empty) for the 1.0
        // website's "Team devices" tab.
        ("team", false) => ("false".to_owned(), Access::Silicon, String::new()),
        ("accessible", true) => (
            format!(
                "EXISTS (SELECT 1 FROM {} a WHERE a.device_id = d.device_id AND a.team = $2 AND a.silicon_id = $1)",
                auth.world.t("device_access")
            ),
            Access::Silicon,
            auth.team()?.to_owned(),
        ),
        ("mine" | "team", true) => {
            return Err(AppError::invalid(
                "A Silicon lists the devices it has access to: use scope=accessible (the default).",
            ));
        }
        ("accessible", false) => {
            return Err(AppError::invalid(
                "A Carbon lists their devices with scope=mine (the default).",
            ));
        }
        (other, _) => {
            return Err(AppError::invalid(format!(
                "scope must be mine or accessible; got {other:?}."
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
    if include_removed && (access != Access::Owner || scope != "mine") {
        return Err(AppError::invalid(format!(
            "include_removed=true works only with scope=mine: a Carbon can list the devices they paired after \
             they're removed, to read their activity log. This request lists scope={scope}, which shows paired \
             devices only."
        ))
        .hint(
            "Drop include_removed, or, as the Carbon who paired the devices, send scope=mine&include_removed=true.",
        ));
    }
    if scope == "team" {
        return Ok(ok("devices", serde_json::json!({"items": [], "next_cursor": null})));
    }
    // `$2` (the Team) is mentioned in every scope so PostgreSQL can type it.
    let mut sql = format!(
        "{} WHERE ($2::text IS NULL OR $2 IS NOT NULL) AND {cond}",
        domain::device_select(&auth.world)
    );
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
    let viewer = Viewer {
        access,
        id: auth.p.id(),
        team: (!team.is_empty()).then_some(team.as_str()),
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
            .bind(auth.p.id())
            .bind(&team);
        if let Some(a) = &scanned_to {
            query = query.bind(a);
        }
        let fetched = query.fetch_all(&state.pool).await?;
        let exhausted = (fetched.len() as i64) < batch;
        for r in fetched {
            scanned_to = Some(r.device_id.clone());
            let v = domain::device_view(&state, &auth.world, &r, viewer, false).await;
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
    let view = domain::device_view(&state, &auth.world, &d, Viewer::of(access, &auth.p), true).await;
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
    // The in-use banner is the Carbons' to decide, never a Silicon's.
    if patch.in_use_indicator.is_some() {
        auth.require_carbon()?;
    }
    let d = domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    if patch.name.is_none()
        && patch.visibility.is_none()
        && patch.pair_ttl_days.is_none()
        && patch.in_use_indicator.is_none()
    {
        return Err(AppError::invalid(
            "Send at least one of name, pair_ttl_days, in_use_indicator.",
        ));
    }
    if patch.in_use_indicator == Some(InUseIndicator::Other) {
        return Err(AppError::invalid("in_use_indicator is \"shown\" or \"hidden\"."));
    }
    if_match(&headers, d.version)?;
    let name = patch.name.as_deref().map(clean_name).transpose()?;
    let ttl = patch.pair_ttl_days.map(|t| check_ttl(Some(t))).transpose()?;
    let mut tx = state.pool.begin().await?;
    // Same lock order as pairing, removal and the shared banner update: instance, then pairs.
    sqlx::query(sql!(
        "SELECT instance_id FROM {} WHERE instance_id = $1 FOR NO KEY UPDATE",
        auth.world.t("device_instances")
    ))
    .bind(d.instance_id)
    .execute(&mut *tx)
    .await?;
    let current: Option<i64> = sqlx::query_scalar(sql!(
        "SELECT version FROM {} WHERE device_id = $1 AND removed_at IS NULL FOR UPDATE",
        auth.world.t("devices")
    ))
    .bind(&device_id)
    .fetch_optional(&mut *tx)
    .await?;
    if current.is_none() {
        return Err(domain::device_not_found(&device_id));
    }
    if current != Some(d.version) {
        return Err(AppError::new(
            ErrorCode::VersionConflict,
            "The device changed while you were updating it.",
        )
        .hint("Read it again and retry."));
    }
    // `visibility` is accepted and ignored: a device is only ever visible to the Carbons who
    // paired it (a 1.0 website may still send it).
    if name.is_some() || ttl.is_some() {
        let updated = sqlx::query(sql!(
            "UPDATE {} SET name = COALESCE($2, name), pair_ttl_days = COALESCE($3, pair_ttl_days),
                    version = version + 1, last_activity_at = now() WHERE device_id = $1 AND version = $4",
            auth.world.t("devices")
        ))
        .bind(&device_id)
        .bind(&name)
        .bind(ttl)
        .bind(d.version)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(AppError::new(
                ErrorCode::VersionConflict,
                "The device changed while you were updating it.",
            )
            .hint("Read it again and retry."));
        }
    }
    let banner_changed = if let Some(value) = patch.in_use_indicator {
        domain::update_in_use_indicator(&mut tx, &auth.world, d.instance_id, value, Some(&device_id)).await?
    } else {
        false
    };
    tx.commit().await?;
    if name.is_some() || ttl.is_some() {
        let mut changes = serde_json::Map::new();
        if let Some(n) = &name {
            changes.insert("name".into(), serde_json::json!({"from": d.name, "to": n}));
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
        // Only this pair's connection learns the change: another Carbon's name is theirs.
        // Refresh makes a host re-read its own pair, so carried metadata needs an Attach frame.
        let frame = if d.host_device_id.is_some() {
            domain::load_device(&state, &auth.world, &device_id)
                .await?
                .ok_or_else(|| domain::device_not_found(&device_id))?
                .attach_frame(false)
        } else {
            ServiceFrame::Refresh
        };
        let _ = state.hub.send(&d.route(&auth.world), frame).await;
    }
    if banner_changed {
        domain::notify_in_use_indicator(
            &state,
            &auth.world,
            d.instance_id,
            patch.in_use_indicator.unwrap(),
            domain::BannerChangedBy::Carbon {
                device_id: &device_id,
                member: &auth.p.member,
            },
        )
        .await?;
    }
    let d = domain::load_device(&state, &auth.world, &device_id)
        .await?
        .ok_or_else(|| domain::device_not_found(&device_id))?;
    let view = domain::device_view(&state, &auth.world, &d, Viewer::owner(&d), true).await;
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
    tracing::info!(world = %world_name(&auth.world), device_id, "device removed");
    Ok(no_content())
}

fn world_name(w: &World) -> &str {
    &w.schema
}

/// Stops the Silicon using the physical device, from the website or CLI, by a Carbon who paired it.
/// It ends the session holding this pair's device, whichever pair it runs through; for a computer,
/// also the sessions on devices it carries that the caller paired too. It never ends a session on a
/// carried device the caller didn't pair: the computer's own Stop does that.
pub async fn stop(State(state): State<Shared>, auth: Auth, Path(device_id): Path<String>) -> AppResult<Response> {
    let d = domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    let me = auth.p.id();
    let mut here = None;
    let mut ended_other = false;
    if let Some(sid) = d.in_use_session.clone() {
        if d.held_here() {
            here = domain::end_session(&state, &auth.world, &sid, EndReason::StoppedByCarbon, &auth.p.member).await?;
        } else {
            // Logged on the session's pair, for its owner, as a Carbon who paired the device.
            ended_other = domain::end_session_with(
                &state,
                &auth.world,
                &sid,
                EndReason::StoppedByCarbon,
                &domain::system_member(),
                serde_json::json!({"stopped_by": "another_carbon"}),
            )
            .await?
            .is_some();
        }
    }
    let mut carried_blocked = false;
    for b in d.carried_busy() {
        if !b.owners.iter().any(|o| o == me) {
            carried_blocked = true;
            continue;
        }
        let ended = if b.carbon == me {
            domain::end_session(
                &state,
                &auth.world,
                &b.session_id,
                EndReason::StoppedByCarbon,
                &auth.p.member,
            )
            .await?
        } else {
            domain::end_session_with(
                &state,
                &auth.world,
                &b.session_id,
                EndReason::StoppedByCarbon,
                &domain::system_member(),
                serde_json::json!({"stopped_by": "another_carbon"}),
            )
            .await?
        };
        ended_other |= ended.is_some();
    }
    if let Some(row) = here {
        return Ok(ok("session", row.view()));
    }
    if ended_other {
        let stopped = DeviceStopped::new(d.device_id.parse().map_err(AppError::internal)?, domain::now());
        return Ok(ok("device_stopped", stopped));
    }
    if carried_blocked {
        return Err(AppError::new(
            ErrorCode::Conflict,
            format!(
                "A device carried by {} is in use. It can be stopped by the Carbon who paired it, or from {}'s Extend app.",
                d.name, d.name
            ),
        ));
    }
    Err(AppError::new(
        ErrorCode::DeviceNotInUse,
        format!("No Silicon is using {} right now.", d.name),
    ))
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
    let name = clean_name(&input.name)?;
    let ttl = check_ttl(input.pair_ttl_days)?;
    let hash = hash_json(&input);
    let world = auth.world.clone();
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
            // Existing attachments replay even when their host disconnected or capacity filled.
            if !st.hub.is_connected(&host.key(&world)).await {
                return Err(AppError::new(
                    ErrorCode::DeviceOffline,
                    format!(
                        "{} is offline; it has to be online to set up a device through it.",
                        host.name
                    ),
                )
                .hint("Open the Extend app on that computer and make sure it's connected."));
            }
            // In a test environment at its limit, a carried device may be one another Carbon already
            // carries through this same computer: it is accepted provisionally, counts nothing, and is
            // removed if the computer doesn't recognise it as that device within the link window.
            let candidate: bool = sqlx::query_scalar(sql!(
                "SELECT EXISTS (SELECT 1 FROM {d} c JOIN {d} h ON h.device_id = c.host_device_id
                                 WHERE h.instance_id = $1 AND c.os = $2 AND c.owner_id <> $3 AND c.removed_at IS NULL)",
                d = world.t("devices")
            ))
            .bind(host.instance_id)
            .bind(input.os.as_str())
            .bind(p.id())
            .fetch_one(&st.pool)
            .await?;
            if !candidate {
                test_limit(&st, &world).await?;
            }
            let mut add = begin_device_add(&st, &world).await?;
            let provisional_until = if world.is_test() && add.paired_before >= TEST_DEVICE_LIMIT {
                if !candidate {
                    return Err(test_limit_error());
                }
                Some(domain::now() + time::Duration::seconds(st.cfg.tuning.test_link_window_s))
            } else {
                None
            };
            let device_id = insert_device(
                &mut add.tx,
                &world,
                NewPair {
                    // Informational: the host's (the Carbon's own pair of the computer).
                    team: &host.team,
                    owner: p.id(),
                    name: &name,
                    os: input.os,
                    ttl,
                    os_version: None,
                    model: None,
                    app_version: None,
                    host: Some(host.device_id.clone()),
                    credential_digest: None,
                    address: input.address.clone(),
                    instance: None,
                    first_pair: true,
                    provisional_until,
                },
            )
            .await?;
            // Read back on the add's transaction (see claim), then commit.
            let d = domain::load_device_in(&mut *add.tx, &world, &device_id)
                .await?
                .ok_or_else(|| AppError::internal("attached device vanished"))?;
            add.commit().await?;
            st.hub.send(&host.key(&world), d.attach_frame(false)).await;
            domain::log(
                &st,
                &world,
                &device_id,
                &p.member,
                "paired",
                None,
                serde_json::json!({"name": name, "through": host.device_id, "provisional": provisional_until.is_some()}),
            )
            .await;
            let view = domain::device_view(&st, &world, &d, Viewer::owner(&d), false).await;
            Ok((
                StatusCode::CREATED,
                "device",
                serde_json::to_value(view).map_err(AppError::internal)?,
            ))
        },
    )
    .await
}

#[derive(Deserialize)]
pub struct SilicaQuery {
    team: Option<String>,
}

/// The Silicons a Carbon can give access to: in the X-Org-ID Team (1.0), or with `team=any` in
/// every Team the Carbon's login reaches, each tagged with its Team. A Team whose directory can't
/// be read is listed in `teams` with why, and the others still answer.
pub async fn team_silicons(
    State(state): State<Shared>,
    auth: Auth,
    Query(q): Query<SilicaQuery>,
) -> AppResult<Response> {
    if q.team.as_deref() != Some("any") {
        auth.team()?;
        let items = state.iam.team_silicons(&auth.p, auth.sel.as_ref()).await?;
        return Ok(ok("team_silicons", serde_json::json!({"items": items})));
    }
    auth.require_carbon()?;
    let mut items = Vec::new();
    let mut teams = Vec::new();
    for t in &auth.p.teams {
        let reached = async {
            let mut p = state.authorize(&auth.p.token, Some(t), auth.sel.as_ref()).await?;
            p.team = Some(t.clone());
            state.iam.team_silicons(&p, auth.sel.as_ref()).await
        }
        .await;
        match reached {
            Ok(list) => {
                items.extend(list.into_iter().map(|mut s| {
                    s.team = Some(t.clone());
                    s
                }));
                teams.push(TeamReach::reached(t.clone()));
            }
            Err(e) => teams.push(TeamReach::failed(t.clone(), *e.0)),
        }
    }
    Ok(ok("team_silicons", TeamSilicons::new(items, teams)))
}

pub async fn setup(State(state): State<Shared>, auth: Auth, Path(device_id): Path<String>) -> AppResult<Response> {
    let d = domain::owned_readable_device(&state, &auth.world, &device_id, &auth.p).await?;
    Ok(ok("setup", domain::setup_of(&state, &auth.world, &d).await))
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

/// `POST /devices/{id}/setup/retry`: asks the device (or, for a carried device, its computer) to
/// run a failed setup step again, or every failed step. The service changes no step itself; the
/// device reports progress as usual.
pub async fn setup_retry(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    body: Bytes,
) -> AppResult<Response> {
    auth.require_carbon()?;
    let d = domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    let input: SetupRetryInput = if body.iter().all(u8::is_ascii_whitespace) {
        SetupRetryInput::all()
    } else {
        let v: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|e| AppError::invalid(format!("The body is not valid JSON: {e}")).status(400))?;
        // Bare `{"step": ...}`, or the usual envelope.
        let data = if v.get("type").is_some() && v.get("data").is_some() {
            v["data"].clone()
        } else {
            v
        };
        serde_json::from_value(data)
            .map_err(|e| AppError::invalid(format!("The body is not a setup retry: {e}")).status(400))?
    };
    let setup = d.setup();
    let keys: Vec<String> = setup.steps.iter().map(|s| s.key.clone()).collect();
    let retrying: Vec<String> = match &input.step {
        Some(key) => {
            let Some(step) = setup.step(key) else {
                return Err(AppError::invalid(format!(
                    "{} has no setup step {key:?}. Its steps are: {}.",
                    d.name,
                    if keys.is_empty() {
                        "none".to_owned()
                    } else {
                        keys.join(", ")
                    }
                ))
                .hint(format!("See them with `extend device setup {device_id}`."))
                .status(400));
            };
            if step.status != StepStatus::Failed {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    format!("Nothing to retry: the step {key:?} hasn't failed."),
                )
                .hint(format!("Follow it with `extend device setup {device_id} --watch`.")));
            }
            vec![key.clone()]
        }
        None => setup.failed().map(|s| s.key.clone()).collect(),
    };
    if retrying.is_empty() {
        return Err(
            AppError::new(ErrorCode::Conflict, "Nothing to retry: no setup step has failed.").hint(format!(
                "Follow the setup with `extend device setup {device_id} --watch`."
            )),
        );
    }
    let route = d.route(&auth.world);
    let online = domain::is_online(&state, &auth.world, &d).await;
    let connected = state.hub.is_connected(&route).await;
    // A carried device's retry goes to its computer, which only needs to be connected.
    if !connected || (d.host_device_id.is_none() && !online) {
        return Err(AppError::new(
            ErrorCode::DeviceOffline,
            format!("{} is offline. Setup carries on when it reconnects.", d.name),
        )
        .hint("Open the Extend app on it (or on the computer it pairs through) and make sure it's connected.")
        .status(409));
    }
    if state.hub.supports(&route, extend_protocol::feature::SETUP_RETRY).await != Some(true) {
        let version = if d.host_device_id.is_some() {
            d.host_app_version.clone()
        } else {
            d.app_version.clone()
        }
        .unwrap_or_else(|| "an older version".into());
        return Err(AppError::new(
            ErrorCode::UpgradeRequired,
            format!(
                "{} runs Silicon Extend {version}, which can't retry from here. Update it to 1.1, or tap Retry on the device.",
                d.name
            ),
        ));
    }
    state
        .rate_limit(
            format!("setup-retry:{}:{device_id}", auth.world.schema),
            1,
            Duration::from_secs(extend_protocol::SETUP_RETRY_EVERY_S as u64),
            "setup retries for this device",
        )
        .await
        .map_err(|e| {
            let wait = e.0.details.get("retry_after_s").and_then(|v| v.as_i64()).unwrap_or(5);
            AppError::new(
                ErrorCode::RateLimited,
                format!(
                    "{} was asked to retry its setup a moment ago. Retry again in {wait} seconds.",
                    d.name
                ),
            )
            .details(serde_json::json!({"retry_after_s": wait}))
        })?;
    let target = d.host_device_id.as_ref().and_then(|_| d.device_id.parse().ok());
    let sent = state
        .hub
        .send(
            &route,
            ServiceFrame::SetupRetry {
                target,
                step: input.step.clone(),
            },
        )
        .await;
    if !sent {
        return Err(AppError::new(
            ErrorCode::DeviceOffline,
            format!("{} is offline. Setup carries on when it reconnects.", d.name),
        )
        .status(409));
    }
    domain::log(
        &state,
        &auth.world,
        &device_id,
        &auth.p.member,
        "setup_retry",
        None,
        serde_json::json!({"steps": retrying}),
    )
    .await;
    Ok(super::envelope(
        StatusCode::ACCEPTED,
        "setup_retry",
        RetryResult::new(retrying),
    ))
}

#[derive(Deserialize)]
pub struct TeamQuery {
    team: Option<String>,
}

fn grant_view(
    (d, s, g, at, used, team, muted): (
        String,
        String,
        String,
        OffsetDateTime,
        Option<OffsetDateTime>,
        String,
        bool,
    ),
) -> Option<AccessGrant> {
    Some(AccessGrant {
        device_id: d.parse().ok()?,
        silicon_id: s,
        granted_by: g,
        granted_at: at,
        last_used_at: used,
        team: Some(team),
        wake_muted: Some(muted),
    })
}

pub async fn access_list(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
) -> AppResult<Response> {
    domain::owned_readable_device(&state, &auth.world, &device_id, &auth.p).await?;
    let rows: Vec<(
        String,
        String,
        String,
        OffsetDateTime,
        Option<OffsetDateTime>,
        String,
        bool,
    )> = sqlx::query_as(sql!(
        "SELECT device_id, silicon_id, granted_by, granted_at, last_used_at, team, wake_muted FROM {}
         WHERE device_id = $1 ORDER BY granted_at",
        auth.world.t("device_access")
    ))
    .bind(&device_id)
    .fetch_all(&state.pool)
    .await?;
    let items: Vec<AccessGrant> = rows.into_iter().filter_map(grant_view).collect();
    Ok(ok("access", serde_json::json!({"items": items})))
}

/// Gives a Silicon access through this pair, in a Team (`?team=` or X-Org-ID) the Carbon's login
/// reaches. The Silicon must be active there, read with the Carbon's login for that Team.
pub async fn access_grant(
    State(state): State<Shared>,
    auth: Auth,
    Path((device_id, silicon_id)): Path<(String, String)>,
    Query(q): Query<TeamQuery>,
) -> AppResult<Response> {
    auth.require_carbon()?;
    let d = domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    let team = q
        .team
        .clone()
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
        .or_else(|| auth.p.team.clone())
        .ok_or_else(|| AppError::invalid("Say which Team the Silicon is in: --team <handle>"))?;
    let unreachable = || {
        AppError::new(
            ErrorCode::NotATeamMember,
            format!("{}'s Extend login doesn't reach {team}.", auth.p.id()),
        )
        .hint(format!(
            "Sign in to Extend again and select {team} (approve Extend for {team} in Silicon IAM), then retry."
        ))
    };
    if !auth.p.teams.contains(&team) {
        return Err(unreachable());
    }
    let mut p = state
        .authorize(&auth.p.token, Some(&team), auth.sel.as_ref())
        .await
        .map_err(|_| unreachable())?;
    p.team = Some(team.clone());
    check_silicons(&state, &p, auth.sel.as_ref(), std::slice::from_ref(&silicon_id)).await?;
    let inserted = sqlx::query(sql!(
        "INSERT INTO {} (device_id, team, silicon_id, granted_by) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        auth.world.t("device_access")
    ))
    .bind(&device_id)
    .bind(&team)
    .bind(&silicon_id)
    .bind(auth.p.id())
    .execute(&state.pool)
    .await?;
    if inserted.rows_affected() > 0 {
        domain::log_in(
            &state,
            &auth.world,
            &device_id,
            &auth.p.member,
            "access_granted",
            None,
            Some(&team),
            serde_json::json!({"silicon_id": silicon_id}),
        )
        .await;
        let _ = state.hub.send(&d.route(&auth.world), ServiceFrame::Refresh).await;
    }
    delivery::register_carbon_if_new(&state, &auth.world, &p, &team);
    let row: (
        String,
        String,
        String,
        OffsetDateTime,
        Option<OffsetDateTime>,
        String,
        bool,
    ) = sqlx::query_as(sql!(
        "SELECT device_id, silicon_id, granted_by, granted_at, last_used_at, team, wake_muted FROM {}
         WHERE device_id = $1 AND team = $2 AND silicon_id = $3",
        auth.world.t("device_access")
    ))
    .bind(&device_id)
    .bind(&team)
    .bind(&silicon_id)
    .fetch_one(&state.pool)
    .await?;
    Ok(ok(
        "access_grant",
        grant_view(row).ok_or_else(|| AppError::internal("grant has an unreadable device id"))?,
    ))
}

/// Takes a Silicon's access away: in `?team=`, or in every Team (the 1.0 meaning). It works on
/// ownership alone, even for a Team the Carbon's login no longer reaches.
pub async fn access_revoke(
    State(state): State<Shared>,
    auth: Auth,
    Path((device_id, silicon_id)): Path<(String, String)>,
    Query(q): Query<TeamQuery>,
) -> AppResult<Response> {
    domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    let team = q.team.as_deref().map(str::trim).filter(|t| !t.is_empty());
    domain::revoke_grants(
        &state,
        &auth.world,
        RevokeScope::Pair {
            device_id: &device_id,
            silicon_id: &silicon_id,
            team,
        },
        GrantEnd::Removed,
        &auth.p.member,
    )
    .await?;
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
    team: Option<String>,
}

pub async fn activity(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    Query(q): Query<ActivityQuery>,
) -> AppResult<Response> {
    // The log outlives the pair: the owner reads it after the device is removed too. Every Team's
    // rows, each tagged with its Team.
    domain::owned_readable_device(&state, &auth.world, &device_id, &auth.p).await?;
    let lim = limit(q.limit)?;
    let before: Option<Uuid> = q
        .cursor
        .as_deref()
        .map(decode_cursor)
        .transpose()?
        .and_then(|c| c.parse().ok());
    let rows: Vec<ActivityRow> = sqlx::query_as(sql!(
        "SELECT id, at, actor_kind, actor_id, action, session_id, command, args, outcome, files, details, team FROM {}
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
            team: r.team,
        })
        .collect();
    let next = more.then(|| encode_cursor(&items.last().map(|i| i.id.to_string()).unwrap_or_default()));
    Ok(ok("activity", serde_json::json!({"items": items, "next_cursor": next})))
}

#[derive(sqlx::FromRow)]
struct RequestRow {
    request_id: Uuid,
    device_id: String,
    team: String,
    from_id: String,
    to_id: String,
    session_id: Option<String>,
    reason: String,
    created_at: OffsetDateTime,
    delivery: String,
    last_error: Option<String>,
    routed_to: String,
    routed_to_id: Option<String>,
    holder_device_id: Option<String>,
    holder_session_id: Option<String>,
    /// The Carbon who owns the requester's pair.
    requester_carbon: Option<String>,
}

impl RequestRow {
    /// The request as `viewer` may see it. A request routed to the Silicon using the device reads
    /// as in 1.0. One routed to a Carbon: the requester's side sees only that it went to "the
    /// Carbon who gave access to the Silicon using it", never who, nor the holder's session; the
    /// Carbon it went to sees it on their own pair, with the asking Silicon and its reason (Carbon
    /// decision, 2026-09-27), and the asker's Team only when it is their own Silicon.
    fn view(self, viewer: &str) -> Option<RequestInfo> {
        let delivery = match self.delivery.as_str() {
            "delivered" => Delivery::Delivered,
            "failed" => Delivery::Failed,
            _ => Delivery::Pending,
        };
        let full_error = if self.delivery == "delivered" {
            None
        } else {
            self.last_error.clone()
        };
        if self.routed_to != "carbon" {
            return Some(RequestInfo {
                request_id: self.request_id,
                device_id: self.device_id.parse().ok()?,
                from: self.from_id,
                to: self.to_id,
                session_id: self.session_id.and_then(|s| s.parse().ok()),
                reason: self.reason,
                created_at: self.created_at,
                delivery,
                last_error: full_error,
                team: Some(self.team),
                routed_to: Some(RequestRoute::Holder),
                to_hidden: false,
                from_hidden: false,
            });
        }
        let recipient = self.routed_to_id.as_deref() == Some(viewer);
        if recipient {
            let own_silicon = self.requester_carbon.as_deref() == Some(viewer);
            return Some(RequestInfo {
                request_id: self.request_id,
                device_id: self
                    .holder_device_id
                    .as_deref()
                    .unwrap_or(&self.device_id)
                    .parse()
                    .ok()?,
                from: self.from_id,
                to: viewer.to_owned(),
                session_id: self.holder_session_id.and_then(|s| s.parse().ok()),
                reason: self.reason,
                created_at: self.created_at,
                delivery,
                last_error: full_error,
                team: own_silicon.then_some(self.team),
                routed_to: Some(RequestRoute::Carbon),
                to_hidden: false,
                from_hidden: false,
            });
        }
        let generic = match delivery {
            Delivery::Delivered => None,
            Delivery::Failed => Some("It couldn't be delivered.".to_owned()),
            _ => Some("Not delivered yet; it is retried.".to_owned()),
        };
        Some(RequestInfo {
            request_id: self.request_id,
            device_id: self.device_id.parse().ok()?,
            from: self.from_id,
            to: extend_protocol::REQUEST_TO_HIDDEN.to_owned(),
            session_id: None,
            reason: self.reason,
            created_at: self.created_at,
            delivery,
            last_error: generic,
            team: Some(self.team),
            routed_to: Some(RequestRoute::Carbon),
            to_hidden: true,
            from_hidden: false,
        })
    }
}

fn request_columns(world: &World) -> String {
    format!(
        "r.request_id, r.device_id, r.team, r.from_id, r.to_id, r.session_id, r.reason, r.created_at, r.delivery, r.last_error,
         r.routed_to, r.routed_to_id, r.holder_device_id, r.holder_session_id,
         (SELECT o.owner_id FROM {} o WHERE o.device_id = r.device_id) AS requester_carbon",
        world.t("devices")
    )
}

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
    viewer: &str,
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
        "SELECT {} FROM {} r WHERE {cond} AND (${}::uuid IS NULL OR r.request_id < ${}) ORDER BY r.request_id DESC LIMIT ${}",
        request_columns(world),
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
        .filter_map(|r| r.view(viewer))
        .collect();
    let next = more.then(|| encode_cursor(&items.last().map(|i| i.request_id.to_string()).unwrap_or_default()));
    Ok(ok("requests", serde_json::json!({"items": items, "next_cursor": next})))
}

/// The requests on a Carbon's pair: those its Silicons sent, and those routed to this Carbon
/// because one of their Silicons was using the device through this pair.
pub async fn requests_for_device(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    Query(q): Query<PageQuery>,
) -> AppResult<Response> {
    domain::owned_readable_device(&state, &auth.world, &device_id, &auth.p).await?;
    request_page(
        &state,
        &auth.world,
        auth.p.id(),
        "(r.device_id = $1 OR (r.routed_to = 'carbon' AND r.routed_to_id = $2 AND r.holder_device_id = $1))",
        vec![device_id, auth.p.id().to_owned()],
        &q,
    )
    .await
}

/// A Silicon's requests in the Team it acts in: those it sent, and those sent to it.
pub async fn my_requests(State(state): State<Shared>, auth: Auth, Query(q): Query<PageQuery>) -> AppResult<Response> {
    auth.require_silicon()?;
    let team = auth.team()?.to_owned();
    let dir = q.direction.as_deref().unwrap_or("all");
    let who = match dir {
        "sent" => "r.from_id = $2",
        "received" => "(r.to_id = $2 AND r.routed_to = 'holder')",
        "all" => "(r.from_id = $2 OR (r.to_id = $2 AND r.routed_to = 'holder'))",
        other => {
            return Err(AppError::invalid(format!(
                "direction must be sent, received or all; got {other:?}."
            )));
        }
    };
    let mut binds = vec![team, auth.p.id().to_owned()];
    let mut cond = format!("r.team = $1 AND {who}");
    if let Some(d) = &q.device_id {
        binds.push(d.clone());
        cond.push_str(" AND r.device_id = $3");
    }
    request_page(&state, &auth.world, auth.p.id(), &cond, binds, &q).await
}

/// Longest a request reason may be including the whitespace around it, which is kept and
/// delivered as sent (the reason itself is 1–300 characters without it).
const REASON_WITH_WHITESPACE_MAX_CHARS: usize = 1_000;

/// Serialises only the recent-request check and insert, including across service processes.
/// Distinct from the test-device limit's advisory-lock class.
const REQUEST_FOLD_LOCK_CLASS: i32 = 7_342_012;
const REQUEST_FOLD_WAIT: Duration = Duration::from_secs(5);

fn request_fold_busy() -> AppError {
    AppError::new(
        ErrorCode::ServiceUnavailable,
        "Another request with this reason is being saved. Try again in a moment.",
    )
    .hint("Retry shortly; use a new Idempotency-Key for a new attempt.")
    .details(serde_json::json!({"retry_after_s": 1}))
}

async fn begin_request_fold(state: &AppState, key: &str) -> AppResult<sqlx::Transaction<'static, sqlx::Postgres>> {
    let deadline = tokio::time::Instant::now() + REQUEST_FOLD_WAIT;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(request_fold_busy());
        }
        let mut tx = tokio::time::timeout_at(deadline, state.pool.begin())
            .await
            .map_err(|_| request_fold_busy())??;
        let acquired: bool = tokio::time::timeout_at(
            deadline,
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1, hashtext($2))")
                .bind(REQUEST_FOLD_LOCK_CLASS)
                .bind(key)
                .fetch_one(&mut *tx),
        )
        .await
        .map_err(|_| request_fold_busy())??;
        if acquired {
            return Ok(tx);
        }
        // A busy fold must not occupy a pooled connection while waiting for another process.
        tokio::time::timeout_at(deadline, tx.rollback())
            .await
            .map_err(|_| request_fold_busy())??;
        if tokio::time::Instant::now() >= deadline {
            return Err(request_fold_busy());
        }
        tokio::time::sleep_until((tokio::time::Instant::now() + Duration::from_millis(25)).min(deadline)).await;
    }
}

/// A request's or a wake request's reason: 1–300 characters not counting the whitespace around it,
/// which is kept exactly as written, and at most 1,000 with it.
pub fn check_reason(reason: &str) -> AppResult<()> {
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
    Ok(())
}

/// A Silicon asks for a device another Silicon is using. When that Silicon is on the asker's side
/// (same Team, given access through the same pair), the request goes to it as in 1.0. Otherwise it
/// goes to the Carbon who gave that Silicon access, who can stop the session: the asker sees only
/// that it went to "the Carbon who gave access to the Silicon using it".
pub async fn request_send(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    headers: HeaderMap,
    Body(input): Body<RequestCreate>,
) -> AppResult<Response> {
    auth.require_silicon()?;
    let team = auth.team()?.to_owned();
    let (d, access) = domain::visible_device(&state, &auth.world, &device_id, &auth.p).await?;
    if access != Access::Silicon {
        return Err(AppError::new(
            ErrorCode::NoAccess,
            format!("{} has no access to {}.", auth.p.id(), d.name),
        ));
    }
    crate::membership::owner_active(
        &state,
        &auth.world,
        &team,
        &d.owner_id,
        &d.name,
        &auth.p,
        auth.sel.as_ref(),
    )
    .await?;
    // The length is counted without surrounding whitespace, but the reason is stored and
    // delivered exactly as the Silicon wrote it.
    let reason = input.reason.clone();
    check_reason(&reason)?;
    let hash = hash_json(&input);
    let world = auth.world.clone();
    let st = state.clone();
    let p = auth.p.clone();
    let sel = auth.sel.clone();
    idempotent(
        &state,
        &auth.world,
        auth.p.id(),
        &format!("requests:{team}:{device_id}"),
        &headers,
        &hash,
        || async move {
            // A stored successful request still replays if the holder has since changed or
            // stopped. Current access was checked above; these live-state checks govern new work.
            let (Some(holder), Some(session), Some(holder_pair), Some(holder_team), Some(holder_carbon)) = (
                d.in_use_silicon.clone(),
                d.in_use_session.clone(),
                d.in_use_device_id.clone(),
                d.in_use_team.clone(),
                d.in_use_carbon.clone(),
            ) else {
                return Err(AppError::new(
                    ErrorCode::DeviceNotInUse,
                    format!("No Silicon is using {} right now, so there's nobody to ask.", d.name),
                )
                .hint(format!("Start using it: extend --team {team} session new {device_id}")));
            };
            if holder == p.id() {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    format!("You are already using {} in session {session}.", d.name),
                ));
            }
            let same_side = holder_pair == d.device_id && holder_team == team;
            let fold_key = serde_json::to_string(&(&world.schema, &device_id, &team, p.id(), &session, &reason))
                .map_err(AppError::internal)?;
            let app_id = st.notifier.app_id().to_owned();
            let mut tx = begin_request_fold(&st, &fold_key).await?;
            // Validate/replay the idempotency key before folding a recent same-reason request.
            // A repeat without a stored key keeps its 200 response and sends nothing; a new
            // reason is a new request. Team and holder session keep unrelated requests separate.
            let recent: Option<RequestRow> = sqlx::query_as(sql!(
                "SELECT {} FROM {} r WHERE r.device_id = $1 AND r.from_id = $2 AND r.team = $5 AND r.reason = $3
                   AND r.holder_session_id = $4 AND r.created_at > now() - interval '60 seconds'
                 ORDER BY r.created_at DESC LIMIT 1",
                request_columns(&world),
                world.t("requests")
            ))
            .bind(&device_id)
            .bind(p.id())
            .bind(&reason)
            .bind(&session)
            .bind(&team)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(r) = recent.and_then(|r| r.view(p.id())) {
                tx.commit().await?;
                return Ok((
                    StatusCode::OK,
                    "request",
                    serde_json::to_value(r).map_err(AppError::internal)?,
                ));
            }
            let id = Uuid::now_v7();
            let (to, session_col, routed_to, routed_to_id, ting_team, body, chain) = if same_side {
                let ting = DeviceRequestTing {
                    request_id: id,
                    device_id: &device_id,
                    device_name: &d.name,
                    from: p.id(),
                    to: &holder,
                    session_id: Some(&session),
                    reason: &reason,
                    routed_to: Some(RequestRoute::Holder),
                    team: Some(&team),
                    link: None,
                    from_hidden: false,
                };
                let body = crate::ting::request_body(&app_id, &team, &ting);
                (
                    holder.clone(),
                    Some(session.clone()),
                    "holder",
                    None,
                    team.clone(),
                    body,
                    vec![Actor::Fresh(p.clone(), sel.clone())],
                )
            } else {
                // The recipient's own name and id for the device.
                let their = domain::load_device_in(&mut *tx, &world, &holder_pair)
                    .await?
                    .ok_or_else(|| domain::device_not_found(&holder_pair))?;
                let own_carbon = holder_carbon == d.owner_id;
                // The Ting goes as the asking Silicon when it shares a Team with the recipient (the
                // Carbon's own Team with it, or the holder's Team when the asker's login reaches it);
                // otherwise as the recipient, to themselves, from their own login. Never as the
                // Silicon using the device.
                let (ting_team, chain) = if own_carbon {
                    (
                        team.clone(),
                        vec![
                            Actor::Fresh(p.clone(), sel.clone()),
                            Actor::Member(holder_carbon.clone()),
                        ],
                    )
                } else if p.teams.contains(&holder_team) {
                    (
                        holder_team.clone(),
                        vec![
                            Actor::Fresh(p.clone(), sel.clone()),
                            Actor::Member(holder_carbon.clone()),
                        ],
                    )
                } else {
                    (holder_team.clone(), vec![Actor::Member(holder_carbon.clone())])
                };
                let link = format!(
                    "{}/devices/{}",
                    st.cfg.website_url.trim_end_matches('/'),
                    their.device_id
                );
                let ting = DeviceRequestTing {
                    request_id: id,
                    device_id: &their.device_id,
                    device_name: &their.name,
                    from: p.id(),
                    to: &holder_carbon,
                    session_id: None,
                    reason: &reason,
                    routed_to: Some(RequestRoute::Carbon),
                    team: own_carbon.then_some(team.as_str()),
                    link: Some(link),
                    from_hidden: false,
                };
                let body = crate::ting::request_body(&app_id, &ting_team, &ting);
                (
                    extend_protocol::REQUEST_TO_HIDDEN.to_owned(),
                    None,
                    "carbon",
                    Some(holder_carbon.clone()),
                    ting_team,
                    body,
                    chain,
                )
            };
            sqlx::query(sql!(
            "INSERT INTO {} (request_id, device_id, team, from_id, to_id, session_id, reason, routed_to, routed_to_id,
                             holder_device_id, holder_team, holder_session_id, ting_team, ting_body)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
            world.t("requests")
        ))
            .bind(id)
            .bind(&device_id)
            .bind(&team)
            .bind(p.id())
            .bind(&to)
            .bind(&session_col)
            .bind(&reason)
            .bind(routed_to)
            .bind(&routed_to_id)
            .bind(&holder_pair)
            .bind(&holder_team)
            .bind(&session)
            .bind(&ting_team)
            .bind(&body)
            .execute(&mut *tx)
            .await?;
            // Delivery and activity helpers can call providers and acquire their own connections.
            // The atomic fold is complete before any of that work begins.
            tx.commit().await?;
            let attempt = delivery::send(&st, &world, &chain, &body).await;
            if !attempt.delivered {
                tracing::warn!(request_id = %id, error = ?attempt.error, "Ting delivery failed; will retry");
            }
            let wait = if attempt.missing_type {
                delivery::MISSING_TYPE_RETRY
            } else {
                delivery::backoff(1)
            };
            sqlx::query(sql!(
            "UPDATE {} SET delivery = CASE WHEN $2 THEN 'delivered' ELSE 'pending' END, attempts = $3, last_error = $4,
                    ting_next_at = CASE WHEN $2 THEN NULL WHEN $5 THEN 'infinity'::timestamptz ELSE now() + $6 END
             WHERE request_id = $1",
            world.t("requests")
        ))
            .bind(id)
            .bind(attempt.delivered)
            .bind(i32::from(attempt.tried))
            .bind(if attempt.delivered { None } else { attempt.error.clone() })
            .bind(attempt.not_registered)
            .bind(wait)
            .execute(&st.pool)
            .await?;
            // On the requester's pair, as the requester sees it (never the holder's session when it is
            // another side's); on the recipient's pair, as they see it.
            domain::log_in(
                &st,
                &world,
                &device_id,
                &p.member,
                "request_sent",
                session_col.as_deref(),
                Some(&team),
                serde_json::json!({"request_id": id, "to": to, "routed_to": routed_to, "reason": reason}),
            )
            .await;
            if holder_pair != device_id {
                domain::log_in(
                    &st,
                    &world,
                    &holder_pair,
                    &p.member,
                    "request_received",
                    Some(&session),
                    Some(&holder_team),
                    serde_json::json!({"request_id": id, "from": p.id(), "reason": reason}),
                )
                .await;
            }
            let row: RequestRow = sqlx::query_as(sql!(
                "SELECT {} FROM {} r WHERE r.request_id = $1",
                request_columns(&world),
                world.t("requests")
            ))
            .bind(id)
            .fetch_one(&st.pool)
            .await?;
            Ok((
                StatusCode::CREATED,
                "request",
                serde_json::to_value(row.view(p.id())).map_err(AppError::internal)?,
            ))
        },
    )
    .await
}

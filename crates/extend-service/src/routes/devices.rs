//! Pairing, devices, access, activity, requests between Silicons, and setup retries.
//!
//! A device belongs to the Carbon who paired it: they see it and give Silicons access to it, by
//! `si:` id (any active Silicon; the grant is the Carbon's decision about their own device). A
//! Silicon sees the devices it was given access to. Nobody else sees a pair.

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use extend_protocol::frames::{EnrollmentFrame, ServiceFrame};
use extend_protocol::model::{
    AccessGrant, ActivityEntry, AttachmentCreate, Delivery, DeviceSettingsPatch as DevicePatch, DeviceStopped,
    EndReason, InUseIndicator, MemberKind, PairingClaim, RequestCreate, RequestInfo, RequestRoute, RetryResult,
    SetupRetryInput, StepStatus, Visibility,
};
use extend_protocol::{DeviceId, DeviceOs, ErrorCode, PairingCode, ids};
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{Body, decode_cursor, encode_cursor, hash_json, idempotent, limit, no_content, ok};
use crate::accounts::{AccountRow, Principal};
use crate::db::World;
use crate::delivery;
use crate::domain::{self, Access, DeviceRow, GrantEnd, RevokeScope, Viewer};
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

/// Devices are private to the Carbon who paired them: `visibility` is always `personal`.
fn check_visibility(v: Option<Visibility>) -> AppResult<()> {
    if v.is_some_and(|v| v != Visibility::Personal) {
        return Err(AppError::invalid(
            "A device is private to the Carbon who paired it and the Silicons they give access to: visibility is always personal.",
        )
        .hint("Give a Silicon access with `extend device access grant <device_id> <si:id>`."));
    }
    Ok(())
}

/// Resolves the Silicons a Carbon names (`si:` ids, or uuids) through Silicon Accounts. Any active
/// Silicon may be given access; unknown ids are refused with what was wrong.
pub async fn resolve_silicons(state: &AppState, given: &[String]) -> AppResult<Vec<AccountRow>> {
    let mut out: Vec<AccountRow> = Vec::new();
    for s in given {
        let row = state.accounts.directory.resolve(s, Some(MemberKind::Silicon)).await?;
        if row.status != "active" {
            return Err(AppError::invalid(format!(
                "{} can't be given access yet: its account is {} in Silicon Accounts.",
                row.shown_id(),
                row.status.replace('_', " ")
            ))
            .hint("A Silicon can use devices once its custodian has accepted it."));
        }
        if !out.iter().any(|r| r.uuid == row.uuid) {
            out.push(row);
        }
    }
    Ok(out)
}

/// A new pair row. `instance` is the physical device when the pair joins one ("Pair with another
/// Carbon"), `None` for a new device.
struct NewPair<'a> {
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
                             host_device_id, credential_digest, address, instance_id, first_pair)
             VALUES ($1, '', $2, $3, $4, $5, $6, $7, 'personal', $8, $9, $10, $11, COALESCE($12, gen_random_uuid()), $13)",
            world.t("devices")
        ))
        .bind(id.as_str())
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

/// Gives each Silicon access through a pair (inside the caller's transaction).
async fn insert_grants(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    world: &World,
    device_id: &str,
    silicons: &[AccountRow],
    granted_by: &str,
) -> AppResult<Vec<String>> {
    let mut added = Vec::new();
    for s in silicons {
        let inserted = sqlx::query(sql!(
            "INSERT INTO {} (device_id, silicon_id, granted_by) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            world.t("device_access")
        ))
        .bind(device_id)
        .bind(&s.uuid)
        .bind(granted_by)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        if inserted > 0 {
            added.push(s.uuid.clone());
        }
    }
    Ok(added)
}

/// Logs `access_granted` on a pair, naming the Silicon by its id (and uuid).
async fn log_granted(state: &AppState, world: &World, device_id: &str, actor: &Principal, silicon: &AccountRow) {
    domain::log(
        state,
        world,
        device_id,
        &actor.actor(),
        "access_granted",
        None,
        serde_json::json!({"silicon_id": silicon.shown_id(), "silicon_uuid": silicon.uuid}),
    )
    .await;
}

pub async fn claim(
    State(state): State<Shared>,
    auth: Auth,
    headers: HeaderMap,
    Body(input): Body<PairingClaim>,
) -> AppResult<Response> {
    auth.require_carbon()?;
    auth.live(&state).await?;
    let limit_key = format!("pair:{}", auth.p.uuid());
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
    check_visibility(input.visibility)?;
    let silicons = resolve_silicons(&state, &input.silicon_ids).await?;
    let world = auth.world.clone();
    let hash = hash_json(&input);
    let p = auth.p.clone();
    let st = state.clone();
    idempotent(&state, &auth.world, auth.p.uuid(), "pairings", &headers, &hash, || async move {
        let mut tx = st.pool.begin().await?;
        let found: Option<(Uuid, String, Option<String>, Option<String>, String, Option<String>, Option<Uuid>)> = sqlx::query_as(
            "SELECT enrollment_id, os, os_version, model, app_version, agent_device_version, instance_id FROM extend_global.enrollments
             WHERE pairing_code = $1 AND code_expires_at > now() AND paired_device_id IS NULL AND world_schema = 'extend' FOR UPDATE",
        )
        .bind(code.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        let Some((enrollment_id, os, os_version, model, app_version, engine_version, instance)) = found else {
            drop(tx);
            st.rate_limit(limit_key.clone(), PAIR_FAILURES + 1, PAIR_WINDOW, "wrong pairing codes").await?;
            return Err(AppError::new(ErrorCode::PairingCodeInvalid, "That pairing code is wrong, expired, or already used.")
                .hint("Codes change every 5 minutes. Enter the one the device shows now."));
        };
        let os: DeviceOs = serde_json::from_value(serde_json::Value::String(os)).map_err(AppError::internal)?;
        // "Pair with another Carbon": the new pair joins the device's other pairs, unless none is
        // left, in which case it pairs as a new device.
        let mut joined: Option<(Uuid, DeviceRow)> = None;
        if let Some(instance) = instance {
            domain::lock_instances(&mut tx, &world, &[instance]).await?;
            let pairs: Vec<DeviceRow> = sqlx::query_as(sql!(
                "{} WHERE d.instance_id = $1 AND d.removed_at IS NULL ORDER BY d.paired_at",
                domain::device_select(&world)
            ))
            .bind(instance)
            .fetch_all(&mut *tx)
            .await?;
            if let Some(mine) = pairs.iter().find(|d| d.owner_id == p.uuid()) {
                return Err(already_paired(&mine.name, &mine.device_id));
            }
            if pairs.len() as i64 >= st.cfg.tuning.max_pairs_per_device {
                return Err(max_pairs_error(pairs.len() as i64));
            }
            if let Some(sibling) = pairs.into_iter().next() {
                joined = Some((instance, sibling));
            }
        }
        let credential = ids::new_secret(ids::DEVICE_CREDENTIAL_PREFIX);
        let device_id = insert_device(
            &mut tx,
            &world,
            NewPair {
                owner: p.uuid(),
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
                .execute(&mut *tx)
                .await?;
            }
            None => {
                sqlx::query(sql!("UPDATE {} SET agent_device_version = $2 WHERE device_id = $1", world.t("devices")))
                    .bind(&device_id)
                    .bind(&engine_version)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        let granted = insert_grants(&mut tx, &world, &device_id, &silicons, p.uuid()).await?;
        sqlx::query(
            "UPDATE extend_global.enrollments SET paired_schema = $2, paired_device_id = $3, paired_credential = $4, paired_environment = NULL
             WHERE enrollment_id = $1",
        )
        .bind(enrollment_id)
        .bind(&world.schema)
        .bind(&device_id)
        .bind(&credential)
        .execute(&mut *tx)
        .await?;
        // Read back before commit, so the answer after commit needs nothing that can fail: a
        // committed pairing always answers 201.
        let d = domain::load_device_in(&mut *tx, &world, &device_id)
            .await?
            .ok_or_else(|| AppError::internal("device vanished while pairing"))?;
        tx.commit().await?;
        st.rate_reset(&limit_key).await;
        st.hub
            .send_enrollment(
                enrollment_id,
                EnrollmentFrame::Paired { device_id: device_id.parse().map_err(AppError::internal)?, device_credential: credential, environment: None },
            )
            .await;
        let shown: Vec<String> = silicons.iter().map(AccountRow::shown_id).collect();
        let mut details = serde_json::json!({"name": name, "access": shown});
        if joined.is_some() {
            details["with_existing_pairs"] = serde_json::json!(true);
        }
        domain::log(&st, &world, &device_id, &p.actor(), "paired", None, details).await;
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
        for s in silicons.iter().filter(|s| granted.contains(&s.uuid)) {
            log_granted(&st, &world, &device_id, &p, s).await;
        }
        delivery::register_carbon_if_new(&st, &world, &p);
        let view = domain::device_view(&st, &world, &d, Viewer::of(Access::Owner, &p), false).await;
        tracing::info!(device_id, os = os.as_str(), joined = joined.is_some(), "device paired");
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
    let (cond, access) = match (scope.as_str(), auth.p.is_silicon()) {
        ("mine", false) => ("d.owner_id = $1".to_owned(), Access::Owner),
        ("accessible", true) => (
            format!(
                "EXISTS (SELECT 1 FROM {} a WHERE a.device_id = d.device_id AND a.silicon_id = $1)",
                auth.world.t("device_access")
            ),
            Access::Silicon,
        ),
        ("mine", true) => {
            return Err(AppError::invalid(
                "A Silicon lists the devices it has access to: use scope=accessible (the default).",
            ));
        }
        ("accessible", false) => {
            return Err(AppError::invalid(
                "A Carbon lists their devices with scope=mine (the default). The devices of a Silicon you look after: \
                 GET /api/v2/silicons/{silicon}/grants.",
            ));
        }
        (other, _) => {
            return Err(AppError::invalid(format!(
                "scope must be mine (Carbons) or accessible (Silicons); got {other:?}."
            ))
            .hint("Devices are private to the Carbon who paired them; there is no shared scope."));
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
    let mut sql = format!("{} WHERE {cond}", domain::device_select(&auth.world));
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
    // more (which says another page exists) or the devices run out.
    let batch = if q.online.is_some() {
        ONLINE_SCAN_BATCH.max(lim + 1)
    } else {
        lim + 1
    };
    let viewer = Viewer::of(access, &auth.p);
    let mut scanned_to = after;
    let mut rows: Vec<(DeviceRow, extend_protocol::model::Device)> = Vec::new();
    loop {
        let mut page_sql = sql.clone();
        if scanned_to.is_some() {
            page_sql.push_str(" AND d.device_id > $2");
        }
        page_sql.push_str(&format!(" ORDER BY d.device_id LIMIT {batch}"));
        let mut query = sqlx::query_as::<_, DeviceRow>(sqlx::AssertSqlSafe(page_sql)).bind(auth.p.uuid());
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
        )
        .hint("Read the device again and retry."));
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
    if patch.name.is_none() && patch.pair_ttl_days.is_none() && patch.in_use_indicator.is_none() {
        if patch.visibility.is_some() {
            check_visibility(patch.visibility)?;
        }
        return Err(AppError::invalid(
            "Send at least one of name, pair_ttl_days, in_use_indicator.",
        ));
    }
    check_visibility(patch.visibility)?;
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
            &auth.p.actor(),
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
    if let (true, Some(value)) = (banner_changed, patch.in_use_indicator) {
        let actor = auth.p.actor();
        domain::notify_in_use_indicator(
            &state,
            &auth.world,
            d.instance_id,
            value,
            domain::BannerChangedBy::Carbon {
                device_id: &device_id,
                member: &actor,
            },
        )
        .await?;
    }
    let d = domain::load_device(&state, &auth.world, &device_id)
        .await?
        .ok_or_else(|| domain::device_not_found(&device_id))?;
    let view = domain::device_view(&state, &auth.world, &d, Viewer::of(Access::Owner, &auth.p), true).await;
    Ok(with_etag(ok("device", view), d.version))
}

/// Removing a device ends the caller's pair of it: its sessions end, its grants and open wake
/// requests go, and the device forgets this Carbon. Other Carbons' pairs of the same device stay.
/// The pair's record and activity log stay readable to its Carbon.
pub async fn remove(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let d = domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    if_match(&headers, d.version)?;
    auth.live(&state).await?;
    domain::unpair(
        &state,
        &auth.world,
        &d.device_id,
        EndReason::DeviceRemoved,
        &auth.p.actor(),
    )
    .await?;
    tracing::info!(device_id, "device removed");
    Ok(no_content())
}

/// Stops the Silicon using the physical device, from the website or CLI, by a Carbon who paired it.
/// It ends the session holding this pair's device, whichever pair it runs through; for a computer,
/// also the sessions on devices it carries that the caller paired too. It never ends a session on a
/// carried device the caller didn't pair: the computer's own Stop does that.
pub async fn stop(State(state): State<Shared>, auth: Auth, Path(device_id): Path<String>) -> AppResult<Response> {
    let d = domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    let mut here = None;
    let mut ended_other = false;
    let mut carried_blocked = false;
    if d.held_here()
        && let Some(sid) = &d.in_use_session
    {
        here = domain::end_session(&state, &auth.world, sid, EndReason::StoppedByCarbon, &auth.p.actor()).await?;
    }
    for b in d.carried_busy() {
        if b.carbon != auth.p.uuid() {
            carried_blocked = true;
            continue;
        }
        ended_other |= domain::end_session(
            &state,
            &auth.world,
            &b.session_id,
            EndReason::StoppedByCarbon,
            &auth.p.actor(),
        )
        .await?
        .is_some();
    }
    if let Some(row) = here {
        return Ok(ok("session", row.view_for(&state).await));
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
    check_visibility(input.visibility)?;
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
        auth.p.uuid(),
        &format!("attachments:{}", host.device_id),
        &headers,
        &hash,
        || async move {
            // Existing attachments replay even when their host disconnected since.
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
            let mut tx = st.pool.begin().await?;
            domain::lock_instances(&mut tx, &world, &[host.instance_id]).await?;
            // Removing the host takes the same lock: once it has, nothing more attaches through it.
            let host_live: bool = sqlx::query_scalar(sql!(
                "SELECT EXISTS (SELECT 1 FROM {} WHERE device_id = $1 AND removed_at IS NULL)",
                world.t("devices")
            ))
            .bind(&host.device_id)
            .fetch_one(&mut *tx)
            .await?;
            if !host_live {
                return Err(AppError::new(
                    ErrorCode::DeviceNotFound,
                    format!("{} was removed, so nothing can be set up through it.", host.name),
                )
                .hint("Pair the computer again, then attach the device through it."));
            }
            let device_id = insert_device(
                &mut tx,
                &world,
                NewPair {
                    owner: p.uuid(),
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
                },
            )
            .await?;
            // Read back on the transaction (see claim), then commit.
            let d = domain::load_device_in(&mut *tx, &world, &device_id)
                .await?
                .ok_or_else(|| AppError::internal("attached device vanished"))?;
            tx.commit().await?;
            st.hub.send(&host.key(&world), d.attach_frame(false)).await;
            domain::log(
                &st,
                &world,
                &device_id,
                &p.actor(),
                "paired",
                None,
                serde_json::json!({"name": name, "through": host.device_id}),
            )
            .await;
            let view = domain::device_view(&st, &world, &d, Viewer::of(Access::Owner, &p), false).await;
            Ok((
                StatusCode::CREATED,
                "device",
                serde_json::to_value(view).map_err(AppError::internal)?,
            ))
        },
    )
    .await
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
            format!("setup-retry:{device_id}"),
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
        &auth.p.actor(),
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

type GrantRow = (String, String, String, OffsetDateTime, Option<OffsetDateTime>, bool);

/// A grant as the API shows it: the Silicon and the granting Carbon by current id and uuid.
async fn grant_view(state: &AppState, (d, s, g, at, used, muted): GrantRow) -> Option<AccessGrant> {
    let directory = &state.accounts.directory;
    Some(AccessGrant {
        device_id: d.parse().ok()?,
        silicon_id: directory.public_id(&s).await,
        granted_by: directory.public_id(&g).await,
        granted_at: at,
        last_used_at: used,
        team: None,
        wake_muted: Some(muted),
        silicon_uuid: Some(s),
        granted_by_uuid: Some(g),
        device_name: None,
        device_os: None,
        owner: None,
    })
}

const GRANT_COLUMNS: &str = "device_id, silicon_id, granted_by, granted_at, last_used_at, wake_muted";

pub async fn access_list(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
) -> AppResult<Response> {
    domain::owned_readable_device(&state, &auth.world, &device_id, &auth.p).await?;
    let rows: Vec<GrantRow> = sqlx::query_as(sql!(
        "SELECT {GRANT_COLUMNS} FROM {} WHERE device_id = $1 ORDER BY granted_at",
        auth.world.t("device_access")
    ))
    .bind(&device_id)
    .fetch_all(&state.pool)
    .await?;
    let mut items = Vec::new();
    for r in rows {
        if let Some(g) = grant_view(&state, r).await {
            items.push(g);
        }
    }
    Ok(ok("access", serde_json::json!({"items": items})))
}

/// Gives a Silicon access through this pair: any active Silicon, named by `si:` id (or uuid). It
/// is the Carbon's decision about their own device, so the Silicon doesn't have to accept it; the
/// grant shows to that Silicon and to its custodian.
pub async fn access_grant(
    State(state): State<Shared>,
    auth: Auth,
    Path((device_id, silicon)): Path<(String, String)>,
) -> AppResult<Response> {
    auth.require_carbon()?;
    let d = domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    auth.live(&state).await?;
    let silicons = resolve_silicons(&state, std::slice::from_ref(&silicon)).await?;
    let mut tx = state.pool.begin().await?;
    domain::lock_instances(&mut tx, &auth.world, &[d.instance_id]).await?;
    let still_paired: bool = sqlx::query_scalar(sql!(
        "SELECT EXISTS (SELECT 1 FROM {} WHERE device_id = $1 AND removed_at IS NULL)",
        auth.world.t("devices")
    ))
    .bind(&device_id)
    .fetch_one(&mut *tx)
    .await?;
    if !still_paired {
        return Err(domain::device_not_found(&device_id));
    }
    let added = insert_grants(&mut tx, &auth.world, &device_id, &silicons, auth.p.uuid()).await?;
    tx.commit().await?;
    let s = &silicons[0];
    if !added.is_empty() {
        log_granted(&state, &auth.world, &device_id, &auth.p, s).await;
        let _ = state.hub.send(&d.route(&auth.world), ServiceFrame::Refresh).await;
    }
    delivery::register_carbon_if_new(&state, &auth.world, &auth.p);
    let row: GrantRow = sqlx::query_as(sql!(
        "SELECT {GRANT_COLUMNS} FROM {} WHERE device_id = $1 AND silicon_id = $2",
        auth.world.t("device_access")
    ))
    .bind(&device_id)
    .bind(&s.uuid)
    .fetch_one(&state.pool)
    .await?;
    Ok(ok(
        "access_grant",
        grant_view(&state, row)
            .await
            .ok_or_else(|| AppError::internal("grant has an unreadable device id"))?,
    ))
}

/// Takes a Silicon's access away (`si:` id or uuid; a grant from before Silicon Accounts that was
/// never re-keyed is named by the old id it still holds).
pub async fn access_revoke(
    State(state): State<Shared>,
    auth: Auth,
    Path((device_id, silicon)): Path<(String, String)>,
) -> AppResult<Response> {
    domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
    auth.live(&state).await?;
    let stored: Option<String> = sqlx::query_scalar(sql!(
        "SELECT silicon_id FROM {} WHERE device_id = $1 AND silicon_id = $2",
        auth.world.t("device_access")
    ))
    .bind(&device_id)
    .bind(silicon.trim())
    .fetch_optional(&state.pool)
    .await?;
    let uuid = match stored {
        Some(s) => s,
        None => {
            state
                .accounts
                .directory
                .resolve(&silicon, Some(MemberKind::Silicon))
                .await?
                .uuid
        }
    };
    domain::revoke_grants(
        &state,
        &auth.world,
        RevokeScope::Pair {
            device_id: &device_id,
            silicon_id: &uuid,
        },
        GrantEnd::Removed,
        &auth.p.actor(),
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
}

pub async fn activity(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
    Query(q): Query<ActivityQuery>,
) -> AppResult<Response> {
    // The pair's history is its Carbon's, and outlives the pair.
    domain::owned_readable_device(&state, &auth.world, &device_id, &auth.p).await?;
    let lim = limit(q.limit)?;
    let before: Option<Uuid> = q
        .cursor
        .as_deref()
        .map(decode_cursor)
        .transpose()?
        .and_then(|c| c.parse().ok());
    let actor = match q.silicon_id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if extend_protocol::ids::member_kind(&s.to_ascii_lowercase()).is_some() => Some(
            state
                .accounts
                .directory
                .resolve(s, Some(MemberKind::Silicon))
                .await
                .map(|r| r.uuid)
                .unwrap_or_else(|_| s.to_owned()),
        ),
        other => other.map(str::to_owned),
    };
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
    .bind(&actor)
    .bind(&q.session_id)
    .bind(q.since)
    .bind(q.until)
    .bind(before)
    .bind(lim + 1)
    .fetch_all(&state.pool)
    .await?;
    let more = rows.len() as i64 > lim;
    let mut items: Vec<ActivityEntry> = Vec::new();
    for r in rows.into_iter().take(lim as usize) {
        let kind = if r.actor_kind == "silicon" {
            MemberKind::Silicon
        } else {
            MemberKind::Carbon
        };
        items.push(ActivityEntry {
            id: r.id,
            at: r.at,
            actor: state.accounts.directory.member(kind, &r.actor_id).await,
            action: r.action,
            session_id: r.session_id.and_then(|s| s.parse().ok()),
            command: r.command,
            args: r.args.and_then(|a| serde_json::from_value(a).ok()),
            outcome: r.outcome,
            files: serde_json::from_value(r.files).unwrap_or_default(),
            details: r.details.unwrap_or(serde_json::Value::Null),
            team: None,
        });
    }
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
    routed_to: String,
    routed_to_id: Option<String>,
    holder_device_id: Option<String>,
    holder_session_id: Option<String>,
}

impl RequestRow {
    /// The request as `viewer` (a uuid) may see it. A request routed to the Silicon using the
    /// device shows both Silicons. One routed to a Carbon: the requester's side sees only that it
    /// went to "the Carbon who gave access to the Silicon using it", never who, nor the holder's
    /// session; the Carbon it went to sees it on their own pair, with the asking Silicon and its
    /// reason (Carbon decision, 2026-09-27).
    async fn view(self, state: &AppState, viewer: &str) -> Option<RequestInfo> {
        let directory = &state.accounts.directory;
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
        let from = directory.public_id(&self.from_id).await;
        if self.routed_to != "carbon" {
            let to = directory.public_id(&self.to_id).await;
            return Some(RequestInfo {
                request_id: self.request_id,
                device_id: self.device_id.parse().ok()?,
                from,
                to,
                session_id: self.session_id.and_then(|s| s.parse().ok()),
                reason: self.reason,
                created_at: self.created_at,
                delivery,
                last_error: full_error,
                team: None,
                routed_to: Some(RequestRoute::Holder),
                to_hidden: false,
                from_hidden: false,
                from_uuid: Some(self.from_id),
                to_uuid: Some(self.to_id),
            });
        }
        if self.routed_to_id.as_deref() == Some(viewer) {
            return Some(RequestInfo {
                request_id: self.request_id,
                device_id: self
                    .holder_device_id
                    .as_deref()
                    .unwrap_or(&self.device_id)
                    .parse()
                    .ok()?,
                from,
                to: directory.public_id(viewer).await,
                session_id: self.holder_session_id.and_then(|s| s.parse().ok()),
                reason: self.reason,
                created_at: self.created_at,
                delivery,
                last_error: full_error,
                team: None,
                routed_to: Some(RequestRoute::Carbon),
                to_hidden: false,
                from_hidden: false,
                from_uuid: Some(self.from_id),
                to_uuid: Some(viewer.to_owned()),
            });
        }
        let generic = match delivery {
            Delivery::Delivered => None,
            Delivery::Failed => Some(
                full_error
                    .filter(|e| e.starts_with("Not sent: notifications through Ting are off"))
                    .unwrap_or_else(|| "It couldn't be delivered.".to_owned()),
            ),
            _ => Some("Not delivered yet; it is retried.".to_owned()),
        };
        Some(RequestInfo {
            request_id: self.request_id,
            device_id: self.device_id.parse().ok()?,
            from,
            to: extend_protocol::REQUEST_TO_HIDDEN.to_owned(),
            session_id: None,
            reason: self.reason,
            created_at: self.created_at,
            delivery,
            last_error: generic,
            team: None,
            routed_to: Some(RequestRoute::Carbon),
            to_hidden: true,
            from_hidden: false,
            from_uuid: Some(self.from_id),
            to_uuid: None,
        })
    }
}

const REQUEST_COLUMNS: &str = "r.request_id, r.device_id, r.from_id, r.to_id, r.session_id, r.reason, r.created_at, r.delivery, \
     r.last_error, r.routed_to, r.routed_to_id, r.holder_device_id, r.holder_session_id";

#[derive(Deserialize)]
pub struct PageQuery {
    limit: Option<i64>,
    cursor: Option<String>,
    direction: Option<String>,
    device_id: Option<String>,
    /// A Carbon's view of a Silicon they look after (`si:` id or uuid).
    silicon: Option<String>,
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
        "SELECT {REQUEST_COLUMNS} FROM {} r WHERE ({cond}) AND (${}::uuid IS NULL OR r.request_id < ${}) ORDER BY r.request_id DESC LIMIT ${}",
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
    let mut items: Vec<RequestInfo> = Vec::new();
    for r in rows.into_iter().take(lim as usize) {
        if let Some(v) = r.view(state, viewer).await {
            items.push(v);
        }
    }
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
        auth.p.uuid(),
        "r.device_id = $1 OR (r.routed_to = 'carbon' AND r.routed_to_id = $2 AND r.holder_device_id = $1)",
        vec![device_id, auth.p.uuid().to_owned()],
        &q,
    )
    .await
}

/// A Silicon's requests: those it sent, and those sent to it, on devices it still has access to.
/// A Carbon sees a Silicon they look after this way with `?silicon=`.
pub async fn my_requests(State(state): State<Shared>, auth: Auth, Query(q): Query<PageQuery>) -> AppResult<Response> {
    let silicon = match (&q.silicon, auth.p.is_silicon()) {
        (None, true) => auth.p.uuid().to_owned(),
        (Some(s), false) => super::silicons::looked_after(&state, &auth, s).await?.uuid,
        (Some(_), true) => {
            return Err(AppError::invalid(
                "A Silicon sees its own requests; leave out ?silicon=.",
            ));
        }
        (None, false) => {
            return Err(AppError::invalid(
                "A Carbon sees the requests on their devices with GET /api/v2/devices/{device_id}/requests, or a Silicon they look after's with ?silicon=<si:id>.",
            ));
        }
    };
    let dir = q.direction.as_deref().unwrap_or("all");
    let who = match dir {
        "sent" => "r.from_id = $1",
        "received" => "(r.to_id = $1 AND r.routed_to = 'holder')",
        "all" => "(r.from_id = $1 OR (r.to_id = $1 AND r.routed_to = 'holder'))",
        other => {
            return Err(AppError::invalid(format!(
                "direction must be sent, received or all; got {other:?}."
            )));
        }
    };
    let mut binds = vec![silicon.clone()];
    let mut cond = format!(
        "{who} AND EXISTS (SELECT 1 FROM {} a WHERE a.device_id = CASE WHEN r.from_id = $1 THEN r.device_id ELSE COALESCE(r.holder_device_id, r.device_id) END
                                              AND a.silicon_id = $1)",
        auth.world.t("device_access")
    );
    if let Some(d) = &q.device_id {
        binds.push(d.clone());
        cond.push_str(" AND r.device_id = $2");
    }
    request_page(&state, &auth.world, &silicon, &cond, binds, &q).await
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

/// A Silicon asks for a device another Silicon is using. When the holder runs through the same
/// pair (the same Carbon gave both access) and is in the asker's custodian circle, the request goes
/// to it. Otherwise it goes to the Carbon who gave the holder access, who can stop the session; the
/// asker sees only that it went to "the Carbon who gave access to the Silicon using it". So a
/// Silicon never hears from outside its circle unasked.
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
            format!("{} has no access to {}.", auth.p.public_id(), d.name),
        ));
    }
    auth.live(&state).await?;
    // The length is counted without surrounding whitespace, but the reason is stored and
    // delivered exactly as the Silicon wrote it.
    let reason = input.reason.clone();
    check_reason(&reason)?;
    let hash = hash_json(&input);
    let world = auth.world.clone();
    let st = state.clone();
    let p = auth.p.clone();
    idempotent(
        &state,
        &auth.world,
        auth.p.uuid(),
        &format!("requests:{device_id}"),
        &headers,
        &hash,
        || async move {
            // A stored successful request still replays if the holder has since changed or
            // stopped. Current access was checked above; these live-state checks govern new work.
            let (Some(holder), Some(session), Some(holder_pair), Some(holder_carbon)) = (
                d.in_use_silicon.clone(),
                d.in_use_session.clone(),
                d.in_use_device_id.clone(),
                d.in_use_carbon.clone(),
            ) else {
                return Err(AppError::new(
                    ErrorCode::DeviceNotInUse,
                    format!("No Silicon is using {} right now, so there's nobody to ask.", d.name),
                )
                .hint(format!("Start using it: extend session new {device_id}")));
            };
            if holder == p.uuid() {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    format!("You are already using {} in session {session}.", d.name),
                ));
            }
            let directory = &st.accounts.directory;
            let to_holder = holder_pair == d.device_id && directory.same_circle(p.uuid(), &holder).await;
            let fold_key = serde_json::to_string(&(&device_id, p.uuid(), &session, &reason)).map_err(AppError::internal)?;
            let app_id = st.notifier.app_id().to_owned();
            let mut tx = begin_request_fold(&st, &fold_key).await?;
            domain::lock_instances(&mut tx, &world, &[d.instance_id]).await?;
            let granted: bool = sqlx::query_scalar(sql!(
                "SELECT EXISTS (SELECT 1 FROM {} WHERE device_id = $1 AND silicon_id = $2)",
                world.t("device_access")
            ))
            .bind(&device_id)
            .bind(p.uuid())
            .fetch_one(&mut *tx)
            .await?;
            if !granted {
                return Err(AppError::new(
                    ErrorCode::NoAccess,
                    format!("{} has no access to {} any more.", p.public_id(), d.name),
                ));
            }
            // A repeat of the same reason within 60 s to the same holder session keeps its first
            // answer and sends nothing; a new reason is a new request.
            let recent: Option<RequestRow> = sqlx::query_as(sql!(
                "SELECT {REQUEST_COLUMNS} FROM {} r WHERE r.device_id = $1 AND r.from_id = $2 AND r.reason = $3
                   AND r.holder_session_id = $4 AND r.created_at > now() - interval '60 seconds'
                 ORDER BY r.created_at DESC LIMIT 1",
                world.t("requests")
            ))
            .bind(&device_id)
            .bind(p.uuid())
            .bind(&reason)
            .bind(&session)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(r) = recent {
                tx.commit().await?;
                let view = r.view(&st, p.uuid()).await;
                return Ok((StatusCode::OK, "request", serde_json::to_value(view).map_err(AppError::internal)?));
            }
            let id = Uuid::now_v7();
            let (to, session_col, routed_to, routed_to_id, body) = if to_holder {
                let holder_id = directory.public_id(&holder).await;
                let ting = DeviceRequestTing {
                    request_id: id,
                    device_id: &device_id,
                    device_name: &d.name,
                    from: p.public_id(),
                    to: &holder,
                    to_id: &holder_id,
                    session_id: Some(&session),
                    reason: &reason,
                    routed_to: RequestRoute::Holder,
                    link: None,
                };
                let body = crate::ting::request_body(&app_id, &ting);
                (holder.clone(), Some(session.clone()), "holder", None, body)
            } else {
                // The recipient's own name and id for the device.
                let their = domain::load_device_in(&mut *tx, &world, &holder_pair)
                    .await?
                    .ok_or_else(|| domain::device_not_found(&holder_pair))?;
                let carbon_id = directory.public_id(&holder_carbon).await;
                let link = format!(
                    "{}/devices/{}",
                    st.cfg.website_url.trim_end_matches('/'),
                    their.device_id
                );
                let ting = DeviceRequestTing {
                    request_id: id,
                    device_id: &their.device_id,
                    device_name: &their.name,
                    from: p.public_id(),
                    to: &holder_carbon,
                    to_id: &carbon_id,
                    session_id: None,
                    reason: &reason,
                    routed_to: RequestRoute::Carbon,
                    link: Some(link),
                };
                let body = crate::ting::request_body(&app_id, &ting);
                (
                    extend_protocol::REQUEST_TO_HIDDEN.to_owned(),
                    None,
                    "carbon",
                    Some(holder_carbon.clone()),
                    body,
                )
            };
            sqlx::query(sql!(
                "INSERT INTO {} (request_id, device_id, from_id, to_id, session_id, reason, routed_to, routed_to_id,
                                 holder_device_id, holder_session_id, ting_body)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
                world.t("requests")
            ))
            .bind(id)
            .bind(&device_id)
            .bind(p.uuid())
            .bind(&to)
            .bind(&session_col)
            .bind(&reason)
            .bind(routed_to)
            .bind(&routed_to_id)
            .bind(&holder_pair)
            .bind(&session)
            .bind(&body)
            .execute(&mut *tx)
            .await?;
            // Delivery and activity helpers can call providers and acquire their own connections.
            // The atomic fold is complete before any of that work begins.
            tx.commit().await?;
            let attempt = delivery::send(&st, &world, &body).await;
            if !attempt.delivered && !attempt.disabled {
                tracing::warn!(request_id = %id, error = ?attempt.error, "Ting delivery failed; will retry");
            }
            let wait = if attempt.missing_type {
                delivery::MISSING_TYPE_RETRY
            } else {
                delivery::backoff(1)
            };
            sqlx::query(sql!(
                "UPDATE {} SET delivery = CASE WHEN $2 THEN 'delivered' WHEN $7 THEN 'failed' ELSE 'pending' END,
                        attempts = $3, last_error = $4,
                        ting_next_at = CASE WHEN $2 OR $7 THEN NULL WHEN $5 THEN 'infinity'::timestamptz ELSE now() + $6 END
                 WHERE request_id = $1",
                world.t("requests")
            ))
            .bind(id)
            .bind(attempt.delivered)
            .bind(i32::from(attempt.tried))
            .bind(if attempt.delivered { None } else { attempt.error.clone() })
            .bind(attempt.not_registered)
            .bind(wait)
            .bind(attempt.disabled)
            .execute(&st.pool)
            .await?;
            // On the requester's pair, as the requester sees it (never the holder's session when it
            // is another side's); on the recipient's pair, as they see it.
            let shown_to = if to_holder { directory.public_id(&holder).await } else { to.clone() };
            domain::log(
                &st,
                &world,
                &device_id,
                &p.actor(),
                "request_sent",
                session_col.as_deref(),
                serde_json::json!({"request_id": id, "to": shown_to, "routed_to": routed_to, "reason": reason}),
            )
            .await;
            if holder_pair != device_id {
                domain::log(
                    &st,
                    &world,
                    &holder_pair,
                    &p.actor(),
                    "request_received",
                    Some(&session),
                    serde_json::json!({"request_id": id, "from": p.public_id(), "from_uuid": p.uuid(), "reason": reason}),
                )
                .await;
            }
            let row: RequestRow = sqlx::query_as(sql!(
                "SELECT {REQUEST_COLUMNS} FROM {} r WHERE r.request_id = $1",
                world.t("requests")
            ))
            .bind(id)
            .fetch_one(&st.pool)
            .await?;
            Ok((
                StatusCode::CREATED,
                "request",
                serde_json::to_value(row.view(&st, p.uuid()).await).map_err(AppError::internal)?,
            ))
        },
    )
    .await
}

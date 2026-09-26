//! Pairing, devices, access, activity, and requests between Silicons.

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
use crate::state::{Auth, Shared};
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
        return Err(AppError::new(ErrorCode::TestDeviceLimit, TEST_DEVICE_LIMIT_MESSAGE)
            .hint("Remove a device from this test environment first, with `extend device rm <device_id> --yes`."));
    }
    Ok(())
}

pub async fn env_view(
    state: &crate::state::AppState,
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
    Some(TestingEnvironment {
        environment_id: s.environment_id,
        name: s.name.clone(),
        state: "ready".into(),
        paired_devices: n,
        device_limit: TEST_DEVICE_LIMIT,
    })
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
        let mut tx = st.pool.begin().await?;
        let found: Option<(Uuid, String, Option<String>, Option<String>, String)> = sqlx::query_as(
            "SELECT enrollment_id, os, os_version, model, app_version FROM extend_global.enrollments
             WHERE pairing_code = $1 AND code_expires_at > now() AND paired_device_id IS NULL FOR UPDATE",
        )
        .bind(code.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        let Some((enrollment_id, os, os_version, model, app_version)) = found else {
            drop(tx);
            st.rate_limit(limit_key.clone(), PAIR_FAILURES + 1, PAIR_WINDOW, "wrong pairing codes").await?;
            return Err(AppError::new(ErrorCode::PairingCodeInvalid, "That pairing code is wrong, expired, or already used.")
                .hint("Codes change every 5 minutes. Enter the one the device shows now."));
        };
        let os: DeviceOs = serde_json::from_value(serde_json::Value::String(os)).map_err(AppError::internal)?;
        let credential = ids::new_secret(ids::DEVICE_CREDENTIAL_PREFIX);
        let device_id = insert_device(
            &mut tx,
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
                .execute(&mut *tx)
                .await?;
        }
        let environment = env_view(&st, sel.as_ref(), &world).await.map(|mut e| {
            e.paired_devices += 1;
            e
        });
        sqlx::query(
            "UPDATE extend_global.enrollments SET paired_schema = $2, paired_device_id = $3, paired_credential = $4, paired_environment = $5
             WHERE enrollment_id = $1",
        )
        .bind(enrollment_id)
        .bind(&world.schema)
        .bind(&device_id)
        .bind(&credential)
        .bind(environment.as_ref().map(|e| serde_json::to_value(e).unwrap_or_default()))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
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
        let d = domain::load_device(&st, &world, &device_id).await?.ok_or_else(|| AppError::internal("device vanished after pairing"))?;
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
}

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
    let mut sql = format!(
        "{} WHERE d.removed_at IS NULL AND d.team = $1 AND {cond}",
        domain::device_select(&auth.world)
    );
    if let Some(os) = &q.os {
        let os: DeviceOs = serde_json::from_value(serde_json::Value::String(os.clone()))
            .map_err(|_| AppError::invalid(format!("unknown os {os:?}")))?;
        sql.push_str(&format!(" AND d.os = '{}'", os.as_str()));
    }
    let after = q.cursor.as_deref().map(decode_cursor).transpose()?;
    if after.is_some() {
        sql.push_str(" AND d.device_id > $3");
    }
    sql.push_str(&format!(" ORDER BY d.device_id LIMIT {}", lim + 1));
    let mut query = sqlx::query_as::<_, DeviceRow>(sqlx::AssertSqlSafe(sql.clone()))
        .bind(&team)
        .bind(auth.p.id());
    if let Some(a) = &after {
        query = query.bind(a);
    }
    let mut rows = query.fetch_all(&state.pool).await?;
    let next = (rows.len() as i64 > lim).then(|| {
        rows.truncate(lim as usize);
        encode_cursor(&rows.last().map(|r| r.device_id.clone()).unwrap_or_default())
    });
    let mut items = Vec::new();
    for r in &rows {
        let v = domain::device_view(&state, &auth.world, r, access, false).await;
        if q.online.is_none_or(|o| o == v.online) {
            items.push(v);
        }
    }
    items.sort_by(|a, b| {
        b.online
            .cmp(&a.online)
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
    let (d, access) = domain::visible_device(&state, &auth.world, &device_id, &auth.p).await?;
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
        )));
    }
    if !input.os.allowed_hosts().contains(&host.os()) {
        let hosts: Vec<&str> = input.os.allowed_hosts().iter().map(|o| o.as_str()).collect();
        return Err(AppError::invalid(format!(
            "{} devices pair through a {}; {} is a {}.",
            input.os.as_str(),
            hosts.join(" or "),
            host.name,
            host.os().as_str()
        )));
    }
    if host.host_device_id.is_some() {
        return Err(AppError::invalid(
            "A device paired through a computer can't carry other devices.",
        ));
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
            let mut tx = st.pool.begin().await?;
            let device_id = insert_device(
                &mut tx,
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
            tx.commit().await?;
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
            let d = domain::load_device(&st, &world, &device_id)
                .await?
                .ok_or_else(|| AppError::internal("attached device vanished"))?;
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
    let d = domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
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
    let code = input.code.trim();
    if code.len() != 4 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AppError::invalid("The setup code is the 4 digits the Apple TV shows."));
    }
    if d.host_device_id.is_none() {
        return Err(AppError::invalid(
            "Only devices paired through a computer take a setup code.",
        ));
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
            "The computer this device pairs through is offline.",
        ));
    }
    Ok(ok("setup", d.setup()))
}

pub async fn access_list(
    State(state): State<Shared>,
    auth: Auth,
    Path(device_id): Path<String>,
) -> AppResult<Response> {
    domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
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
    domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
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
        })
    }
}

const REQUEST_COLUMNS: &str = "request_id, device_id, from_id, to_id, session_id, reason, created_at, delivery";

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
    domain::owned_device(&state, &auth.world, &device_id, &auth.p).await?;
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
    let reason = input.reason.trim().to_owned();
    let n = reason.chars().count();
    if n == 0 || n > extend_protocol::REASON_MAX_CHARS {
        return Err(AppError::invalid(format!(
            "The reason must be 1–300 characters; it is {n}."
        )));
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
    // One open request per Silicon per device per minute; a repeat returns the existing one.
    let recent: Option<RequestRow> = sqlx::query_as(sql!(
        "SELECT {REQUEST_COLUMNS} FROM {} WHERE device_id = $1 AND from_id = $2 AND created_at > now() - interval '60 seconds' ORDER BY created_at DESC LIMIT 1",
        auth.world.t("requests")
    ))
    .bind(&device_id)
    .bind(auth.p.id())
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
                sqlx::query(sql!("UPDATE {} SET attempts = attempts + 1, last_error = $2 WHERE request_id = $1", world.t("requests")))
                    .bind(id)
                    .bind(&e.0.message)
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

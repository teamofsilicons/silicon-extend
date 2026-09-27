//! Sessions: one Silicon at a time on a device, commands relayed to it, takeovers.

use std::time::Duration;

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use base64::Engine as _;
use extend_protocol::capability::{self, Origin, RESERVED_FLAGS};
use extend_protocol::frames::{CommandFrame, CommandOutcome, ServiceFrame};
use extend_protocol::model::{
    CommandError, CommandRequest, CommandResult, EndReason, FileInfo, FileKind, SessionCreate, Takeover, TakeoverCreate,
};
use extend_protocol::{
    COMMAND_TIMEOUT_DEFAULT_MS, COMMAND_TIMEOUT_MAX_MS, COMMAND_TIMEOUT_MIN_MS, ErrorCode, SELF_DESTRUCT_DEFAULT_MIN,
    SELF_DESTRUCT_MAX_MIN, SESSION_IDLE_S, SessionId, TAKEOVER_MAX_S, TEAM_HEADER,
};
use rand::Rng as _;
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{Body, decode_cursor, encode_cursor, hash_json, idempotent, limit, no_content, ok};
use crate::db::World;
use crate::domain::{self, Access, SESSION_COLUMNS, SessionRow};
use crate::error::{AppError, AppResult};
use crate::files::NewFile;
use crate::hub::SendError;
use crate::iam::{Principal, TestingSelection};
use crate::state::{AppState, Auth, Sel, Shared};

/// How long a Silicon whose access token IAM refused has to come back with a live login before
/// its running sessions end as `silicon_logged_out`. An expired token is refreshed and retried by
/// the CLI within seconds; a revoked one (a logout anywhere) never comes back.
pub const LOGOUT_GRACE: Duration = Duration::from_secs(15);

/// [`Auth`] for the session routes. When IAM refuses a Silicon that has running sessions, its
/// sessions end too, so logging out or leaving the team ends access even when no IAM webhook
/// arrives (TECHNICAL.md section 9):
/// - an active login that no longer reaches the session's team (`not_a_team_member`), once IAM
///   confirms the Silicon is no longer an active member, ends those sessions at once as
///   `left_team`;
/// - a login IAM no longer accepts (`token_expired`) ends every running session of that Silicon as
///   `silicon_logged_out` after [`LOGOUT_GRACE`], unless the Silicon has used a live login since.
pub struct SessionAuth(pub Auth);

impl FromRequestParts<Shared> for SessionAuth {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &Shared) -> AppResult<Self> {
        match Auth::from_request_parts(parts, state).await {
            Ok(auth) => Ok(Self(auth)),
            Err(e) => Err(on_refused(state, parts, e).await),
        }
    }
}

fn bearer_token(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

/// The Silicon a token belonged to, from the logins Extend saw it use in this world.
async fn silicon_of(state: &AppState, world: &World, token: &str) -> Option<Principal> {
    let from_sessions = state
        .session_principals
        .read()
        .await
        .iter()
        .find(|((schema, _), (p, _))| schema == &world.schema && p.is_silicon() && p.token == token)
        .map(|(_, (p, _))| p.clone());
    match from_sessions {
        Some(p) => Some(p),
        None => {
            state
                .auth_cache
                .latest(world.environment_id, |p| p.is_silicon() && p.token == token)
                .await
        }
    }
}

async fn running_sessions(state: &AppState, world: &World, silicon: &str, team: Option<&str>) -> Vec<String> {
    sqlx::query_as::<_, (String,)>(sql!(
        "SELECT session_id FROM {} WHERE silicon_id = $1 AND ($2::text IS NULL OR team = $2) AND state <> 'ended' ORDER BY started_at",
        world.t("sessions")
    ))
    .bind(silicon)
    .bind(team)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(s,)| s)
    .collect()
}

/// Ends a refused Silicon's sessions as [`SessionAuth`] describes, and returns the refusal
/// (naming the sessions it ended).
async fn on_refused(state: &Shared, parts: &mut Parts, err: AppError) -> AppError {
    if !matches!(err.code(), ErrorCode::TokenExpired | ErrorCode::NotATeamMember) {
        return err;
    }
    let Some(token) = bearer_token(parts) else {
        return err;
    };
    // The world was selected before IAM refused, so this is answered from the selection cache.
    let Ok(Sel { world, sel }) = Sel::from_request_parts(parts, state).await else {
        return err;
    };
    let Some(silicon) = silicon_of(state, &world, &token).await else {
        return err;
    };
    if err.code() == ErrorCode::NotATeamMember {
        let Some(team) = parts
            .headers
            .get(TEAM_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
        else {
            return err;
        };
        left_team(state, &world, sel.as_ref(), silicon.id(), &team, err).await
    } else {
        logged_out_later(state, world, sel, silicon.id().to_owned(), token).await;
        err
    }
}

/// IAM still accepts the Silicon's login but it no longer reaches `team`. Unless IAM says it is
/// still an active member there, its sessions in `team` end (on doubt, as before). Its grants in
/// `team` are deleted only when IAM definitely says it left and a second reader confirms
/// (crate::membership): a Silicon that signed in with another Team selected keeps them.
async fn left_team(
    state: &Shared,
    world: &World,
    sel: Option<&TestingSelection>,
    silicon: &str,
    team: &str,
    mut err: AppError,
) -> AppError {
    let running = running_sessions(state, world, silicon, Some(team)).await;
    if running.is_empty() {
        return err;
    }
    // IAM answers directory questions only for a signed-in member of the team.
    let reader = crate::scheduler::latest_principal(state, world, None, team, |p| p.id() != silicon).await;
    let answer = state
        .iam
        .membership(team, silicon, reader.as_ref().map(|(p, _)| p), sel)
        .await;
    if answer == crate::iam::Membership::Active {
        // Still a member: this login just doesn't reach the team (another team was selected when
        // approving it). The Silicon fixes that by signing in again; nothing ends.
        return err;
    }
    if answer == crate::iam::Membership::Gone
        && let Some((r, _)) = &reader
    {
        let (st, world, silicon, team, first) = (
            state.clone(),
            world.clone(),
            silicon.to_owned(),
            team.to_owned(),
            r.id().to_owned(),
        );
        tokio::spawn(async move {
            let Some(_fence) = st.world_open(&world).await else {
                return;
            };
            crate::membership::confirm_gone(&st, &world, &silicon, &team, &first).await;
        });
    }
    let mut ended = Vec::new();
    for sid in running {
        match domain::end_session(state, world, &sid, EndReason::LeftTeam, &domain::system_member()).await {
            Ok(Some(_)) => ended.push(sid),
            Ok(None) => {}
            Err(e) => {
                tracing::error!(session_id = sid, error = %e, "ending a session after IAM refused its Silicon failed")
            }
        }
    }
    if !ended.is_empty() {
        tracing::info!(world = %world.schema, silicon, team, sessions = ?ended, "IAM no longer lets the Silicon into the team; its sessions ended");
        err.0.message.push_str(&format!(
            " Its running session{} {} in {team} ended (left_team).",
            if ended.len() == 1 { "" } else { "s" },
            ended.join(", ")
        ));
    }
    err
}

/// IAM refused the Silicon's access token. Checks back after [`LOGOUT_GRACE`]: if the Silicon has
/// not used a live login since (a refresh after an ordinary expiry would have), it logged out, and
/// its running sessions end.
async fn logged_out_later(state: &Shared, world: World, sel: Option<TestingSelection>, silicon: String, token: String) {
    if running_sessions(state, &world, &silicon, None).await.is_empty() {
        return;
    }
    // One pending check per Silicon and world.
    let key = format!("logout-check:{}:{silicon}", world.schema);
    if state.rate_limit(key, 1, LOGOUT_GRACE, "logout checks").await.is_err() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(LOGOUT_GRACE).await;
        match has_live_login(&state, &world, sel.as_ref(), &silicon, &token).await {
            Some(false) => {}
            Some(true) => return,
            None => {
                tracing::warn!(world = %world.schema, silicon, "IAM could not say whether the Silicon still has a live login; its sessions are left running");
                return;
            }
        }
        for sid in running_sessions(&state, &world, &silicon, None).await {
            match domain::end_session(
                &state,
                &world,
                &sid,
                EndReason::SiliconLoggedOut,
                &domain::system_member(),
            )
            .await
            {
                Ok(Some(_)) => {
                    tracing::info!(world = %world.schema, silicon, session_id = sid, "IAM no longer accepts the Silicon's login; session ended")
                }
                Ok(None) => {}
                Err(e) => tracing::error!(session_id = sid, error = %e, "ending a logged-out Silicon's session failed"),
            }
        }
    });
}

/// Whether IAM accepts any login Extend has seen `silicon` use in this world, other than `refused`
/// (`None` when IAM could not answer).
async fn has_live_login(
    state: &AppState,
    world: &World,
    sel: Option<&TestingSelection>,
    silicon: &str,
    refused: &str,
) -> Option<bool> {
    let mut tokens: Vec<String> = Vec::new();
    if let Some(p) = state
        .auth_cache
        .latest(world.environment_id, |p| p.id() == silicon && p.token != refused)
        .await
    {
        tokens.push(p.token);
    }
    for ((schema, _), (p, _)) in state.session_principals.read().await.iter() {
        if schema == &world.schema && p.id() == silicon && p.token != refused && !tokens.contains(&p.token) {
            tokens.push(p.token.clone());
        }
    }
    let mut unsure = false;
    for token in tokens {
        match state.iam.authorize(&token, None, sel).await {
            Ok(_) => return Some(true),
            Err(e)
                if matches!(
                    e.code(),
                    ErrorCode::TokenExpired | ErrorCode::NotATeamMember | ErrorCode::Unauthorized
                ) => {}
            Err(_) => unsure = true,
        }
    }
    (!unsure).then_some(false)
}

/// The `device_in_use` refusal a Silicon acting in `team` gets. The holder is named, as in 1.0,
/// only when it is on the caller's side (same Team, given access through the same pair) or is the
/// caller itself; otherwise nothing about it is said.
pub fn in_use_error(d: &domain::DeviceRow, team: &str, me: &str) -> AppError {
    let same_side = d.in_use_silicon.as_deref() == Some(me) || (d.held_here() && d.held_by_side(team));
    let hint = format!(
        "Ask for it with: extend --team {team} request send {} --reason \"<why, up to 300 characters>\"",
        d.device_id
    );
    if !same_side {
        return AppError::new(
            ErrorCode::DeviceInUse,
            format!(
                "Device {} ({}) is in use. Only one Silicon can use a device at a time.",
                d.device_id, d.name
            ),
        )
        .hint(hint)
        .details(serde_json::json!({"in_use": {"hidden": true}}));
    }
    let since = d
        .in_use_since
        .map(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default()
        })
        .unwrap_or_default();
    AppError::new(
        ErrorCode::DeviceInUse,
        format!(
            "Device {} ({}) is being used by {} in session {} since {since}. Only one Silicon can use a device at a time.",
            d.device_id,
            d.name,
            d.in_use_silicon.clone().unwrap_or_default(),
            d.in_use_session.clone().unwrap_or_default()
        ),
    )
    .hint(hint)
    .details(serde_json::json!({"in_use": {"silicon_id": d.in_use_silicon, "session_id": d.in_use_session, "since": since}}))
}

/// Another member of the device's lock group (its computer, or a device the computer carries) is
/// in use by another side.
pub fn group_in_use_error(d: &domain::DeviceRow, team: &str) -> AppError {
    let message = match (&d.host_device_id, &d.host_name) {
        (Some(_), Some(host)) => format!(
            "{} pairs through {host}, and something on that computer is in use.",
            d.name
        ),
        _ => format!("A device {} carries is in use.", d.name),
    };
    AppError::new(ErrorCode::DeviceInUse, message)
        .hint(format!(
            "A computer and the devices it carries are used by one side at a time. Ask for it with: extend --team {team} request send {} --reason \"...\"",
            d.device_id
        ))
        .details(serde_json::json!({"in_use": {"hidden": true}}))
}

/// Picks an unused session id of the shortest length that still has one (TECHNICAL.md section 1).
async fn allocate_session_id(
    state: &AppState,
    world: &World,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> AppResult<String> {
    let mut len = 3usize;
    loop {
        let used: i64 = sqlx::query_scalar(sql!(
            "SELECT count(*) FROM {} WHERE length(session_id) = $1",
            world.t("session_ids")
        ))
        .bind(len as i32)
        .fetch_one(&mut **tx)
        .await?;
        let space = SessionId::space(len) as i64;
        if used < space {
            // Random tries first; near the end of a length, scan for the gaps.
            for _ in 0..32 {
                let candidate = SessionId::from_parts(rand::rng().random_range(0..space as u64), len).to_string();
                let res = sqlx::query(sql!(
                    "INSERT INTO {} (session_id) VALUES ($1) ON CONFLICT DO NOTHING",
                    world.t("session_ids")
                ))
                .bind(&candidate)
                .execute(&mut **tx)
                .await?;
                if res.rows_affected() == 1 {
                    return Ok(candidate);
                }
            }
            if len <= 4 {
                let taken: Vec<(String,)> = sqlx::query_as(sql!(
                    "SELECT session_id FROM {} WHERE length(session_id) = $1",
                    world.t("session_ids")
                ))
                .bind(len as i32)
                .fetch_all(&mut **tx)
                .await?;
                let taken: std::collections::HashSet<String> = taken.into_iter().map(|(s,)| s).collect();
                if let Some(free) = (0..space as u64)
                    .map(|v| SessionId::from_parts(v, len).to_string())
                    .find(|c| !taken.contains(c))
                {
                    let res = sqlx::query(sql!(
                        "INSERT INTO {} (session_id) VALUES ($1) ON CONFLICT DO NOTHING",
                        world.t("session_ids")
                    ))
                    .bind(&free)
                    .execute(&mut **tx)
                    .await?;
                    if res.rows_affected() == 1 {
                        return Ok(free);
                    }
                }
            }
            let _ = state;
            continue;
        }
        len += 1;
        if len > 16 {
            return Err(AppError::internal("session id space exhausted"));
        }
    }
}

pub async fn start(
    State(state): State<Shared>,
    SessionAuth(auth): SessionAuth,
    headers: HeaderMap,
    Body(input): Body<SessionCreate>,
) -> AppResult<Response> {
    auth.require_silicon()?;
    let team = auth.team()?.to_owned();
    let device_id = input.device_id.to_string();
    let d = domain::load_device(&state, &auth.world, &device_id)
        .await?
        .ok_or_else(|| domain::device_not_found_in(&device_id, &team))?;
    if domain::access_of(&state, &auth.world, &d, &auth.p).await? != Some(Access::Silicon) {
        return Err(domain::device_not_found_in(&device_id, &team));
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
    let online = domain::is_online(&state, &auth.world, &d).await;
    if !online {
        let seen = d
            .last_seen_at
            .map(|t| format!(" It was last seen {t}."))
            .unwrap_or_default();
        return Err(AppError::new(
            ErrorCode::DeviceOffline,
            format!("{} is offline, so it can't be used right now.{seen}", d.name),
        )
        .hint(offline_hint(&d, &team)));
    }
    if !d.is_ready() {
        let left: Vec<String> = domain::setup_of(&state, &auth.world, &d)
            .await
            .steps
            .into_iter()
            .filter(|s| s.status != extend_protocol::model::StepStatus::Done)
            .map(|s| match s.error {
                Some(e) => format!("{} ({e})", s.title),
                None => s.title,
            })
            .collect();
        return Err(AppError::new(
            ErrorCode::DeviceNotReady,
            format!("{} hasn't finished setup. Steps left: {}.", d.name, left.join("; ")),
        )
        .hint(format!(
            "{} can finish them on the device; watch with `extend --team {team} device setup {device_id}`.",
            d.owner_id
        )));
    }
    if d.in_use_session.is_some() {
        return Err(in_use_error(&d, &team, auth.p.id()));
    }
    let hash = hash_json(&input);
    let world = auth.world.clone();
    let st = state.clone();
    let p = auth.p.clone();
    let sel = auth.sel.clone();
    let isi = auth.isi.clone();
    idempotent(&state, &auth.world, auth.p.id(), &format!("sessions:{team}"), &headers, &hash, || async move {
        let mut tx = st.pool.begin().await?;
        // The lock order: the whole lock group's instances first, so a revoke (which takes the
        // instance first too) either sees this session or this start sees no grant.
        let group = domain::lock_group(&mut *tx, &world, d.instance_id).await?;
        domain::lock_instances(&mut tx, &world, &group.members).await?;
        let granted: Option<(i32,)> = sqlx::query_as(sql!(
            "SELECT 1 FROM {} WHERE device_id = $1 AND team = $2 AND silicon_id = $3",
            world.t("device_access")
        ))
        .bind(&device_id)
        .bind(&team)
        .bind(p.id())
        .fetch_optional(&mut *tx)
        .await?;
        let owner_gone: Option<(i32,)> = sqlx::query_as(sql!(
            "SELECT 1 FROM {} WHERE member_id = $1 AND team = $2 AND state = 'gone'",
            world.t("membership_checks")
        ))
        .bind(&d.owner_id)
        .bind(&team)
        .fetch_optional(&mut *tx)
        .await?;
        if owner_gone.is_some() {
            return Err(crate::membership::owner_gone(&d.owner_id, &d.name, &team));
        }
        if granted.is_none() {
            return Err(AppError::new(
                ErrorCode::NoAccess,
                format!("{} has no access to {} ({device_id}) in {team}.", p.id(), d.name),
            ));
        }
        // One side at a time across the lock group.
        let holders: Vec<(Uuid, String, String)> = sqlx::query_as(sql!(
            "SELECT l.instance_id, s.team, o.owner_id FROM {} l JOIN {} s ON s.session_id = l.session_id
               JOIN {} o ON o.device_id = s.device_id WHERE l.instance_id = ANY($1)",
            world.t("device_locks"),
            world.t("sessions"),
            world.t("devices")
        ))
        .bind(&group.members)
        .fetch_all(&mut *tx)
        .await?;
        for (instance, h_team, h_carbon) in &holders {
            if *instance == d.instance_id {
                drop(tx);
                let now = domain::load_device(&st, &world, &device_id).await?.ok_or_else(|| domain::device_not_found(&device_id))?;
                return Err(in_use_error(&now, &team, p.id()));
            }
            if *h_team != team || *h_carbon != d.owner_id {
                drop(tx);
                return Err(group_in_use_error(&d, &team));
            }
        }
        let sid = allocate_session_id(&st, &world, &mut tx).await?;
        let idle = OffsetDateTime::now_utc() + time::Duration::seconds(SESSION_IDLE_S);
        sqlx::query(sql!(
            "INSERT INTO {} (session_id, device_id, silicon_id, team, state, idle_ends_at) VALUES ($1, $2, $3, $4, 'active', $5)",
            world.t("sessions")
        ))
        .bind(&sid)
        .bind(&device_id)
        .bind(p.id())
        .bind(&team)
        .bind(idle)
        .execute(&mut *tx)
        .await?;
        let locked = sqlx::query(sql!(
            "INSERT INTO {} (device_id, session_id, instance_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            world.t("device_locks")
        ))
        .bind(&device_id)
        .bind(&sid)
        .bind(d.instance_id)
        .execute(&mut *tx)
        .await?;
        if locked.rows_affected() == 0 {
            drop(tx);
            let now = domain::load_device(&st, &world, &device_id).await?.ok_or_else(|| domain::device_not_found(&device_id))?;
            return Err(in_use_error(&now, &team, p.id()));
        }
        // A pair lasts while the physical device has activity, through any pair.
        domain::bump_activity(&mut *tx, &world, d.instance_id).await?;
        sqlx::query(sql!("UPDATE {} SET last_used_at = now() WHERE device_id = $1", world.t("devices")))
            .bind(&device_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(sql!(
            "UPDATE {} SET last_used_at = now() WHERE device_id = $1 AND team = $2 AND silicon_id = $3",
            world.t("device_access")
        ))
        .bind(&device_id)
        .bind(&team)
        .bind(p.id())
        .execute(&mut *tx)
        .await?;
        let awake: Option<Option<bool>> = sqlx::query_scalar(sql!(
            "SELECT awake FROM {} WHERE instance_id = $1",
            world.t("device_instances")
        ))
        .bind(d.instance_id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        st.session_principals.write().await.insert((world.schema.clone(), sid.clone()), (p.clone(), sel.clone()));
        register_for_requests(&st, &world, &p, sel.as_ref());
        // A Silicon that starts on a device whose awake state is unknown got what it asked for; on
        // one known not to be awake, its wake request stays open (and "woken" still comes).
        if awake.flatten().is_none() {
            crate::wake::withdraw(
                &st,
                &world,
                crate::wake::Withdraw::Session { instance: d.instance_id, silicon_id: p.id(), team: &team },
                extend_protocol::model::WakeEndReason::SessionStarted,
            )
            .await;
        }
        let row = domain::load_session(&st, &world, &sid).await?.ok_or_else(|| AppError::internal("session vanished"))?;
        let target = d.host_device_id.as_ref().and_then(|_| d.device_id.parse().ok());
        let side = domain::side_of(&st, &world, &d, &team).await.ok();
        st.hub
            .send(
                &d.route(&world),
                ServiceFrame::SessionStarted {
                    target,
                    session_id: sid.parse().map_err(AppError::internal)?,
                    silicon_id: p.id().to_owned(),
                    since: row.started_at,
                    side,
                },
            )
            .await;
        // What devices in the group show of other sides' wake requests follows the new holder.
        crate::wake::resend_group(&st, &world, d.instance_id).await;
        domain::log_in(&st, &world, &device_id, &p.member, "session_started", Some(&sid), Some(&team), serde_json::json!({"isi": isi})).await;
        tracing::info!(world = %world.schema, session_id = %sid, device_id, silicon = p.id(), "session started");
        Ok((StatusCode::CREATED, "session", serde_json::to_value(row.view()).map_err(AppError::internal)?))
    })
    .await
}

/// Lets Ting deliver other Silicons' requests for this device to the Silicon now using it. Ting
/// only delivers an app's notifications to recipients that registered the app with their own
/// login, and a session start is where Extend holds that login. Runs in the background: a failure
/// is logged and the session goes on (requests stay pending and are retried).
fn register_for_requests(state: &Shared, world: &World, silicon: &Principal, sel: Option<&TestingSelection>) {
    crate::delivery::register_silicon(state, world, silicon, sel);
}

/// Why a device may be offline, and, when it last reported itself not awake, the wake hint.
fn offline_hint(d: &domain::DeviceRow, team: &str) -> String {
    let base = match d.os() {
        extend_protocol::DeviceOs::Android => "Usual causes: no Wi-Fi, the Extend app was stopped, or battery optimisation paused it. Ask the device's Carbon to open the Extend app.".to_owned(),
        extend_protocol::DeviceOs::AndroidTv => "Usual causes: the TV is off or asleep, or not on the network. Ask the device's Carbon to open Silicon Extend TV.".to_owned(),
        extend_protocol::DeviceOs::Macos | extend_protocol::DeviceOs::Windows | extend_protocol::DeviceOs::Linux => {
            "Usual causes: the computer is asleep, off, or the Extend app isn't running.".to_owned()
        }
        _ => "It pairs through a computer: that computer must be online and on the same network as this device.".to_owned(),
    };
    match crate::wake::hint(d, false, team) {
        Some(h) => format!("{base} {h}"),
        None => base,
    }
}

#[derive(Deserialize)]
pub struct ListQuery {
    device_id: Option<String>,
    state: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
}

pub async fn list(
    State(state): State<Shared>,
    SessionAuth(auth): SessionAuth,
    Query(q): Query<ListQuery>,
) -> AppResult<Response> {
    // A Silicon sees its own sessions in the Team it acts in; a Carbon, the sessions through
    // their pairs in every Team.
    let team = if auth.p.is_silicon() {
        Some(auth.team()?.to_owned())
    } else {
        None
    };
    let lim = limit(q.limit)?;
    let before = q.cursor.as_deref().map(decode_cursor).transpose()?;
    let who = if auth.p.is_silicon() {
        "s.silicon_id = $2".to_owned()
    } else {
        format!(
            "EXISTS (SELECT 1 FROM {} d WHERE d.device_id = s.device_id AND d.owner_id = $2)",
            auth.world.t("devices")
        )
    };
    let state_filter = match q.state.as_deref() {
        None => None,
        Some(s @ ("active" | "paused" | "ended")) => Some(s.to_owned()),
        Some(other) => {
            return Err(AppError::invalid(format!(
                "state must be active, paused or ended; got {other:?}."
            )));
        }
    };
    let rows: Vec<SessionRow> = sqlx::query_as(sql!(
        "SELECT {} FROM {} s WHERE ($1::text IS NULL OR s.team = $1) AND {who}
           AND ($3::text IS NULL OR s.device_id = $3) AND ($4::text IS NULL OR s.state = $4)
           AND ($5::text IS NULL OR (s.started_at, s.session_id) < ((SELECT started_at FROM {} WHERE session_id = $5), $5))
         ORDER BY s.started_at DESC, s.session_id DESC LIMIT $6",
        SESSION_COLUMNS.split(", ").map(|c| format!("s.{c}")).collect::<Vec<_>>().join(", "),
        auth.world.t("sessions"),
        auth.world.t("sessions"),
    ))
    .bind(&team)
    .bind(auth.p.id())
    .bind(&q.device_id)
    .bind(&state_filter)
    .bind(&before)
    .bind(lim + 1)
    .fetch_all(&state.pool)
    .await?;
    let more = rows.len() as i64 > lim;
    let items: Vec<_> = rows.iter().take(lim as usize).map(SessionRow::view).collect();
    let next = more.then(|| encode_cursor(&items.last().map(|s| s.session_id.to_string()).unwrap_or_default()));
    Ok(ok("sessions", serde_json::json!({"items": items, "next_cursor": next})))
}

/// Loads a session the caller may see: its Silicon, or the Carbon who owns its device.
async fn visible_session(
    state: &AppState,
    auth: &Auth,
    session_id: &str,
) -> AppResult<(SessionRow, domain::DeviceRow)> {
    if session_id.parse::<SessionId>().is_err() {
        return Err(AppError::invalid(format!(
            "{session_id:?} is not a session id; session ids are 3 or more lowercase hexadecimal characters, like a3f."
        )));
    }
    let not_found = || {
        AppError::new(
            ErrorCode::SessionNotFound,
            format!("No session {session_id} is visible to you."),
        )
        .hint("List yours with `extend session ls`.")
    };
    let s = domain::load_session(state, &auth.world, session_id)
        .await?
        .ok_or_else(not_found)?;
    let d: domain::DeviceRow = sqlx::query_as(sql!("{} WHERE d.device_id = $1", domain::device_select(&auth.world)))
        .bind(&s.device_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(not_found)?;
    // Its Silicon, in the Team it runs in; or the Carbon whose pair it runs through, in any Team.
    let mine = auth.p.is_silicon() && s.silicon_id == auth.p.id() && auth.p.team.as_deref() == Some(s.team.as_str());
    let owner = d.is_owner(&auth.p);
    if !(mine || owner) {
        return Err(not_found());
    }
    Ok((s, d))
}

pub async fn get(
    State(state): State<Shared>,
    SessionAuth(auth): SessionAuth,
    Path(session_id): Path<String>,
) -> AppResult<Response> {
    let (s, d) = visible_session(&state, &auth, &session_id).await?;
    let access = if d.is_owner(&auth.p) {
        Access::Owner
    } else {
        Access::Silicon
    };
    let device = domain::device_view(&state, &auth.world, &d, domain::Viewer::of(access, &auth.p), true).await;
    let mut view = s.view();
    view.capabilities = device.capabilities.clone();
    view.commands = device.commands.clone();
    view.device = Some(Box::new(device));
    Ok(ok("session", view))
}

pub async fn end(
    State(state): State<Shared>,
    SessionAuth(auth): SessionAuth,
    Path(session_id): Path<String>,
) -> AppResult<Response> {
    let (s, _) = visible_session(&state, &auth, &session_id).await?;
    if s.silicon_id != auth.p.id() {
        return Err(AppError::new(
            ErrorCode::NotSessionOwner,
            format!("Session {session_id} belongs to {}.", s.silicon_id),
        )
        .hint("The device's Carbon stops a session with `extend device stop <device_id>`."));
    }
    let row = match domain::end_session(
        &state,
        &auth.world,
        &session_id,
        EndReason::EndedBySilicon,
        &auth.p.member,
    )
    .await?
    {
        Some(r) => r,
        None => s,
    };
    Ok(ok("session", row.view()))
}

fn takeover_of(s: &SessionRow) -> Option<Takeover> {
    s.takeover.clone().and_then(|t| serde_json::from_value(t).ok())
}

pub async fn takeover_start(
    State(state): State<Shared>,
    SessionAuth(auth): SessionAuth,
    Path(session_id): Path<String>,
    Body(input): Body<TakeoverCreate>,
) -> AppResult<Response> {
    let (s, d) = visible_session(&state, &auth, &session_id).await?;
    if s.silicon_id != auth.p.id() {
        return Err(AppError::new(
            ErrorCode::NotSessionOwner,
            format!("Session {session_id} belongs to {}.", s.silicon_id),
        ));
    }
    match s.state.as_str() {
        "ended" => return Err(session_ended(&s)),
        "paused" => {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "A takeover is already in progress in this session.",
            )
            .hint("See it with `extend takeover status`."));
        }
        _ => {}
    }
    let reason = input.reason.trim().to_owned();
    let n = reason.chars().count();
    if n == 0 || n > 300 {
        return Err(AppError::invalid(format!(
            "The takeover reason must be 1–300 characters; it is {n}."
        )));
    }
    let now = OffsetDateTime::now_utc();
    let t = Takeover {
        takeover_id: Uuid::now_v7(),
        session_id: s.session_id.parse().map_err(AppError::internal)?,
        reason: reason.clone(),
        started_at: now,
        expires_at: now + time::Duration::seconds(TAKEOVER_MAX_S),
    };
    sqlx::query(sql!(
        "UPDATE {} SET state = 'paused', takeover = $2, idle_ends_at = $3 WHERE session_id = $1 AND state = 'active'",
        auth.world.t("sessions")
    ))
    .bind(&session_id)
    .bind(serde_json::to_value(&t).map_err(AppError::internal)?)
    .bind(t.expires_at)
    .execute(&state.pool)
    .await?;
    let target = d.host_device_id.as_ref().and_then(|_| d.device_id.parse().ok());
    state
        .hub
        .send(
            &d.route(&auth.world),
            ServiceFrame::Takeover {
                target,
                session_id: t.session_id.clone(),
                reason: reason.clone(),
                expires_at: t.expires_at,
            },
        )
        .await;
    domain::log(
        &state,
        &auth.world,
        &d.device_id,
        &auth.p.member,
        "takeover_started",
        Some(&session_id),
        serde_json::json!({"reason": reason}),
    )
    .await;
    Ok(super::created("takeover", t))
}

pub async fn takeover_get(
    State(state): State<Shared>,
    SessionAuth(auth): SessionAuth,
    Path(session_id): Path<String>,
) -> AppResult<Response> {
    let (s, _) = visible_session(&state, &auth, &session_id).await?;
    Ok(ok("takeover", if s.state == "paused" { takeover_of(&s) } else { None }))
}

/// Resumes a paused session. Used by the Silicon (`DELETE …/takeover`) and the device's Done button.
pub async fn release(
    state: &AppState,
    world: &World,
    session_id: &str,
    actor: &extend_protocol::model::Member,
) -> AppResult<bool> {
    let idle = OffsetDateTime::now_utc() + time::Duration::seconds(SESSION_IDLE_S);
    let row: Option<(String,)> = sqlx::query_as(sql!(
        "UPDATE {} SET state = 'active', takeover = NULL, idle_ends_at = $2 WHERE session_id = $1 AND state = 'paused' RETURNING device_id",
        world.t("sessions")
    ))
    .bind(session_id)
    .bind(idle)
    .fetch_optional(&state.pool)
    .await?;
    let Some((device_id,)) = row else {
        return Ok(false);
    };
    if let Some(d) = domain::load_device(state, world, &device_id).await? {
        let target = d.host_device_id.as_ref().and_then(|_| d.device_id.parse().ok());
        let _ = state
            .hub
            .send(
                &d.route(world),
                ServiceFrame::TakeoverEnded {
                    target,
                    session_id: session_id.parse().map_err(AppError::internal)?,
                },
            )
            .await;
    }
    domain::log(
        state,
        world,
        &device_id,
        actor,
        "takeover_released",
        Some(session_id),
        serde_json::json!({}),
    )
    .await;
    Ok(true)
}

pub async fn takeover_release(
    State(state): State<Shared>,
    SessionAuth(auth): SessionAuth,
    Path(session_id): Path<String>,
) -> AppResult<Response> {
    let (s, d) = visible_session(&state, &auth, &session_id).await?;
    if s.silicon_id != auth.p.id() && !d.is_owner(&auth.p) {
        return Err(AppError::new(
            ErrorCode::NotSessionOwner,
            format!("Session {session_id} belongs to {}.", s.silicon_id),
        ));
    }
    if !release(&state, &auth.world, &session_id, &auth.p.member).await? {
        return Err(AppError::new(
            ErrorCode::Conflict,
            "No takeover is in progress in this session.",
        ));
    }
    Ok(no_content())
}

fn session_ended(s: &SessionRow) -> AppError {
    let reason = s.end_reason.as_deref().and_then(EndReason::parse);
    AppError::new(
        ErrorCode::SessionEnded,
        format!(
            "Session {} has ended: {}.",
            s.session_id,
            reason.map_or("unknown reason", EndReason::explain)
        ),
    )
    .hint(format!("Start a new one with `extend session new {}`.", s.device_id))
    .details(serde_json::json!({"end_reason": s.end_reason}))
}

/// Replaces typed text with `[redacted N chars]` in the activity log.
pub fn redact(spec: &capability::CommandSpec, args: &[String]) -> Vec<String> {
    if !spec.redact_text {
        return args.to_vec();
    }
    args.iter()
        .enumerate()
        .map(|(i, a)| {
            let keep = a.starts_with('-')
                || a.starts_with('@')
                || (spec.name == "clipboard" && i == 0)
                || (spec.name == "fill" && i == 0);
            if keep {
                a.clone()
            } else {
                format!("[redacted {} chars]", a.chars().count())
            }
        })
        .collect()
}

fn check_args(req: &CommandRequest) -> AppResult<&'static capability::CommandSpec> {
    let name = req.command.trim();
    if let Some(replacement) = capability::not_exposed(name) {
        let mut e = AppError::new(
            ErrorCode::UnknownCommand,
            format!("`{name}` is a device engine command Extend doesn't relay."),
        );
        if let Some(r) = replacement {
            e = e.hint(format!("Use `{r}` instead."));
        } else {
            e = e.hint(
                "Extend leaves out the device engine's tools for app developers (simulators, emulators, React Native, web).",
            );
        }
        return Err(e);
    }
    let spec = capability::command(name).ok_or_else(|| {
        AppError::new(ErrorCode::UnknownCommand, format!("`{name}` is not an Extend command."))
            .hint("See the commands for this device with `extend --help` while connected.")
    })?;
    for a in &req.args {
        let flag = a.split('=').next().unwrap_or_default();
        if RESERVED_FLAGS.contains(&flag) {
            return Err(AppError::invalid(format!(
                "`{flag}` picks a device or session inside the device engine; Extend picks those."
            ))
            .hint("Drop the flag; the session already names the device."));
        }
        if a.len() > 64 * 1024 {
            return Err(AppError::invalid("An argument is longer than 64 KiB."));
        }
    }
    if req.args.len() > 256 {
        return Err(AppError::invalid("A command takes at most 256 arguments."));
    }
    Ok(spec)
}

/// Refuses a command in a session that ended or is paused for a takeover.
fn refuse_unless_active(s: &SessionRow, d: &domain::DeviceRow) -> AppResult<()> {
    match s.state.as_str() {
        "ended" => Err(session_ended(s)),
        "paused" => {
            let reason = takeover_of(s).map(|t| t.reason).unwrap_or_default();
            Err(AppError::new(
                ErrorCode::SessionPaused,
                format!("The session is paused while {} uses the device: {reason}", d.owner_id),
            )
            .hint("Wait for them to tap Done, or run `extend takeover release` if you started it and no longer need them."))
        }
        _ => Ok(()),
    }
}

pub async fn command(
    State(state): State<Shared>,
    SessionAuth(auth): SessionAuth,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Body(req): Body<CommandRequest>,
) -> AppResult<Response> {
    auth.require_silicon()?;
    let (s, d) = visible_session(&state, &auth, &session_id).await?;
    if s.silicon_id != auth.p.id() {
        return Err(AppError::new(
            ErrorCode::NotSessionOwner,
            format!("Session {session_id} belongs to {}.", s.silicon_id),
        ));
    }
    refuse_unless_active(&s, &d)?;
    // Access is re-checked on every command, not only at session start: the grant on the
    // session's pair in the session's Team, and that the Carbon who gave it is still in that Team.
    if domain::access_of(&state, &auth.world, &d, &auth.p).await? != Some(Access::Silicon) {
        domain::end_session(
            &state,
            &auth.world,
            &session_id,
            EndReason::AccessRemoved,
            &domain::system_member(),
        )
        .await?;
        return Err(AppError::new(
            ErrorCode::AccessRemoved,
            format!("Your access to {} was removed; session {session_id} has ended.", d.name),
        ));
    }
    // When the Carbon who gave access left the Team, the refusal comes after the session ended.
    crate::membership::owner_active(
        &state,
        &auth.world,
        &s.team,
        &d.owner_id,
        &d.name,
        &auth.p,
        auth.sel.as_ref(),
    )
    .await?;
    let spec = check_args(&req)?;
    let online = domain::is_online(&state, &auth.world, &d).await;
    if !online {
        return Err(
            AppError::new(ErrorCode::DeviceOffline, format!("{} is offline right now.", d.name))
                .hint(offline_hint(&d, &s.team)),
        );
    }
    let wake_hint = crate::wake::hint(&d, online, &s.team);
    let caps = d.capabilities();
    if !spec.any_of.iter().any(|c| caps.contains(c)) {
        let missing = d.missing();
        let why: Vec<String> = spec
            .any_of
            .iter()
            .map(|c| {
                missing.iter().find(|m| m.capability == *c).map_or_else(
                    || format!("{} is not something a {} can do", c.as_str(), d.os().as_str()),
                    |m| format!("{}: {}", c.as_str(), m.reason),
                )
            })
            .collect();
        let mut hint = format!(
            "See what works with `extend --team {} device show {}` or `extend --help` while connected.",
            s.team, d.device_id
        );
        if let Some(w) = &wake_hint {
            hint = format!("{w} {hint}");
        }
        return Err(AppError::new(
            ErrorCode::UnsupportedOnDevice,
            format!(
                "`{}` doesn't work on {} right now. {}",
                spec.name,
                d.name,
                why.join("; ")
            ),
        )
        .hint(hint)
        .details(serde_json::json!({"needs_any_of": spec.any_of, "missing": missing})));
    }
    let timeout_ms = req.timeout_ms.unwrap_or(COMMAND_TIMEOUT_DEFAULT_MS);
    if !(COMMAND_TIMEOUT_MIN_MS..=COMMAND_TIMEOUT_MAX_MS).contains(&timeout_ms) {
        return Err(AppError::invalid(format!(
            "timeout_ms must be 1000–300000; got {timeout_ms}."
        )));
    }
    let self_destruct = req.self_destruct_minutes.unwrap_or(SELF_DESTRUCT_DEFAULT_MIN);
    if !(1..=SELF_DESTRUCT_MAX_MIN).contains(&self_destruct) {
        return Err(AppError::invalid(format!(
            "self_destruct_minutes must be 1–43200 (30 days); got {self_destruct}."
        )));
    }
    let mut total_attach = 0usize;
    for a in &req.attachments {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&a.content_base64)
            .map_err(|_| AppError::invalid(format!("attachment {} is not valid base64.", a.name)))?;
        total_attach += bytes.len();
    }
    if total_attach > 8 << 20 || req.attachments.len() > 8 {
        return Err(AppError::invalid(
            "Attachments are limited to 8 files and 8 MiB in total.",
        ));
    }
    let _ = spec.origin == Origin::Extend;

    let lock = state
        .hub
        .session_lock((auth.world.schema.clone(), session_id.clone()))
        .await;
    let _guard = lock.lock().await;
    // The session may have ended or been paused while this command waited for the one before it.
    let now = domain::load_session(&state, &auth.world, &session_id)
        .await?
        .unwrap_or_else(|| s.clone());
    refuse_unless_active(&now, &d)?;
    state.session_principals.write().await.insert(
        (auth.world.schema.clone(), session_id.clone()),
        (auth.p.clone(), auth.sel.clone()),
    );

    let command_id = Uuid::now_v7();
    let upload_ids: Vec<Uuid> = (0..4).map(|_| Uuid::now_v7()).collect();
    let expires = OffsetDateTime::now_utc() + time::Duration::milliseconds(timeout_ms as i64 + 60_000);
    for u in &upload_ids {
        sqlx::query(sql!(
            "INSERT INTO {} (upload_id, device_id, command_id, expires_at) VALUES ($1, $2, $3, $4)",
            auth.world.t("uploads")
        ))
        .bind(u)
        .bind(d.host_device_id.as_ref().unwrap_or(&d.device_id))
        .bind(command_id)
        .bind(expires)
        .execute(&state.pool)
        .await?;
    }
    // A command in flight holds the idle timer (TECHNICAL.md section 5): until its deadline, plus
    // the idle window. The answer resets the window to 300 s from then.
    let held = OffsetDateTime::now_utc()
        + time::Duration::milliseconds(timeout_ms as i64 + 2_000)
        + time::Duration::seconds(SESSION_IDLE_S);
    sqlx::query(sql!(
        "UPDATE {} SET idle_ends_at = $2 WHERE session_id = $1 AND state = 'active'",
        auth.world.t("sessions")
    ))
    .bind(&session_id)
    .bind(held)
    .execute(&state.pool)
    .await?;
    let route = d.route(&auth.world);
    let started = OffsetDateTime::now_utc();
    let frame = ServiceFrame::Command(CommandFrame {
        id: command_id,
        session_id: session_id.parse().map_err(AppError::internal)?,
        target: d.host_device_id.as_ref().and_then(|_| d.device_id.parse().ok()),
        command: spec.name.to_owned(),
        args: req.args.clone(),
        attachments: req.attachments.clone(),
        timeout_ms,
        upload_ids: upload_ids.clone(),
    });
    // Wait for the device's answer, unless the session ends first (device removed, pair revoked,
    // access removed, Stop, logout...): then the Silicon hears why at once, not at the deadline.
    let relayed = {
        let answer = state
            .hub
            .command(&route, command_id, frame, Duration::from_millis(timeout_ms + 2_000));
        tokio::pin!(answer);
        tokio::select! {
            biased;
            outcome = &mut answer => Ok(outcome),
            ended = ended_while_running(&state, &auth.world, &session_id) => Err(ended),
        }
    };
    let duration_ms = (OffsetDateTime::now_utc() - started).whole_milliseconds() as i64;
    let redacted = redact(spec, &req.args);

    let log_command = |outcome_word: &'static str, files: Vec<Uuid>, error: Option<String>, warnings: Vec<String>| {
        let st = state.clone();
        let world = auth.world.clone();
        let device_id = d.device_id.clone();
        let sid = session_id.clone();
        let who = auth.p.member.clone();
        let args = redacted.clone();
        let cmd = spec.name.to_owned();
        let isi = auth.isi.clone();
        let team = s.team.clone();
        let instance = d.instance_id;
        async move {
            let mut details = serde_json::json!({"duration_ms": duration_ms, "error": error, "isi": isi});
            if !warnings.is_empty() {
                details["warnings"] = serde_json::json!(warnings);
            }
            let _ = sqlx::query(sql!(
                "INSERT INTO {} (id, device_id, actor_kind, actor_id, action, session_id, command, args, outcome, files, details, team)
                 VALUES ($1, $2, 'silicon', $3, 'command', $4, $5, $6, $7, $8, $9, $10)",
                world.t("activity")
            ))
            .bind(command_id)
            .bind(&device_id)
            .bind(&who.id)
            .bind(&sid)
            .bind(&cmd)
            .bind(serde_json::to_value(&args).unwrap_or_default())
            .bind(outcome_word)
            .bind(serde_json::to_value(&files).unwrap_or_default())
            .bind(details)
            .bind(&team)
            .execute(&st.pool)
            .await;
            let idle = OffsetDateTime::now_utc() + time::Duration::seconds(SESSION_IDLE_S);
            let _ = sqlx::query(sql!(
                "UPDATE {} SET last_command_at = now(), idle_ends_at = $2, command_count = command_count + 1 WHERE session_id = $1 AND state = 'active'",
                world.t("sessions")
            ))
            .bind(&sid)
            .bind(idle)
            .execute(&st.pool)
            .await;
            // A pair lasts while the physical device has activity, through any pair.
            let _ = domain::bump_activity(&st.pool, &world, instance).await;
            let _ = sqlx::query(sql!(
                "UPDATE {} SET last_used_at = now() WHERE device_id = $1",
                world.t("devices")
            ))
            .bind(&device_id)
            .execute(&st.pool)
            .await;
            idle
        }
    };

    let outcome = match relayed {
        Ok(Ok(o)) => o,
        Err(ended) => {
            // Tell the device to drop the command (apps also cancel on session_ended), and stop
            // waiting for an answer nobody will read.
            let _ = state.hub.send(&route, ServiceFrame::Cancel { id: command_id }).await;
            state.hub.resolve(&route, abandoned(command_id)).await;
            log_command("unknown", vec![], Some("session_ended".into()), vec![]).await;
            return Err(ended_mid_command(
                &session_id,
                ended.as_ref(),
                &d,
                spec.name,
                command_id,
            ));
        }
        Ok(Err(e)) => {
            // The socket may have closed because the pair ended; that is the precise answer.
            if let Ok(Some(now)) = domain::load_session(&state, &auth.world, &session_id).await
                && now.state == "ended"
            {
                log_command("unknown", vec![], Some("session_ended".into()), vec![]).await;
                return Err(ended_mid_command(&session_id, Some(&now), &d, spec.name, command_id));
            }
            match e {
                SendError::Timeout => {
                    log_command("timeout", vec![], Some("command_timeout".into()), vec![]).await;
                    return Err(AppError::new(
                        ErrorCode::CommandTimeout,
                        format!(
                            "{} did not answer `{}` within {} ms. It may still have run.",
                            d.name, spec.name, timeout_ms
                        ),
                    )
                    .hint("Check the screen with `extend snapshot` before retrying, or pass a longer --timeout."));
                }
                SendError::Offline | SendError::Dropped => {
                    log_command("unknown", vec![], Some("device_offline".into()), vec![]).await;
                    return Err(AppError::new(
                        ErrorCode::DeviceOffline,
                        format!(
                            "{} went offline while running `{}`; it may have run.",
                            d.name, spec.name
                        ),
                    )
                    .hint(offline_hint(&d, &s.team)));
                }
            }
        }
    };

    // Store every file the device uploaded in Briefcase for the Silicon. A file that can't be
    // stored (or shared with the device's Carbon) is reported in `warnings`, never dropped quietly.
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    for f in &outcome.files {
        if !upload_ids.contains(&f.upload_id) {
            tracing::warn!(upload_id = %f.upload_id, name = f.name, "device reported a file under an upload id this command did not issue");
            warnings.push(format!(
                "{} was not stored: the device reported it under upload id {}, which Extend did not issue for this command. \
                 Run the command again; if it keeps happening, report it with `extend report`.",
                f.name, f.upload_id
            ));
            continue;
        }
        let path = state.cfg.data_dir.join("uploads").join(f.upload_id.to_string());
        let Ok(bytes) = tokio::fs::read(&path).await else {
            tracing::warn!(upload_id = %f.upload_id, name = f.name, "device listed a file it never uploaded");
            warnings.push(format!(
                "{} was not stored: {} made it but never uploaded it (the upload failed or was cut off). \
                 Run the command again; if it keeps happening, report it with `extend report`.",
                f.name, d.name
            ));
            continue;
        };
        let _ = tokio::fs::remove_file(&path).await;
        let size = bytes.len() as i64;
        let stored = state
            .files
            .store(
                &auth.p,
                NewFile {
                    name: &f.name,
                    content_type: &f.content_type,
                    bytes,
                    owner_carbon: &d.owner_id,
                },
                auth.sel.as_ref(),
            )
            .await;
        let stored = match stored {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(name = f.name, command_id = %command_id, error = %e.0.message, "storing a command file failed");
                warnings.push(format!(
                    "{} was not stored: {}{} Run the command again to make a new one.",
                    f.name,
                    e.0.message,
                    e.0.hint.as_deref().map(|h| format!(" {h}")).unwrap_or_default()
                ));
                continue;
            }
        };
        if let Some(why) = &stored.share_error {
            warnings.push(format!(
                "{} is stored, but not shared with {}, so they can't open it in Briefcase yet: {why}",
                f.name, d.owner_id
            ));
        }
        let self_destruct_at =
            (!req.permanent).then(|| OffsetDateTime::now_utc() + time::Duration::minutes(i64::from(self_destruct)));
        let recorded = sqlx::query(sql!(
            "INSERT INTO {} (file_id, team, device_id, session_id, command_id, created_by, shared_with, name, kind, content_type, size_bytes, url, self_destruct_at, permanent)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
            auth.world.t("files")
        ))
        .bind(stored.file_id)
        .bind(&s.team)
        .bind(&d.device_id)
        .bind(&session_id)
        .bind(command_id)
        .bind(auth.p.id())
        .bind(&stored.shared_with)
        .bind(&f.name)
        .bind(f.kind.as_str())
        .bind(&f.content_type)
        .bind(size)
        .bind(&stored.url)
        .bind(self_destruct_at)
        .bind(req.permanent)
        .execute(&state.pool)
        .await;
        if let Err(e) = recorded {
            tracing::error!(file_id = %stored.file_id, url = stored.url, error = %e, "recording a stored file failed");
            warnings.push(format!(
                "{} is stored at {}, but Extend could not record it, so it won't be listed or self-destruct. \
                 Delete it in Briefcase when you no longer need it, and report this with `extend report`.",
                f.name, stored.url
            ));
            continue;
        }
        files.push(FileInfo {
            file_id: stored.file_id,
            name: f.name.clone(),
            kind: f.kind,
            content_type: f.content_type.clone(),
            size_bytes: size,
            url: stored.url,
            self_destruct_at,
            permanent: req.permanent,
            session_id: s.session_id.parse().ok(),
            device_id: d.device_id.parse().ok(),
            command_id: Some(command_id),
            created_by: Some(auth.p.id().to_owned()),
            shared_with: stored.shared_with,
            created_at: Some(OffsetDateTime::now_utc()),
            team: Some(s.team.clone()),
        });
    }
    let _ = FileKind::Other;
    // A command that fails on a device known not to be awake says how to have it woken. Awake is
    // never a gate: the command ran.
    if !outcome.ok
        && let Some(w) = &wake_hint
    {
        warnings.push(w.clone());
    }
    if !crate::telemetry::opted_out(&headers) {
        crate::telemetry::record(
            &state,
            &auth.world,
            Some(auth.p.id()),
            serde_json::json!({
                "source": "service", "event": "command", "step": "session.command.relay", "success": outcome.ok,
                "duration_ms": duration_ms, "command": spec.name, "device_os": d.os().as_str(),
                "session_id": session_id, "command_id": command_id, "files": files.len(),
                "warnings": warnings.len(), "error_code": outcome.error.as_ref().map(|e| e.code.clone()),
            }),
        )
        .await;
    }
    let idle = log_command(
        if outcome.ok { "ok" } else { "failed" },
        files.iter().map(|f| f.file_id).collect(),
        outcome.error.as_ref().map(|e| e.code.clone()),
        warnings.clone(),
    )
    .await;
    Ok(ok(
        "command_result",
        CommandResult {
            command_id,
            session_id: s.session_id.parse().map_err(AppError::internal)?,
            command: spec.name.to_owned(),
            ok: outcome.ok,
            output: outcome.output,
            text: outcome.text,
            files,
            error: outcome.error.map(|e| CommandError {
                code: e.code,
                message: e.message,
                details: e.details,
            }),
            started_at: started,
            duration_ms,
            idle_ends_at: Some(idle),
            warnings,
        },
    ))
}

/// Resolves once the session has ended (checked in memory every 200 ms, and in the database every
/// 2 s). `None` when the session row is gone (its test environment was cleaned or removed).
async fn ended_while_running(state: &AppState, world: &World, session_id: &str) -> Option<SessionRow> {
    let key = (world.schema.clone(), session_id.to_owned());
    let mut ticks = 0u32;
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        ticks += 1;
        // `end_session` forgets the session's principal right after it commits the end.
        let forgotten = !state.session_principals.read().await.contains_key(&key);
        if forgotten || ticks.is_multiple_of(10) {
            match domain::load_session(state, world, session_id).await {
                Ok(Some(s)) if s.state == "ended" => return Some(s),
                Ok(None) => return None,
                _ => {}
            }
        }
    }
}

/// Clears the hub's wait for a command whose session ended (the waiter is already gone).
fn abandoned(id: Uuid) -> CommandOutcome {
    CommandOutcome {
        id,
        ok: false,
        output: serde_json::Value::Null,
        text: None,
        error: None,
        files: vec![],
    }
}

/// The answer to a command whose session ended while it ran: what ended it, and what to do.
fn ended_mid_command(
    session_id: &str,
    ended: Option<&SessionRow>,
    d: &domain::DeviceRow,
    command: &str,
    command_id: Uuid,
) -> AppError {
    let reason = ended.and_then(|s| s.end_reason.as_deref()).and_then(EndReason::parse);
    let why = match (ended, reason) {
        (_, Some(r)) => r.explain(),
        (None, None) => "its test environment was cleaned or removed",
        (Some(_), None) => "unknown reason",
    };
    let device_id = &d.device_id;
    let hint = match reason {
        Some(EndReason::DeviceRemoved | EndReason::PairRevoked | EndReason::PairExpired) => format!(
            "{} is no longer paired. See the devices you can use with `extend device ls`.",
            d.name
        ),
        Some(EndReason::LeftTeam) => format!(
            "You, or {} who owns the device, are no longer an active member of the team in Silicon IAM. \
             A Team admin can add the member back; then sign in again with `extend login <slt>`.",
            d.owner_id
        ),
        Some(EndReason::AccessRemoved) => format!(
            "{} took your access away, or signed out of Extend (which ends the sessions of the Silicons they gave access to). \
             See whether you still have access with `extend device ls`; if not, ask them to grant it again.",
            d.owner_id
        ),
        // Any Carbon who paired the device can stop it; the one who did isn't named.
        Some(EndReason::StoppedByCarbon) => format!(
            "A Carbon who paired {} stopped it. Start a new session with `extend session new {device_id}` when they are done.",
            d.name
        ),
        Some(EndReason::SiliconLoggedOut) => {
            "Your login ended. Get a new short-lived token from Silicon IAM and run `extend login <slt>`.".to_owned()
        }
        Some(EndReason::DeviceOffline) => {
            format!("Start a new session with `extend session new {device_id}` once it is back online.")
        }
        _ => format!("Start a new session with `extend session new {device_id}`."),
    };
    AppError::new(
        ErrorCode::SessionEnded,
        format!(
            "Session {session_id} ended while `{command}` was running on {}: {why}. `{command}` may have run.",
            d.name
        ),
    )
    .hint(hint)
    .details(serde_json::json!({
        "end_reason": ended.and_then(|s| s.end_reason.clone()),
        "command_id": command_id,
        "may_have_run": true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_typed_text() {
        let fill = capability::command("fill").unwrap();
        assert_eq!(
            redact(fill, &["@e3".into(), "hunter2".into()]),
            vec!["@e3".to_owned(), "[redacted 7 chars]".into()]
        );
        let clip = capability::command("clipboard").unwrap();
        assert_eq!(
            redact(clip, &["write".into(), "secret".into()]),
            vec!["write".to_owned(), "[redacted 6 chars]".into()]
        );
        let click = capability::command("click").unwrap();
        assert_eq!(redact(click, &["@e2".into()]), vec!["@e2".to_owned()]);
    }

    #[test]
    fn refuses_reserved_flags_and_hidden_commands() {
        let req = |c: &str, a: &[&str]| CommandRequest {
            command: c.into(),
            args: a.iter().map(|s| (*s).to_owned()).collect(),
            timeout_ms: None,
            self_destruct_minutes: None,
            permanent: false,
            attachments: vec![],
        };
        assert!(check_args(&req("click", &["@e2"])).is_ok());
        assert_eq!(
            check_args(&req("click", &["@e2", "--platform", "ios"]))
                .unwrap_err()
                .code(),
            ErrorCode::InvalidInput
        );
        assert_eq!(
            check_args(&req("boot", &[])).unwrap_err().code(),
            ErrorCode::UnknownCommand
        );
        assert!(
            check_args(&req("devices", &[]))
                .unwrap_err()
                .0
                .hint
                .unwrap()
                .contains("extend device ls")
        );
    }
}

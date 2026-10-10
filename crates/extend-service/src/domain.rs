//! Operations several handlers share: reading devices, checking who may do what, ending sessions,
//! ending pairs and grants, sides, and the activity log.
//!
//! # Pairs and devices (1.1)
//!
//! A `devices` row is one pair: one Carbon's device, with its own id, name, credential, access list
//! and lifetime. Its `instance_id` names the physical device; the pairs of one device share it, and
//! one Silicon at a time holds the instance (`device_locks` has one row per instance). A computer
//! and the devices it carries form a lock group: while a Silicon of one side uses any member, no
//! Silicon of another side starts on any of them.
//!
//! The side of a session, request or wake request is the Carbon who owns its pair (the Carbon who
//! gave the Silicon access). Each Carbon sees only their own side. A Silicon sees who holds a
//! device only when the holder runs through the same pair and is in its custodian circle (its
//! custodian and the custodian's other Silicons); everything else shows only as "in use".
//!
//! # Identities
//!
//! Every identity column holds a Silicon Accounts uuid; views show the current public id
//! (crate::accounts::directory). There are no Teams.
//!
//! # Lock order
//!
//! Every transaction that changes who may use a device takes its rows in this order:
//! 1. `device_instances`, `FOR NO KEY UPDATE`, in `instance_id` order (a session start: its whole
//!    lock group);
//! 2. `devices`; 3. `device_access`; 4. `sessions` and `device_locks`; 5. `wake_requests`.
//!
//! Every change that ends access takes the instance row first, so it serialises with a session
//! start's re-check, and the two can't deadlock.

use extend_protocol::capability::DeviceKind;
use extend_protocol::frames::ServiceFrame;
use extend_protocol::model::{
    Device, DeviceState, EndReason, InUse, InUseIndicator, Member, MemberKind, MissingCapability, Session,
    SessionState, Setup, SetupStep, SleepState, StepStatus, Visibility,
};
use extend_protocol::{Capability, DeviceOs, ErrorCode};
use hmac::{Hmac, Mac as _};
use sha2::Sha256;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::accounts::Principal;
use crate::db::World;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// The actor id of rows Extend writes as itself (not on anyone's behalf).
pub const SYSTEM_ACTOR: &str = "extend";

#[derive(Debug, Clone, FromRow)]
pub struct DeviceRow {
    pub device_id: String,
    /// History: the Team the Carbon had selected when pairing before 4.0 ('' since). It
    /// authorizes nothing; `DeviceSelf.team` (which installed apps decode) still carries it.
    pub team: String,
    /// The Carbon who paired it (uuid).
    pub owner_id: String,
    pub name: String,
    pub os: String,
    pub os_version: Option<String>,
    pub model: Option<String>,
    pub address: Option<String>,
    pub visibility: String,
    pub pair_ttl_days: i32,
    pub paired_at: OffsetDateTime,
    pub last_activity_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
    pub last_seen_at: Option<OffsetDateTime>,
    pub version: i64,
    pub host_device_id: Option<String>,
    pub state: String,
    pub setup: serde_json::Value,
    pub capabilities: serde_json::Value,
    pub missing: serde_json::Value,
    pub app_version: Option<String>,
    /// The device engine's version (the host's, for a carried device).
    pub engine_version: Option<String>,
    pub removed_at: Option<OffsetDateTime>,
    pub removed_reason: Option<String>,
    pub instance_id: Uuid,
    pub wake_muted: bool,
    pub hardware_key: Option<String>,
    pub duplicate: Option<String>,
    pub provisional_until: Option<OffsetDateTime>,
    pub first_pair: bool,
    /// The session holding the physical device, through whichever pair.
    pub in_use_session: Option<String>,
    pub in_use_silicon: Option<String>,
    pub in_use_since: Option<OffsetDateTime>,
    pub in_use_state: Option<String>,
    /// The pair that session runs through, and that pair's Carbon.
    pub in_use_device_id: Option<String>,
    pub in_use_carbon: Option<String>,
    pub awake: Option<bool>,
    pub sleep_state: Option<String>,
    pub awake_changed_at: Option<OffsetDateTime>,
    /// `shown` or `hidden`: the physical device's in-use banner, shared by its pairs.
    pub in_use_indicator: String,
    /// Whether another Carbon has a live pair of the same device.
    pub paired_by_others: bool,
    /// Grants on this pair.
    pub access_count: i64,
    /// For a computer: the running sessions on devices carried by any pair of it,
    /// `[{device_id, instance_id, carbon, owners}]` (`owners`: the Carbons with a live pair of
    /// that carried device).
    pub carried_busy: serde_json::Value,
    /// Open wake requests on the physical device: `[{from, device_id}]`.
    pub open_wakes: serde_json::Value,
    /// For a carried device: its host pair's app version and name.
    pub host_app_version: Option<String>,
    pub host_name: Option<String>,
}

/// A running session on a device a computer carries (see [`DeviceRow::carried_busy`]).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CarriedBusy {
    pub session_id: String,
    pub device_id: String,
    pub instance_id: Uuid,
    pub carbon: String,
    #[serde(default)]
    pub owners: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct OpenWake {
    pub from: String,
    pub device_id: String,
}

impl DeviceRow {
    pub fn os(&self) -> DeviceOs {
        serde_json::from_value(serde_json::Value::String(self.os.clone())).unwrap_or(DeviceOs::Linux)
    }
    /// TV element clicks need the app's 1.1 accessibility command gate. Earlier apps still use
    /// the pointer/touch gate, so never offer them a command they would refuse locally.
    pub fn command_requirements(&self, spec: &extend_protocol::CommandSpec) -> &'static [Capability] {
        if self.os() == DeviceOs::AndroidTv
            && !self
                .app_version
                .as_deref()
                .is_some_and(|v| crate::routes::enroll::version_at_least(v, "1.1.0"))
        {
            spec.any_of
        } else {
            spec.any_of_for(self.os())
        }
    }
    pub fn key(&self, world: &World) -> (String, String) {
        (world.schema.clone(), self.device_id.clone())
    }
    /// The socket commands for this device travel on: its own, or its host's.
    pub fn route(&self, world: &World) -> (String, String) {
        (
            world.schema.clone(),
            self.host_device_id.clone().unwrap_or_else(|| self.device_id.clone()),
        )
    }
    pub fn is_computer(&self) -> bool {
        self.os().kind() == DeviceKind::Computer
    }
    /// Carbon decision (2026-09-27): the terminal runs as the computer's own account, so on a
    /// computer several Carbons paired only Silicons given access by the Carbon who installed
    /// Extend on it (the pair made by the app's first enrollment) get it. The others use the
    /// screen, the keyboard and the apps.
    pub fn terminal_withheld(&self) -> bool {
        self.is_computer() && self.paired_by_others && !self.first_pair
    }
    /// What the device can do through this pair (what it reported, less what this pair may not use).
    pub fn capabilities(&self) -> Vec<Capability> {
        let mut caps: Vec<Capability> = serde_json::from_value(self.capabilities.clone()).unwrap_or_default();
        if self.terminal_withheld() {
            caps.retain(|c| *c != Capability::Terminal);
        }
        caps
    }
    pub fn missing(&self) -> Vec<MissingCapability> {
        let mut missing: Vec<MissingCapability> = serde_json::from_value(self.missing.clone()).unwrap_or_default();
        if self.terminal_withheld() {
            missing.retain(|m| m.capability != Capability::Terminal);
            missing.push(MissingCapability {
                capability: Capability::Terminal,
                reason: extend_protocol::TERMINAL_NOT_SHARED_REASON.into(),
            });
        }
        missing
    }
    /// The setup the device reported. [`setup_of`] adds the steps the service itself knows.
    pub fn setup(&self) -> Setup {
        serde_json::from_value(self.setup.clone()).unwrap_or_else(|_| Setup::from_steps(vec![]))
    }
    /// A carried pair refused as a duplicate, or still waiting to be recognised: it can't be used.
    pub fn held_back(&self) -> bool {
        self.duplicate.is_some() || self.provisional_until.is_some()
    }
    pub fn is_ready(&self) -> bool {
        self.state == "ready" && !self.held_back()
    }
    /// Whether the Carbon who owns this pair is `p`.
    pub fn is_owner(&self, p: &Principal) -> bool {
        p.is_carbon() && self.owner_id == p.uuid()
    }
    /// Whether the pair ended. A removed device keeps its row and activity log, read-only.
    pub fn is_removed(&self) -> bool {
        self.removed_at.is_some()
    }
    pub fn removed_reason(&self) -> Option<EndReason> {
        self.removed_reason.as_deref().and_then(EndReason::parse)
    }
    /// Whether the session holding the device runs through this pair.
    pub fn held_here(&self) -> bool {
        self.in_use_device_id.as_deref() == Some(self.device_id.as_str())
    }
    /// The holder's session, when it runs through this pair.
    pub fn session_here(&self) -> Option<&str> {
        self.in_use_session.as_deref().filter(|_| self.held_here())
    }
    /// Whether the holder's session runs through a pair of this pair's Carbon (their side).
    pub fn held_by_side(&self) -> bool {
        self.in_use_carbon.as_deref() == Some(self.owner_id.as_str())
    }
    pub fn carried_busy(&self) -> Vec<CarriedBusy> {
        serde_json::from_value(self.carried_busy.clone()).unwrap_or_default()
    }
    pub fn open_wakes(&self) -> Vec<OpenWake> {
        serde_json::from_value(self.open_wakes.clone()).unwrap_or_default()
    }
    /// Whether Extend can tell when this device wakes: an app (or, for a carried device, a host
    /// app) of 1.1.0 or later, and not an iPhone or iPad (until a real device's lock state has
    /// been captured).
    pub fn wake_detectable(&self) -> bool {
        let app = if self.host_device_id.is_some() {
            self.host_app_version.as_deref()
        } else {
            self.app_version.as_deref()
        };
        app.is_some_and(|v| crate::routes::enroll::version_at_least(v, "1.1.0"))
            && !matches!(self.os(), DeviceOs::Ios | DeviceOs::Ipados)
    }
    pub fn sleep(&self) -> Option<SleepState> {
        self.sleep_state.as_deref().map(SleepState::parse)
    }
    pub fn in_use_indicator(&self) -> InUseIndicator {
        InUseIndicator::parse(&self.in_use_indicator)
    }
    /// The `attach` frame its host is sent for this carried pair.
    pub fn attach_frame(&self, removed: bool) -> ServiceFrame {
        ServiceFrame::Attach {
            device_id: self
                .device_id
                .parse()
                .unwrap_or_else(|_| extend_protocol::DeviceId::random()),
            os: self.os(),
            name: self.name.clone(),
            address: self.address.clone(),
            removed,
            in_use_indicator: self.in_use_indicator(),
        }
    }
}

pub fn device_select(world: &World) -> String {
    format!(
        "SELECT d.device_id, d.team, d.owner_id, d.name, d.os, d.os_version, d.model, d.address, d.visibility, d.pair_ttl_days,
                d.paired_at, d.last_activity_at, d.last_used_at, d.last_seen_at, d.version, d.host_device_id, d.state, d.setup,
                d.capabilities, d.missing, d.app_version,
                COALESCE(d.agent_device_version, hp.agent_device_version) AS engine_version,
                d.removed_at, d.removed_reason,
                d.instance_id, d.wake_muted, d.hardware_key, d.duplicate, d.provisional_until, d.first_pair,
                s.session_id AS in_use_session, s.silicon_id AS in_use_silicon, s.started_at AS in_use_since, s.state AS in_use_state,
                s.device_id AS in_use_device_id, sd.owner_id AS in_use_carbon,
                i.awake, i.sleep_state, i.awake_changed_at, i.in_use_indicator,
                EXISTS (SELECT 1 FROM {devices} o WHERE o.instance_id = d.instance_id AND o.removed_at IS NULL
                          AND o.owner_id <> d.owner_id) AS paired_by_others,
                (SELECT count(*) FROM {access} a WHERE a.device_id = d.device_id) AS access_count,
                CASE WHEN d.host_device_id IS NOT NULL THEN '[]'::jsonb ELSE COALESCE((
                    SELECT jsonb_agg(jsonb_build_object('session_id', cs.session_id, 'device_id', cs.device_id, 'instance_id', cl.instance_id,
                                                        'carbon', cd.owner_id,
                                                        'owners', (SELECT jsonb_agg(o.owner_id) FROM {devices} o
                                                                    WHERE o.instance_id = cl.instance_id AND o.removed_at IS NULL)))
                      FROM {locks} cl JOIN {sessions} cs ON cs.session_id = cl.session_id JOIN {devices} cd ON cd.device_id = cs.device_id
                     WHERE cl.instance_id IN (SELECT c.instance_id FROM {devices} c JOIN {devices} h ON h.device_id = c.host_device_id
                                               WHERE h.instance_id = d.instance_id AND c.removed_at IS NULL)
                ), '[]'::jsonb) END AS carried_busy,
                COALESCE((SELECT jsonb_agg(jsonb_build_object('from', w.from_id, 'device_id', w.device_id))
                            FROM {wakes} w WHERE w.instance_id = d.instance_id AND w.state = 'open'), '[]'::jsonb) AS open_wakes,
                hp.app_version AS host_app_version, hp.name AS host_name
         FROM {devices} d
         JOIN {instances} i ON i.instance_id = d.instance_id
         LEFT JOIN {devices} hp ON hp.device_id = d.host_device_id
         LEFT JOIN {locks} l ON l.instance_id = d.instance_id
         LEFT JOIN {sessions} s ON s.session_id = l.session_id
         LEFT JOIN {devices} sd ON sd.device_id = s.device_id",
        access = world.t("device_access"),
        devices = world.t("devices"),
        locks = world.t("device_locks"),
        sessions = world.t("sessions"),
        instances = world.t("device_instances"),
        wakes = world.t("wake_requests"),
    )
}

/// Loads a paired device; a removed one reads as absent.
pub async fn load_device(state: &AppState, world: &World, device_id: &str) -> AppResult<Option<DeviceRow>> {
    load_device_in(&state.pool, world, device_id).await
}

/// [`load_device`] through `db`: the pool, or a transaction that has to see its own writes (a
/// device it just inserted) without taking a second connection.
pub async fn load_device_in<'c>(
    db: impl sqlx::PgExecutor<'c>,
    world: &World,
    device_id: &str,
) -> AppResult<Option<DeviceRow>> {
    let sql = format!(
        "{} WHERE d.device_id = $1 AND d.removed_at IS NULL",
        device_select(world)
    );
    Ok(sqlx::query_as::<_, DeviceRow>(sqlx::AssertSqlSafe(sql.clone()))
        .bind(device_id)
        .fetch_optional(db)
        .await?)
}

/// Loads a device whether or not its pair has ended.
pub async fn load_device_any(state: &AppState, world: &World, device_id: &str) -> AppResult<Option<DeviceRow>> {
    let sql = format!("{} WHERE d.device_id = $1", device_select(world));
    Ok(sqlx::query_as::<_, DeviceRow>(sqlx::AssertSqlSafe(sql.clone()))
        .bind(device_id)
        .fetch_optional(&state.pool)
        .await?)
}

/// Every live pair of one physical device.
pub async fn pairs_of(state: &AppState, world: &World, instance_id: Uuid) -> AppResult<Vec<DeviceRow>> {
    Ok(sqlx::query_as::<_, DeviceRow>(sql!(
        "{} WHERE d.instance_id = $1 AND d.removed_at IS NULL ORDER BY d.paired_at",
        device_select(world)
    ))
    .bind(instance_id)
    .fetch_all(&state.pool)
    .await?)
}

pub fn device_not_found(device_id: &str) -> AppError {
    AppError::new(
        ErrorCode::DeviceNotFound,
        format!("No device {device_id} is visible to you."),
    )
    .hint("List the devices you can see with `extend device ls`.")
}

/// What the Carbon who paired a removed device hears when they try to change or use it. Everyone
/// else gets [`device_not_found`], so nobody else learns the device existed.
pub fn device_removed(d: &DeviceRow) -> AppError {
    let when = d
        .removed_at
        .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_default();
    let reason = d.removed_reason();
    let why = match reason {
        Some(EndReason::DeviceRemoved) | None => "its Carbon removed it",
        Some(EndReason::PairRevoked) => "the pair was revoked on the device",
        Some(EndReason::PairExpired) => "it went unused for longer than its pairing lasts",
        Some(r) => r.explain(),
    };
    AppError::new(
        ErrorCode::DeviceNotFound,
        format!(
            "Device {} ({}) was removed at {when}: {why}. A removed device can't be changed or used.",
            d.device_id, d.name
        ),
    )
    .hint(format!(
        "Its activity log stays readable: `extend device activity {}`, or the device's page on the website. \
         To use the device again, pair it again.",
        d.device_id
    ))
    .details(serde_json::json!({"removed_at": when, "removed_reason": reason.map(EndReason::as_str)}))
}

/// What a caller may do with a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// The Carbon who paired it (this pair).
    Owner,
    /// A Silicon with a grant on this pair.
    Silicon,
}

/// The access `p` has to a pair: its Carbon, or a Silicon granted on it. Nobody else sees it.
pub async fn access_of(state: &AppState, world: &World, d: &DeviceRow, p: &Principal) -> AppResult<Option<Access>> {
    if d.is_owner(p) {
        return Ok(Some(Access::Owner));
    }
    if !p.is_silicon() {
        return Ok(None);
    }
    let has: Option<(i32,)> = sqlx::query_as(sql!(
        "SELECT 1 FROM {} WHERE device_id = $1 AND silicon_id = $2",
        world.t("device_access")
    ))
    .bind(&d.device_id)
    .bind(p.uuid())
    .fetch_optional(&state.pool)
    .await?;
    Ok(has.map(|_| Access::Silicon))
}

fn check_device_id(device_id: &str) -> AppResult<()> {
    if device_id.parse::<extend_protocol::DeviceId>().is_err() {
        return Err(AppError::invalid(format!(
            "{device_id:?} is not a device id; device ids are 8 lowercase hexadecimal characters, like 7c1e09ab."
        )));
    }
    Ok(())
}

/// Loads a paired device the caller can see, with the access they have. A removed device answers
/// device_not_found: with what happened for the Carbon who paired it, plainly for everyone else.
pub async fn visible_device(
    state: &AppState,
    world: &World,
    device_id: &str,
    p: &Principal,
) -> AppResult<(DeviceRow, Access)> {
    check_device_id(device_id)?;
    let d = load_device_any(state, world, device_id)
        .await?
        .ok_or_else(|| device_not_found(device_id))?;
    if d.is_removed() {
        return Err(if d.is_owner(p) {
            device_removed(&d)
        } else {
            device_not_found(device_id)
        });
    }
    let access = access_of(state, world, &d, p)
        .await?
        .ok_or_else(|| device_not_found(device_id))?;
    Ok((d, access))
}

/// Like [`visible_device`], for reads: the Carbon who paired a removed device can still read it and
/// its activity log. Nobody else can see a removed device.
pub async fn readable_device(
    state: &AppState,
    world: &World,
    device_id: &str,
    p: &Principal,
) -> AppResult<(DeviceRow, Access)> {
    check_device_id(device_id)?;
    let d = load_device_any(state, world, device_id)
        .await?
        .ok_or_else(|| device_not_found(device_id))?;
    if d.is_removed() {
        return if d.is_owner(p) {
            Ok((d, Access::Owner))
        } else {
            Err(device_not_found(device_id))
        };
    }
    let access = access_of(state, world, &d, p)
        .await?
        .ok_or_else(|| device_not_found(device_id))?;
    Ok((d, access))
}

async fn require_owner(state: &AppState, d: DeviceRow, access: Access) -> AppResult<DeviceRow> {
    if access != Access::Owner {
        let owner = state.accounts.directory.public_id(&d.owner_id).await;
        return Err(AppError::new(
            ErrorCode::NotOwner,
            format!("Only {owner} (the Carbon who paired {}) can do this.", d.name),
        ));
    }
    Ok(d)
}

/// A paired device the caller owns, for changing it.
pub async fn owned_device(state: &AppState, world: &World, device_id: &str, p: &Principal) -> AppResult<DeviceRow> {
    let (d, access) = visible_device(state, world, device_id, p).await?;
    require_owner(state, d, access).await
}

/// A device the caller owns, paired or removed, for reading its record and logs.
pub async fn owned_readable_device(
    state: &AppState,
    world: &World,
    device_id: &str,
    p: &Principal,
) -> AppResult<DeviceRow> {
    let (d, access) = readable_device(state, world, device_id, p).await?;
    require_owner(state, d, access).await
}

pub async fn is_online(state: &AppState, world: &World, d: &DeviceRow) -> bool {
    let route = d.route(world);
    if d.host_device_id.is_some() {
        return state
            .hub
            .attached(&route, &d.key(world))
            .await
            .is_some_and(|a| a.online);
    }
    state.hub.is_connected(&route).await
}

/// Who a device view is for.
#[derive(Debug, Clone, Copy)]
pub struct Viewer<'a> {
    pub access: Access,
    /// The viewer's uuid.
    pub id: &'a str,
    /// The device app's own view of its pair: shaped exactly as 1.x apps read it (no uuids).
    pub device_wire: bool,
}

impl<'a> Viewer<'a> {
    pub fn of(access: Access, p: &'a Principal) -> Self {
        Self {
            access,
            id: p.uuid(),
            device_wire: false,
        }
    }
    /// The device app's own view (an owner view of its pair).
    pub fn owner(d: &'a DeviceRow) -> Self {
        Self {
            access: Access::Owner,
            id: &d.owner_id,
            device_wire: true,
        }
    }
}

/// The setup a device's owner sees: what the device reported, plus a failing step when the service
/// holds a carried pair back (a duplicate, or waiting to be recognised in a test environment).
pub async fn setup_of(state: &AppState, world: &World, d: &DeviceRow) -> Setup {
    let mut setup = d.setup();
    let what = format!("This {}", kind_word(d.os()));
    let step = match (d.duplicate.as_deref(), d.provisional_until) {
        (Some(dup), _) if dup.starts_with("own:") => {
            let other = dup.trim_start_matches("own:");
            let (name, host): (Option<String>, Option<String>) = match load_device_any(state, world, other).await {
                Ok(Some(o)) => (Some(o.name), o.host_name),
                _ => (None, None),
            };
            Some(SetupStep {
                key: "duplicate_device".into(),
                title: "Added twice".into(),
                status: StepStatus::Failed,
                help: None,
                error: Some(format!(
                    "{what} is already added as {} ({other}) through {}; remove one of them.",
                    name.unwrap_or_else(|| "another device".into()),
                    host.unwrap_or_else(|| "a computer".into())
                )),
                input: None,
            })
        }
        (Some(_), _) => Some(SetupStep {
            key: "duplicate_device".into(),
            title: "Added through another computer".into(),
            status: StepStatus::Failed,
            help: None,
            error: Some(format!(
                "{what} is already added to Extend through another computer. Add it through that computer instead: \
                 on its Extend app choose Pair with another Carbon, then add the {} there.",
                kind_word(d.os())
            )),
            input: None,
        }),
        (None, Some(_)) => Some(SetupStep {
            key: "recognising".into(),
            title: format!(
                "Waiting for {} to recognise this device",
                d.host_name.as_deref().unwrap_or("the computer")
            ),
            status: StepStatus::InProgress,
            help: None,
            error: None,
            input: None,
        }),
        (None, None) => None,
    };
    if let Some(step) = step {
        setup.steps.insert(0, step);
        setup = Setup::from_steps(setup.steps);
    }
    setup
}

/// A device as `viewer` may see it (see the module docs for sides).
pub async fn device_view(state: &AppState, world: &World, d: &DeviceRow, viewer: Viewer<'_>, detail: bool) -> Device {
    let os = d.os();
    // A removed device's host may still be connected; the device itself is gone.
    let removed = d.is_removed();
    let online = !removed && is_online(state, world, d).await;
    let owner_view = viewer.access == Access::Owner;
    let directory = &state.accounts.directory;
    let mut owner = directory.member(MemberKind::Carbon, &d.owner_id).await;
    owner.kind = MemberKind::Carbon;
    if viewer.device_wire {
        owner = Member::new(MemberKind::Carbon, owner.id);
    }
    let expires = d.last_activity_at + time::Duration::days(i64::from(d.pair_ttl_days));
    let days_left = ((expires - OffsetDateTime::now_utc()).whole_hours() as f64 / 24.0)
        .ceil()
        .max(0.0) as i64;
    let holder = match (&d.in_use_session, &d.in_use_silicon, d.in_use_since) {
        (Some(s), Some(si), Some(since)) => match s.parse() {
            Ok(session_id) => Some(InUse {
                silicon_id: directory.public_id(si).await,
                session_id,
                since,
                paused: d.in_use_state.as_deref() == Some("paused"),
                team: None,
                silicon_uuid: (!viewer.device_wire).then(|| si.clone()),
            }),
            Err(_) => None,
        },
        _ => None,
    };
    // What the viewer may see of who holds the device, and of what its computer carries.
    let busy = d.carried_busy();
    let (in_use, in_use_by_other, in_use_by_other_carried) = if owner_view {
        let other_carried: Vec<&CarriedBusy> = busy.iter().filter(|b| b.carbon != viewer.id).collect();
        if d.held_here() {
            (holder, false, false)
        } else if d.in_use_session.is_some() {
            (None, true, false)
        } else if !other_carried.is_empty() {
            let carried_only = other_carried.iter().all(|b| !b.owners.iter().any(|o| o == viewer.id));
            (None, true, carried_only)
        } else {
            (None, false, false)
        }
    } else {
        let own = d.in_use_silicon.as_deref() == Some(viewer.id);
        // Another Silicon is named only when it runs through this same pair (the same Carbon gave
        // both access) and is in the viewer's custodian circle.
        let named = match &d.in_use_silicon {
            Some(holder_id) if !own && d.held_here() && d.held_by_side() => {
                directory.same_circle(viewer.id, holder_id).await
            }
            _ => false,
        };
        let other_carried = busy.iter().any(|b| b.carbon != d.owner_id);
        if d.in_use_session.is_some() && (own || named) {
            (holder, false, false)
        } else if d.in_use_session.is_some() || other_carried {
            (None, true, false)
        } else {
            (None, false, false)
        }
    };
    let awake = if online { d.awake } else { None };
    let sleep_state = if online && d.awake == Some(false) {
        d.sleep()
    } else {
        None
    };
    let last_sleep_state = if !online && !removed { d.sleep() } else { None };
    let wakes = d.open_wakes();
    let open_wake_requests = if owner_view {
        wakes.iter().filter(|w| w.device_id == d.device_id).count()
    } else {
        wakes.iter().filter(|w| w.from == viewer.id).count()
    } as i64;
    let mut caps = d.capabilities();
    if !online {
        caps.clear();
    }
    let same_device = if !owner_view && d.paired_by_others && !removed {
        same_device(state, world, d, viewer).await
    } else {
        None
    };
    let wake_requests = if detail && !removed {
        Some(crate::wake::open_views(state, world, d, viewer).await)
    } else {
        None
    };
    Device {
        device_id: d
            .device_id
            .parse()
            .unwrap_or_else(|_| extend_protocol::DeviceId::random()),
        name: d.name.clone(),
        os,
        os_version: d.os_version.clone(),
        model: d.model.clone(),
        kind: os.kind(),
        owner,
        team: None,
        visibility: Visibility::Personal,
        host_device_id: if owner_view {
            d.host_device_id.as_ref().and_then(|h| h.parse().ok())
        } else {
            None
        },
        state: if d.is_ready() {
            DeviceState::Ready
        } else {
            DeviceState::Setup
        },
        online,
        last_seen_at: d.last_seen_at,
        in_use,
        last_used_at: if owner_view { d.last_used_at } else { None },
        paired_at: Some(d.paired_at),
        pair_ttl_days: Some(d.pair_ttl_days),
        // A removed device's pair no longer runs out; it already ended.
        pair_expires_at: (!removed).then_some(expires),
        days_left: (!removed).then_some(days_left),
        access_count: Some(d.access_count),
        app_version: d.app_version.clone(),
        version: Some(d.version),
        capabilities: detail.then(|| caps.clone()),
        missing: detail.then(|| {
            let mut m = d.missing();
            if removed {
                m.insert(
                    0,
                    MissingCapability {
                        capability: Capability::ScreenRead,
                        reason: "The device was removed, so nothing works on it any more. Pair it again to use it."
                            .into(),
                    },
                );
            } else if !online {
                m.insert(
                    0,
                    MissingCapability {
                        capability: Capability::ScreenRead,
                        reason: "The device is offline, so nothing works on it right now.".into(),
                    },
                );
            }
            m
        }),
        commands: detail.then(|| {
            extend_protocol::COMMANDS
                .iter()
                .filter(|spec| d.command_requirements(spec).iter().any(|c| caps.contains(c)))
                .map(|spec| spec.name.to_owned())
                .collect()
        }),
        removed_at: d.removed_at,
        removed_reason: d.removed_reason(),
        engine_version: d.engine_version.clone(),
        agent_device_version: d.engine_version.clone(),
        awake,
        sleep_state,
        last_sleep_state,
        awake_changed_at: if owner_view { d.awake_changed_at } else { None },
        wake_detectable: (!removed).then(|| d.wake_detectable()),
        in_use_by_other,
        in_use_by_other_carried,
        open_wake_requests: (!removed).then_some(open_wake_requests),
        wake_requests,
        wake_muted: owner_view.then_some(d.wake_muted),
        paired_by_others: owner_view.then_some(d.paired_by_others),
        same_device,
        in_use_indicator: d.in_use_indicator(),
    }
}

/// Who changed a device's in-use banner (see [`set_in_use_indicator`]).
pub enum BannerChangedBy<'a> {
    /// A Carbon who paired it, through their own pair (the website or the CLI).
    Carbon { device_id: &'a str, member: &'a Member },
    /// The device's own Extend app, with one of its pair credentials.
    Device,
}

/// Shows or hides the in-use banner on a physical device: one setting shared by every pair of it.
/// Returns false when it already had that value (nothing is logged or sent then).
///
/// The side that changed it logs it: a Carbon on their own pair only, so no other Carbon learns
/// who; the device's own app on every pair, as the device (it names no Carbon). Then every live
/// connection of the device re-reads it: a pair's own app gets `refresh`; the computer carrying a
/// device gets its `attach` again, with the new value.
pub async fn set_in_use_indicator(
    state: &AppState,
    world: &World,
    instance_id: Uuid,
    value: InUseIndicator,
    by: BannerChangedBy<'_>,
) -> AppResult<bool> {
    let own = match &by {
        BannerChangedBy::Carbon { device_id, .. } => Some(*device_id),
        BannerChangedBy::Device => None,
    };
    let mut tx = state.pool.begin().await?;
    let changed = update_in_use_indicator(&mut tx, world, instance_id, value, own).await?;
    tx.commit().await?;
    if changed {
        notify_in_use_indicator(state, world, instance_id, value, by).await?;
    }
    Ok(changed)
}

/// Part of the caller's transaction so a settings PATCH and its If-Match check are atomic.
pub async fn update_in_use_indicator(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    world: &World,
    instance_id: Uuid,
    value: InUseIndicator,
    own: Option<&str>,
) -> AppResult<bool> {
    // Lock order: the instance row first (see the module docs).
    let current: Option<String> = sqlx::query_scalar(sql!(
        "SELECT in_use_indicator FROM {} WHERE instance_id = $1 FOR NO KEY UPDATE",
        world.t("device_instances")
    ))
    .bind(instance_id)
    .fetch_optional(&mut **tx)
    .await?;
    if current.as_deref() == Some(value.as_str()) || current.is_none() {
        return Ok(false);
    }
    sqlx::query(sql!(
        "UPDATE {} SET in_use_indicator = $2 WHERE instance_id = $1",
        world.t("device_instances")
    ))
    .bind(instance_id)
    .bind(value.as_str())
    .execute(&mut **tx)
    .await?;
    // Every pair's view changed, so every pair's ETag does; only the changing Carbon's pair counts
    // it as activity (it keeps that pair from running out).
    sqlx::query(sql!(
        "UPDATE {} SET version = version + 1,
                last_activity_at = CASE WHEN device_id = $2 THEN now() ELSE last_activity_at END
         WHERE instance_id = $1 AND removed_at IS NULL",
        world.t("devices")
    ))
    .bind(instance_id)
    .bind(own)
    .execute(&mut **tx)
    .await?;
    Ok(true)
}

pub async fn notify_in_use_indicator(
    state: &AppState,
    world: &World,
    instance_id: Uuid,
    value: InUseIndicator,
    by: BannerChangedBy<'_>,
) -> AppResult<()> {
    let action = if value.shows() { "banner_shown" } else { "banner_hidden" };
    let pairs = pairs_of(state, world, instance_id).await?;
    match by {
        BannerChangedBy::Carbon { device_id, member } => {
            log(
                state,
                world,
                device_id,
                member,
                action,
                None,
                serde_json::json!({"in_use_indicator": value.as_str()}),
            )
            .await;
        }
        BannerChangedBy::Device => {
            for p in &pairs {
                log(
                    state,
                    world,
                    &p.device_id,
                    &system_member(),
                    action,
                    None,
                    serde_json::json!({"in_use_indicator": value.as_str(), "on_device": true}),
                )
                .await;
            }
        }
    }
    for p in &pairs {
        if p.host_device_id.is_some() {
            let _ = state.hub.send(&p.route(world), p.attach_frame(false)).await;
        } else {
            let _ = state.hub.send(&p.key(world), ServiceFrame::Refresh).await;
        }
    }
    Ok(())
}

/// Silicon views: the other pairs of the same physical device the Silicon has a grant on
/// (another Carbon's id for it).
async fn same_device(
    state: &AppState,
    world: &World,
    d: &DeviceRow,
    viewer: Viewer<'_>,
) -> Option<Vec<extend_protocol::DeviceId>> {
    let ids: Vec<(String,)> = sqlx::query_as(sql!(
        "SELECT o.device_id FROM {} o JOIN {} a ON a.device_id = o.device_id
         WHERE o.instance_id = $1 AND o.device_id <> $2 AND o.removed_at IS NULL AND a.silicon_id = $3
         ORDER BY o.device_id",
        world.t("devices"),
        world.t("device_access")
    ))
    .bind(d.instance_id)
    .bind(&d.device_id)
    .bind(viewer.id)
    .fetch_all(&state.pool)
    .await
    .ok()?;
    let ids: Vec<_> = ids.into_iter().filter_map(|(i,)| i.parse().ok()).collect();
    (!ids.is_empty()).then_some(ids)
}

#[derive(Debug, Clone, FromRow)]
pub struct SessionRow {
    pub session_id: String,
    pub device_id: String,
    /// The Silicon (uuid).
    pub silicon_id: String,
    pub state: String,
    pub started_at: OffsetDateTime,
    pub last_command_at: Option<OffsetDateTime>,
    pub idle_ends_at: Option<OffsetDateTime>,
    pub ended_at: Option<OffsetDateTime>,
    pub end_reason: Option<String>,
    pub command_count: i64,
    pub takeover: Option<serde_json::Value>,
}

impl SessionRow {
    pub fn view(&self) -> Session {
        Session {
            session_id: self
                .session_id
                .parse()
                .unwrap_or_else(|_| extend_protocol::SessionId::from_parts(0, 3)),
            device_id: self
                .device_id
                .parse()
                .unwrap_or_else(|_| extend_protocol::DeviceId::random()),
            silicon_id: self.silicon_id.clone(),
            state: match self.state.as_str() {
                "active" => SessionState::Active,
                "paused" => SessionState::Paused,
                _ => SessionState::Ended,
            },
            started_at: self.started_at,
            last_command_at: self.last_command_at,
            idle_ends_at: if self.state == "ended" { None } else { self.idle_ends_at },
            ended_at: self.ended_at,
            end_reason: self.end_reason.as_deref().and_then(EndReason::parse),
            command_count: self.command_count,
            device: None,
            capabilities: None,
            commands: None,
            team: None,
            silicon_uuid: Some(self.silicon_id.clone()),
        }
    }

    /// [`Self::view`] with the Silicon shown by its current public id.
    pub async fn view_for(&self, state: &AppState) -> Session {
        let mut v = self.view();
        v.silicon_id = state.accounts.directory.public_id(&self.silicon_id).await;
        v
    }
}

pub const SESSION_COLUMNS: &str = "session_id, device_id, silicon_id, state, started_at, last_command_at, idle_ends_at, ended_at, end_reason, command_count, takeover";

pub async fn load_session(state: &AppState, world: &World, session_id: &str) -> AppResult<Option<SessionRow>> {
    Ok(sqlx::query_as::<_, SessionRow>(sql!(
        "SELECT {SESSION_COLUMNS} FROM {} WHERE session_id = $1",
        world.t("sessions")
    ))
    .bind(session_id)
    .fetch_optional(&state.pool)
    .await?)
}

/// Writes one activity row on a pair. Rows stay per pair, so each Carbon's log is their own side;
/// `details` never name another side's Silicon, Carbon or session. `actor.id` is a uuid (or
/// [`SYSTEM_ACTOR`]).
pub async fn log(
    state: &AppState,
    world: &World,
    device_id: &str,
    actor: &Member,
    action: &str,
    session_id: Option<&str>,
    details: serde_json::Value,
) {
    let res = sqlx::query(sql!(
        "INSERT INTO {} (id, device_id, actor_kind, actor_id, action, session_id, details) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        world.t("activity")
    ))
    .bind(Uuid::now_v7())
    .bind(device_id)
    .bind(match actor.kind {
        MemberKind::Carbon => "carbon",
        MemberKind::Silicon => "silicon",
    })
    .bind(&actor.id)
    .bind(action)
    .bind(session_id)
    .bind(details)
    .execute(&state.pool)
    .await;
    if let Err(e) = res {
        tracing::error!(error = %e, device_id, action, "writing the activity log failed");
    }
}

pub fn system_member() -> Member {
    let mut m = Member::new(MemberKind::Carbon, SYSTEM_ACTOR);
    m.display_name = Some("Silicon Extend".into());
    m
}

// ───────────── Sides and lock groups ─────────────

/// The first [`extend_protocol::SIDE_TAG_LEN`] hex characters of HMAC-SHA256(salt, owner uuid).
/// Equal exactly when the sides are equal inside one lock group; they name no Carbon, and differ
/// between devices (each group host has its own salt).
pub fn side_tag(salt: &str, owner_id: &str) -> String {
    let mut mac = <Hmac<Sha256> as hmac::Mac>::new_from_slice(salt.as_bytes()).expect("HMAC takes any key length");
    mac.update(owner_id.as_bytes());
    let hex = extend_protocol::ids::hex_lower(&mac.finalize().into_bytes());
    hex[..extend_protocol::SIDE_TAG_LEN].to_owned()
}

/// A lock group: a computer's instance and every instance carried by any pair of it. A carried
/// device's group is its computer's.
#[derive(Debug, Clone)]
pub struct LockGroup {
    pub host: Uuid,
    /// Every instance in the group, the host's included, in `instance_id` order (the lock order).
    pub members: Vec<Uuid>,
}

pub async fn lock_group<'c>(db: impl sqlx::PgExecutor<'c>, world: &World, instance: Uuid) -> AppResult<LockGroup> {
    let rows: Vec<(Uuid, bool)> = sqlx::query_as(sql!(
        "WITH host AS (
             SELECT COALESCE((SELECT h.instance_id FROM {devices} c JOIN {devices} h ON h.device_id = c.host_device_id
                               WHERE c.instance_id = $1 AND c.removed_at IS NULL LIMIT 1), $1) AS id)
         SELECT host.id, true FROM host
         UNION
         SELECT c.instance_id, false FROM {devices} c JOIN {devices} h ON h.device_id = c.host_device_id, host
          WHERE h.instance_id = host.id AND c.removed_at IS NULL AND h.removed_at IS NULL",
        devices = world.t("devices")
    ))
    .bind(instance)
    .fetch_all(db)
    .await?;
    let host = rows.iter().find(|(_, h)| *h).map_or(instance, |(i, _)| *i);
    let mut members: Vec<Uuid> = rows.into_iter().map(|(i, _)| i).collect();
    members.sort();
    members.dedup();
    Ok(LockGroup { host, members })
}

/// Takes the lock order's first step: these instance rows, `FOR NO KEY UPDATE`, in id order.
pub async fn lock_instances(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    world: &World,
    instances: &[Uuid],
) -> AppResult<()> {
    let mut ids = instances.to_vec();
    ids.sort();
    ids.dedup();
    sqlx::query(sql!(
        "SELECT instance_id FROM {} WHERE instance_id = ANY($1) ORDER BY instance_id FOR NO KEY UPDATE",
        world.t("device_instances")
    ))
    .bind(&ids)
    .fetch_all(&mut **tx)
    .await?;
    Ok(())
}

/// The salt a lock group's side tags are keyed with: its host instance's.
pub async fn group_salt(state: &AppState, world: &World, instance: Uuid) -> AppResult<String> {
    let group = lock_group(&state.pool, world, instance).await?;
    let salt: Option<String> = sqlx::query_scalar(sql!(
        "SELECT side_salt FROM {} WHERE instance_id = $1",
        world.t("device_instances")
    ))
    .bind(group.host)
    .fetch_optional(&state.pool)
    .await?;
    Ok(salt.unwrap_or_default())
}

/// The side tag of the Carbon who owns `d`, on that device's lock group.
pub async fn side_of(state: &AppState, world: &World, d: &DeviceRow) -> AppResult<String> {
    Ok(side_tag(&group_salt(state, world, d.instance_id).await?, &d.owner_id))
}

/// Records activity on every live pair of a physical device: a pair lasts while the device has
/// activity, through any pair (UNDERSTANDING.md, Pairing).
pub async fn bump_activity<'c>(db: impl sqlx::PgExecutor<'c>, world: &World, instance: Uuid) -> AppResult<()> {
    sqlx::query(sql!(
        "UPDATE {} SET last_activity_at = now() WHERE instance_id = $1 AND removed_at IS NULL",
        world.t("devices")
    ))
    .bind(instance)
    .execute(db)
    .await?;
    Ok(())
}

// ───────────── Ending sessions ─────────────

/// Ends a session: releases the device, tells the device, and logs why. Safe to call twice.
pub async fn end_session(
    state: &AppState,
    world: &World,
    session_id: &str,
    reason: EndReason,
    actor: &Member,
) -> AppResult<Option<SessionRow>> {
    end_session_with(state, world, session_id, reason, actor, serde_json::Value::Null).await
}

/// [`end_session`], with more to say in the `session_ended` activity row (`{"stopped_by":
/// "another_carbon"}` when a Carbon who paired the device through another pair stopped it).
pub async fn end_session_with(
    state: &AppState,
    world: &World,
    session_id: &str,
    reason: EndReason,
    actor: &Member,
    extra: serde_json::Value,
) -> AppResult<Option<SessionRow>> {
    let mut tx = state.pool.begin().await?;
    let row: Option<SessionRow> = sqlx::query_as(sql!(
        "UPDATE {} SET state = 'ended', ended_at = now(), end_reason = $2, idle_ends_at = NULL
         WHERE session_id = $1 AND state <> 'ended' RETURNING {SESSION_COLUMNS}",
        world.t("sessions")
    ))
    .bind(session_id)
    .bind(reason.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    sqlx::query(sql!("DELETE FROM {} WHERE session_id = $1", world.t("device_locks")))
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let Some(row) = row else {
        return Ok(None);
    };
    state
        .session_principals
        .write()
        .await
        .remove(&(world.schema.clone(), session_id.to_owned()));
    let device = load_device(state, world, &row.device_id).await?;
    if let Some(d) = &device {
        let target = d.host_device_id.as_ref().and_then(|_| d.device_id.parse().ok());
        let _ = state
            .hub
            .send(
                &d.route(world),
                ServiceFrame::SessionEnded {
                    target,
                    session_id: row
                        .session_id
                        .parse()
                        .unwrap_or_else(|_| extend_protocol::SessionId::from_parts(0, 3)),
                    reason,
                },
            )
            .await;
    }
    let mut details = serde_json::json!({"reason": reason.as_str(), "explain": reason.explain()});
    if let (Some(d), Some(extra)) = (details.as_object_mut(), extra.as_object()) {
        d.extend(extra.clone());
    }
    log(
        state,
        world,
        &row.device_id,
        actor,
        "session_ended",
        Some(session_id),
        details,
    )
    .await;
    tracing::info!(world = %world.schema, session_id, reason = reason.as_str(), "session ended");
    if let Some(d) = device {
        // Wake requests the holder's side hid can show again; and on a computer several Carbons
        // paired, credentials a Silicon could have copied during the session stop working.
        crate::wake::resend_group(state, world, d.instance_id).await;
        rotate_credentials(state, world, d.instance_id).await;
    }
    Ok(Some(row))
}

/// Ends every running session on a pair.
pub async fn end_device_sessions(
    state: &AppState,
    world: &World,
    device_id: &str,
    reason: EndReason,
    actor: &Member,
) -> AppResult<()> {
    let ids: Vec<(String,)> = sqlx::query_as(sql!(
        "SELECT session_id FROM {} WHERE device_id = $1 AND state <> 'ended'",
        world.t("sessions")
    ))
    .bind(device_id)
    .fetch_all(&state.pool)
    .await?;
    for (id,) in ids {
        end_session(state, world, &id, reason, actor).await?;
    }
    Ok(())
}

/// Ends the running sessions of the Silicons one Carbon gave access to: every session through that
/// Carbon's pairs. Never another Carbon's side.
pub async fn end_carbon_side(
    state: &AppState,
    world: &World,
    carbon: &str,
    reason: EndReason,
    actor: &Member,
) -> AppResult<Vec<String>> {
    let ids: Vec<(String,)> = sqlx::query_as(sql!(
        "SELECT s.session_id FROM {} s JOIN {} d ON d.device_id = s.device_id
         WHERE d.owner_id = $1 AND s.state <> 'ended'",
        world.t("sessions"),
        world.t("devices")
    ))
    .bind(carbon)
    .fetch_all(&state.pool)
    .await?;
    let mut ended = Vec::new();
    for (id,) in ids {
        if end_session(state, world, &id, reason, actor).await?.is_some() {
            ended.push(id);
        }
    }
    Ok(ended)
}

/// Ends every running session of one Silicon (it signed out, removed Extend's access, or was
/// deleted).
pub async fn end_silicon_sessions(
    state: &AppState,
    world: &World,
    silicon: &str,
    reason: EndReason,
    actor: &Member,
) -> AppResult<Vec<String>> {
    let ids: Vec<(String,)> = sqlx::query_as(sql!(
        "SELECT session_id FROM {} WHERE silicon_id = $1 AND state <> 'ended'",
        world.t("sessions")
    ))
    .bind(silicon)
    .fetch_all(&state.pool)
    .await?;
    let mut ended = Vec::new();
    for (id,) in ids {
        if end_session(state, world, &id, reason, actor).await?.is_some() {
            ended.push(id);
        }
    }
    Ok(ended)
}

/// Replaces the credentials of a computer several Carbons paired, at the end of a session in its
/// lock group: its terminal runs as the computer's account, so a Silicon could have copied any
/// pair's credential from it during the session. Each pair gets a new one over its own connection
/// (or at its next connect), and the old one stops working once the app confirms. Android pairs are
/// never rotated (their credentials are sealed with the app's keystore key), a computer one Carbon
/// paired exposes nothing new, and apps older than 1.1.0 can't take a new credential.
pub async fn rotate_credentials(state: &AppState, world: &World, instance: Uuid) {
    let result = async {
        let group = lock_group(&state.pool, world, instance).await?;
        let pairs = pairs_of(state, world, group.host).await?;
        let owners: std::collections::HashSet<&str> = pairs.iter().map(|p| p.owner_id.as_str()).collect();
        let eligible = pairs.first().is_some_and(DeviceRow::is_computer)
            && owners.len() >= 2
            && pairs.iter().all(|p| {
                p.app_version
                    .as_deref()
                    .is_some_and(|v| crate::routes::enroll::version_at_least(v, "1.1.0"))
            });
        if !eligible {
            return AppResult::Ok(());
        }
        for p in &pairs {
            send_new_credential(state, world, &p.device_id).await?;
        }
        tracing::info!(world = %world.schema, instance = %group.host, pairs = pairs.len(), "rotated the credentials of a computer several Carbons paired");
        Ok(())
    }
    .await;
    if let Err(e) = result {
        tracing::error!(world = %world.schema, instance = %instance, error = %e, "rotating credentials failed");
    }
}

/// Makes a new credential for a pair, keeps its digest as the one waiting for confirmation (a
/// second rotation before the app confirms replaces it), and sends it on the pair's connection.
/// Returns whether it was sent. The credential itself is never stored or logged.
pub async fn send_new_credential(state: &AppState, world: &World, device_id: &str) -> AppResult<bool> {
    let key = (world.schema.clone(), device_id.to_owned());
    if !state.hub.is_connected(&key).await {
        // A rotation is owed: the pair's next connection gets a new credential in its greeting.
        // The marker can never equal a credential's digest (64 hex characters), so it authenticates
        // nothing.
        sqlx::query(sql!(
            "UPDATE {} SET next_credential_digest = COALESCE(next_credential_digest, 'pending:' || device_id)
             WHERE device_id = $1 AND removed_at IS NULL",
            world.t("devices")
        ))
        .bind(device_id)
        .execute(&state.pool)
        .await?;
        return Ok(false);
    }
    let credential = extend_protocol::ids::new_secret(extend_protocol::ids::DEVICE_CREDENTIAL_PREFIX);
    let digest = extend_protocol::ids::secret_digest(&credential);
    let stored = sqlx::query(sql!(
        "UPDATE {} SET next_credential_digest = $2 WHERE device_id = $1 AND removed_at IS NULL",
        world.t("devices")
    ))
    .bind(device_id)
    .bind(&digest)
    .execute(&state.pool)
    .await?
    .rows_affected();
    if stored == 0 {
        return Ok(false);
    }
    Ok(state
        .hub
        .send(
            &key,
            ServiceFrame::Credential {
                device_credential: extend_protocol::DeviceCredential::new(credential),
            },
        )
        .await)
}

// ───────────── Ending grants and pairs ─────────────

/// Which grants [`revoke_grants`] ends.
#[derive(Debug, Clone, Copy)]
pub enum RevokeScope<'a> {
    /// One Silicon's grant on one pair.
    Pair { device_id: &'a str, silicon_id: &'a str },
    /// Every grant of a Silicon (its account was deleted).
    Silicon { silicon_id: &'a str },
    /// Every grant a Carbon gave a Silicon (the Silicon's custodian changed away from that Carbon).
    Granter { carbon_id: &'a str, silicon_id: &'a str },
}

/// Why grants ended, for the activity log, the sessions and the wake requests they end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantEnd {
    /// The Carbon took access away.
    Removed,
    /// The Silicon's custodian gave the grant up for it.
    Renounced,
    /// The Silicon's custodian changed: the grants the previous custodian gave end.
    CustodianChanged,
    /// The Silicon's account was deleted.
    AccountDeleted,
}

impl GrantEnd {
    fn word(self) -> Option<&'static str> {
        match self {
            Self::Removed => None,
            Self::Renounced => Some("renounced_by_custodian"),
            Self::CustodianChanged => Some("custodian_changed"),
            Self::AccountDeleted => Some("account_deleted"),
        }
    }
}

/// A grant that [`revoke_grants`] ended.
#[derive(Debug, Clone)]
pub struct Revoked {
    pub device_id: String,
    pub silicon_id: String,
}

/// Ends grants: deletes them under the lock order (so a session start either sees no grant or
/// has its new session ended here), ends the sessions they allowed, withdraws their open wake
/// requests, and logs `access_revoked` on each pair. A grant that ends because its Silicon was
/// deleted is archived first (`device_access_archive`), so the history keeps it.
pub async fn revoke_grants(
    state: &AppState,
    world: &World,
    scope: RevokeScope<'_>,
    why: GrantEnd,
    actor: &Member,
) -> AppResult<Vec<Revoked>> {
    let access = world.t("device_access");
    let devices = world.t("devices");
    let (cond, a, b): (&str, &str, Option<&str>) = match scope {
        RevokeScope::Pair { device_id, silicon_id } => {
            ("a.device_id = $1 AND a.silicon_id = $2", device_id, Some(silicon_id))
        }
        RevokeScope::Silicon { silicon_id } => ("a.silicon_id = $1 AND $2::text IS NULL", silicon_id, None),
        RevokeScope::Granter { carbon_id, silicon_id } => {
            ("a.granted_by = $1 AND a.silicon_id = $2", carbon_id, Some(silicon_id))
        }
    };
    let mut tx = state.pool.begin().await?;
    let instances: Vec<(Uuid,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT d.instance_id FROM {access} a JOIN {devices} d ON d.device_id = a.device_id WHERE {cond}"
    )))
    .bind(a)
    .bind(b)
    .fetch_all(&mut *tx)
    .await?;
    if instances.is_empty() {
        return Ok(vec![]);
    }
    lock_instances(&mut tx, world, &instances.iter().map(|(i,)| *i).collect::<Vec<_>>()).await?;
    if let Some(reason) = why.word().filter(|_| why == GrantEnd::AccountDeleted) {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {archive} (device_id, silicon_id, granted_by, granted_at, last_used_at, team, wake_muted,
                                    silicon_iam_id, granted_by_iam_id, archive_reason)
             SELECT a.device_id, a.silicon_id, a.granted_by, a.granted_at, a.last_used_at, a.team, a.wake_muted,
                    a.silicon_iam_id, a.granted_by_iam_id, $3 FROM {access} a WHERE {cond}",
            archive = world.t("device_access_archive")
        )))
        .bind(a)
        .bind(b)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
    }
    let gone: Vec<(String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "DELETE FROM {access} a WHERE {cond} RETURNING a.device_id, a.silicon_id"
    )))
    .bind(a)
    .bind(b)
    .fetch_all(&mut *tx)
    .await?;
    // Read under the instance locks: a session started just before is visible here, and one
    // starting after this commits finds no grant.
    let mut sessions = Vec::new();
    for (device_id, silicon_id) in &gone {
        let running: Vec<(String,)> = sqlx::query_as(sql!(
            "SELECT session_id FROM {} WHERE device_id = $1 AND silicon_id = $2 AND state <> 'ended'",
            world.t("sessions")
        ))
        .bind(device_id)
        .bind(silicon_id)
        .fetch_all(&mut *tx)
        .await?;
        sessions.extend(running.into_iter().map(|(s,)| s));
    }
    tx.commit().await?;
    let mut out = Vec::new();
    for (device_id, silicon_id) in gone {
        let shown = state.accounts.directory.public_id(&silicon_id).await;
        let mut details = serde_json::json!({"silicon_id": shown, "silicon_uuid": silicon_id});
        if let Some(r) = why.word() {
            details["reason"] = serde_json::json!(r);
        }
        log(state, world, &device_id, actor, "access_revoked", None, details).await;
        crate::wake::withdraw(
            state,
            world,
            crate::wake::Withdraw::Asker {
                device_id: &device_id,
                silicon_id: &silicon_id,
            },
            extend_protocol::model::WakeEndReason::AccessRemoved,
        )
        .await;
        out.push(Revoked { device_id, silicon_id });
    }
    for sid in sessions {
        end_session(state, world, &sid, EndReason::AccessRemoved, actor).await?;
    }
    Ok(out)
}

/// Ends one pair: its sessions (and those of devices carried through it) end, its grants go, its
/// open wake requests are withdrawn, its credential stops working, and `unpaired` goes to its own
/// socket only. The physical device and its other pairs are untouched. The log stays readable.
pub fn unpair<'a>(
    state: &'a AppState,
    world: &'a World,
    device_id: &'a str,
    reason: EndReason,
    actor: &'a Member,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = AppResult<()>> + Send + 'a>> {
    unpair_with(
        state,
        world,
        device_id,
        reason,
        actor,
        serde_json::json!({"reason": reason.as_str()}),
    )
}

/// [`unpair`], with the details its activity row gets.
pub fn unpair_with<'a>(
    state: &'a AppState,
    world: &'a World,
    device_id: &'a str,
    reason: EndReason,
    actor: &'a Member,
    details: serde_json::Value,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = AppResult<()>> + Send + 'a>> {
    Box::pin(async move {
        let Some(d) = load_device(state, world, device_id).await? else {
            return Ok(());
        };
        end_device_sessions(state, world, device_id, reason, actor).await?;
        let hosted: Vec<(String,)> = sqlx::query_as(sql!(
            "SELECT device_id FROM {} WHERE host_device_id = $1 AND removed_at IS NULL",
            world.t("devices")
        ))
        .bind(device_id)
        .fetch_all(&state.pool)
        .await?;
        for (child,) in hosted {
            unpair(state, world, &child, reason, actor).await?;
        }
        let mut tx = state.pool.begin().await?;
        lock_instances(&mut tx, world, &[d.instance_id]).await?;
        let ended = sqlx::query(sql!(
            "UPDATE {} SET removed_at = now(), removed_reason = $2, credential_digest = NULL, next_credential_digest = NULL
             WHERE device_id = $1 AND removed_at IS NULL",
            world.t("devices")
        ))
        .bind(device_id)
        .bind(reason.as_str())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        sqlx::query(sql!("DELETE FROM {} WHERE device_id = $1", world.t("device_access")))
            .bind(device_id)
            .execute(&mut *tx)
            .await?;
        // A carried pair refused as a duplicate of this one can be checked again at its host's
        // next report.
        if let Some(key) = &d.hardware_key {
            sqlx::query(sql!(
                "UPDATE {} SET duplicate = NULL WHERE hardware_key = $1 AND removed_at IS NULL AND duplicate IS NOT NULL",
                world.t("devices")
            ))
            .bind(key)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(sql!(
            "UPDATE {} SET duplicate = NULL WHERE duplicate = $1 AND removed_at IS NULL",
            world.t("devices")
        ))
        .bind(format!("own:{device_id}"))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        if ended == 0 {
            return Ok(());
        }
        crate::wake::withdraw(
            state,
            world,
            crate::wake::Withdraw::Pair { device_id },
            extend_protocol::model::WakeEndReason::DeviceRemoved,
        )
        .await;
        if let Some(host) = &d.host_device_id {
            let _ = state
                .hub
                .send(&(world.schema.clone(), host.clone()), d.attach_frame(true))
                .await;
        } else {
            let key = d.key(world);
            let _ = state.hub.send(&key, ServiceFrame::Unpaired { reason }).await;
            // Give the frame a moment to flush before the socket goes.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            state.hub.disconnect(&key).await;
        }
        let action = match reason {
            EndReason::PairRevoked => "pair_revoked",
            EndReason::PairExpired => "pair_expired",
            _ => "removed",
        };
        log(state, world, device_id, actor, action, None, details).await;
        Ok(())
    })
}

/// The device kind label used in messages.
pub fn kind_word(os: DeviceOs) -> &'static str {
    match os.kind() {
        DeviceKind::Phone => "phone",
        DeviceKind::Tablet => "tablet",
        DeviceKind::Tv => "TV",
        DeviceKind::Computer => "computer",
    }
}

pub fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_tags_are_short_stable_and_keyed() {
        let a = side_tag("salt-one", "aLiCe001");
        assert_eq!(a.len(), extend_protocol::SIDE_TAG_LEN);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(a, side_tag("salt-one", "aLiCe001"));
        assert_ne!(a, side_tag("salt-one", "b0B00002"));
        assert_ne!(a, side_tag("salt-two", "aLiCe001"));
        // uuids are case-sensitive.
        assert_ne!(a, side_tag("salt-one", "alice001"));
    }
}

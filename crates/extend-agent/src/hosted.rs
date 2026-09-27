//! Devices this computer carries (iPhone and iPad through a Mac; Apple TV; Samsung and LG TVs).
//!
//! The service sends `attach` with the device's id, OS, name and address, on the connection of
//! the pair it is carried for (each Carbon's carried devices come through that Carbon's pair of
//! this computer). The agent builds its driver with [`extend_hosted::driver_for`], routes commands
//! whose `target` is that id to it, and reports the driver's probe in `attached` frames on that
//! pair's connection. The list is kept in `{state}/attached.json` so the drivers are back after a
//! restart before the service says anything.
//!
//! Two Carbons may carry the same TV through the same computer (each through their own pair):
//! two device ids, one physical device. The drivers report a stable hardware id; the agent sends
//! only `hardware_key`, an HMAC of it keyed with the world's salt from `GET /api/v1/device`, so the
//! service can link the two without ever seeing the id, and the two ids share one driver here, so
//! the second Carbon doesn't pair the TV a second time. A request to wake a carried device has it
//! checked every 5 s until the request ends.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use extend_driver::{Driver, Probe};
use extend_hosted::HostedDevice;
use extend_protocol::frames::AttachedStatus;
use extend_protocol::model::{Setup, SetupState, SetupStep, StepStatus, Timestamp};
use extend_protocol::{DeviceId, DeviceOs};
use hmac::{Hmac, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

use crate::config::write_private_file;
use crate::status::{AttachedInfo, InUseInfo, TakeoverInfo};

/// Builds a hosted driver; `extend_hosted::driver_for` in production, a fake in tests.
pub type DriverFactory = Arc<dyn Fn(HostedDevice) -> Result<Box<dyn Driver>, String> + Send + Sync>;

/// How often a carried device is checked while a Silicon's request to wake it is open.
pub const WAKE_PROBE_EVERY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachRecord {
    #[serde(default)]
    pub in_use_indicator: extend_protocol::model::InUseIndicator,
    pub device_id: DeviceId,
    pub os: DeviceOs,
    pub name: String,
    #[serde(default)]
    pub address: Option<String>,
    /// The pair of this computer it is carried for. Absent in lists 1.0 saved (one pair).
    #[serde(default)]
    pub host: Option<DeviceId>,
}

struct Entry {
    record: AttachRecord,
    /// When it was attached, to prefer the older one's driver when two share a device.
    order: u64,
    driver: Result<Arc<dyn Driver>, String>,
    /// Another carried id this one shares its driver with (same physical device).
    shares_with: Option<DeviceId>,
    last_sent: Option<AttachedStatus>,
    hardware_id: Option<String>,
    in_use: Option<InUseInfo>,
    takeover: Option<TakeoverInfo>,
    setup_error: Option<String>,
    /// Open requests to wake it, with when each expires.
    wakes: HashMap<Uuid, Timestamp>,
}

pub struct HostedRegistry {
    factory: DriverFactory,
    state_dir: PathBuf,
    engine: Vec<String>,
    entries: Mutex<BTreeMap<DeviceId, Entry>>,
    next_order: Mutex<u64>,
    /// The world's salt for `hardware_key`; no key is sent until it is known.
    salt: Mutex<Option<String>>,
}

/// The name a driver kind goes by in `hardware_key`: an iPad added as an iPhone is one device.
pub fn driver_kind(os: DeviceOs) -> &'static str {
    match os {
        DeviceOs::Ios | DeviceOs::Ipados => "ios",
        DeviceOs::Tvos => "tvos",
        DeviceOs::SamsungTv => "samsung_tv",
        DeviceOs::LgTv => "lg_tv",
        other => other.as_str(),
    }
}

/// `hex(HMAC-SHA256(salt, driver + ":" + hardware id))`: a pseudonym for one physical device in
/// one world. Anyone holding the salt can test a guessed id, so it isn't a secret, only never the
/// raw id.
pub fn hardware_key(salt: &str, os: DeviceOs, hardware_id: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(salt.as_bytes()).expect("HMAC takes any key length");
    mac.update(driver_kind(os).as_bytes());
    mac.update(b":");
    mac.update(hardware_id.as_bytes());
    extend_protocol::ids::hex_lower(&mac.finalize().into_bytes())
}

impl HostedRegistry {
    pub fn new(factory: DriverFactory, state_dir: PathBuf, engine: Vec<String>) -> Self {
        Self {
            factory,
            state_dir,
            engine,
            entries: Mutex::new(BTreeMap::new()),
            next_order: Mutex::new(0),
            salt: Mutex::new(None),
        }
    }

    fn list_path(&self) -> PathBuf {
        self.state_dir.join("attached.json")
    }

    /// Re-creates the drivers saved by a previous run.
    pub fn restore(&self) {
        let Ok(bytes) = std::fs::read(self.list_path()) else {
            return;
        };
        let Ok(records) = serde_json::from_slice::<Vec<AttachRecord>>(&bytes) else {
            return;
        };
        for r in records {
            self.attach(r);
        }
    }

    fn save(&self) {
        let mut records: Vec<(u64, AttachRecord)> = self
            .entries
            .lock()
            .unwrap()
            .values()
            .map(|e| (e.order, e.record.clone()))
            .collect();
        records.sort_by_key(|(o, _)| *o);
        let records: Vec<AttachRecord> = records.into_iter().map(|(_, r)| r).collect();
        if let Ok(bytes) = serde_json::to_vec_pretty(&records)
            && let Err(e) = write_private_file(&self.list_path(), &bytes)
        {
            tracing::warn!("couldn't save the attached devices: {e:#}");
        }
    }

    /// The world's salt, from `GET /api/v1/device`. Everything is reported again when it changes.
    pub fn set_salt(&self, salt: Option<String>) {
        let changed = {
            let mut s = self.salt.lock().unwrap();
            let changed = *s != salt;
            *s = salt;
            changed
        };
        if changed {
            for e in self.entries.lock().unwrap().values_mut() {
                e.last_sent = None;
            }
        }
    }

    /// Starts carrying a device (or updates its name, address and host).
    pub fn attach(&self, record: AttachRecord) {
        let device = HostedDevice {
            device_id: record.device_id.to_string(),
            os: record.os,
            name: record.name.clone(),
            address: record.address.clone(),
            state_dir: self.state_dir.join(record.device_id.as_str()),
            agent_device: self.engine.clone(),
        };
        let _ = std::fs::create_dir_all(&device.state_dir);
        {
            let mut entries = self.entries.lock().unwrap();
            // Metadata-only changes must not replace a live driver or its recording state.
            if let Some(e) = entries.get_mut(&record.device_id)
                && e.record.os == record.os
                && e.record.address == record.address
                && e.record.host == record.host
            {
                e.record = record;
                drop(entries);
                self.save();
                return;
            }
        }
        let driver = (self.factory)(device).map(Arc::from);
        if let Err(e) = &driver {
            tracing::warn!("can't carry {} ({}): {e}", record.name, record.os.as_str());
        }
        {
            let mut entries = self.entries.lock().unwrap();
            let old = entries.remove(&record.device_id);
            let order = match &old {
                Some(e) => e.order,
                None => {
                    let mut n = self.next_order.lock().unwrap();
                    *n += 1;
                    *n
                }
            };
            let (in_use, takeover, wakes, hardware_id) = old
                .map(|e| (e.in_use, e.takeover, e.wakes, e.hardware_id))
                .unwrap_or_default();
            entries.insert(
                record.device_id.clone(),
                Entry {
                    record,
                    order,
                    driver,
                    shares_with: None,
                    last_sent: None,
                    hardware_id,
                    in_use,
                    takeover,
                    setup_error: None,
                    wakes,
                },
            );
        }
        self.save();
    }

    /// Stops carrying a device. Returns its driver so the caller can end its session (unless
    /// another carried id still shares it: then that one's session goes on).
    pub fn remove(&self, id: &DeviceId) -> Option<Arc<dyn Driver>> {
        let removed = {
            let mut entries = self.entries.lock().unwrap();
            let removed = entries.remove(id);
            if let Some(r) = &removed
                && let Ok(d) = &r.driver
                && entries
                    .values()
                    .any(|e| e.driver.as_ref().is_ok_and(|o| Arc::ptr_eq(o, d)))
            {
                None
            } else {
                removed
            }
        };
        self.save();
        removed.and_then(|e| e.driver.ok())
    }

    pub fn driver(&self, id: &DeviceId) -> Result<Arc<dyn Driver>, String> {
        match self.entries.lock().unwrap().get(id) {
            None => Err(format!("Device {id} isn't carried by this computer.")),
            Some(e) => e.driver.clone(),
        }
    }

    pub fn ids(&self) -> Vec<DeviceId> {
        self.entries.lock().unwrap().keys().cloned().collect()
    }

    /// The pair `id` is carried for.
    pub fn host_of(&self, id: &DeviceId) -> Option<DeviceId> {
        self.entries.lock().unwrap().get(id).and_then(|e| e.record.host.clone())
    }

    /// The devices carried for `host`. A record saved by 1.0 (no host) counts for every pair.
    pub fn ids_for_host(&self, host: &DeviceId) -> Vec<DeviceId> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, e)| e.record.host.as_ref().is_none_or(|h| h == host))
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn set_in_use(&self, id: &DeviceId, in_use: Option<InUseInfo>) {
        if let Some(e) = self.entries.lock().unwrap().get_mut(id) {
            if in_use.is_none() {
                e.takeover = None;
            }
            e.in_use = in_use;
        }
    }

    pub fn in_use(&self, id: &DeviceId) -> Option<InUseInfo> {
        self.entries.lock().unwrap().get(id).and_then(|e| e.in_use.clone())
    }

    /// The side of any session running on a carried device.
    pub fn active_side(&self) -> Option<String> {
        self.entries
            .lock()
            .unwrap()
            .values()
            .find_map(|e| e.in_use.as_ref().and_then(|u| u.side.clone()))
    }

    pub fn set_takeover(&self, id: &DeviceId, takeover: Option<TakeoverInfo>) {
        if let Some(e) = self.entries.lock().unwrap().get_mut(id) {
            e.takeover = takeover;
        }
    }

    pub fn set_setup_error(&self, id: &DeviceId, error: Option<String>) {
        if let Some(e) = self.entries.lock().unwrap().get_mut(id) {
            e.setup_error = error;
            e.last_sent = None;
        }
    }

    /// A Silicon asked to wake `id`: it is checked every [`WAKE_PROBE_EVERY`] until the request
    /// ends or expires.
    pub fn watch_wake(&self, id: &DeviceId, wake_id: Uuid, expires_at: Timestamp) {
        if let Some(e) = self.entries.lock().unwrap().get_mut(id) {
            e.wakes.insert(wake_id, expires_at);
        }
    }

    pub fn unwatch_wake(&self, id: &DeviceId, wake_id: &Uuid) {
        if let Some(e) = self.entries.lock().unwrap().get_mut(id) {
            e.wakes.remove(wake_id);
        }
    }

    /// The devices with an open wake request, forgetting expired requests.
    pub fn waking(&self, now: Timestamp) -> Vec<DeviceId> {
        let mut out = Vec::new();
        for (id, e) in self.entries.lock().unwrap().iter_mut() {
            e.wakes.retain(|_, until| *until > now);
            if !e.wakes.is_empty() {
                out.push(id.clone());
            }
        }
        out
    }

    /// The Carbon asked to retry a failed setup step of `id` (`setup_retry`).
    pub async fn retry_setup(&self, id: &DeviceId, step: Option<&str>) -> Result<(), String> {
        let driver = self.driver(id)?;
        // A wrong code is the one step only the Carbon can redo: it shows as waiting for the code
        // again, and the next code they enter is tried.
        if step.is_none_or(|s| s == SETUP_CODE_STEP) {
            self.set_setup_error(id, None);
        }
        driver.retry_setup(step).await;
        if let Some(e) = self.entries.lock().unwrap().get_mut(id) {
            e.last_sent = None;
        }
        Ok(())
    }

    /// Probes every carried device; returns the `attached` frames whose content changed since
    /// they were last sent (all of them when `force`), each with the pair to send it on.
    pub async fn probe_changes(&self, force: bool) -> Vec<(Option<DeviceId>, AttachedStatus)> {
        let ids = self.ids();
        self.probe_these(&ids, force).await
    }

    /// [`Self::probe_changes`] for the devices carried for `host` only.
    pub async fn probe_host(&self, host: &DeviceId, force: bool) -> Vec<(Option<DeviceId>, AttachedStatus)> {
        let ids = self.ids_for_host(host);
        self.probe_these(&ids, force).await
    }

    /// [`Self::probe_changes`] for `ids`.
    pub async fn probe_these(&self, ids: &[DeviceId], force: bool) -> Vec<(Option<DeviceId>, AttachedStatus)> {
        type Row = (DeviceId, DeviceOs, Result<Arc<dyn Driver>, String>, Option<String>);
        let snapshot: Vec<Row> = {
            let entries = self.entries.lock().unwrap();
            ids.iter()
                .filter_map(|id| {
                    entries
                        .get(id)
                        .map(|e| (id.clone(), e.record.os, e.driver.clone(), e.setup_error.clone()))
                })
                .collect()
        };
        let mut changed = Vec::new();
        for (id, os, driver, setup_error) in snapshot {
            let status = match driver {
                Ok(d) => {
                    let probe = match tokio::time::timeout(Duration::from_secs(20), d.probe()).await {
                        Ok(p) => p,
                        Err(_) => {
                            let s = unreachable_status(
                                &id,
                                "The device didn't answer within 20 seconds. Check that it is on and on the same network as this computer.",
                            );
                            changed.extend(self.record_sent(&id, s, force));
                            continue;
                        }
                    };
                    self.learn_hardware(&id, os, probe.hardware_id.clone());
                    let salt = self.salt.lock().unwrap().clone();
                    status_from_probe(&id, probe, setup_error.as_deref(), salt.as_deref())
                }
                Err(why) => unreachable_status(&id, &why),
            };
            changed.extend(self.record_sent(&id, status, force));
        }
        changed
    }

    /// Notes `id`'s hardware id, and shares one driver between the ids of one physical device:
    /// the one attached first keeps its driver (and its pairing), the other uses it too.
    fn learn_hardware(&self, id: &DeviceId, os: DeviceOs, hardware_id: Option<String>) {
        let Some(hw) = hardware_id else { return };
        let mut entries = self.entries.lock().unwrap();
        if let Some(e) = entries.get_mut(id) {
            e.hardware_id = Some(hw.clone());
        }
        let kind = driver_kind(os);
        let mut same: Vec<(u64, DeviceId)> = entries
            .iter()
            .filter(|(_, e)| {
                e.hardware_id.as_deref() == Some(hw.as_str()) && driver_kind(e.record.os) == kind && e.driver.is_ok()
            })
            .map(|(i, e)| (e.order, i.clone()))
            .collect();
        if same.len() < 2 {
            return;
        }
        same.sort();
        let (_, keeper) = same[0].clone();
        let Some(Ok(shared)) = entries.get(&keeper).map(|e| e.driver.clone()) else {
            return;
        };
        for (_, other) in &same[1..] {
            if let Some(e) = entries.get_mut(other)
                && !e.driver.as_ref().is_ok_and(|d| Arc::ptr_eq(d, &shared))
            {
                tracing::info!("{other} is the same device as {keeper}; they now share one driver");
                e.driver = Ok(shared.clone());
                e.shares_with = Some(keeper.clone());
                e.last_sent = None;
            }
        }
    }

    fn record_sent(
        &self,
        id: &DeviceId,
        status: AttachedStatus,
        force: bool,
    ) -> Option<(Option<DeviceId>, AttachedStatus)> {
        let mut entries = self.entries.lock().unwrap();
        let e = entries.get_mut(id)?;
        if !force && e.last_sent.as_ref() == Some(&status) {
            return None;
        }
        e.last_sent = Some(status.clone());
        Some((e.record.host.clone(), status))
    }

    /// Forgets what was sent, so the next probe reports everything (after a reconnect).
    pub fn forget_sent(&self) {
        for e in self.entries.lock().unwrap().values_mut() {
            e.last_sent = None;
        }
    }

    /// [`Self::forget_sent`] for the devices carried for `host`.
    pub fn forget_sent_for(&self, host: &DeviceId) {
        for e in self.entries.lock().unwrap().values_mut() {
            if e.record.host.as_ref().is_none_or(|h| h == host) {
                e.last_sent = None;
            }
        }
    }

    /// What the tray and window show.
    pub fn infos(&self) -> Vec<AttachedInfo> {
        let now = time::OffsetDateTime::now_utc();
        self.entries
            .lock()
            .unwrap()
            .values()
            .map(|e| AttachedInfo {
                in_use_indicator: e.record.in_use_indicator,
                device_id: e.record.device_id.to_string(),
                name: e.record.name.clone(),
                os: e.record.os,
                online: e.last_sent.as_ref().is_some_and(|s| s.online),
                in_use: e.in_use.clone(),
                takeover: e.takeover.clone(),
                setup: e.last_sent.as_ref().map(|s| s.setup.clone()),
                error: e.driver.as_ref().err().cloned().or_else(|| e.setup_error.clone()),
                awake: e.last_sent.as_ref().and_then(|s| s.awake),
                sleep_state: e.last_sent.as_ref().and_then(|s| s.sleep_state),
                host: e.record.host.as_ref().map(|h| h.to_string()),
                wake_requested: e.wakes.values().any(|until| *until > now),
            })
            .collect()
    }
}

/// The key of the step a wrong setup code shows as.
pub const SETUP_CODE_STEP: &str = "setup_code";

pub fn status_from_probe(id: &DeviceId, probe: Probe, setup_error: Option<&str>, salt: Option<&str>) -> AttachedStatus {
    let mut setup = probe.setup;
    if let Some(err) = setup_error {
        setup.steps.push(SetupStep {
            key: SETUP_CODE_STEP.into(),
            title: "Enter the code the device shows".into(),
            status: StepStatus::Failed,
            help: None,
            error: Some(err.to_owned()),
            input: None,
        });
        setup = Setup::from_steps(setup.steps);
    }
    let hardware_key = match (salt, &probe.hardware_id) {
        (Some(salt), Some(hw)) => Some(hardware_key(salt, probe.os, hw)),
        _ => None,
    };
    AttachedStatus {
        device_id: id.clone(),
        online: probe.online,
        os_version: probe.os_version,
        model: probe.model,
        capabilities: probe.capabilities,
        missing: probe.missing,
        setup,
        awake: if probe.online { probe.awake } else { None },
        sleep_state: if probe.online && probe.awake == Some(false) {
            probe.sleep_state
        } else {
            None
        },
        hardware_key,
    }
}

/// A device the host can't carry: offline, with the reason as a failed setup step.
pub fn unreachable_status(id: &DeviceId, why: &str) -> AttachedStatus {
    AttachedStatus {
        device_id: id.clone(),
        online: false,
        os_version: None,
        model: None,
        capabilities: vec![],
        missing: vec![],
        setup: Setup {
            state: SetupState::NeedsCarbon,
            steps: vec![SetupStep {
                key: "host".into(),
                title: "Connect through this computer".into(),
                status: StepStatus::Failed,
                help: None,
                error: Some(why.to_owned()),
                input: None,
            }],
        },
        awake: None,
        sleep_state: None,
        hardware_key: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use extend_driver::{Invocation, Output};
    use extend_protocol::Capability;
    use extend_protocol::model::SleepState;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A Samsung TV; `hw` is the hardware id it reports, `standby` whether it is in standby.
    #[derive(Default)]
    struct Tv {
        hw: Option<String>,
        standby: bool,
        probes: AtomicUsize,
        retried: Mutex<Vec<Option<String>>>,
    }
    #[async_trait]
    impl Driver for Tv {
        async fn probe(&self) -> Probe {
            self.probes.fetch_add(1, Ordering::SeqCst);
            Probe {
                os: DeviceOs::SamsungTv,
                os_version: None,
                model: Some("QN90".into()),
                capabilities: vec![Capability::InputRemote],
                missing: vec![],
                setup: Setup::complete(),
                engine_version: None,
                online: true,
                awake: Some(!self.standby),
                sleep_state: self.standby.then_some(SleepState::Standby),
                hardware_id: self.hw.clone(),
            }
        }
        async fn run(&self, _inv: Invocation<'_>) -> Output {
            Output::ok(serde_json::Value::Null, "pressed")
        }
        async fn retry_setup(&self, step: Option<&str>) {
            self.retried.lock().unwrap().push(step.map(str::to_owned));
        }
    }

    fn registry(dir: &std::path::Path) -> HostedRegistry {
        let factory: DriverFactory = Arc::new(|d: HostedDevice| {
            if d.os == DeviceOs::Tvos {
                Err("Apple TVs need a Mac".into())
            } else {
                Ok(Box::new(Tv {
                    // Every TV at 10.0.0.5 is the same TV.
                    hw: d.address.as_ref().map(|a| format!("duid-{a}")),
                    standby: d.name.contains("standby"),
                    ..Default::default()
                }) as Box<dyn Driver>)
            }
        });
        HostedRegistry::new(factory, dir.to_path_buf(), vec!["node".into()])
    }

    fn record(id: &str, os: DeviceOs, name: &str, address: Option<&str>, host: Option<&str>) -> AttachRecord {
        AttachRecord {
            in_use_indicator: Default::default(),
            device_id: id.parse().unwrap(),
            os,
            name: name.into(),
            address: address.map(str::to_owned),
            host: host.map(|h| h.parse().unwrap()),
        }
    }

    #[tokio::test]
    async fn attach_probe_remove_and_restore() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(dir.path());
        let tv: DeviceId = "0000aaaa".parse().unwrap();
        let atv: DeviceId = "0000bbbb".parse().unwrap();
        reg.attach(record(
            "0000aaaa",
            DeviceOs::SamsungTv,
            "Lounge TV",
            Some("10.0.0.5"),
            Some("7c1e09ab"),
        ));
        reg.attach(record("0000bbbb", DeviceOs::Tvos, "Apple TV", None, Some("7c1e09ab")));
        assert!(reg.driver(&tv).is_ok());
        let before = reg.driver(&tv).unwrap();
        let mut changed = record(
            "0000aaaa",
            DeviceOs::SamsungTv,
            "Lounge TV",
            Some("10.0.0.5"),
            Some("7c1e09ab"),
        );
        changed.in_use_indicator = extend_protocol::model::InUseIndicator::Hidden;
        reg.attach(changed);
        assert!(
            Arc::ptr_eq(&before, &reg.driver(&tv).unwrap()),
            "hiding the banner must preserve the live driver"
        );
        assert_eq!(
            reg.infos()
                .iter()
                .find(|a| a.device_id == "0000aaaa")
                .unwrap()
                .in_use_indicator,
            extend_protocol::model::InUseIndicator::Hidden
        );
        assert_eq!(reg.driver(&atv).err().unwrap(), "Apple TVs need a Mac");
        assert!(reg.driver(&"0000cccc".parse().unwrap()).is_err());

        let first = reg.probe_changes(false).await;
        assert_eq!(first.len(), 2);
        assert!(
            first
                .iter()
                .all(|(host, _)| host.as_ref().map(|h| h.as_str()) == Some("7c1e09ab"))
        );
        let tv_status = &first.iter().find(|(_, s)| s.device_id == tv).unwrap().1;
        assert!(tv_status.online);
        assert_eq!(tv_status.capabilities, vec![Capability::InputRemote]);
        assert_eq!(tv_status.awake, Some(true));
        // No salt yet: no key.
        assert_eq!(tv_status.hardware_key, None);
        let atv_status = &first.iter().find(|(_, s)| s.device_id == atv).unwrap().1;
        assert!(!atv_status.online);
        assert_eq!(atv_status.setup.steps[0].error.as_deref(), Some("Apple TVs need a Mac"));
        // Nothing changed: nothing to send.
        assert!(reg.probe_changes(false).await.is_empty());
        assert_eq!(reg.probe_changes(true).await.len(), 2);
        // The salt arrives: everything goes again, now with the key.
        reg.set_salt(Some("salt".into()));
        let keyed = reg.probe_changes(false).await;
        let tv_status = &keyed.iter().find(|(_, s)| s.device_id == tv).unwrap().1;
        assert_eq!(
            tv_status.hardware_key.as_deref(),
            Some(hardware_key("salt", DeviceOs::SamsungTv, "duid-10.0.0.5").as_str())
        );

        // A second registry over the same directory restores both, with their hosts.
        let again = registry(dir.path());
        again.restore();
        assert_eq!(again.ids(), vec![tv.clone(), atv.clone()]);
        assert_eq!(again.host_of(&tv).unwrap().as_str(), "7c1e09ab");

        assert!(reg.remove(&tv).is_some());
        assert_eq!(reg.ids(), vec![atv]);
        let infos = reg.infos();
        assert_eq!(infos[0].error.as_deref(), Some("Apple TVs need a Mac"));
    }

    #[tokio::test]
    async fn each_pair_gets_its_own_carried_devices() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(dir.path());
        reg.attach(record(
            "0000aaaa",
            DeviceOs::SamsungTv,
            "Alice's TV",
            Some("10.0.0.5"),
            Some("7c1e09ab"),
        ));
        reg.attach(record(
            "0000bbbb",
            DeviceOs::LgTv,
            "Bob's TV",
            Some("10.0.0.9"),
            Some("0d44e1f2"),
        ));
        let bob: DeviceId = "0d44e1f2".parse().unwrap();
        assert_eq!(reg.ids_for_host(&bob), vec!["0000bbbb".parse::<DeviceId>().unwrap()]);
        let sent = reg.probe_host(&bob, true).await;
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0.as_ref(), Some(&bob));
        assert_eq!(sent[0].1.device_id.as_str(), "0000bbbb");
    }

    #[tokio::test]
    async fn two_ids_of_one_device_share_one_driver() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(dir.path());
        let alice: DeviceId = "0000aaaa".parse().unwrap();
        let bob: DeviceId = "0000bbbb".parse().unwrap();
        reg.attach(record(
            "0000aaaa",
            DeviceOs::SamsungTv,
            "Living room TV",
            Some("10.0.0.5"),
            Some("7c1e09ab"),
        ));
        reg.attach(record(
            "0000bbbb",
            DeviceOs::SamsungTv,
            "Family TV",
            Some("10.0.0.5"),
            Some("0d44e1f2"),
        ));
        assert!(!Arc::ptr_eq(&reg.driver(&alice).unwrap(), &reg.driver(&bob).unwrap()));
        reg.set_salt(Some("world-salt".into()));
        let sent = reg.probe_changes(false).await;
        let keys: Vec<Option<String>> = sent.iter().map(|(_, s)| s.hardware_key.clone()).collect();
        assert_eq!(keys[0], keys[1], "one device, one key");
        // The later one now drives the TV through the first one's driver (and its pairing).
        assert!(Arc::ptr_eq(&reg.driver(&alice).unwrap(), &reg.driver(&bob).unwrap()));
        // Removing one leaves the other's driver in use: nothing to end.
        assert!(reg.remove(&alice).is_none());
        assert!(reg.driver(&bob).is_ok());
    }

    #[tokio::test]
    async fn a_wake_request_is_watched_until_it_ends_or_expires() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(dir.path());
        let tv: DeviceId = "0000aaaa".parse().unwrap();
        reg.attach(record(
            "0000aaaa",
            DeviceOs::SamsungTv,
            "TV in standby",
            Some("10.0.0.5"),
            None,
        ));
        let now = time::OffsetDateTime::now_utc();
        assert!(reg.waking(now).is_empty());
        let w1 = Uuid::from_u128(1);
        reg.watch_wake(&tv, w1, now + time::Duration::minutes(30));
        reg.watch_wake(&tv, Uuid::from_u128(2), now + time::Duration::seconds(1));
        assert_eq!(reg.waking(now), vec![tv.clone()]);
        assert!(reg.infos()[0].wake_requested);
        reg.unwatch_wake(&tv, &w1);
        assert_eq!(reg.waking(now), vec![tv.clone()]);
        assert!(reg.waking(now + time::Duration::seconds(2)).is_empty());
        // A TV in standby is online and not awake.
        let sent = reg.probe_changes(true).await;
        assert_eq!(sent[0].1.awake, Some(false));
        assert_eq!(sent[0].1.sleep_state, Some(SleepState::Standby));
    }

    #[tokio::test]
    async fn retrying_setup_reaches_the_driver_and_clears_a_wrong_code() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(dir.path());
        let tv: DeviceId = "0000aaaa".parse().unwrap();
        reg.attach(record("0000aaaa", DeviceOs::SamsungTv, "TV", Some("10.0.0.5"), None));
        reg.set_setup_error(
            &tv,
            Some("That code didn't work. Enter the code the TV shows now.".into()),
        );
        let before = reg.probe_changes(false).await;
        assert!(before[0].1.setup.steps.iter().any(|s| s.key == SETUP_CODE_STEP));
        reg.retry_setup(&tv, None).await.unwrap();
        let after = reg.probe_changes(false).await;
        assert!(!after[0].1.setup.steps.iter().any(|s| s.key == SETUP_CODE_STEP));
        assert!(reg.retry_setup(&"0000cccc".parse().unwrap(), None).await.is_err());
    }

    #[test]
    fn hardware_keys_are_stable_per_world_and_never_the_raw_id() {
        let a = hardware_key("salt-a", DeviceOs::Ios, "00008110-001234");
        assert_eq!(a, hardware_key("salt-a", DeviceOs::Ios, "00008110-001234"));
        // An iPad added as an iPhone is still the same device.
        assert_eq!(a, hardware_key("salt-a", DeviceOs::Ipados, "00008110-001234"));
        assert_ne!(a, hardware_key("salt-b", DeviceOs::Ios, "00008110-001234"));
        assert_ne!(a, hardware_key("salt-a", DeviceOs::Tvos, "00008110-001234"));
        assert_eq!(a.len(), 64);
        assert!(!a.contains("00008110"));
        // RFC 4231 test case 2, so the service or another app can compute the same key.
        let mut mac = Hmac::<Sha256>::new_from_slice(b"Jefe").unwrap();
        mac.update(b"what do ya want for nothing?");
        assert_eq!(
            extend_protocol::ids::hex_lower(&mac.finalize().into_bytes()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn setup_code_errors_show_as_a_failed_step() {
        let id: DeviceId = "0000aaaa".parse().unwrap();
        let probe = Probe {
            os: DeviceOs::Tvos,
            os_version: None,
            model: None,
            capabilities: vec![],
            missing: vec![],
            setup: Setup::complete(),
            engine_version: None,
            online: true,
            awake: None,
            sleep_state: None,
            hardware_id: None,
        };
        let s = status_from_probe(&id, probe, Some("wrong code"), None);
        assert_eq!(s.setup.state, SetupState::NeedsCarbon);
        assert_eq!(s.setup.steps[0].error.as_deref(), Some("wrong code"));
    }
}

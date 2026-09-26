//! Devices this computer carries (iPhone and iPad through a Mac; Apple TV; Samsung and LG TVs).
//!
//! The service sends `attach` with the device's id, OS, name and address; the agent builds its
//! driver with [`extend_hosted::driver_for`], routes commands whose `target` is that id to it, and
//! reports the driver's probe in `attached` frames. The list is kept in `{state}/attached.json`
//! so the drivers are back after a restart before the service says anything.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use extend_driver::{Driver, Probe};
use extend_hosted::HostedDevice;
use extend_protocol::frames::AttachedStatus;
use extend_protocol::model::{Setup, SetupState, SetupStep, StepStatus};
use extend_protocol::{DeviceId, DeviceOs};
use serde::{Deserialize, Serialize};

use crate::config::write_private_file;
use crate::status::{AttachedInfo, InUseInfo, TakeoverInfo};

/// Builds a hosted driver; `extend_hosted::driver_for` in production, a fake in tests.
pub type DriverFactory = Arc<dyn Fn(HostedDevice) -> Result<Box<dyn Driver>, String> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachRecord {
    pub device_id: DeviceId,
    pub os: DeviceOs,
    pub name: String,
    #[serde(default)]
    pub address: Option<String>,
}

struct Entry {
    record: AttachRecord,
    driver: Result<Arc<dyn Driver>, String>,
    last_sent: Option<AttachedStatus>,
    in_use: Option<InUseInfo>,
    takeover: Option<TakeoverInfo>,
    setup_error: Option<String>,
}

pub struct HostedRegistry {
    factory: DriverFactory,
    state_dir: PathBuf,
    agent_device: Vec<String>,
    entries: Mutex<BTreeMap<DeviceId, Entry>>,
}

impl HostedRegistry {
    pub fn new(factory: DriverFactory, state_dir: PathBuf, agent_device: Vec<String>) -> Self {
        Self {
            factory,
            state_dir,
            agent_device,
            entries: Mutex::new(BTreeMap::new()),
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
        let records: Vec<AttachRecord> = self
            .entries
            .lock()
            .unwrap()
            .values()
            .map(|e| e.record.clone())
            .collect();
        if let Ok(bytes) = serde_json::to_vec_pretty(&records)
            && let Err(e) = write_private_file(&self.list_path(), &bytes)
        {
            tracing::warn!("couldn't save the attached devices: {e:#}");
        }
    }

    /// Starts carrying a device (or updates its name and address).
    pub fn attach(&self, record: AttachRecord) {
        let device = HostedDevice {
            device_id: record.device_id.to_string(),
            os: record.os,
            name: record.name.clone(),
            address: record.address.clone(),
            state_dir: self.state_dir.join(record.device_id.as_str()),
            agent_device: self.agent_device.clone(),
        };
        let _ = std::fs::create_dir_all(&device.state_dir);
        let driver = (self.factory)(device).map(Arc::from);
        if let Err(e) = &driver {
            tracing::warn!("can't carry {} ({}): {e}", record.name, record.os.as_str());
        }
        {
            let mut entries = self.entries.lock().unwrap();
            let (in_use, takeover) = entries
                .get(&record.device_id)
                .map(|e| (e.in_use.clone(), e.takeover.clone()))
                .unwrap_or_default();
            entries.insert(
                record.device_id.clone(),
                Entry {
                    record,
                    driver,
                    last_sent: None,
                    in_use,
                    takeover,
                    setup_error: None,
                },
            );
        }
        self.save();
    }

    /// Stops carrying a device. Returns its driver so the caller can end its session.
    pub fn remove(&self, id: &DeviceId) -> Option<Arc<dyn Driver>> {
        let removed = self.entries.lock().unwrap().remove(id);
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

    pub fn set_in_use(&self, id: &DeviceId, in_use: Option<InUseInfo>) {
        if let Some(e) = self.entries.lock().unwrap().get_mut(id) {
            if in_use.is_none() {
                e.takeover = None;
            }
            e.in_use = in_use;
        }
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

    /// Probes every carried device; returns the `attached` frames whose content changed since
    /// they were last sent (all of them when `force`).
    pub async fn probe_changes(&self, force: bool) -> Vec<AttachedStatus> {
        type Row = (DeviceId, Result<Arc<dyn Driver>, String>, Option<String>);
        let snapshot: Vec<Row> = self
            .entries
            .lock()
            .unwrap()
            .iter()
            .map(|(id, e)| (id.clone(), e.driver.clone(), e.setup_error.clone()))
            .collect();
        let mut changed = Vec::new();
        for (id, driver, setup_error) in snapshot {
            let status = match driver {
                Ok(d) => {
                    let probe = match tokio::time::timeout(std::time::Duration::from_secs(20), d.probe()).await {
                        Ok(p) => p,
                        Err(_) => {
                            changed.extend(self.record_sent(
                                &id,
                                unreachable_status(&id, "The device didn't answer its check within 20 seconds."),
                                force,
                            ));
                            continue;
                        }
                    };
                    status_from_probe(&id, probe, setup_error.as_deref())
                }
                Err(why) => unreachable_status(&id, &why),
            };
            changed.extend(self.record_sent(&id, status, force));
        }
        changed
    }

    fn record_sent(&self, id: &DeviceId, status: AttachedStatus, force: bool) -> Option<AttachedStatus> {
        let mut entries = self.entries.lock().unwrap();
        let e = entries.get_mut(id)?;
        if !force && e.last_sent.as_ref() == Some(&status) {
            return None;
        }
        e.last_sent = Some(status.clone());
        Some(status)
    }

    /// Forgets what was sent, so the next probe reports everything (after a reconnect).
    pub fn forget_sent(&self) {
        for e in self.entries.lock().unwrap().values_mut() {
            e.last_sent = None;
        }
    }

    /// What the tray and window show.
    pub fn infos(&self) -> Vec<AttachedInfo> {
        self.entries
            .lock()
            .unwrap()
            .values()
            .map(|e| AttachedInfo {
                device_id: e.record.device_id.to_string(),
                name: e.record.name.clone(),
                os: e.record.os,
                online: e.last_sent.as_ref().is_some_and(|s| s.online),
                in_use: e.in_use.clone(),
                takeover: e.takeover.clone(),
                setup: e.last_sent.as_ref().map(|s| s.setup.clone()),
                error: e.driver.as_ref().err().cloned().or_else(|| e.setup_error.clone()),
            })
            .collect()
    }
}

pub fn status_from_probe(id: &DeviceId, probe: Probe, setup_error: Option<&str>) -> AttachedStatus {
    let mut setup = probe.setup;
    if let Some(err) = setup_error {
        setup.steps.push(SetupStep {
            key: "setup_code".into(),
            title: "Enter the code the device shows".into(),
            status: StepStatus::Failed,
            help: None,
            error: Some(err.to_owned()),
            input: None,
        });
        setup = Setup::from_steps(setup.steps);
    }
    AttachedStatus {
        device_id: id.clone(),
        online: probe.online,
        os_version: probe.os_version,
        model: probe.model,
        capabilities: probe.capabilities,
        missing: probe.missing,
        setup,
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use extend_driver::{Invocation, Output};
    use extend_protocol::Capability;

    struct Tv;
    #[async_trait]
    impl Driver for Tv {
        async fn probe(&self) -> Probe {
            Probe {
                os: DeviceOs::SamsungTv,
                os_version: None,
                model: Some("QN90".into()),
                capabilities: vec![Capability::InputRemote],
                missing: vec![],
                setup: Setup::complete(),
                agent_device_version: None,
                online: true,
            }
        }
        async fn run(&self, _inv: Invocation<'_>) -> Output {
            Output::ok(serde_json::Value::Null, "pressed")
        }
    }

    fn registry(dir: &std::path::Path) -> HostedRegistry {
        let factory: DriverFactory = Arc::new(|d: HostedDevice| {
            if d.os == DeviceOs::Tvos {
                Err("Apple TVs need a Mac".into())
            } else {
                Ok(Box::new(Tv) as Box<dyn Driver>)
            }
        });
        HostedRegistry::new(factory, dir.to_path_buf(), vec!["node".into()])
    }

    #[tokio::test]
    async fn attach_probe_remove_and_restore() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(dir.path());
        let tv: DeviceId = "0000aaaa".parse().unwrap();
        let atv: DeviceId = "0000bbbb".parse().unwrap();
        reg.attach(AttachRecord {
            device_id: tv.clone(),
            os: DeviceOs::SamsungTv,
            name: "Lounge TV".into(),
            address: Some("10.0.0.5".into()),
        });
        reg.attach(AttachRecord {
            device_id: atv.clone(),
            os: DeviceOs::Tvos,
            name: "Apple TV".into(),
            address: None,
        });
        assert!(reg.driver(&tv).is_ok());
        assert_eq!(reg.driver(&atv).err().unwrap(), "Apple TVs need a Mac");
        assert!(reg.driver(&"0000cccc".parse().unwrap()).is_err());

        let first = reg.probe_changes(false).await;
        assert_eq!(first.len(), 2);
        let tv_status = first.iter().find(|s| s.device_id == tv).unwrap();
        assert!(tv_status.online);
        assert_eq!(tv_status.capabilities, vec![Capability::InputRemote]);
        let atv_status = first.iter().find(|s| s.device_id == atv).unwrap();
        assert!(!atv_status.online);
        assert_eq!(atv_status.setup.steps[0].error.as_deref(), Some("Apple TVs need a Mac"));
        // Nothing changed: nothing to send.
        assert!(reg.probe_changes(false).await.is_empty());
        assert_eq!(reg.probe_changes(true).await.len(), 2);

        // A second registry over the same directory restores both.
        let again = registry(dir.path());
        again.restore();
        assert_eq!(again.ids(), vec![tv.clone(), atv.clone()]);

        assert!(reg.remove(&tv).is_some());
        assert_eq!(reg.ids(), vec![atv]);
        let infos = reg.infos();
        assert_eq!(infos[0].error.as_deref(), Some("Apple TVs need a Mac"));
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
            agent_device_version: None,
            online: true,
        };
        let s = status_from_probe(&id, probe, Some("wrong code"));
        assert_eq!(s.setup.state, SetupState::NeedsCarbon);
        assert_eq!(s.setup.steps[0].error.as_deref(), Some("wrong code"));
    }
}

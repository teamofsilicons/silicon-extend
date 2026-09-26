//! Live connections: every paired device's socket, every unpaired app's enrollment socket, and the
//! commands waiting for an answer.
//!
//! One service instance holds the sockets it accepted. Running several instances needs a shared
//! relay (see docs/operations.md); a single instance is the supported deployment for 1.0.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use extend_protocol::Capability;
use extend_protocol::frames::{CommandOutcome, EnrollmentFrame, ServiceFrame};
use extend_protocol::model::MissingCapability;
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use uuid::Uuid;

pub type DeviceKey = (String, String); // (world schema, device id)

struct Conn {
    conn_id: Uuid,
    tx: mpsc::UnboundedSender<ServiceFrame>,
}

#[derive(Debug, Clone, Default)]
pub struct AttachedState {
    pub online: bool,
    pub capabilities: Vec<Capability>,
    pub missing: Vec<MissingCapability>,
}

#[derive(Default)]
pub struct Hub {
    conns: RwLock<HashMap<DeviceKey, Conn>>,
    attached: RwLock<HashMap<DeviceKey, AttachedState>>,
    enrollments: RwLock<HashMap<Uuid, mpsc::UnboundedSender<EnrollmentFrame>>>,
    pending: Mutex<HashMap<Uuid, (DeviceKey, oneshot::Sender<CommandOutcome>)>>,
    session_locks: Mutex<HashMap<DeviceKey, Arc<Mutex<()>>>>,
}

pub enum SendError {
    Offline,
    Timeout,
    Dropped,
}

impl Hub {
    /// Registers a device socket, replacing (and superseding) any older one for the same device.
    pub async fn register(&self, key: DeviceKey) -> (Uuid, mpsc::UnboundedReceiver<ServiceFrame>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let conn_id = Uuid::new_v4();
        if let Some(old) = self.conns.write().await.insert(key, Conn { conn_id, tx }) {
            let _ = old.tx.send(ServiceFrame::Superseded);
        }
        (conn_id, rx)
    }

    /// Removes a socket if it's still the current one. Returns true when it was.
    pub async fn unregister(&self, key: &DeviceKey, conn_id: Uuid) -> bool {
        let mut conns = self.conns.write().await;
        if conns.get(key).is_some_and(|c| c.conn_id == conn_id) {
            conns.remove(key);
            drop(conns);
            // Anything waiting on this device will never be answered.
            let mut pending = self.pending.lock().await;
            let dead: Vec<Uuid> = pending
                .iter()
                .filter(|(_, (k, _))| k == key)
                .map(|(id, _)| *id)
                .collect();
            for id in dead {
                pending.remove(&id);
            }
            true
        } else {
            false
        }
    }

    pub async fn is_connected(&self, key: &DeviceKey) -> bool {
        self.conns.read().await.contains_key(key)
    }

    pub async fn send(&self, key: &DeviceKey, frame: ServiceFrame) -> bool {
        self.conns
            .read()
            .await
            .get(key)
            .is_some_and(|c| c.tx.send(frame).is_ok())
    }

    /// Drops a device's socket (used when its pair ends).
    pub async fn disconnect(&self, key: &DeviceKey) {
        self.conns.write().await.remove(key);
    }

    pub async fn set_attached(&self, key: DeviceKey, state: AttachedState) {
        self.attached.write().await.insert(key, state);
    }

    pub async fn attached(&self, key: &DeviceKey) -> Option<AttachedState> {
        self.attached.read().await.get(key).cloned()
    }

    /// Sends a command to `route` (the device, or its host) and waits for the answer.
    pub async fn command(
        &self,
        route: &DeviceKey,
        id: Uuid,
        frame: ServiceFrame,
        timeout: Duration,
    ) -> Result<CommandOutcome, SendError> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, (route.clone(), tx));
        if !self.send(route, frame).await {
            self.pending.lock().await.remove(&id);
            return Err(SendError::Offline);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(_)) => Err(SendError::Dropped),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                let _ = self.send(route, ServiceFrame::Cancel { id }).await;
                Err(SendError::Timeout)
            }
        }
    }

    /// Delivers a device's answer. Answers nobody waits for are dropped.
    pub async fn resolve(&self, route: &DeviceKey, outcome: CommandOutcome) {
        let mut pending = self.pending.lock().await;
        if pending.get(&outcome.id).is_some_and(|(k, _)| k == route)
            && let Some((_, tx)) = pending.remove(&outcome.id)
        {
            let _ = tx.send(outcome);
        }
    }

    /// Serialises commands within one session: they run one at a time, in order.
    pub async fn session_lock(&self, key: DeviceKey) -> Arc<Mutex<()>> {
        self.session_locks
            .lock()
            .await
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub async fn register_enrollment(&self, id: Uuid) -> mpsc::UnboundedReceiver<EnrollmentFrame> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.enrollments.write().await.insert(id, tx);
        rx
    }

    pub async fn unregister_enrollment(&self, id: Uuid) {
        self.enrollments.write().await.remove(&id);
    }

    pub async fn send_enrollment(&self, id: Uuid, frame: EnrollmentFrame) -> bool {
        self.enrollments
            .read()
            .await
            .get(&id)
            .is_some_and(|tx| tx.send(frame).is_ok())
    }

    pub async fn enrollment_connected(&self, id: Uuid) -> bool {
        self.enrollments.read().await.contains_key(&id)
    }
}

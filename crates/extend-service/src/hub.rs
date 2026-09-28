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
    /// When this connection last answered a ping.
    last_pong_at: Option<std::time::Instant>,
    /// What the app said it can do beyond 1.0 (`hello.features`).
    features: Vec<String>,
    /// Only reports from this connection establish whether its carried devices are online.
    attached: HashMap<DeviceKey, AttachedState>,
}

/// How recently a connection must have answered a ping for a new one replacing it to count as a
/// take-over (logged as `connection_replaced` on the pair).
pub const LIVE_WITHIN: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Default)]
pub struct AttachedState {
    pub online: bool,
    pub capabilities: Vec<Capability>,
    pub missing: Vec<MissingCapability>,
}

#[derive(Default)]
pub struct Hub {
    conns: RwLock<HashMap<DeviceKey, Conn>>,
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
    /// The flag says whether the one it replaced was live: it answered a ping within
    /// [`LIVE_WITHIN`], so something else took over a working connection (a copied credential, or
    /// a second copy of the app), rather than the app reconnecting after a drop.
    pub async fn register(&self, key: DeviceKey) -> (Uuid, mpsc::UnboundedReceiver<ServiceFrame>, bool) {
        let (tx, rx) = mpsc::unbounded_channel();
        let conn_id = Uuid::new_v4();
        let conn = Conn {
            conn_id,
            tx,
            last_pong_at: None,
            features: vec![],
            attached: HashMap::new(),
        };
        let mut replaced_live = false;
        if let Some(old) = self.conns.write().await.insert(key, conn) {
            replaced_live = old.last_pong_at.is_some_and(|t| t.elapsed() <= LIVE_WITHIN);
            let _ = old.tx.send(ServiceFrame::Superseded);
        }
        (conn_id, rx, replaced_live)
    }

    /// Records a pong on a connection.
    pub async fn pong(&self, key: &DeviceKey, conn_id: Uuid) {
        if let Some(c) = self.conns.write().await.get_mut(key)
            && c.conn_id == conn_id
        {
            c.last_pong_at = Some(std::time::Instant::now());
        }
    }

    /// Records what a connection's app advertised in its hello.
    pub async fn set_features(&self, key: &DeviceKey, conn_id: Uuid, features: Vec<String>) {
        if let Some(c) = self.conns.write().await.get_mut(key)
            && c.conn_id == conn_id
        {
            c.features = features;
        }
    }

    /// Whether the app connected for `key` advertised `feature`. `None` when nothing is connected.
    pub async fn supports(&self, key: &DeviceKey, feature: &str) -> Option<bool> {
        self.conns
            .read()
            .await
            .get(key)
            .map(|c| c.features.iter().any(|f| f == feature))
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

    /// Accepts a carried-device report only from the current connection of its host.
    pub async fn set_attached(&self, host: &DeviceKey, conn_id: Uuid, key: DeviceKey, state: AttachedState) -> bool {
        let mut conns = self.conns.write().await;
        let Some(conn) = conns.get_mut(host).filter(|conn| conn.conn_id == conn_id) else {
            return false;
        };
        conn.attached.insert(key, state);
        true
    }

    pub async fn attached(&self, host: &DeviceKey, key: &DeviceKey) -> Option<AttachedState> {
        self.conns
            .read()
            .await
            .get(host)
            .and_then(|conn| conn.attached.get(key))
            .cloned()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn report(online: bool) -> AttachedState {
        AttachedState {
            online,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn attached_reports_follow_the_host_connection_generation() {
        let hub = Hub::default();
        let host = ("extend".into(), "host".into());
        let child = ("extend".into(), "child".into());
        let (old, _old_rx, _) = hub.register(host.clone()).await;
        assert!(hub.set_attached(&host, old, child.clone(), report(true)).await);
        assert!(hub.attached(&host, &child).await.unwrap().online);

        let (current, _current_rx, _) = hub.register(host.clone()).await;
        assert!(hub.attached(&host, &child).await.is_none());
        assert!(!hub.set_attached(&host, old, child.clone(), report(true)).await);
        assert!(hub.attached(&host, &child).await.is_none());
        assert!(hub.set_attached(&host, current, child.clone(), report(false)).await);
        assert!(!hub.set_attached(&host, old, child.clone(), report(true)).await);
        assert!(!hub.attached(&host, &child).await.unwrap().online);

        assert!(hub.set_attached(&host, current, child.clone(), report(true)).await);
        assert!(!hub.unregister(&host, old).await);
        assert!(hub.attached(&host, &child).await.unwrap().online);
        assert!(hub.unregister(&host, current).await);
        assert!(hub.attached(&host, &child).await.is_none());
        assert!(!hub.set_attached(&host, current, child, report(true)).await);
    }

    #[tokio::test]
    async fn attached_reports_are_isolated_by_host_and_world() {
        let hub = Hub::default();
        let host = ("extend".into(), "host".into());
        let other_host = ("extend".into(), "other-host".into());
        let test_host = ("extend_test_a".into(), "host".into());
        let child = ("extend".into(), "child".into());
        let test_child = ("extend_test_a".into(), "child".into());
        let (id, _rx, _) = hub.register(host.clone()).await;
        let (other_id, _other_rx, _) = hub.register(other_host.clone()).await;
        let (test_id, _test_rx, _) = hub.register(test_host.clone()).await;
        assert!(hub.set_attached(&host, id, child.clone(), report(true)).await);
        assert!(hub.attached(&other_host, &child).await.is_none());
        assert!(hub.attached(&test_host, &test_child).await.is_none());
        assert!(hub.attached(&host, &test_child).await.is_none());
        assert!(!hub.set_attached(&other_host, id, child.clone(), report(true)).await);
        assert!(
            hub.set_attached(&other_host, other_id, child.clone(), report(false))
                .await
        );
        assert!(
            hub.set_attached(&test_host, test_id, test_child.clone(), report(true))
                .await
        );
        assert!(!hub.attached(&other_host, &child).await.unwrap().online);
        assert!(hub.attached(&host, &child).await.unwrap().online);
        hub.disconnect(&host).await;
        assert!(hub.attached(&host, &child).await.is_none());
        assert!(hub.attached(&test_host, &test_child).await.unwrap().online);
        assert!(!hub.attached(&other_host, &child).await.unwrap().online);
    }
}

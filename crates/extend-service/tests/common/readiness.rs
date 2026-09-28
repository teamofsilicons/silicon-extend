use std::time::Duration;

use extend_protocol::model::{Device, DeviceState};
use silicon_extend_client::Authed;

/// A Hello/Attached send only queues the frame; observe the service's committed state before
/// starting a session. This helper is only for fixtures that report complete setup.
pub async fn ready(client: &Authed<'_>, id: &str) -> Device {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let device = client.device(id).await.expect("read paired fixture device");
            if device.online && device.state == DeviceState::Ready {
                return device;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("fixture device {id} never became online and ready"))
}

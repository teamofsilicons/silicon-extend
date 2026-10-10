use std::time::Duration;

use extend_protocol::model::{Device, DeviceState};

/// A Hello/Attached send only queues the frame; observe the service's committed state, as the
/// device's Carbon reads it (`token`, API v2), before starting a session. This helper is only for
/// fixtures that report complete setup.
pub async fn ready(base: &str, token: &str, id: &str) -> Device {
    let http = reqwest::Client::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let v: serde_json::Value = http
                .get(format!("{base}/api/v2/devices/{id}"))
                .bearer_auth(token)
                .send()
                .await
                .expect("read paired fixture device")
                .json()
                .await
                .expect("a device envelope");
            let device: Device = serde_json::from_value(v["data"].clone()).expect("a device");
            if device.online && device.state == DeviceState::Ready {
                return device;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("fixture device {id} never became online and ready"))
}

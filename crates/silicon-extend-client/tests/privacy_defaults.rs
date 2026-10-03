//! Exercise the public protocol dependency as an SDK consumer, including packaged builds.

use silicon_extend_client::protocol::API_VERSION;
use silicon_extend_client::protocol::model::{DeviceImport, PairingClaim, Visibility};

#[test]
fn sdk_default_visibility_keeps_new_devices_private() {
    let visibility = Visibility::default();
    assert_eq!(visibility, Visibility::Personal);
    let pair = PairingClaim {
        pairing_code: "4f9c2a".into(),
        name: "Private device".into(),
        visibility: Some(visibility),
        pair_ttl_days: None,
        silicon_ids: vec![],
    };
    assert_eq!(serde_json::to_value(pair).unwrap()["visibility"], "personal");
    let import = DeviceImport {
        visibility: Some(visibility),
    };
    assert_eq!(serde_json::to_value(import).unwrap()["visibility"], "personal");
    assert_eq!(API_VERSION, 1, "crate patch must not change the physical wire API");
}

//! Exercise the public protocol dependency as an SDK consumer, including packaged builds: a device
//! is private to the Carbon who paired it, and the device wire keeps its major.

use silicon_extend_client::protocol::model::{PairingClaim, Visibility};
use silicon_extend_client::protocol::{ACCOUNT_API_VERSION, API_VERSION};

#[test]
fn sdk_default_visibility_keeps_new_devices_private() {
    let visibility = Visibility::default();
    assert_eq!(visibility, Visibility::Personal);
    let pair = PairingClaim {
        pairing_code: "4f9c2a".into(),
        name: "Saket's Pixel".into(),
        visibility: Some(visibility),
        pair_ttl_days: None,
        silicon_ids: vec![],
    };
    assert_eq!(serde_json::to_value(pair).unwrap()["visibility"], "personal");
    // Without a visibility the field is left out, and the service keeps the device private.
    let pair = PairingClaim {
        pairing_code: "4f9c2a".into(),
        name: "Saket's Pixel".into(),
        visibility: None,
        pair_ttl_days: None,
        silicon_ids: vec!["si:chef".into()],
    };
    assert!(serde_json::to_value(pair).unwrap().get("visibility").is_none());
    assert_eq!(API_VERSION, 1, "the device wire installed apps speak stays API v1");
    assert_eq!(ACCOUNT_API_VERSION, 2, "the account API is v2 (Silicon Accounts)");
    assert_eq!(silicon_extend_client::SUPPORTED_API_VERSIONS, &[2]);
}

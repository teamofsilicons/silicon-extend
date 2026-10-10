//! The 4.0 request bodies and queries, built with exactly their field lists, so a field added in a
//! 4.x release fails to compile here before it ships (callers' struct literals would break too).

use silicon_extend_client::protocol::DeviceOs;
use silicon_extend_client::protocol::model::{AttachmentCreate, DevicePatch, PairingClaim};
use silicon_extend_client::{ActivityQuery, DeviceQuery, ListQuery};

#[test]
fn request_bodies_keep_their_4_0_fields() {
    let _ = DeviceQuery {
        scope: Some("mine".into()),
        online: Some(true),
        os: Some("android".into()),
        limit: Some(100),
        cursor: None,
    };
    let _ = ActivityQuery {
        silicon_id: Some("si:chef".into()),
        session_id: None,
        since: None,
        until: None,
        limit: Some(50),
        cursor: None,
    };
    let _ = ListQuery {
        device_id: None,
        session_id: None,
        state: None,
        kind: None,
        direction: None,
        silicon: Some("si:chef".into()),
        limit: None,
        cursor: None,
    };
    let claim = PairingClaim {
        pairing_code: "4f9c2a".into(),
        name: "Pixel".into(),
        visibility: None,
        pair_ttl_days: Some(14),
        silicon_ids: vec!["si:chef".into()],
    };
    assert!(serde_json::to_value(&claim).unwrap().get("visibility").is_none());
    let _ = AttachmentCreate {
        os: DeviceOs::Tvos,
        name: "Living room".into(),
        visibility: None,
        pair_ttl_days: None,
        address: None,
    };
    let _ = DevicePatch {
        name: Some("Studio phone".into()),
        visibility: None,
        pair_ttl_days: None,
    };
}

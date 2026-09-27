//! Code written against silicon-extend-client 1.0 still builds its request bodies and queries:
//! 1.1 changed none of them (a `visibility` is accepted and ignored by a 1.1 service). Each is built
//! here with exactly its 1.0 field list, so a new field there fails to compile before it ships.

use silicon_extend_client::protocol::DeviceOs;
use silicon_extend_client::protocol::model::{AttachmentCreate, DevicePatch, PairingClaim, Visibility};
use silicon_extend_client::{ActivityQuery, DeviceQuery, ListQuery};

#[test]
fn request_bodies_keep_their_1_0_fields() {
    let _ = DeviceQuery {
        scope: Some("mine".into()),
        online: Some(true),
        os: Some("android".into()),
        limit: Some(100),
        cursor: None,
    };
    let _ = ActivityQuery {
        silicon_id: None,
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
        limit: None,
        cursor: None,
    };
    let claim = PairingClaim {
        pairing_code: "4f9c2a".into(),
        name: "Pixel".into(),
        visibility: Some(Visibility::Team),
        pair_ttl_days: Some(14),
        silicon_ids: vec!["si:chef".into()],
    };
    assert_eq!(serde_json::to_value(&claim).unwrap()["visibility"], "team");
    let _ = AttachmentCreate {
        os: DeviceOs::Tvos,
        name: "Living room".into(),
        visibility: None,
        pair_ttl_days: None,
        address: None,
    };
    let _ = DevicePatch {
        name: Some("Studio phone".into()),
        visibility: Some(Visibility::Personal),
        pair_ttl_days: None,
    };
}

//! 1.0 and 1.1 on the wire, in both directions.
//!
//! The modules beside this file are silicon-extend-protocol 1.0.0's sources as published, with
//! only their unit tests cut; this file stands in for its lib.rs. So "1.0" below is the exact code
//! 1.0.x services, apps, clients and CLIs decode with, and the tests check what they make of what
//! 1.1 sends, and what 1.1 makes of what they send.

#![allow(dead_code, unused_imports, clippy::all)]

pub mod capability;
pub mod envelope;
pub mod error;
pub mod frames;
pub mod ids;
pub mod model;

/// The one 1.0.0 lib.rs item its modules use.
pub const TEST_DEVICE_LIMIT: i64 = 5;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use silicon_extend_protocol as v11;
use time::macros::datetime;
use uuid::Uuid;

const WAKE: &str = "0192f3a4-0000-7000-8000-000000000001";
const RUN: &str = "0192f3a4-5b6c-7d8e-9f00-112233445566";

fn v1_0<T: DeserializeOwned>(value: &impl Serialize) -> T {
    let json = serde_json::to_value(value).unwrap();
    serde_json::from_value(json.clone()).unwrap_or_else(|e| panic!("1.0 can't read {json}: {e}"))
}

fn v1_1<T: DeserializeOwned>(value: &impl Serialize) -> T {
    let json = serde_json::to_value(value).unwrap();
    serde_json::from_value(json.clone()).unwrap_or_else(|e| panic!("1.1 can't read {json}: {e}"))
}

fn wake_request() -> v11::model::WakeRequest {
    let mut w = v11::model::WakeRequest::new(
        WAKE.parse().unwrap(),
        "0d44e1f2".parse().unwrap(),
        "labs",
        "si:chef",
        "c:alice",
        "Check the order screen",
        datetime!(2026-09-27 10:02 UTC),
        datetime!(2026-09-27 10:32 UTC),
    );
    w.ting = Some(v11::model::TingDelivery::Deferred);
    w
}

/// A 1.1 owner view with every new field set, as the service sends it.
fn device_1_1() -> v11::model::Device {
    let json = json!({"device_id":"7c1e09ab","name":"Living room TV","os":"android_tv","kind":"tv",
        "owner":{"type":"carbon","id":"c:alice"},"visibility":"personal","state":"ready","online":true,
        "in_use":{"silicon_id":"si:chef","session_id":"a3f","since":"2026-09-27T09:58:00Z","paused":false,"team":"labs"},
        "capabilities":["input.remote"],"missing":[{"capability":"terminal","reason":v11::TERMINAL_NOT_SHARED_REASON}]});
    let mut d: v11::model::Device = serde_json::from_value(json).unwrap();
    d.engine_version = Some("0.21.15".into());
    d.agent_device_version = Some("0.21.15".into());
    d.awake = Some(false);
    d.sleep_state = Some(v11::model::SleepState::Standby);
    d.last_sleep_state = Some(v11::model::SleepState::Standby);
    d.awake_changed_at = Some(datetime!(2026-09-27 10:01 UTC));
    d.wake_detectable = Some(false);
    d.in_use_by_other = true;
    d.in_use_by_other_carried = true;
    d.open_wake_requests = Some(1);
    d.wake_requests = Some(vec![wake_request()]);
    d.wake_muted = Some(true);
    d.paired_by_others = Some(true);
    d.same_device = Some(vec!["3a2b0c1d".parse().unwrap()]);
    d
}

#[test]
fn a_1_0_reader_decodes_1_1_resources() {
    let d = device_1_1();
    let old: model::Device = v1_0(&d);
    assert_eq!(old.visibility, model::Visibility::Personal);
    assert_eq!(old.team, None);
    assert_eq!(old.in_use.unwrap().silicon_id, "si:chef");

    // Carbons may list devices across Teams, with a Team on each in-use and grant.
    let _: model::InUse = v1_0(d.in_use.as_ref().unwrap());
    let grant: v11::model::AccessGrant = serde_json::from_value(json!({"device_id":"7c1e09ab",
        "silicon_id":"si:chef","granted_by":"c:alice","granted_at":"2026-09-01T00:00:00Z",
        "last_used_at":null,"team":"labs","wake_muted":false}))
    .unwrap();
    let _: model::AccessGrant = v1_0(&grant);

    // A request routed to a Carbon keeps `to` a string the 1.0 CLI prints ("Sent to ...").
    let routed: v11::model::RequestInfo = serde_json::from_value(json!({"request_id":WAKE,"device_id":"7c1e09ab",
        "from":"si:chef","to":v11::REQUEST_TO_HIDDEN,"reason":"I need the TV","created_at":"2026-09-27T10:00:00Z",
        "delivery":"pending","team":"labs","routed_to":"carbon","to_hidden":true}))
    .unwrap();
    let old: model::RequestInfo = v1_0(&routed);
    assert_eq!(old.to, "the Carbon who gave access to the Silicon using it");
    assert_eq!(old.session_id, None);

    let session: v11::model::Session = serde_json::from_value(json!({"session_id":"a3f","device_id":"7c1e09ab",
        "silicon_id":"si:chef","state":"active","started_at":"2026-09-27T10:00:00Z","last_command_at":null,
        "idle_ends_at":null,"ended_at":null,"end_reason":null,"command_count":0,"team":"labs"}))
    .unwrap();
    let _: model::Session = v1_0(&session);

    let file: v11::model::FileInfo = serde_json::from_value(json!({"file_id":WAKE,"name":"screenshot.png",
        "kind":"screenshot","content_type":"image/png","size_bytes":28,"url":"https://briefcase.example/f",
        "self_destruct_at":null,"permanent":false,"team":"labs"}))
    .unwrap();
    let _: model::FileInfo = v1_0(&file);

    let entry: v11::model::ActivityEntry = serde_json::from_value(json!({"id":WAKE,"at":"2026-09-27T10:00:00Z",
        "actor":{"type":"silicon","id":"si:chef"},"action":"wake_requested","files":[],"team":"labs",
        "details":{"reason":"Check the order screen"}}))
    .unwrap();
    let _: model::ActivityEntry = v1_0(&entry);

    // GET /team/silicons?team=any is a superset of the 1.0 answer ({"items": [...]}).
    let all = v11::model::TeamSilicons::new(
        vec![v11::model::TeamSilicon {
            id: "si:chef".into(),
            display_name: None,
            team: Some("labs".into()),
        }],
        vec![v11::model::TeamReach::reached("labs")],
    );
    #[derive(serde::Deserialize)]
    struct Items {
        items: Vec<model::TeamSilicon>,
    }
    assert_eq!(v1_0::<Items>(&all).items[0].id, "si:chef");

    // A 1.0 agent reads GET /api/v1/device from a 1.1 service.
    let me: v11::model::DeviceSelf = serde_json::from_value(json!({"device_id":"7c1e09ab","name":"Studio Mac",
        "owner":{"type":"carbon","id":"c:alice"},"team":"labs","os":"macos","in_use":null,"takeover":null,
        "setup":{"state":"complete","steps":[]},"environment":null,"instance_id":WAKE,
        "hardware_salt":"ab","first_pair":true}))
    .unwrap();
    let _: model::DeviceSelf = v1_0(&me);
}

#[test]
fn the_in_use_indicator_is_additive() {
    use v11::model::{DeviceSelfPatch, DeviceSettingsPatch as DevicePatch, InUseIndicator};

    // A 1.0 CLI, client or website reads a 1.1 device with the banner off.
    let mut d = device_1_1();
    d.in_use_indicator = InUseIndicator::Hidden;
    assert_eq!(serde_json::to_value(&d).unwrap()["in_use_indicator"], "hidden");
    let old: model::Device = v1_0(&d);
    assert_eq!(old.name, "Living room TV");

    // A 1.0 agent reads GET /api/v1/device with it.
    let me: v11::model::DeviceSelf = serde_json::from_value(json!({"device_id":"7c1e09ab","name":"Studio Mac",
        "owner":{"type":"carbon","id":"c:alice"},"team":"labs","os":"macos","in_use":null,"takeover":null,
        "setup":{"state":"complete","steps":[]},"environment":null,"in_use_indicator":"hidden"}))
    .unwrap();
    assert_eq!(me.in_use_indicator, InUseIndicator::Hidden);
    let _: model::DeviceSelf = v1_0(&me);

    // A 1.0 computer still carries a device from an attach frame that has the new field.
    let attach = v11::frames::ServiceFrame::Attach {
        device_id: "3a2b0c1d".parse().unwrap(),
        os: v11::DeviceOs::Ios,
        name: "Alice's iPhone".into(),
        address: None,
        removed: false,
        in_use_indicator: InUseIndicator::Hidden,
    };
    let frames::ServiceFrame::Attach { device_id, removed, .. } = v1_0(&attach) else {
        panic!()
    };
    assert_eq!((device_id.as_str(), removed), ("3a2b0c1d", false));

    // A 1.0 service reads a 1.1 PATCH body and ignores the new field (it changes nothing there).
    let patch = DevicePatch {
        name: Some("Den TV".into()),
        in_use_indicator: Some(InUseIndicator::Hidden),
        ..Default::default()
    };
    let old: model::DevicePatch = v1_0(&patch);
    assert_eq!(old.name.as_deref(), Some("Den TV"));
    let _: model::DevicePatch = v1_0(&DeviceSelfPatch::in_use_indicator(InUseIndicator::Hidden));

    // A 1.1 app, agent, client or CLI reads a 1.0 service: shown.
    let old_attach = frames::ServiceFrame::Attach {
        device_id: "3a2b0c1d".parse().unwrap(),
        os: capability::DeviceOs::Ios,
        name: "Alice's iPhone".into(),
        address: None,
        removed: false,
    };
    let v11::frames::ServiceFrame::Attach { in_use_indicator, .. } = v1_1(&old_attach) else {
        panic!()
    };
    assert_eq!(in_use_indicator, InUseIndicator::Shown);
    let me = model::DeviceSelf {
        device_id: "7c1e09ab".parse().unwrap(),
        name: "Studio Mac".into(),
        owner: model::Member {
            kind: model::MemberKind::Carbon,
            id: "c:alice".into(),
            display_name: None,
        },
        team: "labs".into(),
        os: capability::DeviceOs::Macos,
        in_use: None,
        takeover: None,
        setup: model::Setup::complete(),
        environment: None,
    };
    assert_eq!(
        v1_1::<v11::model::DeviceSelf>(&me).in_use_indicator,
        InUseIndicator::Shown
    );
    let patch: v11::model::DeviceSettingsPatch = v1_1(&model::DevicePatch {
        name: None,
        visibility: None,
        pair_ttl_days: Some(30),
    });
    assert_eq!(patch.in_use_indicator, None);
}

#[test]
fn a_1_0_app_reads_the_1_1_frames_it_knows_and_skips_the_rest() {
    let started = v11::frames::ServiceFrame::SessionStarted {
        target: None,
        session_id: "a3f".parse().unwrap(),
        silicon_id: "si:chef".into(),
        since: datetime!(2026-09-27 10:05 UTC),
        side: Some("9f2c4b1a0d3e5f67".into()),
    };
    assert!(matches!(
        v1_0::<frames::ServiceFrame>(&started),
        frames::ServiceFrame::SessionStarted { .. }
    ));

    // New frame types don't decode on 1.0; its agent and app log them and carry on
    // (the 1.0 service does the same for new device frames), so the service sends them anyway.
    let new_service_frames = [
        json!({"type":"wake_request","target":null,"wake_id":WAKE,"alert":true,
               "created_at":"2026-09-27T10:02:00Z","expires_at":"2026-09-27T10:32:00Z"}),
        json!({"type":"wake_request_ended","target":null,"wake_id":WAKE,"reason":"woken"}),
        json!({"type":"credential","device_credential":"edc_x"}),
        json!({"type":"setup_retry","target":null,"step":null}),
    ];
    for f in new_service_frames {
        let _: v11::frames::ServiceFrame = serde_json::from_value(f.clone()).unwrap();
        assert!(serde_json::from_value::<frames::ServiceFrame>(f).is_err());
    }
    let new_device_frames = [
        json!({"type":"awake","awake":true,"input_seen":true,"run":RUN,"seq":42}),
        json!({"type":"wake_request_shown","wake_id":WAKE,"shown":true}),
        json!({"type":"credential_saved"}),
    ];
    for f in new_device_frames {
        let _: v11::frames::DeviceFrame = serde_json::from_value(f.clone()).unwrap();
        assert!(serde_json::from_value::<frames::DeviceFrame>(f).is_err());
    }
}

#[test]
fn a_1_0_service_reads_a_1_1_hello_and_attached() {
    let hello: v11::frames::DeviceFrame = serde_json::from_value(json!({"type":"hello","app_version":"1.1.0",
        "os":"macos","engine_version":"0.21.15","capabilities":["screen.read"],"missing":[],
        "setup":{"state":"complete","steps":[]},"features":["setup_retry"]}))
    .unwrap();
    let frames::DeviceFrame::Hello(old) = v1_0(&hello) else {
        panic!()
    };
    // The engine version is lost on a 1.0 service (deploy the service first); nothing else is.
    assert_eq!(old.agent_device_version, None);
    assert_eq!(old.app_version, "1.1.0");

    let attached: v11::frames::DeviceFrame = serde_json::from_value(json!({"type":"attached","device_id":"3a2b0c1d",
        "online":true,"setup":{"state":"complete","steps":[]},"awake":false,"sleep_state":"standby",
        "hardware_key":"5f5f"}))
    .unwrap();
    assert!(matches!(
        v1_0::<frames::DeviceFrame>(&attached),
        frames::DeviceFrame::Attached(_)
    ));

    let enroll: v11::model::EnrollmentCreate =
        serde_json::from_value(json!({"os":"android","app_version":"1.1.0","engine_version":"0.21.15"})).unwrap();
    let _: model::EnrollmentCreate = v1_0(&enroll);
}

#[test]
fn a_1_1_reader_decodes_what_1_0_sends() {
    // A 1.0 app's hello and enrollment carry the engine version under its old name.
    let hello = frames::DeviceFrame::Hello(frames::Hello {
        app_version: "1.0.2".into(),
        os: capability::DeviceOs::Android,
        os_version: Some("14".into()),
        model: Some("Pixel 8".into()),
        agent_device_version: Some("0.13.0".into()),
        capabilities: vec![capability::Capability::ScreenRead, capability::Capability::Adb],
        missing: vec![],
        setup: model::Setup::complete(),
    });
    let v11::frames::DeviceFrame::Hello(new) = v1_1(&hello) else {
        panic!()
    };
    assert_eq!(new.engine_version.as_deref(), Some("0.13.0"));
    assert!(new.features.is_empty() && !new.supports(v11::feature::SETUP_RETRY));

    let enroll = model::EnrollmentCreate {
        os: capability::DeviceOs::Android,
        os_version: None,
        model: None,
        app_version: "1.0.2".into(),
        agent_device_version: Some("0.13.0".into()),
    };
    let new: v11::model::EnrollmentCreate = v1_1(&enroll);
    assert_eq!(new.engine_version.as_deref(), Some("0.13.0"));

    let attached = frames::DeviceFrame::Attached(frames::AttachedStatus {
        device_id: "3a2b0c1d".parse().unwrap(),
        online: true,
        os_version: None,
        model: None,
        capabilities: vec![],
        missing: vec![],
        setup: model::Setup::complete(),
    });
    let v11::frames::DeviceFrame::Attached(new) = v1_1(&attached) else {
        panic!()
    };
    assert_eq!((new.awake, new.hardware_key), (None, None));

    // Every 1.0 device frame still decodes.
    for f in [
        frames::DeviceFrame::Stop { target: None },
        frames::DeviceFrame::TakeoverDone { target: None },
        frames::DeviceFrame::Pong { nonce: 7 },
        frames::DeviceFrame::SetupProgress {
            setup: model::Setup::complete(),
        },
    ] {
        let _: v11::frames::DeviceFrame = v1_1(&f);
    }

    // A 1.0 service's frames, resources and errors, as a 1.1 app, client or CLI reads them.
    let started = frames::ServiceFrame::SessionStarted {
        target: None,
        session_id: "a3f".parse().unwrap(),
        silicon_id: "si:chef".into(),
        since: datetime!(2026-09-27 10:05 UTC),
    };
    let v11::frames::ServiceFrame::SessionStarted { side, .. } = v1_1(&started) else {
        panic!()
    };
    assert_eq!(side, None);
    for f in [
        frames::ServiceFrame::Refresh,
        frames::ServiceFrame::Superseded,
        frames::ServiceFrame::Ping { nonce: 1 },
        frames::ServiceFrame::Unpaired {
            reason: model::EndReason::PairRevoked,
        },
        frames::ServiceFrame::SessionEnded {
            target: None,
            session_id: "a3f".parse().unwrap(),
            reason: model::EndReason::StoppedByCarbon,
        },
    ] {
        let _: v11::frames::ServiceFrame = v1_1(&f);
    }

    let device = model::Device {
        device_id: "7c1e09ab".parse().unwrap(),
        name: "Living room TV".into(),
        os: capability::DeviceOs::AndroidTv,
        os_version: None,
        model: None,
        kind: capability::DeviceKind::Tv,
        owner: model::Member {
            kind: model::MemberKind::Carbon,
            id: "c:alice".into(),
            display_name: None,
        },
        team: Some("labs".into()),
        visibility: model::Visibility::Team,
        host_device_id: None,
        state: model::DeviceState::Ready,
        online: true,
        last_seen_at: None,
        in_use: Some(model::InUse {
            silicon_id: "si:chef".into(),
            session_id: "a3f".parse().unwrap(),
            since: datetime!(2026-09-27 09:58 UTC),
            paused: false,
        }),
        last_used_at: None,
        paired_at: None,
        pair_ttl_days: Some(30),
        pair_expires_at: None,
        days_left: Some(29),
        access_count: Some(1),
        app_version: Some("1.0.2".into()),
        version: Some(3),
        capabilities: None,
        missing: None,
        commands: None,
        removed_at: None,
        removed_reason: None,
    };
    let new: v11::model::Device = v1_1(&device);
    assert_eq!(new.awake, None);
    assert!(!new.in_use_by_other);
    assert_eq!(new.in_use.unwrap().team, None);

    let request = model::RequestInfo {
        request_id: Uuid::nil(),
        device_id: "7c1e09ab".parse().unwrap(),
        from: "si:chef".into(),
        to: "si:sous".into(),
        session_id: Some("a3f".parse().unwrap()),
        reason: "I need the TV".into(),
        created_at: datetime!(2026-09-27 10:00 UTC),
        delivery: model::Delivery::Delivered,
        last_error: None,
    };
    let new: v11::model::RequestInfo = v1_1(&request);
    assert_eq!((new.routed_to, new.to_hidden, new.from_hidden), (None, false, false));

    let me = model::DeviceSelf {
        device_id: "7c1e09ab".parse().unwrap(),
        name: "Studio Mac".into(),
        owner: model::Member {
            kind: model::MemberKind::Carbon,
            id: "c:alice".into(),
            display_name: None,
        },
        team: "labs".into(),
        os: capability::DeviceOs::Macos,
        in_use: None,
        takeover: None,
        setup: model::Setup::complete(),
        environment: None,
    };
    let new: v11::model::DeviceSelf = v1_1(&me);
    assert_eq!((new.instance_id, new.hardware_salt, new.first_pair), (None, None, None));

    // No ErrorCode, EndReason, Capability or DeviceOs value changed: every 1.0 value reads the same.
    for code in [
        "device_offline",
        "upgrade_required",
        "rate_limited",
        "conflict",
        "invalid_input",
        "carbon_only",
    ] {
        let old: error::ErrorCode = serde_json::from_value(json!(code)).unwrap();
        let new: v11::ErrorCode = serde_json::from_value(json!(code)).unwrap();
        assert_eq!(old.http_status(), new.http_status());
        assert_eq!(old.exit_code(), new.exit_code());
    }
    for c in capability::Capability::ALL {
        let _: v11::Capability = v1_1(&c);
    }
}

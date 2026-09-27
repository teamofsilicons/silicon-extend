//! Extend's Ting notification types (1.1.0).
//!
//! Ting keeps notification types per environment, Team and app, and refuses a Ting whose type
//! isn't registered in the Ting's Team. So each type must be registered in every Team Extend sends
//! in, by that Team's Ting manager. The service reports the ones Ting doesn't know
//! (`TingRegistration.missing_types`), and the CLI and website show [`register_command`] for them.

use uuid::Uuid;

/// One of Extend's Ting types. Its full name is `{app_id}.{event}` (`extend.device.woken`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TingType {
    /// The name after the app id.
    pub event: &'static str,
    /// What it tells the recipient, as registered.
    pub description: &'static str,
}

impl TingType {
    pub fn full_name(&self, app_id: &str) -> String {
        format!("{app_id}.{}", self.event)
    }
}

/// A Silicon asks to use a device another Silicon is using. It goes to the Silicon using it
/// (same Carbon and Team as the asker) or to the Carbon who gave that Silicon access.
pub const DEVICE_REQUESTED: TingType = TingType {
    event: "device.requested",
    description: "A Silicon asks to use a device another Silicon is using",
};
/// A Silicon asks its Carbon to wake a device.
pub const WAKE_REQUESTED: TingType = TingType {
    event: "device.wake_requested",
    description: "A Silicon asks its Carbon to wake a device",
};
/// A device a Silicon asked to wake is awake.
pub const WOKEN: TingType = TingType {
    event: "device.woken",
    description: "A device a Silicon asked to wake is awake",
};
/// A Carbon turned down a request to wake a device.
pub const WAKE_DECLINED: TingType = TingType {
    event: "device.wake_declined",
    description: "A Carbon turned down a request to wake a device",
};

/// Every type Extend sends.
pub const ALL_TYPES: [TingType; 4] = [DEVICE_REQUESTED, WAKE_REQUESTED, WOKEN, WAKE_DECLINED];

/// Finds a type by its full name (`extend.device.woken`) or its event (`device.woken`).
pub fn find(name: &str) -> Option<TingType> {
    let event = name
        .split_once('.')
        .filter(|(app, _)| *app != "device")
        .map_or(name, |(_, e)| e);
    ALL_TYPES.into_iter().find(|t| t.event == event)
}

/// The command a Team's Ting manager runs to register a type in that Team.
pub fn register_command(team: &str, app_id: &str, ty: TingType) -> String {
    format!(
        "ting --org {team} types register --type {} --description '{}'",
        ty.full_name(app_id),
        ty.description
    )
}

/// Ting idempotency key of the Carbon's Ting for one ask of a wake request.
pub fn wake_requested_key(wake_id: Uuid, ask: i64) -> String {
    format!("wake:{wake_id}:{ask}")
}

/// Ting idempotency key of the Silicon's "it's awake" Ting.
pub fn woken_key(wake_id: Uuid) -> String {
    format!("woken:{wake_id}")
}

/// Ting idempotency key of the Silicon's "declined" Ting.
pub fn declined_key(wake_id: Uuid) -> String {
    format!("declined:{wake_id}")
}

open_enum! {
    /// Who ended a wake request as woken, in `extend.device.woken` (never which Carbon).
    pub enum WokenBy {
        /// The device reported itself awake.
        Device => "device",
        /// A Carbon answered "It's awake".
        Carbon => "carbon",
    }
}

open_enum! {
    /// In `extend.device.woken`: what the asking Silicon can do now.
    pub enum WakeNow {
        /// Nobody is using the device: start a session.
        Free => "free",
        /// The Silicon already has a session on it.
        Yours => "yours",
        /// Another Silicon is using it: ask for it with `request send`.
        InUse => "in_use",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_commands() {
        assert_eq!(WAKE_REQUESTED.full_name("extend"), "extend.device.wake_requested");
        assert_eq!(
            register_command("labs", "extend", WAKE_REQUESTED),
            "ting --org labs types register --type extend.device.wake_requested --description 'A Silicon asks its Carbon to wake a device'"
        );
        assert_eq!(find("extend.device.woken"), Some(WOKEN));
        assert_eq!(find("device.woken"), Some(WOKEN));
        assert_eq!(find("extend.device.requested"), Some(DEVICE_REQUESTED));
        assert_eq!(find("extend.device.unknown"), None);
        let id = Uuid::nil();
        assert_eq!(wake_requested_key(id, 2), "wake:00000000-0000-0000-0000-000000000000:2");
        assert_eq!(woken_key(id), "woken:00000000-0000-0000-0000-000000000000");
        assert_eq!(declined_key(id), "declined:00000000-0000-0000-0000-000000000000");
    }

    #[test]
    fn open_enums() {
        assert_eq!(serde_json::to_string(&WakeNow::InUse).unwrap(), "\"in_use\"");
        assert_eq!(serde_json::from_str::<WakeNow>("\"later\"").unwrap(), WakeNow::Other);
        assert_eq!(serde_json::from_str::<WokenBy>("\"carbon\"").unwrap(), WokenBy::Carbon);
        assert_eq!(WokenBy::parse("robot"), WokenBy::Other);
    }
}

//! Drivers for devices a computer carries: iPhone and iPad (through agent-device on a Mac), Apple TV,
//! Samsung (Tizen) and LG (webOS) TVs.
//!
//! The desktop agent (`extend-agent`) calls [`driver_for`] when the service sends an `attach` frame
//! and keeps the returned driver for that device id. Everything else goes through the
//! [`extend_driver::Driver`] trait. [`discover`] finds candidate devices on the local network (or,
//! for iPhone and iPad, on this Mac) so the Carbon can pick one while adding it.
//!
//! | Device | How | Host |
//! |---|---|---|
//! | iPhone, iPad | agent-device's physical-iOS driver (XCTest runner on the device) | Mac |
//! | Apple TV | Companion protocol (HAP pairing, OPACK) for buttons and apps; AirPlay for pictures and videos | Mac |
//! | Samsung TV | Tizen remote-control WebSocket (8002 TLS, 8001 plain) and REST (8001) | Mac, Windows, Linux |
//! | LG TV | webOS SSAP WebSocket (3000 plain, 3001 TLS) and its pointer socket | Mac, Windows, Linux |

use std::path::PathBuf;

use extend_driver::Driver;
use extend_protocol::DeviceOs;

mod appletv;
mod common;
pub mod discover;
mod http;
mod ios;
mod lg;
mod samsung;
mod script;
mod tls;
mod ws;

pub use discover::{Found, discover};

/// What the host knows about a device it should carry.
#[derive(Debug, Clone)]
pub struct HostedDevice {
    pub device_id: String,
    pub os: DeviceOs,
    pub name: String,
    /// Network address (TVs) or device identifier (iPhone UDID), when known.
    pub address: Option<String>,
    /// A directory this driver may keep state in (pairing keys, client tokens).
    pub state_dir: PathBuf,
    /// Command that runs agent-device (`node …/bin.js` split into argv), for iPhone and iPad.
    pub agent_device: Vec<String>,
}

/// Builds the driver for a hosted device. Errors say exactly why (wrong host OS, missing helper).
///
/// Construction does no I/O and needs no async runtime; drivers connect lazily on the first
/// `probe` or `run`. Built inside a tokio runtime, an iPhone's or iPad's driver also starts looking
/// after its device's XCTest runner every 20 s until it is dropped.
pub fn driver_for(device: HostedDevice) -> Result<Box<dyn Driver>, String> {
    match device.os {
        DeviceOs::Ios | DeviceOs::Ipados => ios::driver(device),
        DeviceOs::Tvos => appletv::driver(device),
        DeviceOs::SamsungTv => Ok(Box::new(samsung::SamsungDriver::new(device))),
        DeviceOs::LgTv => Ok(Box::new(lg::LgDriver::new(device))),
        other => Err(format!(
            "{} devices run the Extend app themselves; they are not carried by a host computer",
            other.as_str()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(os: DeviceOs) -> HostedDevice {
        HostedDevice {
            device_id: "dev_1".into(),
            os,
            name: "Living room".into(),
            address: Some("192.0.2.10".into()),
            state_dir: std::env::temp_dir().join("extend-hosted-test"),
            agent_device: vec!["agent-device".into()],
        }
    }

    #[test]
    fn tvs_build_on_every_host() {
        assert!(driver_for(device(DeviceOs::SamsungTv)).is_ok());
        assert!(driver_for(device(DeviceOs::LgTv)).is_ok());
    }

    #[test]
    fn self_hosted_devices_are_refused() {
        let err = driver_for(device(DeviceOs::Android)).err().unwrap();
        assert!(err.contains("run the Extend app themselves"), "{err}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn apple_devices_build_on_a_mac() {
        assert!(driver_for(device(DeviceOs::Tvos)).is_ok());
        assert!(driver_for(device(DeviceOs::Ios)).is_ok());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn apple_devices_need_a_mac() {
        let err = driver_for(device(DeviceOs::Tvos)).err().unwrap();
        assert!(err.contains("Mac"), "{err}");
    }
}

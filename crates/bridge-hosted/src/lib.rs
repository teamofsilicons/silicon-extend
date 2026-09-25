//! Drivers for devices a computer carries: iPhone and iPad (through agent-device on a Mac), Apple TV,
//! Samsung (Tizen) and LG (webOS) TVs.
//!
//! The desktop agent (`bridge-agent`) calls [`driver_for`] when the service sends an `attach` frame
//! and keeps the returned driver for that device id. Everything else goes through the
//! [`bridge_driver::Driver`] trait.

use std::path::PathBuf;

use bridge_driver::Driver;
use bridge_protocol::DeviceOs;

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
pub fn driver_for(device: HostedDevice) -> Result<Box<dyn Driver>, String> {
    Err(format!("{} devices are not supported by this build of the host agent", device.os.as_str()))
}

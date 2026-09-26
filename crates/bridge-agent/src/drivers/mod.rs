//! Drivers that operate this computer.
//!
//! * [`agent_device`] runs Bridge's fork of agent-device on macOS and Linux.
//! * [`windows`] is Bridge's own Windows driver (agent-device doesn't support Windows).
//! * [`terminal`] runs shell commands on all three.
//!
//! [`local::LocalDriver`] puts the right ones together for the computer the agent runs on.

pub mod args;
pub mod local;
pub mod terminal;

#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub mod agent_device;
#[cfg(any(target_os = "linux", test))]
pub mod probe_linux;
#[cfg(any(target_os = "macos", test))]
pub mod probe_macos;
#[cfg(any(windows, test))]
pub mod windows;

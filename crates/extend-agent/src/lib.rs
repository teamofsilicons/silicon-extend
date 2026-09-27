//! The Silicon Extend device agent for Mac, Windows and Linux.
//!
//! It shows a pairing code until a Carbon claims it, then keeps the computer connected to Extend
//! (one connection per Carbon who paired it), runs the commands Silicons send (through the device
//! engine on Mac and Linux, Extend's own driver on Windows, and a terminal on all three), and
//! shows which Silicon is using the computer with a Stop button. See `README.md` and
//! `docs/device-protocol.md`.

pub mod agent;
pub mod autostart;
pub mod awake;
pub mod config;
pub mod credential;
pub mod dispatch;
pub mod display;
pub mod drivers;
pub mod enroll;
pub mod hosted;
mod indicator;
pub mod notify;
pub mod service;
pub mod status;
pub mod sysinfo;
#[cfg(feature = "tray")]
pub mod ui;
pub mod ws;

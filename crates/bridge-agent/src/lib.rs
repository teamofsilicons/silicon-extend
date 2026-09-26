//! The Silicon Bridge device agent for Mac, Windows and Linux.
//!
//! It shows a pairing code until a Carbon claims it, then keeps the computer connected to Bridge,
//! runs the commands Silicons send (through agent-device on Mac and Linux, Bridge's own driver on
//! Windows, and a terminal on all three), and shows which Silicon is using the computer with a
//! Stop button. See `README.md` and `docs/device-protocol.md`.

pub mod agent;
pub mod autostart;
pub mod config;
pub mod credential;
pub mod dispatch;
pub mod drivers;
pub mod enroll;
pub mod hosted;
pub mod service;
pub mod status;
pub mod sysinfo;
#[cfg(feature = "tray")]
pub mod ui;
pub mod ws;

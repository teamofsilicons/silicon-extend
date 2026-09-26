//! Bridge's own Windows driver. agent-device doesn't support Windows, so Bridge reads the screen
//! with UI Automation, acts with `SendInput`, captures with GDI, and maps it all onto the same
//! command set, snapshot shape and `@eN` refs agent-device gives on a Mac or Linux computer.
//!
//! Everything that doesn't touch Windows APIs (the snapshot model, selectors, command parsing,
//! keystroke plans, app lists, pixels, the probe's answer) is plain Rust that unit-tests on any
//! host. The Win32 parts run on one worker thread that owns all COM objects.

pub mod apps;
pub mod commands;
pub mod image;
pub mod keys;
pub mod model;
pub mod probe_logic;
pub mod selector;

#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod clipboard;
#[cfg(windows)]
mod driver;
#[cfg(windows)]
mod input;
#[cfg(windows)]
mod shell;
#[cfg(windows)]
mod uia;
#[cfg(windows)]
mod worker;

#[cfg(windows)]
pub use driver::WindowsDriver;

//! What this computer is: OS, version, model and name, for enrollment and `hello`.

use extend_protocol::DeviceOs;

/// The OS this build runs on.
pub fn device_os() -> DeviceOs {
    if cfg!(target_os = "macos") {
        DeviceOs::Macos
    } else if cfg!(windows) {
        DeviceOs::Windows
    } else {
        DeviceOs::Linux
    }
}

/// A word for this kind of computer in UI text ("Mac", "computer").
pub fn computer_word() -> &'static str {
    if cfg!(target_os = "macos") { "Mac" } else { "computer" }
}

/// OS version, e.g. `15.1`, `10.0.26100`, `24.04`.
pub fn os_version() -> Option<String> {
    let info = os_info::get();
    let v = info.version().to_string();
    if v.is_empty() || v == "Unknown" {
        None
    } else {
        Some(truncate(&v, 64))
    }
}

/// Hardware model where the OS says it (`Mac15,6`, a DMI product name), else the OS name.
pub fn model() -> Option<String> {
    let found = platform_model();
    let fallback = || {
        let t = os_info::get().os_type().to_string();
        if t.is_empty() { None } else { Some(t) }
    };
    found.or_else(fallback).map(|m| truncate(&m, 128))
}

/// The computer's network name.
pub fn hostname() -> String {
    gethostname::gethostname()
        .to_string_lossy()
        .trim_end_matches(".local")
        .to_string()
}

#[cfg(target_os = "macos")]
fn platform_model() -> Option<String> {
    let out = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.model"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

#[cfg(target_os = "linux")]
fn platform_model() -> Option<String> {
    for p in [
        "/sys/devices/virtual/dmi/id/product_name",
        "/sys/firmware/devicetree/base/model",
    ] {
        if let Ok(s) = std::fs::read_to_string(p) {
            let s = s.trim_matches(|c: char| c.is_whitespace() || c == '\0').to_string();
            if !s.is_empty() && s != "To Be Filled By O.E.M." {
                return Some(s);
            }
        }
    }
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_model() -> Option<String> {
    None
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_this_computer() {
        assert!(matches!(
            device_os(),
            DeviceOs::Macos | DeviceOs::Linux | DeviceOs::Windows
        ));
        assert!(!hostname().is_empty());
        if let Some(m) = model() {
            assert!(m.chars().count() <= 128);
        }
    }
}

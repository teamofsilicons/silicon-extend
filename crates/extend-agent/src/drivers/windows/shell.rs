//! Opening apps and links, listing Start-menu apps, closing windows.

use std::os::windows::process::CommandExt;
use std::process::Command;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SW_SHOWNORMAL, SetForegroundWindow, WM_CLOSE};
use windows::core::{HSTRING, PCWSTR, w};

use super::apps::{StartApp, parse_start_apps};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Opens a file, program, `shell:` path or link with its default handler.
pub fn shell_open(target: &str) -> Result<(), String> {
    let target_w = HSTRING::from(target);
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &target_w,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute returns a value greater than 32 on success.
    if result.0 as isize > 32 {
        Ok(())
    } else {
        Err(format!(
            "Windows couldn't open {target:?} (error {}).",
            result.0 as isize
        ))
    }
}

/// Launches a Start-menu app by its AppID.
pub fn launch_app(app: &StartApp) -> Result<(), String> {
    if app.app_id.contains(":\\") && std::path::Path::new(&app.app_id).exists() {
        return shell_open(&app.app_id);
    }
    shell_open(&format!("shell:AppsFolder\\{}", app.app_id))
}

/// Runs a PowerShell one-liner without a console window.
fn powershell(script: &str) -> Result<String, String> {
    let out = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("couldn't run PowerShell: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "PowerShell failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Every app in the Start menu.
pub fn start_apps() -> Result<Vec<StartApp>, String> {
    let json = powershell(
        "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; Get-StartApps | Select-Object Name, AppID | ConvertTo-Json -Compress",
    )?;
    parse_start_apps(&json)
}

/// Asks a window to close, as clicking its close button would.
pub fn close_window(hwnd: HWND) -> bool {
    unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok() }
}

pub fn bring_to_front(hwnd: HWND) {
    unsafe {
        let _ = SetForegroundWindow(hwnd);
    }
}

/// `cmd /c ver`, for the probe's OS version.
pub fn ver() -> Option<String> {
    let out = Command::new("cmd.exe")
        .args(["/D", "/C", "ver"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    super::probe_logic::parse_ver(&String::from_utf8_lossy(&out.stdout))
}

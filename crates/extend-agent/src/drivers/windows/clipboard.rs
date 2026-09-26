//! Plain-text clipboard through the Win32 clipboard API.

use std::time::Duration;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// Opens the clipboard, retrying briefly while another app holds it.
fn open() -> Result<(), String> {
    for _ in 0..20 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err("another app is holding the clipboard; try again".into())
}

pub fn read() -> Result<String, String> {
    open()?;
    let result = unsafe {
        match GetClipboardData(CF_UNICODETEXT.0 as u32) {
            Ok(handle) if !handle.is_invalid() => {
                let global = HGLOBAL(handle.0);
                let ptr = GlobalLock(global) as *const u16;
                if ptr.is_null() {
                    Err("couldn't read the clipboard".to_owned())
                } else {
                    let mut len = 0usize;
                    while *ptr.add(len) != 0 {
                        len += 1;
                    }
                    let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
                    let _ = GlobalUnlock(global);
                    Ok(text)
                }
            }
            // No text on the clipboard reads as empty, like agent-device.
            _ => Ok(String::new()),
        }
    };
    unsafe {
        let _ = CloseClipboard();
    }
    result
}

pub fn write(text: &str) -> Result<(), String> {
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    open()?;
    let result = unsafe {
        (|| {
            EmptyClipboard().map_err(|e| format!("couldn't clear the clipboard: {e}"))?;
            if text.is_empty() {
                return Ok(());
            }
            let global = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2).map_err(|e| format!("out of memory: {e}"))?;
            let ptr = GlobalLock(global) as *mut u16;
            if ptr.is_null() {
                let _ = GlobalFree(Some(global));
                return Err("couldn't write the clipboard".to_owned());
            }
            std::ptr::copy_nonoverlapping(units.as_ptr(), ptr, units.len());
            let _ = GlobalUnlock(global);
            if let Err(e) = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(global.0))) {
                let _ = GlobalFree(Some(global));
                return Err(format!("couldn't write the clipboard: {e}"));
            }
            Ok(())
        })()
    };
    unsafe {
        let _ = CloseClipboard();
    }
    result
}

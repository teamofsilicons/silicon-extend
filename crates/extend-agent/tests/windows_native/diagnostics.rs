//! Read-only diagnostics for the disposable runner fixture. Never switches desktops, changes
//! DPI contexts, activates a window or invokes the product driver's capability/input probe.

use std::path::Path;

use serde_json::{Value, json};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::StationsAndDesktops::*;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PWSTR;

pub fn outcome(result: windows::core::Result<()>) -> Value {
    match result {
        Ok(()) => json!({"ok": true}),
        Err(error) => json!({"ok": false, "hresult": error.code().0, "error": error.to_string()}),
    }
}

fn rect(r: RECT) -> Value {
    json!({"left": r.left, "top": r.top, "right": r.right, "bottom": r.bottom})
}

fn dpi(context: DPI_AWARENESS_CONTEXT) -> Value {
    unsafe {
        json!({"context": context.0 as isize,
            "awareness": GetAwarenessFromDpiAwarenessContext(context).0,
            "per_monitor_v2": AreDpiAwarenessContextsEqual(context, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).as_bool()})
    }
}

fn object_name(handle: HANDLE) -> Value {
    let mut text = [0u16; 1024];
    let mut needed = 0;
    let result = unsafe {
        GetUserObjectInformationW(
            handle,
            UOI_NAME,
            Some(text.as_mut_ptr().cast()),
            std::mem::size_of_val(&text) as u32,
            Some(&mut needed),
        )
    };
    match result {
        Ok(()) => {
            let n = text.iter().position(|c| *c == 0).unwrap_or(text.len());
            json!({"name": String::from_utf16_lossy(&text[..n])})
        }
        Err(error) => json!({"error": error.to_string(), "needed_bytes": needed}),
    }
}

fn desktop_for_thread(id: u32) -> Value {
    match unsafe { GetThreadDesktop(id) } {
        Ok(handle) => object_name(HANDLE(handle.0)), // borrowed: do not CloseDesktop
        Err(error) => json!({"error": error.to_string()}),
    }
}

fn desktops() -> Value {
    let station = match unsafe { GetProcessWindowStation() } {
        Ok(handle) => object_name(HANDLE(handle.0)), // borrowed: do not CloseWindowStation
        Err(error) => json!({"error": error.to_string()}),
    };
    // Read permission only. Never call SwitchDesktop or SetThreadDesktop in this test lane.
    let input = match unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) } {
        Ok(handle) => {
            let mut value = object_name(HANDLE(handle.0));
            let mut receives_input: u32 = 0;
            let result = unsafe {
                GetUserObjectInformationW(
                    HANDLE(handle.0),
                    UOI_IO,
                    Some((&mut receives_input as *mut u32).cast()),
                    std::mem::size_of::<u32>() as u32,
                    None,
                )
            };
            value["receives_input_query"] = outcome(result);
            value["receives_input"] = json!(receives_input != 0);
            value["close"] = outcome(unsafe { CloseDesktop(handle) });
            value
        }
        Err(error) => json!({"error": error.to_string()}),
    };
    json!({"station": station, "thread": desktop_for_thread(unsafe { GetCurrentThreadId() }), "input": input})
}

fn process(id: u32) -> Value {
    let mut session = 0;
    let session_result = outcome(unsafe { ProcessIdToSessionId(id, &mut session) });
    let mut value = json!({"pid": id, "session_id": session, "session_query": session_result});
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, id) } {
        Ok(handle) => {
            let mut name = [0u16; 32768];
            let mut len = name.len() as u32;
            let image =
                unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(name.as_mut_ptr()), &mut len) };
            value["image_query"] = outcome(image);
            if value["image_query"]["ok"] == true {
                value["image"] = json!(String::from_utf16_lossy(&name[..(len as usize).min(name.len())]));
            }
            value["dpi"] = dpi(unsafe { GetDpiAwarenessContextForProcess(handle) });
            value["close"] = outcome(unsafe { CloseHandle(handle) });
        }
        Err(error) => value["open_error"] = json!(error.to_string()),
    }
    value
}

pub fn current() -> Value {
    json!({"process": process(std::process::id()), "thread_id": unsafe { GetCurrentThreadId() },
        "thread_dpi": dpi(unsafe { GetThreadDpiAwarenessContext() }),
        "process_dpi": dpi(unsafe { GetDpiAwarenessContextForProcess(GetCurrentProcess()) }),
        "desktops": desktops()})
}

pub fn window(hwnd: HWND) -> Value {
    unsafe {
        let mut pid = 0;
        let thread = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let mut title = [0u16; 2048];
        let title_len = GetWindowTextW(hwnd, &mut title).max(0) as usize;
        let mut class = [0u16; 256];
        let class_len = GetClassNameW(hwnd, &mut class).max(0) as usize;
        let mut bounds = RECT::default();
        let bounds_result = outcome(GetWindowRect(hwnd, &mut bounds));
        let mut frame = RECT::default();
        let frame_result = outcome(DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut frame as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        ));
        let mut cloak: u32 = 0;
        let cloak_result = outcome(DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloak as *mut u32).cast(),
            std::mem::size_of::<u32>() as u32,
        ));
        let mut titlebar = TITLEBARINFO {
            cbSize: std::mem::size_of::<TITLEBARINFO>() as u32,
            ..Default::default()
        };
        let titlebar_result = outcome(GetTitleBarInfo(hwnd, &mut titlebar));
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        json!({"hwnd": hwnd.0 as usize, "process": process(pid), "thread_id": thread,
            "thread_desktop": desktop_for_thread(thread),
            "class": String::from_utf16_lossy(&class[..class_len.min(class.len())]),
            "title": String::from_utf16_lossy(&title[..title_len.min(title.len())]),
            "window_dpi": dpi(GetWindowDpiAwarenessContext(hwnd)), "dpi": GetDpiForWindow(hwnd),
            "bounds": rect(bounds), "bounds_query": bounds_result,
            "extended_frame": rect(frame), "extended_frame_query": frame_result,
            "titlebar": rect(titlebar.rcTitleBar), "titlebar_state": titlebar.rgstate, "titlebar_query": titlebar_result,
            "style": style, "ex_style": ex_style, "topmost": ex_style & WS_EX_TOPMOST.0 != 0,
            "visible": IsWindowVisible(hwnd).as_bool(), "enabled": IsWindowEnabled(hwnd).as_bool(), "iconic": IsIconic(hwnd).as_bool(),
            "cloaked": cloak, "cloaked_query": cloak_result,
            "previous_z_order_hwnd": GetWindow(hwnd, GW_HWNDPREV).ok().map(|h| h.0 as usize),
            "next_z_order_hwnd": GetWindow(hwnd, GW_HWNDNEXT).ok().map(|h| h.0 as usize)})
    }
}

pub fn failure(owned: HWND, hit: HWND, point: POINT, child_ready: &Value, screenshot: &Path) -> Value {
    let caller = current();
    let own_before = window(owned);
    let foreign = window(hit);
    let mut unused = 0;
    let acknowledged = unsafe {
        SendMessageTimeoutW(
            owned,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            2_000,
            Some(&mut unused),
        )
    }
    .0 != 0;
    let hit_after = unsafe { WindowFromPoint(point) };
    // This observation cannot rescue the failed guard: the caller still asserts its original
    // exact HWND/PID/HTCAPTION results and never sends input on that failure path.
    json!({"parent": caller, "child_ready": child_ready, "owned_before_ack": own_before,
        "hit": foreign, "owned_queue_acknowledged": acknowledged, "owned_after_ack": window(owned),
        "hit_after_ack": window(hit_after), "foreground": window(unsafe { GetForegroundWindow() }),
        "screenshot": screenshot_read_only(screenshot, &caller["desktops"])})
}

fn screenshot_read_only(path: &Path, context: &Value) -> Value {
    // Only capture the normal input desktop already assigned to this disposable runner.
    // If either read fails, retain JSON evidence instead of trying to switch/open another desktop.
    if context["station"]["name"].as_str() != Some("WinSta0")
        || context["thread"]["name"].as_str().is_none()
        || context["thread"]["name"] != context["input"]["name"]
        || context["input"]["receives_input"] != true
    {
        return json!({"skipped": "not confirmed on the same WinSta0 input desktop"});
    }
    match capture(path) {
        Ok(bounds) => {
            json!({"path": path, "bounds": rect(bounds), "scope": "read-only disposable runner desktop DC clip box; not isolated-window capture"})
        }
        Err(error) => json!({"error": error}),
    }
}

fn capture(path: &Path) -> Result<RECT, String> {
    // Resources are local GDI objects; no foreign window messages, driver probe or input calls.
    struct Capture {
        screen: HDC,
        memory: HDC,
        bitmap: HBITMAP,
        previous: HGDIOBJ,
    }
    impl Drop for Capture {
        fn drop(&mut self) {
            unsafe {
                if !self.previous.is_invalid() {
                    SelectObject(self.memory, self.previous);
                }
                if !self.bitmap.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
                }
                if !self.memory.is_invalid() {
                    let _ = DeleteDC(self.memory);
                }
                if !self.screen.is_invalid() {
                    ReleaseDC(None, self.screen);
                }
            }
        }
    }
    unsafe {
        let mut objects = Capture {
            screen: GetDC(None),
            memory: HDC::default(),
            bitmap: HBITMAP::default(),
            previous: HGDIOBJ::default(),
        };
        if objects.screen.is_invalid() {
            return Err("GetDC failed".into());
        }
        let mut bounds = RECT::default();
        if GetClipBox(objects.screen, &mut bounds).0 <= 1 {
            return Err("desktop clip box unavailable".into());
        }
        let (width, height) = (bounds.right - bounds.left, bounds.bottom - bounds.top);
        if width <= 0 || height <= 0 || i64::from(width) * i64::from(height) > 16_777_216 {
            return Err(format!("desktop dimensions outside bounded capture: {width}x{height}"));
        }
        objects.memory = CreateCompatibleDC(Some(objects.screen));
        if objects.memory.is_invalid() {
            return Err("CreateCompatibleDC failed".into());
        }
        objects.bitmap = CreateCompatibleBitmap(objects.screen, width, height);
        if objects.bitmap.is_invalid() {
            return Err("CreateCompatibleBitmap failed".into());
        }
        objects.previous = SelectObject(objects.memory, HGDIOBJ(objects.bitmap.0));
        if objects.previous.is_invalid() {
            return Err("SelectObject failed".into());
        }
        BitBlt(
            objects.memory,
            0,
            0,
            width,
            height,
            Some(objects.screen),
            bounds.left,
            bounds.top,
            SRCCOPY | CAPTUREBLT,
        )
        .map_err(|e| e.to_string())?;
        if SelectObject(objects.memory, objects.previous).is_invalid() {
            return Err("could not deselect the captured bitmap".into());
        }
        objects.previous = HGDIOBJ::default();
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        if GetDIBits(
            objects.memory,
            objects.bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        ) != height
        {
            return Err("incomplete desktop pixel capture".into());
        }
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&pixels).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
        Ok(bounds)
    }
}

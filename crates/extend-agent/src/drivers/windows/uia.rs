//! Reading windows and their element trees through UI Automation.
//!
//! Every call here happens on the driver's worker thread, which owns the COM apartment.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetForegroundWindow, GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, PWSTR};

use super::model::{MAX_DEPTH, MAX_NODES, RawNode, Rect, ct};

/// A visible top-level window.
#[derive(Debug, Clone)]
pub struct TopWindow {
    pub hwnd: HWND,
    pub pid: u32,
    pub title: String,
    pub rect: Rect,
    pub minimized: bool,
}

fn rect_of(r: RECT) -> Rect {
    Rect { x: r.left as f64, y: r.top as f64, width: (r.right - r.left) as f64, height: (r.bottom - r.top) as f64 }
}

unsafe extern "system" fn collect_hwnd(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `&mut Vec<HWND>` passed to `EnumWindows` below, alive for the call.
    let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    list.push(hwnd);
    BOOL(1)
}

pub fn window_title(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

pub fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

pub fn describe_window(hwnd: HWND) -> TopWindow {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    TopWindow {
        hwnd,
        pid: window_pid(hwnd),
        title: window_title(hwnd),
        rect: rect_of(r),
        minimized: unsafe { IsIconic(hwnd).as_bool() },
    }
}

/// Visible, titled, non-tool top-level windows, front to back.
pub fn top_windows() -> Vec<TopWindow> {
    let mut hwnds: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect_hwnd), LPARAM(&mut hwnds as *mut Vec<HWND> as isize));
    }
    hwnds
        .into_iter()
        .filter(|&h| unsafe { IsWindowVisible(h).as_bool() })
        .filter(|&h| unsafe { (GetWindowLongW(h, GWL_EXSTYLE) as u32) & WS_EX_TOOLWINDOW.0 == 0 })
        .map(describe_window)
        .filter(|w| !w.title.is_empty() && (!w.rect.is_empty() || w.minimized))
        .collect()
}

pub fn foreground_window() -> Option<TopWindow> {
    let hwnd = unsafe { GetForegroundWindow() };
    (!hwnd.is_invalid()).then(|| describe_window(hwnd))
}

/// Full path of a process's executable.
pub fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut size);
        let _ = CloseHandle(handle);
        ok.ok()?;
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    }
}

/// `notepad` for `C:\Windows\notepad.exe`.
pub fn process_stem(pid: u32) -> Option<String> {
    let path = process_path(pid)?;
    let file = path.rsplit(['\\', '/']).next()?.to_owned();
    Some(file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(&file).to_owned())
}

/// A capture: raw nodes plus the live elements behind them (None for Extend's synthetic root).
pub struct Capture {
    pub raw: Vec<RawNode>,
    pub elements: Vec<Option<IUIAutomationElement>>,
    pub truncated: bool,
}

pub struct Uia {
    automation: IUIAutomation,
    walker: IUIAutomationTreeWalker,
    cache: IUIAutomationCacheRequest,
}

const PROPERTIES: &[UIA_PROPERTY_ID] = &[
    UIA_NamePropertyId,
    UIA_ControlTypePropertyId,
    UIA_BoundingRectanglePropertyId,
    UIA_IsEnabledPropertyId,
    UIA_HasKeyboardFocusPropertyId,
    UIA_IsKeyboardFocusablePropertyId,
    UIA_AutomationIdPropertyId,
    UIA_ClassNamePropertyId,
    UIA_ProcessIdPropertyId,
    UIA_IsOffscreenPropertyId,
    UIA_ValueValuePropertyId,
    UIA_ValueIsReadOnlyPropertyId,
    UIA_ToggleToggleStatePropertyId,
    UIA_SelectionItemIsSelectedPropertyId,
    UIA_ScrollVerticallyScrollablePropertyId,
    UIA_ScrollHorizontallyScrollablePropertyId,
];

const PATTERNS: &[UIA_PATTERN_ID] = &[
    UIA_ValuePatternId,
    UIA_TogglePatternId,
    UIA_SelectionItemPatternId,
    UIA_ScrollPatternId,
    UIA_InvokePatternId,
    UIA_ExpandCollapsePatternId,
];

impl Uia {
    /// Joins the multithreaded apartment and creates the automation client.
    pub fn new() -> Result<Self, String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("UI Automation isn't available on this computer: {e}"))?;
            let walker = automation.ControlViewWalker().map_err(|e| format!("UI Automation walker: {e}"))?;
            let cache = automation.CreateCacheRequest().map_err(|e| format!("UI Automation cache: {e}"))?;
            for p in PROPERTIES {
                let _ = cache.AddProperty(*p);
            }
            for p in PATTERNS {
                let _ = cache.AddPattern(*p);
            }
            Ok(Self { automation, walker, cache })
        }
    }

    /// Reads the element trees of `windows` under one synthetic application node.
    pub fn capture(&self, windows: &[TopWindow], app_name: &str, app_id: &str, cancel: &AtomicBool) -> Capture {
        let mut cap = Capture { raw: Vec::new(), elements: Vec::new(), truncated: false };
        let root_pid = windows.first().map_or(0, |w| w.pid);
        cap.raw.push(RawNode {
            depth: 0,
            parent: None,
            control_type: ct::APPLICATION,
            name: app_name.to_owned(),
            enabled: true,
            pid: root_pid,
            app_name: app_name.to_owned(),
            app_id: app_id.to_owned(),
            ..Default::default()
        });
        cap.elements.push(None);
        for w in windows {
            if cap.raw.len() >= MAX_NODES || cancel.load(Ordering::Relaxed) {
                cap.truncated = true;
                break;
            }
            let Ok(el) = (unsafe { self.automation.ElementFromHandleBuildCache(w.hwnd, &self.cache) }) else { continue };
            let ctx = Ctx { app_name, app_id, window_title: &w.title };
            self.walk(&mut cap, el, 1, Some(0), &ctx, cancel);
        }
        cap
    }

    fn walk(&self, cap: &mut Capture, el: IUIAutomationElement, depth: usize, parent: Option<usize>, ctx: &Ctx, cancel: &AtomicBool) {
        if cap.raw.len() >= MAX_NODES {
            cap.truncated = true;
            return;
        }
        let mut node = read_cached(&el);
        node.depth = depth;
        node.parent = parent;
        node.app_name = ctx.app_name.to_owned();
        node.app_id = ctx.app_id.to_owned();
        node.window_title = ctx.window_title.to_owned();
        let index = cap.raw.len();
        cap.raw.push(node);
        cap.elements.push(Some(el.clone()));
        if depth >= MAX_DEPTH || cancel.load(Ordering::Relaxed) {
            return;
        }
        let mut child = unsafe { self.walker.GetFirstChildElementBuildCache(&el, &self.cache) }.ok();
        while let Some(c) = child {
            if cap.raw.len() >= MAX_NODES {
                cap.truncated = true;
                return;
            }
            let next = unsafe { self.walker.GetNextSiblingElementBuildCache(&c, &self.cache) }.ok();
            self.walk(cap, c, depth + 1, Some(index), ctx, cancel);
            child = next;
        }
    }

    /// Fresh properties of one element (for `get`, `is` and waits on a ref).
    pub fn refresh(&self, el: &IUIAutomationElement) -> Option<RawNode> {
        let fresh = unsafe { el.BuildUpdatedCache(&self.cache) }.ok()?;
        Some(read_cached(&fresh))
    }
}

struct Ctx<'a> {
    app_name: &'a str,
    app_id: &'a str,
    window_title: &'a str,
}

fn bstr(r: windows::core::Result<windows::core::BSTR>) -> String {
    r.map(|b| b.to_string()).unwrap_or_default()
}

fn flag(r: windows::core::Result<BOOL>) -> bool {
    r.map(|b| b.as_bool()).unwrap_or(false)
}

/// Reads a node from an element's cached properties.
fn read_cached(el: &IUIAutomationElement) -> RawNode {
    unsafe {
        let rect = el.CachedBoundingRectangle().ok().map(rect_of).filter(|r| !r.is_empty());
        let value_pattern = el.GetCachedPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId).ok();
        let (value, editable) = match &value_pattern {
            Some(p) => (p.CachedValue().ok().map(|b| b.to_string()), !flag(p.CachedIsReadOnly())),
            None => (None, false),
        };
        let checked = el
            .GetCachedPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
            .ok()
            .and_then(|p| p.CachedToggleState().ok())
            .map(|s| s == ToggleState_On);
        let selected = el
            .GetCachedPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
            .ok()
            .map(|p| flag(p.CachedIsSelected()));
        let scrollable = el
            .GetCachedPatternAs::<IUIAutomationScrollPattern>(UIA_ScrollPatternId)
            .ok()
            .is_some_and(|p| flag(p.CachedVerticallyScrollable()) || flag(p.CachedHorizontallyScrollable()));
        let invokable = el.GetCachedPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId).is_ok()
            || el.GetCachedPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId).is_ok()
            || checked.is_some()
            || selected.is_some();
        RawNode {
            control_type: el.CachedControlType().map(|c| c.0).unwrap_or(0),
            class_name: bstr(el.CachedClassName()),
            name: bstr(el.CachedName()),
            value,
            automation_id: bstr(el.CachedAutomationId()),
            rect,
            enabled: flag(el.CachedIsEnabled()),
            focused: flag(el.CachedHasKeyboardFocus()),
            keyboard_focusable: flag(el.CachedIsKeyboardFocusable()),
            offscreen: flag(el.CachedIsOffscreen()),
            selected,
            checked,
            editable,
            scrollable,
            invokable,
            pid: el.CachedProcessId().unwrap_or(0) as u32,
            ..Default::default()
        }
    }
}

/// The on-screen rectangle of an element right now.
pub fn current_rect(el: &IUIAutomationElement) -> Option<Rect> {
    unsafe { el.CurrentBoundingRectangle() }.ok().map(rect_of).filter(|r| !r.is_empty())
}

pub fn set_focus(el: &IUIAutomationElement) -> Result<(), String> {
    unsafe { el.SetFocus() }.map_err(|e| format!("couldn't focus the element: {e}"))
}

/// Sets a text value through the Value pattern (the fallback when typing isn't possible).
pub fn set_value(el: &IUIAutomationElement, text: &str) -> Result<(), String> {
    unsafe {
        let p = el
            .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            .map_err(|_| "the element doesn't take text".to_owned())?;
        p.SetValue(&windows::core::BSTR::from(text)).map_err(|e| format!("couldn't set the text: {e}"))
    }
}

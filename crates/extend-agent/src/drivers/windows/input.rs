//! The real mouse and keyboard, through `SendInput`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MOUSE_EVENT_FLAGS, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput,
    VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;

use super::commands::{Button, WHEEL_NOTCH};
use super::keys::KeyStroke;

fn mouse(flags: MOUSE_EVENT_FLAGS, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> Result<(), String> {
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err("Windows blocked the input. The computer may be locked, or an admin prompt is in front (those always need the Carbon).".into())
    }
}

pub fn move_to(x: i32, y: i32) -> Result<(), String> {
    unsafe { SetCursorPos(x, y) }.map_err(|e| format!("couldn't move the pointer: {e}"))
}

pub fn click(x: i32, y: i32, button: Button, count: u32, interval_ms: u64) -> Result<(), String> {
    move_to(x, y)?;
    let (down, up) = match button {
        Button::Primary => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        Button::Secondary => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        Button::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
    };
    for i in 0..count {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(interval_ms));
        }
        send(&[mouse(down, 0)])?;
        std::thread::sleep(Duration::from_millis(40));
        send(&[mouse(up, 0)])?;
    }
    Ok(())
}

/// Scrolls the wheel at a point: positive `vertical` scrolls up, positive `horizontal` right.
pub fn wheel(x: i32, y: i32, vertical: i32, horizontal: i32) -> Result<(), String> {
    move_to(x, y)?;
    // Send in notch-sized steps so apps that ignore large deltas still move.
    let mut steps = Vec::new();
    for (total, flags) in [(vertical, MOUSEEVENTF_WHEEL), (horizontal, MOUSEEVENTF_HWHEEL)] {
        let mut left = total;
        while left != 0 {
            let step = left.signum() * left.abs().min(WHEEL_NOTCH);
            steps.push(mouse(flags, step));
            left -= step;
        }
    }
    for s in steps {
        send(&[s])?;
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

/// Types keystrokes; stops early when `cancel` is set.
pub fn keystrokes(strokes: &[KeyStroke], cancel: &AtomicBool) -> Result<(), String> {
    for s in strokes {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        match *s {
            KeyStroke::Unicode(unit) => send(&[
                key(0, unit, KEYEVENTF_UNICODE),
                key(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
            ])?,
            KeyStroke::Key(vk) => send(&[key(vk, 0, KEYBD_EVENT_FLAGS(0)), key(vk, 0, KEYEVENTF_KEYUP)])?,
            KeyStroke::Chord(modifier, vk) => send(&[
                key(modifier, 0, KEYBD_EVENT_FLAGS(0)),
                key(vk, 0, KEYBD_EVENT_FLAGS(0)),
                key(vk, 0, KEYEVENTF_KEYUP),
                key(modifier, 0, KEYEVENTF_KEYUP),
            ])?,
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    Ok(())
}

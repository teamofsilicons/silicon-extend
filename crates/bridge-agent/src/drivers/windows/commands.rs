//! Parsing a relayed command into what the Windows driver will do.
//!
//! The grammar follows agent-device's CLI (`understanding/cli.yaml`, `device_commands`), so the same
//! `bridge click @e2` works on every computer. Anything Windows can't do is refused here with
//! `unsupported_on_device` and a message saying what to use instead.

use super::model::SnapshotOptions;
use super::selector::{self, Selector};
use crate::drivers::args::{self, ParsedArgs};

#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

fn invalid(message: impl Into<String>) -> Refusal {
    Refusal { code: "invalid_args", message: message.into() }
}

fn unsupported(message: impl Into<String>) -> Refusal {
    Refusal { code: "unsupported_on_device", message: message.into() }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Ref(String),
    Selector(Selector),
    Point(i32, i32),
}

impl Target {
    pub fn describe(&self) -> String {
        match self {
            Target::Ref(r) => r.clone(),
            Target::Selector(s) => s.source.clone(),
            Target::Point(x, y) => format!("{x},{y}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Primary,
    Secondary,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    App,
    FrontmostApp,
    Desktop,
}

impl Surface {
    pub fn as_str(self) -> &'static str {
        match self {
            Surface::App => "app",
            Surface::FrontmostApp => "frontmost-app",
            Surface::Desktop => "desktop",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Predicate {
    Visible,
    Hidden,
    Exists,
    Absent,
    Editable,
    Selected,
    Focused,
    Text,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FindAction {
    Click,
    Fill(String),
    Focus,
    List,
    Wait(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locator {
    Any,
    Text,
    Label,
    Value,
    Role,
    Id,
}

impl Locator {
    pub fn as_str(self) -> &'static str {
        match self {
            Locator::Any => "any",
            Locator::Text => "text",
            Locator::Label => "label",
            Locator::Value => "value",
            Locator::Role => "role",
            Locator::Id => "id",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Wait {
    Ms(u64),
    Text(String, u64),
    Present(Target, u64),
    Absent(Target, u64),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Snapshot(SnapshotOptions),
    GetText(Target),
    GetAttrs(Target),
    Find { locator: Locator, query: String, action: FindAction },
    Is { predicate: Predicate, target: Target, value: Option<String> },
    Wait(Wait),
    Screenshot { name: String, scale: Option<f64>, fullscreen: bool },
    Click { target: Target, button: Button, count: u32, interval_ms: u64 },
    Hover(Target),
    Focus(Target),
    Fill { target: Target, text: String },
    Type(String),
    Scroll { direction: Direction, fraction: f64, pixels: Option<i32> },
    ClipboardRead,
    ClipboardWrite(String),
    Open { target: Option<String>, surface: Surface },
    Close { app: Option<String> },
    Apps { all: bool },
    AppState,
}

/// Default for `wait` and `find … wait` when no timeout is given.
pub const DEFAULT_WAIT_MS: u64 = 10_000;

/// Flags every command accepts and the Windows driver ignores (they tune agent-device's own daemon).
const IGNORED_SWITCHES: &[&str] = &["--settle", "--force-full", "--no-record", "--verbose", "--debug", "-v", "--foreground"];
const VALUE_FLAGS: &[&str] = &[
    "-d", "--depth", "-s", "--scope", "--timeout", "--button", "--count", "--pixels", "--scale", "--surface",
    "--delay-ms", "--hold-ms", "--interval-ms", "--crop-on",
];

fn check_flags(p: &ParsedArgs, command: &str, allowed: &[&str]) -> Result<(), Refusal> {
    for (name, _) in &p.flags {
        if allowed.contains(&name.as_str()) || IGNORED_SWITCHES.contains(&name.as_str()) || name == "--timeout" {
            continue;
        }
        return Err(invalid(format!("{command} doesn't take {name} on Windows.")));
    }
    Ok(())
}

fn parse_u64(v: &str, what: &str) -> Result<u64, Refusal> {
    v.parse::<u64>().map_err(|_| invalid(format!("{what} must be a whole number of milliseconds, got {v:?}.")))
}

fn parse_selector(s: &str) -> Result<Selector, Refusal> {
    selector::parse(s).map_err(|e| invalid(format!("Bad selector: {e}.")))
}

/// A target from the positionals: `@e3`, `<x> <y>`, or a selector (several tokens are joined).
pub fn parse_target(positionals: &[String]) -> Result<Target, Refusal> {
    match positionals {
        [] => Err(invalid("Give a target: a ref like @e3 from the last snapshot, or a selector like 'label=\"Save\"'.")),
        [x, y] if args::is_number(x) && args::is_number(y) => {
            let (x, y) = (x.parse::<f64>().unwrap_or(0.0), y.parse::<f64>().unwrap_or(0.0));
            Ok(Target::Point(x.round() as i32, y.round() as i32))
        }
        [one] if one.starts_with('@') && one.len() > 1 => Ok(Target::Ref(one.clone())),
        many => Ok(Target::Selector(parse_selector(&many.join(" "))?)),
    }
}

/// One target token followed by text (`fill @e3 hello world`).
fn target_and_text(positionals: &[String], command: &str) -> Result<(Target, String), Refusal> {
    if positionals.len() < 2 {
        return Err(invalid(format!("{command} needs a target and the text: {command} <@ref|selector> <text>.")));
    }
    Ok((parse_target(&positionals[..1])?, positionals[1..].join(" ")))
}

fn parse_predicate(s: &str) -> Option<Predicate> {
    Some(match s {
        "visible" => Predicate::Visible,
        "hidden" => Predicate::Hidden,
        "exists" => Predicate::Exists,
        "absent" => Predicate::Absent,
        "editable" => Predicate::Editable,
        "selected" => Predicate::Selected,
        "focused" => Predicate::Focused,
        "text" => Predicate::Text,
        _ => return None,
    })
}

fn optional_ms(p: &ParsedArgs, positional: Option<&String>) -> Result<u64, Refusal> {
    if let Some(v) = positional {
        return parse_u64(v, "The timeout");
    }
    match p.value("--timeout") {
        Some(v) => parse_u64(v, "--timeout"),
        None => Ok(DEFAULT_WAIT_MS),
    }
}

fn unsupported_command(command: &str) -> Option<&'static str> {
    Some(match command {
        "record" => "Screen recording isn't available in Silicon Bridge for Windows yet. Take screenshots with `bridge screenshot` instead.",
        "logs" => "Device logs aren't available in Silicon Bridge for Windows yet. Use `bridge terminal run` to read log files.",
        "alert" => "System pop-up handling isn't available on Windows yet. Read the dialog with `bridge snapshot -i` and click its buttons.",
        "replay" | "test" | "batch" => "Replaying saved steps isn't available in Silicon Bridge for Windows yet. Run the commands one at a time.",
        "diff" => "diff isn't available on Windows yet. Take a new `bridge snapshot` or `bridge screenshot` and compare.",
        "longpress" | "swipe" | "gesture" => "Touch gestures don't exist on a Windows computer. Use click, press, scroll or hover.",
        "back" | "home" | "app-switcher" => "Windows has no system back, home or app-switcher buttons. Use click, open or close.",
        "tv-remote" => "This is a computer, not a TV: there are no remote buttons.",
        "keyboard" => "Windows computers have a physical keyboard; there's no on-screen keyboard to check or hide.",
        "install" | "reinstall" => "Installing apps isn't supported on Windows through Bridge. Use `bridge terminal run` with an installer.",
        "adb" => "adb is for Android devices, not Windows computers.",
        "notifications" => "Reading notifications is only available on Android.",
        "display" => "display is for TVs.",
        "terminal" => "terminal is handled by the Bridge agent, not the Windows screen driver.",
        _ => return None,
    })
}

/// Parses a command. `Err` is an `invalid_args` or `unsupported_on_device` refusal.
pub fn parse(command: &str, raw_args: &[String]) -> Result<Action, Refusal> {
    if let Some(msg) = unsupported_command(command) {
        return Err(unsupported(msg));
    }
    let p = args::parse(raw_args, VALUE_FLAGS);
    let pos = &p.positionals;
    match command {
        "snapshot" => {
            check_flags(&p, command, &["-i", "--interactive", "-d", "--depth", "-s", "--scope", "--raw", "--actions", "-c", "--compact", "--diff"])?;
            if p.has("--diff") {
                return Err(unsupported("snapshot --diff isn't available on Windows yet. Take a new snapshot."));
            }
            if !pos.is_empty() {
                return Err(invalid(format!("snapshot takes no positional arguments, got {:?}.", pos.join(" "))));
            }
            let depth = match p.value("-d").or(p.value("--depth")) {
                Some(v) => Some(v.parse::<usize>().map_err(|_| invalid(format!("-d must be a whole number, got {v:?}.")))?),
                None => None,
            };
            Ok(Action::Snapshot(SnapshotOptions {
                interactive: p.has("-i") || p.has("--interactive"),
                depth,
                scope: p.value("-s").or(p.value("--scope")).map(str::to_owned),
                raw: p.has("--raw"),
            }))
        }
        "get" => {
            check_flags(&p, command, &[])?;
            match pos.first().map(String::as_str) {
                Some("text") => Ok(Action::GetText(parse_target(&pos[1..])?)),
                Some("attrs") => Ok(Action::GetAttrs(parse_target(&pos[1..])?)),
                _ => Err(invalid("Use get text <@ref|selector> or get attrs <@ref|selector>.")),
            }
        }
        "find" => {
            check_flags(&p, command, &[])?;
            let (locator, rest) = match pos.first().map(String::as_str) {
                Some("text") if pos.len() > 1 => (Locator::Text, &pos[1..]),
                Some("label") if pos.len() > 1 => (Locator::Label, &pos[1..]),
                Some("value") if pos.len() > 1 => (Locator::Value, &pos[1..]),
                Some("role") if pos.len() > 1 => (Locator::Role, &pos[1..]),
                Some("id") if pos.len() > 1 => (Locator::Id, &pos[1..]),
                Some(_) => (Locator::Any, &pos[..]),
                None => return Err(invalid("find needs something to look for: find <text> [click|fill <text>|list].")),
            };
            let query = rest[0].clone();
            let action = match rest.get(1).map(String::as_str) {
                None | Some("list") => FindAction::List,
                Some("click") | Some("press") | Some("tap") => FindAction::Click,
                Some("focus") => FindAction::Focus,
                Some("fill") | Some("type") => {
                    if rest.len() < 3 {
                        return Err(invalid("find … fill needs the text to type."));
                    }
                    FindAction::Fill(rest[2..].join(" "))
                }
                Some("wait") => FindAction::Wait(optional_ms(&p, rest.get(2))?),
                Some(other) => return Err(invalid(format!("find doesn't know the action {other:?}; use click, fill <text>, focus, wait [ms] or list."))),
            };
            Ok(Action::Find { locator, query, action })
        }
        "is" => {
            check_flags(&p, command, &[])?;
            let Some(predicate) = pos.first().and_then(|s| parse_predicate(s)) else {
                return Err(invalid("Use is visible|hidden|exists|absent|editable|selected|focused|text <selector> [value]."));
            };
            if pos.len() < 2 {
                return Err(invalid("is needs a selector."));
            }
            let (target, value) = if predicate == Predicate::Text {
                if pos.len() < 3 {
                    return Err(invalid("is text needs a selector and the expected text."));
                }
                (parse_target(&pos[1..2])?, Some(pos[2..].join(" ")))
            } else {
                (parse_target(&pos[1..])?, None)
            };
            Ok(Action::Is { predicate, target, value })
        }
        "wait" => {
            check_flags(&p, command, &[])?;
            match pos.first().map(String::as_str) {
                None => Err(invalid("Use wait <ms>, wait text <text> [ms], wait <selector> [ms] or wait absent <selector> [ms].")),
                Some(ms) if pos.len() == 1 && ms.parse::<u64>().is_ok() => Ok(Action::Wait(Wait::Ms(parse_u64(ms, "wait")?))),
                Some("text") => {
                    let text = pos.get(1).ok_or_else(|| invalid("wait text needs the text."))?.clone();
                    Ok(Action::Wait(Wait::Text(text, optional_ms(&p, pos.get(2))?)))
                }
                Some("absent") => {
                    let target = parse_target(pos.get(1..2).unwrap_or_default())?;
                    Ok(Action::Wait(Wait::Absent(target, optional_ms(&p, pos.get(2))?)))
                }
                Some(_) => {
                    let (target_tokens, ms) = match pos.last() {
                        Some(last) if pos.len() > 1 && last.parse::<u64>().is_ok() => (&pos[..pos.len() - 1], Some(last)),
                        _ => (&pos[..], None),
                    };
                    Ok(Action::Wait(Wait::Present(parse_target(target_tokens)?, optional_ms(&p, ms)?)))
                }
            }
        }
        "screenshot" => {
            check_flags(&p, command, &["--scale", "--fullscreen", "--full", "-f", "--overlay-refs", "--crop-on", "--normalize-status-bar"])?;
            if p.has("--crop-on") {
                return Err(unsupported("screenshot --crop-on isn't available on Windows."));
            }
            if p.has("--overlay-refs") {
                return Err(unsupported("screenshot --overlay-refs isn't available on Windows yet."));
            }
            if pos.len() > 1 {
                return Err(invalid("screenshot takes at most one name."));
            }
            let scale = match p.value("--scale") {
                Some(v) => {
                    let s = v.parse::<f64>().map_err(|_| invalid(format!("--scale must be a number from 0.01 to 1, got {v:?}.")))?;
                    if !(0.01..=1.0).contains(&s) {
                        return Err(invalid(format!("--scale must be from 0.01 to 1, got {v}.")));
                    }
                    Some(s)
                }
                None => None,
            };
            let name = args::with_extension(&args::safe_file_name(pos.first().map_or("", String::as_str), "screenshot.png"), &[".png"]);
            Ok(Action::Screenshot { name, scale, fullscreen: p.has("--fullscreen") || p.has("--full") || p.has("-f") })
        }
        "click" | "press" => {
            check_flags(&p, command, &["--button", "--count", "--hold-ms", "--interval-ms", "--double-tap"])?;
            let button = match p.value("--button") {
                None | Some("primary") | Some("left") => Button::Primary,
                Some("secondary") | Some("right") => Button::Secondary,
                Some("middle") => Button::Middle,
                Some(other) => return Err(invalid(format!("--button must be primary or secondary, got {other:?}."))),
            };
            let mut count = match p.value("--count") {
                Some(v) => v.parse::<u32>().ok().filter(|c| (1..=20).contains(c)).ok_or_else(|| invalid(format!("--count must be 1 to 20, got {v:?}.")))?,
                None => 1,
            };
            if p.has("--double-tap") {
                count *= 2;
            }
            let interval_ms = match p.value("--interval-ms") {
                Some(v) => parse_u64(v, "--interval-ms")?,
                None => 80,
            };
            Ok(Action::Click { target: parse_target(pos)?, button, count, interval_ms })
        }
        "hover" => {
            check_flags(&p, command, &[])?;
            Ok(Action::Hover(parse_target(pos)?))
        }
        "focus" => {
            check_flags(&p, command, &[])?;
            Ok(Action::Focus(parse_target(pos)?))
        }
        "fill" => {
            check_flags(&p, command, &["--delay-ms"])?;
            let (target, text) = target_and_text(pos, command)?;
            Ok(Action::Fill { target, text })
        }
        "type" => {
            check_flags(&p, command, &["--delay-ms"])?;
            if pos.is_empty() {
                return Err(invalid("type needs the text to type."));
            }
            Ok(Action::Type(pos.join(" ")))
        }
        "scroll" => {
            check_flags(&p, command, &["--pixels"])?;
            let direction = match pos.first().map(String::as_str) {
                Some("up") => Direction::Up,
                Some("down") => Direction::Down,
                Some("left") => Direction::Left,
                Some("right") => Direction::Right,
                Some("top") => Direction::Top,
                Some("bottom") => Direction::Bottom,
                _ => return Err(invalid("Use scroll up|down|left|right [fraction] [--pixels <n>].")),
            };
            let fraction = match pos.get(1) {
                Some(v) => v.parse::<f64>().ok().filter(|f| *f > 0.0 && *f <= 10.0).ok_or_else(|| invalid(format!("The scroll amount must be a positive number, got {v:?}.")))?,
                None => 0.5,
            };
            let pixels = match p.value("--pixels") {
                Some(v) => Some(v.parse::<i32>().ok().filter(|n| *n > 0).ok_or_else(|| invalid(format!("--pixels must be a positive whole number, got {v:?}.")))?),
                None => None,
            };
            Ok(Action::Scroll { direction, fraction, pixels })
        }
        "clipboard" => {
            check_flags(&p, command, &[])?;
            match pos.first().map(String::as_str) {
                Some("read") => Ok(Action::ClipboardRead),
                Some("write") => Ok(Action::ClipboardWrite(pos[1..].join(" "))),
                _ => Err(invalid("Use clipboard read or clipboard write <text>.")),
            }
        }
        "open" => {
            check_flags(&p, command, &["--surface", "--relaunch", "--activity", "--save-script"])?;
            if p.has("--save-script") {
                return Err(unsupported("--save-script isn't available on Windows: replay scripts need agent-device, which doesn't run on Windows."));
            }
            let surface = match p.value("--surface") {
                None | Some("app") => {
                    if pos.is_empty() { Surface::FrontmostApp } else { Surface::App }
                }
                Some("frontmost-app") => Surface::FrontmostApp,
                Some("desktop") => Surface::Desktop,
                Some("menubar") => return Err(unsupported("Windows has no menu bar surface. Use --surface desktop to read the taskbar and every window.")),
                Some(other) => return Err(invalid(format!("--surface must be app, frontmost-app or desktop, got {other:?}."))),
            };
            if surface != Surface::App && !pos.is_empty() && !is_url(&pos[0]) {
                return Err(invalid(format!("open --surface {} doesn't take an app.", surface.as_str())));
            }
            Ok(Action::Open { target: (!pos.is_empty()).then(|| pos.join(" ")), surface })
        }
        "close" => {
            check_flags(&p, command, &["--save-script", "--shutdown"])?;
            if p.has("--save-script") {
                return Err(unsupported("--save-script isn't available on Windows: replay scripts need agent-device, which doesn't run on Windows."));
            }
            Ok(Action::Close { app: (!pos.is_empty()).then(|| pos.join(" ")) })
        }
        "apps" => {
            check_flags(&p, command, &["--all"])?;
            Ok(Action::Apps { all: p.has("--all") })
        }
        "appstate" => {
            check_flags(&p, command, &[])?;
            Ok(Action::AppState)
        }
        other => Err(unsupported(format!("{other} isn't available on Windows."))),
    }
}

/// One notch of a mouse wheel (`WHEEL_DELTA`).
pub const WHEEL_NOTCH: i32 = 120;

/// Wheel movement for a scroll: `(vertical, horizontal)` in wheel units. Positive vertical scrolls
/// up, positive horizontal scrolls right. A fraction is of a page (about ten notches); pixels are
/// about 40 per notch; top and bottom scroll far enough to reach the end.
pub fn wheel_delta(direction: Direction, fraction: f64, pixels: Option<i32>) -> (i32, i32) {
    let notches = match (direction, pixels) {
        (Direction::Top | Direction::Bottom, _) => 100.0,
        (_, Some(px)) => (px as f64 / 40.0).max(1.0),
        (_, None) => (fraction * 10.0).round().max(1.0),
    };
    let amount = (notches * WHEEL_NOTCH as f64).round() as i32;
    match direction {
        Direction::Up | Direction::Top => (amount, 0),
        Direction::Down | Direction::Bottom => (-amount, 0),
        Direction::Left => (0, -amount),
        Direction::Right => (0, amount),
    }
}

/// A link rather than an app name.
pub fn is_url(s: &str) -> bool {
    let s = s.trim();
    if let Some((scheme, rest)) = s.split_once(':') {
        let scheme_ok = !scheme.is_empty()
            && scheme.len() > 1 // `C:\…` is a drive, not a scheme
            && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        return scheme_ok && (rest.starts_with("//") || matches!(scheme.to_ascii_lowercase().as_str(), "mailto" | "tel" | "ms-settings"));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn snapshot_flags() {
        assert_eq!(
            parse("snapshot", &s(&["-i", "-d", "3", "-s", "Save"])).unwrap(),
            Action::Snapshot(SnapshotOptions { interactive: true, depth: Some(3), scope: Some("Save".into()), raw: false })
        );
        assert_eq!(parse("snapshot", &s(&["--raw"])).unwrap(), Action::Snapshot(SnapshotOptions { raw: true, ..Default::default() }));
        assert_eq!(parse("snapshot", &s(&["-d", "x"])).unwrap_err().code, "invalid_args");
        assert_eq!(parse("snapshot", &s(&["--diff"])).unwrap_err().code, "unsupported_on_device");
        assert_eq!(parse("snapshot", &s(&["--bogus"])).unwrap_err().code, "invalid_args");
        assert!(parse("snapshot", &s(&["--settle"])).is_ok(), "harmless agent-device flags are ignored");
    }

    #[test]
    fn targets() {
        assert_eq!(parse_target(&s(&["@e3"])).unwrap(), Target::Ref("@e3".into()));
        assert_eq!(parse_target(&s(&["100", "200.4"])).unwrap(), Target::Point(100, 200));
        match parse_target(&s(&["role=button", "label=Save"])).unwrap() {
            Target::Selector(sel) => assert_eq!(sel.source, "role=button label=Save"),
            other => panic!("{other:?}"),
        }
        assert_eq!(parse_target(&[]).unwrap_err().code, "invalid_args");
    }

    #[test]
    fn click_and_friends() {
        assert_eq!(
            parse("click", &s(&["@e2", "--button", "secondary"])).unwrap(),
            Action::Click { target: Target::Ref("@e2".into()), button: Button::Secondary, count: 1, interval_ms: 80 }
        );
        assert_eq!(
            parse("press", &s(&["10", "20", "--count", "2"])).unwrap(),
            Action::Click { target: Target::Point(10, 20), button: Button::Primary, count: 2, interval_ms: 80 }
        );
        assert_eq!(parse("click", &s(&["@e2", "--count", "0"])).unwrap_err().code, "invalid_args");
        assert_eq!(parse("click", &s(&["@e2", "--button", "thumb"])).unwrap_err().code, "invalid_args");
        assert_eq!(parse("hover", &s(&["@e1"])).unwrap(), Action::Hover(Target::Ref("@e1".into())));
        assert_eq!(parse("focus", &s(&["@e1"])).unwrap(), Action::Focus(Target::Ref("@e1".into())));
    }

    #[test]
    fn text_commands() {
        assert_eq!(
            parse("fill", &s(&["@e3", "hello", "world"])).unwrap(),
            Action::Fill { target: Target::Ref("@e3".into()), text: "hello world".into() }
        );
        assert_eq!(parse("fill", &s(&["@e3"])).unwrap_err().code, "invalid_args");
        assert_eq!(parse("type", &s(&["hi there"])).unwrap(), Action::Type("hi there".into()));
        assert_eq!(parse("type", &[]).unwrap_err().code, "invalid_args");
        assert_eq!(parse("clipboard", &s(&["read"])).unwrap(), Action::ClipboardRead);
        assert_eq!(parse("clipboard", &s(&["write", "a", "b"])).unwrap(), Action::ClipboardWrite("a b".into()));
        assert_eq!(parse("clipboard", &s(&["write"])).unwrap(), Action::ClipboardWrite(String::new()));
        assert_eq!(parse("clipboard", &s(&["peek"])).unwrap_err().code, "invalid_args");
    }

    #[test]
    fn queries() {
        assert_eq!(parse("get", &s(&["text", "@e1"])).unwrap(), Action::GetText(Target::Ref("@e1".into())));
        assert!(matches!(parse("get", &s(&["attrs", "label=OK"])).unwrap(), Action::GetAttrs(Target::Selector(_))));
        assert_eq!(parse("get", &s(&["size", "@e1"])).unwrap_err().code, "invalid_args");
        assert_eq!(
            parse("find", &s(&["Save", "click"])).unwrap(),
            Action::Find { locator: Locator::Any, query: "Save".into(), action: FindAction::Click }
        );
        assert_eq!(
            parse("find", &s(&["label", "Email", "fill", "a@b.c"])).unwrap(),
            Action::Find { locator: Locator::Label, query: "Email".into(), action: FindAction::Fill("a@b.c".into()) }
        );
        assert_eq!(
            parse("find", &s(&["Save"])).unwrap(),
            Action::Find { locator: Locator::Any, query: "Save".into(), action: FindAction::List }
        );
        assert_eq!(
            parse("find", &s(&["text", "OK", "wait", "500"])).unwrap(),
            Action::Find { locator: Locator::Text, query: "OK".into(), action: FindAction::Wait(500) }
        );
        assert_eq!(parse("find", &s(&["x", "fly"])).unwrap_err().code, "invalid_args");
        match parse("is", &s(&["visible", "label=OK"])).unwrap() {
            Action::Is { predicate: Predicate::Visible, value: None, .. } => {}
            other => panic!("{other:?}"),
        }
        match parse("is", &s(&["text", "@e2", "Hello", "there"])).unwrap() {
            Action::Is { predicate: Predicate::Text, value: Some(v), target: Target::Ref(r) } => {
                assert_eq!(v, "Hello there");
                assert_eq!(r, "@e2");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(parse("is", &s(&["shiny", "x"])).unwrap_err().code, "invalid_args");
    }

    #[test]
    fn waits() {
        assert_eq!(parse("wait", &s(&["1500"])).unwrap(), Action::Wait(Wait::Ms(1500)));
        assert_eq!(parse("wait", &s(&["text", "Welcome", "2000"])).unwrap(), Action::Wait(Wait::Text("Welcome".into(), 2000)));
        assert_eq!(parse("wait", &s(&["text", "Welcome"])).unwrap(), Action::Wait(Wait::Text("Welcome".into(), DEFAULT_WAIT_MS)));
        assert_eq!(parse("wait", &s(&["@e12"])).unwrap(), Action::Wait(Wait::Present(Target::Ref("@e12".into()), DEFAULT_WAIT_MS)));
        assert!(matches!(parse("wait", &s(&["role=button label=Go", "5000"])).unwrap(), Action::Wait(Wait::Present(Target::Selector(_), 5000))));
        assert!(matches!(parse("wait", &s(&["absent", "label=Loading", "300"])).unwrap(), Action::Wait(Wait::Absent(_, 300))));
        assert_eq!(parse("wait", &s(&["@e1", "--timeout", "700"])).unwrap(), Action::Wait(Wait::Present(Target::Ref("@e1".into()), 700)));
        assert_eq!(parse("wait", &[]).unwrap_err().code, "invalid_args");
    }

    #[test]
    fn screenshots() {
        assert_eq!(parse("screenshot", &[]).unwrap(), Action::Screenshot { name: "screenshot.png".into(), scale: None, fullscreen: false });
        assert_eq!(
            parse("screenshot", &s(&["../../x/page", "--scale", "0.5", "--fullscreen"])).unwrap(),
            Action::Screenshot { name: "page.png".into(), scale: Some(0.5), fullscreen: true }
        );
        assert_eq!(parse("screenshot", &s(&["--scale", "2"])).unwrap_err().code, "invalid_args");
        assert_eq!(parse("screenshot", &s(&["--overlay-refs"])).unwrap_err().code, "unsupported_on_device");
    }

    #[test]
    fn apps_and_surfaces() {
        assert_eq!(parse("open", &s(&["Notepad"])).unwrap(), Action::Open { target: Some("Notepad".into()), surface: Surface::App });
        assert_eq!(parse("open", &s(&["Visual", "Studio", "Code"])).unwrap(), Action::Open { target: Some("Visual Studio Code".into()), surface: Surface::App });
        assert_eq!(parse("open", &s(&["--surface", "desktop"])).unwrap(), Action::Open { target: None, surface: Surface::Desktop });
        assert_eq!(parse("open", &[]).unwrap(), Action::Open { target: None, surface: Surface::FrontmostApp });
        assert_eq!(parse("open", &s(&["--surface", "menubar"])).unwrap_err().code, "unsupported_on_device");
        assert_eq!(parse("open", &s(&["Notepad", "--surface", "desktop"])).unwrap_err().code, "invalid_args");
        assert_eq!(parse("close", &[]).unwrap(), Action::Close { app: None });
        assert_eq!(parse("close", &s(&["--save-script"])).unwrap_err().code, "unsupported_on_device");
        assert_eq!(parse("apps", &s(&["--all"])).unwrap(), Action::Apps { all: true });
        assert_eq!(parse("appstate", &[]).unwrap(), Action::AppState);
        assert_eq!(
            parse("scroll", &s(&["down", "0.3", "--pixels", "200"])).unwrap(),
            Action::Scroll { direction: Direction::Down, fraction: 0.3, pixels: Some(200) }
        );
        assert_eq!(parse("scroll", &s(&["sideways"])).unwrap_err().code, "invalid_args");
    }

    #[test]
    fn unsupported_commands_explain_themselves() {
        for c in ["record", "logs", "alert", "replay", "test", "batch", "diff", "longpress", "swipe", "back", "home", "tv-remote", "keyboard", "install", "adb"] {
            let e = parse(c, &[]).unwrap_err();
            assert_eq!(e.code, "unsupported_on_device", "{c}");
            assert!(e.message.len() > 20, "{c}");
        }
        assert_eq!(parse("warp", &[]).unwrap_err().code, "unsupported_on_device");
    }

    #[test]
    fn wheel_amounts() {
        assert_eq!(wheel_delta(Direction::Down, 0.5, None), (-600, 0));
        assert_eq!(wheel_delta(Direction::Up, 0.01, None), (120, 0));
        assert_eq!(wheel_delta(Direction::Right, 0.5, Some(200)), (0, 600));
        assert_eq!(wheel_delta(Direction::Left, 0.5, Some(10)), (0, -120));
        assert_eq!(wheel_delta(Direction::Bottom, 0.5, None), (-12000, 0));
    }

    #[test]
    fn urls() {
        assert!(is_url("https://example.com"));
        assert!(is_url("mailto:a@b.c"));
        assert!(is_url("ms-settings:display"));
        assert!(!is_url("C:\\Windows\\notepad.exe"));
        assert!(!is_url("Notepad"));
    }
}

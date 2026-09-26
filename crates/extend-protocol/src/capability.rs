//! What each kind of device can do, and which commands each capability unlocks
//! (TECHNICAL.md section 5). The service enforces this table and the CLI filters `--help` with it,
//! so the two can't disagree.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Capability {
    #[serde(rename = "screen.read")]
    ScreenRead,
    #[serde(rename = "screen.capture")]
    ScreenCapture,
    #[serde(rename = "screen.record")]
    ScreenRecord,
    #[serde(rename = "input.touch")]
    InputTouch,
    #[serde(rename = "input.pointer")]
    InputPointer,
    #[serde(rename = "input.text")]
    InputText,
    #[serde(rename = "input.keyboard")]
    InputKeyboard,
    #[serde(rename = "input.remote")]
    InputRemote,
    #[serde(rename = "nav.system")]
    NavSystem,
    #[serde(rename = "apps.launch")]
    AppsLaunch,
    #[serde(rename = "apps.list")]
    AppsList,
    #[serde(rename = "apps.install")]
    AppsInstall,
    #[serde(rename = "alerts")]
    Alerts,
    #[serde(rename = "clipboard")]
    Clipboard,
    #[serde(rename = "logs")]
    Logs,
    #[serde(rename = "replay")]
    Replay,
    #[serde(rename = "takeover")]
    Takeover,
    #[serde(rename = "notifications")]
    Notifications,
    #[serde(rename = "adb")]
    Adb,
    #[serde(rename = "terminal")]
    Terminal,
    #[serde(rename = "display")]
    Display,
    #[serde(rename = "links")]
    Links,
}

impl Capability {
    pub const ALL: [Capability; 22] = [
        Self::ScreenRead,
        Self::ScreenCapture,
        Self::ScreenRecord,
        Self::InputTouch,
        Self::InputPointer,
        Self::InputText,
        Self::InputKeyboard,
        Self::InputRemote,
        Self::NavSystem,
        Self::AppsLaunch,
        Self::AppsList,
        Self::AppsInstall,
        Self::Alerts,
        Self::Clipboard,
        Self::Logs,
        Self::Replay,
        Self::Takeover,
        Self::Notifications,
        Self::Adb,
        Self::Terminal,
        Self::Display,
        Self::Links,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ScreenRead => "screen.read",
            Self::ScreenCapture => "screen.capture",
            Self::ScreenRecord => "screen.record",
            Self::InputTouch => "input.touch",
            Self::InputPointer => "input.pointer",
            Self::InputText => "input.text",
            Self::InputKeyboard => "input.keyboard",
            Self::InputRemote => "input.remote",
            Self::NavSystem => "nav.system",
            Self::AppsLaunch => "apps.launch",
            Self::AppsList => "apps.list",
            Self::AppsInstall => "apps.install",
            Self::Alerts => "alerts",
            Self::Clipboard => "clipboard",
            Self::Logs => "logs",
            Self::Replay => "replay",
            Self::Takeover => "takeover",
            Self::Notifications => "notifications",
            Self::Adb => "adb",
            Self::Terminal => "terminal",
            Self::Display => "display",
            Self::Links => "links",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceOs {
    /// Android phones and tablets.
    Android,
    /// Android TV, Google TV and Fire OS.
    AndroidTv,
    Macos,
    Windows,
    Linux,
    /// iPhone, through a paired Mac.
    Ios,
    /// iPad, through a paired Mac.
    Ipados,
    /// Apple TV, through a paired Mac.
    Tvos,
    /// Samsung (Tizen) TV, through a paired computer.
    SamsungTv,
    /// LG (webOS) TV, through a paired computer.
    LgTv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Phone,
    Tablet,
    Tv,
    Computer,
}

impl DeviceOs {
    pub fn kind(self) -> DeviceKind {
        match self {
            Self::Android | Self::Ios => DeviceKind::Phone,
            Self::Ipados => DeviceKind::Tablet,
            Self::AndroidTv | Self::Tvos | Self::SamsungTv | Self::LgTv => DeviceKind::Tv,
            Self::Macos | Self::Windows | Self::Linux => DeviceKind::Computer,
        }
    }

    /// Devices that can't run the Extend app and pair through a host computer.
    pub fn needs_host(self) -> bool {
        matches!(
            self,
            Self::Ios | Self::Ipados | Self::Tvos | Self::SamsungTv | Self::LgTv
        )
    }

    /// Which host operating systems can carry this device.
    pub fn allowed_hosts(self) -> &'static [DeviceOs] {
        match self {
            Self::Ios | Self::Ipados | Self::Tvos => &[DeviceOs::Macos],
            Self::SamsungTv | Self::LgTv => &[DeviceOs::Macos, DeviceOs::Windows, DeviceOs::Linux],
            _ => &[],
        }
    }

    /// Everything this kind of device can do when fully set up.
    pub fn full_capabilities(self) -> &'static [Capability] {
        use Capability::*;
        match self {
            Self::Android => &[
                ScreenRead,
                ScreenCapture,
                ScreenRecord,
                InputTouch,
                InputText,
                InputKeyboard,
                NavSystem,
                AppsLaunch,
                AppsList,
                AppsInstall,
                Alerts,
                Clipboard,
                Logs,
                Replay,
                Takeover,
                Notifications,
                Adb,
                Links,
            ],
            Self::AndroidTv => &[
                ScreenRead,
                ScreenCapture,
                InputText,
                InputRemote,
                NavSystem,
                AppsLaunch,
                AppsList,
                AppsInstall,
                Alerts,
                Logs,
                Replay,
                Takeover,
                Adb,
                Display,
                Links,
            ],
            Self::Macos | Self::Windows => &[
                ScreenRead,
                ScreenCapture,
                ScreenRecord,
                InputPointer,
                InputText,
                AppsLaunch,
                AppsList,
                Alerts,
                Clipboard,
                Logs,
                Replay,
                Takeover,
                Terminal,
                Links,
            ],
            Self::Linux => &[
                ScreenRead,
                ScreenCapture,
                ScreenRecord,
                InputPointer,
                InputText,
                AppsLaunch,
                AppsList,
                Clipboard,
                Logs,
                Replay,
                Takeover,
                Terminal,
                Links,
            ],
            Self::Ios | Self::Ipados => &[
                ScreenRead,
                ScreenCapture,
                ScreenRecord,
                InputTouch,
                InputText,
                InputKeyboard,
                NavSystem,
                AppsLaunch,
                AppsList,
                Alerts,
                Replay,
                Takeover,
                Links,
            ],
            Self::Tvos => &[InputRemote, NavSystem, AppsLaunch, AppsList, Replay, Takeover, Display],
            Self::SamsungTv | Self::LgTv => &[InputRemote, NavSystem, AppsLaunch, AppsList, Replay, Takeover, Links],
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Android => "android",
            Self::AndroidTv => "android_tv",
            Self::Macos => "macos",
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Ios => "ios",
            Self::Ipados => "ipados",
            Self::Tvos => "tvos",
            Self::SamsungTv => "samsung_tv",
            Self::LgTv => "lg_tv",
        }
    }
}

/// One command a Silicon can run inside a session.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    /// Top-level name, as typed after `extend` (`record`, `tv-remote`).
    pub name: &'static str,
    /// The command runs if the device has any one of these.
    pub any_of: &'static [Capability],
    /// Where the command comes from.
    pub origin: Origin,
    pub usage: &'static str,
    pub summary: &'static str,
    /// Typed text in these commands is redacted in the activity log.
    pub redact_text: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    AgentDevice,
    Extend,
}

use Capability as C;

macro_rules! cmd {
    ($name:literal, [$($cap:ident),+], $origin:ident, $usage:literal, $summary:literal) => {
        CommandSpec { name: $name, any_of: &[$(C::$cap),+], origin: Origin::$origin, usage: $usage, summary: $summary, redact_text: false }
    };
    ($name:literal, [$($cap:ident),+], $origin:ident, $usage:literal, $summary:literal, redact) => {
        CommandSpec { name: $name, any_of: &[$(C::$cap),+], origin: Origin::$origin, usage: $usage, summary: $summary, redact_text: true }
    };
}

/// Every command Extend relays to a device.
pub const COMMANDS: &[CommandSpec] = &[
    cmd!(
        "snapshot",
        [ScreenRead],
        AgentDevice,
        "snapshot [-i] [--diff] [-d <depth>] [-s <scope>] [--raw] [--actions]",
        "Read what is on screen as a tree of elements with @refs"
    ),
    cmd!(
        "diff",
        [ScreenRead, ScreenCapture],
        AgentDevice,
        "diff snapshot | diff screenshot --baseline <file>",
        "Compare with a previous snapshot or screenshot"
    ),
    cmd!(
        "get",
        [ScreenRead],
        AgentDevice,
        "get text|attrs <@ref|selector>",
        "Read an element's text or attributes"
    ),
    cmd!(
        "find",
        [ScreenRead],
        AgentDevice,
        "find <text> [click|fill <text>|list]",
        "Find an element by meaning and act on it"
    ),
    cmd!(
        "is",
        [ScreenRead],
        AgentDevice,
        "is visible|hidden|exists|absent|editable|selected|focused|text <selector> [value]",
        "Check the screen; exits 1 when false"
    ),
    cmd!(
        "wait",
        [ScreenRead],
        AgentDevice,
        "wait <ms> | wait text <text> [ms] | wait <selector> [ms] | wait absent <selector> [ms]",
        "Wait for something to appear or go away"
    ),
    cmd!(
        "screenshot",
        [ScreenCapture],
        AgentDevice,
        "screenshot [name] [--scale <0.01-1>] [--overlay-refs]",
        "Take a screenshot; stored in Briefcase"
    ),
    cmd!(
        "record",
        [ScreenRecord],
        AgentDevice,
        "record start [name] | record stop",
        "Record the screen; stored in Briefcase on stop"
    ),
    cmd!(
        "click",
        [InputPointer, InputTouch],
        AgentDevice,
        "click <@ref|selector> [--button primary|secondary]",
        "Click or tap an element"
    ),
    cmd!(
        "press",
        [InputTouch, InputPointer],
        AgentDevice,
        "press <x> <y> | press <@ref|selector>",
        "Tap a point or an element"
    ),
    cmd!(
        "longpress",
        [InputTouch],
        AgentDevice,
        "longpress <x> <y> [ms]",
        "Press and hold"
    ),
    cmd!(
        "fill",
        [InputText],
        AgentDevice,
        "fill <@ref|selector> <text>",
        "Clear a field and type into it",
        redact
    ),
    cmd!(
        "type",
        [InputText],
        AgentDevice,
        "type <text>",
        "Type into the focused field",
        redact
    ),
    cmd!(
        "focus",
        [InputText],
        AgentDevice,
        "focus <@ref|selector>",
        "Focus a field"
    ),
    cmd!(
        "scroll",
        [InputTouch, InputPointer],
        AgentDevice,
        "scroll up|down|left|right [fraction] [--pixels <n>]",
        "Scroll"
    ),
    cmd!(
        "swipe",
        [InputTouch],
        AgentDevice,
        "swipe <x1> <y1> <x2> <y2>",
        "Swipe between two points"
    ),
    cmd!(
        "gesture",
        [InputTouch],
        AgentDevice,
        "gesture pan|fling|drag|pinch|rotate|transform ...",
        "Multi-touch gestures"
    ),
    cmd!(
        "hover",
        [InputPointer],
        AgentDevice,
        "hover <@ref|selector>",
        "Move the pointer over an element"
    ),
    cmd!("back", [NavSystem], AgentDevice, "back", "Press back"),
    cmd!("home", [NavSystem], AgentDevice, "home", "Go to the home screen"),
    cmd!(
        "app-switcher",
        [NavSystem],
        AgentDevice,
        "app-switcher",
        "Show recent apps"
    ),
    cmd!(
        "tv-remote",
        [InputRemote],
        AgentDevice,
        "tv-remote press|longpress <button>",
        "Press a TV remote button"
    ),
    cmd!(
        "keyboard",
        [InputKeyboard],
        AgentDevice,
        "keyboard status|dismiss",
        "Check or hide the on-screen keyboard"
    ),
    cmd!(
        "clipboard",
        [Clipboard],
        AgentDevice,
        "clipboard read | clipboard write <text>",
        "Read or write the clipboard",
        redact
    ),
    cmd!(
        "open",
        [AppsLaunch, Links],
        AgentDevice,
        "open <app|url> [url] [--surface app|frontmost-app|desktop|menubar]",
        "Open an app or a link"
    ),
    cmd!(
        "close",
        [AppsLaunch],
        AgentDevice,
        "close [app] [--save-script [path]]",
        "Close the app"
    ),
    cmd!("apps", [AppsList], AgentDevice, "apps [--all]", "List installed apps"),
    cmd!(
        "appstate",
        [AppsLaunch],
        AgentDevice,
        "appstate",
        "Which app is in front"
    ),
    cmd!(
        "install",
        [AppsInstall],
        AgentDevice,
        "install <app> <file_id|path>",
        "Install an app"
    ),
    cmd!(
        "reinstall",
        [AppsInstall],
        AgentDevice,
        "reinstall <app> <file_id|path>",
        "Reinstall an app with fresh data"
    ),
    cmd!(
        "alert",
        [Alerts],
        AgentDevice,
        "alert [get|wait <ms>|accept|dismiss]",
        "Handle a system pop-up"
    ),
    cmd!(
        "logs",
        [Logs],
        AgentDevice,
        "logs start|stop|mark <label>|clear",
        "Capture device logs; stored in Briefcase on stop"
    ),
    cmd!(
        "replay",
        [Replay],
        AgentDevice,
        "replay <script.ad>",
        "Run saved steps again"
    ),
    cmd!(
        "test",
        [Replay],
        AgentDevice,
        "test <script.ad>...",
        "Run several saved scripts in order"
    ),
    cmd!(
        "batch",
        [Replay],
        AgentDevice,
        "batch --steps '<json>'",
        "Run several commands in one request"
    ),
    cmd!(
        "terminal",
        [Terminal],
        Extend,
        "terminal run <command> [--cwd <dir>]",
        "Run a shell command on the computer"
    ),
    cmd!(
        "adb",
        [Adb],
        Extend,
        "adb <args>...",
        "Android debugging on the device itself"
    ),
    cmd!(
        "notifications",
        [Notifications],
        Extend,
        "notifications",
        "Read the device's notifications"
    ),
    cmd!(
        "display",
        [Display],
        Extend,
        "display show --url|--image|--video|--text <value> | display clear",
        "Show something full screen on the TV"
    ),
];

/// agent-device commands Extend does not relay, with the Extend replacement where there is one.
pub const NOT_EXPOSED: &[(&str, Option<&str>)] = &[
    ("devices", Some("extend device ls")),
    ("connect", Some("extend session connect <session_id>")),
    ("disconnect", Some("extend session disconnect")),
    ("session", Some("extend session ls")),
    ("takeover", Some("extend takeover --reason \"...\"")),
    ("help", Some("extend --help")),
    ("boot", None),
    ("shutdown", None),
    ("web", None),
    ("viewport", None),
    ("react-native", None),
    ("react-devtools", None),
    ("metro", None),
    ("cdp", None),
    ("perf", None),
    ("trace", None),
    ("network", None),
    ("debug", None),
    ("audio", None),
    ("push", None),
    ("trigger-app-event", None),
    ("install-from-source", None),
    ("settings", None),
    ("fold", None),
    ("orientation", None),
    ("action-button", None),
    ("prepare", None),
    ("mcp", None),
    ("doctor", None),
    ("daemon", None),
    ("proxy", None),
    ("auth", None),
    ("runtime", None),
    ("events", None),
    ("artifacts", None),
    ("capabilities", None),
];

/// Flags that choose a device or session inside agent-device. Extend chooses those, so they're refused.
pub const RESERVED_FLAGS: &[&str] = &[
    "--platform",
    "--device",
    "--udid",
    "--serial",
    "--target",
    "--session",
    "--remote",
    "--daemon",
    "--state-dir",
];

pub fn command(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|c| c.name == name)
}

pub fn not_exposed(name: &str) -> Option<Option<&'static str>> {
    NOT_EXPOSED.iter().find(|(n, _)| *n == name).map(|(_, r)| *r)
}

/// Commands allowed by a capability set, in catalogue order.
pub fn commands_for(caps: &[Capability]) -> Vec<&'static str> {
    COMMANDS
        .iter()
        .filter(|c| c.any_of.iter().any(|cap| caps.contains(cap)))
        .map(|c| c.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_reachable_on_some_device() {
        let all = [
            DeviceOs::Android,
            DeviceOs::AndroidTv,
            DeviceOs::Macos,
            DeviceOs::Windows,
            DeviceOs::Linux,
            DeviceOs::Ios,
            DeviceOs::Ipados,
            DeviceOs::Tvos,
            DeviceOs::SamsungTv,
            DeviceOs::LgTv,
        ];
        for c in COMMANDS {
            assert!(
                all.iter()
                    .any(|os| c.any_of.iter().any(|cap| os.full_capabilities().contains(cap))),
                "{} unreachable",
                c.name
            );
        }
    }

    #[test]
    fn tv_help_shows_remote_not_terminal() {
        let tv = commands_for(DeviceOs::AndroidTv.full_capabilities());
        assert!(tv.contains(&"tv-remote"));
        assert!(!tv.contains(&"terminal"));
        let mac = commands_for(DeviceOs::Macos.full_capabilities());
        assert!(mac.contains(&"terminal"));
        assert!(!mac.contains(&"tv-remote"));
        let phone = commands_for(DeviceOs::Android.full_capabilities());
        assert!(phone.contains(&"adb"));
    }

    #[test]
    fn serde_names() {
        assert_eq!(
            serde_json::to_string(&Capability::ScreenRead).unwrap(),
            "\"screen.read\""
        );
        assert_eq!(serde_json::to_string(&DeviceOs::AndroidTv).unwrap(), "\"android_tv\"");
        for c in Capability::ALL {
            assert_eq!(serde_json::to_value(c).unwrap(), c.as_str());
        }
    }
}

/**
 * Everything the website needs to know that is not fetched from Extend: where the API is,
 * where the Extend apps download from, and how each kind of device is added.
 *
 * Download links are placeholders under `/download/<platform>` until the apps are published.
 * Change them here and nowhere else.
 */
import type { DeviceOs } from "./lib/types";

const PRODUCTION_API = "https://backend.extend.teamofsilicons.com";

/** Extend API base URL without a trailing slash; "" means same origin. */
export function apiBaseUrl(): string {
  const raw =
    import.meta.env.VITE_EXTEND_API_URL ??
    (import.meta.env.DEV ? "same-origin" : PRODUCTION_API);
  if (!raw || raw === "same-origin" || raw === "/") return "";
  return raw.replace(/\/+$/, "");
}

/** Used only when `GET /api/v1/iam` gives nothing better; see `iamLoginUrl` in lib/auth.ts. */
export const IAM_LOGIN_URL_OVERRIDE = import.meta.env.VITE_IAM_LOGIN_URL || "";

export const LINKS = {
  repository: "https://github.com/teamofsilicons/silicon-extend",
  crate: "https://crates.io/crates/silicon-extend-client",
  docs: "https://docs.extend.teamofsilicons.com",
  website: "https://extend.teamofsilicons.com",
  api: PRODUCTION_API,
  install: "honeycomb install 'extend'",
};

export type Platform =
  | "android"
  | "android-tv"
  | "mac"
  | "windows"
  | "linux";

export interface Download {
  platform: Platform;
  app: string;
  href: string;
  note: string;
}

export const DOWNLOADS: Record<Platform, Download> = {
  android: {
    platform: "android",
    app: "Silicon Extend for Android",
    href: "/download/android",
    note: "Install the .apk on the phone or tablet. Android asks once to allow installs from your browser.",
  },
  "android-tv": {
    platform: "android-tv",
    app: "Silicon Extend TV",
    href: "/download/android-tv",
    note: "For Android TV, Google TV and Fire TV. Install it on the TV itself.",
  },
  mac: {
    platform: "mac",
    app: "Silicon Extend for Mac",
    href: "/download/mac",
    note: "A menu bar app for macOS. Open the .dmg and drag it to Applications.",
  },
  windows: {
    platform: "windows",
    app: "Silicon Extend for Windows",
    href: "/download/windows",
    note: "A system tray app. Run the installer and allow it when Windows asks.",
  },
  linux: {
    platform: "linux",
    app: "Silicon Extend for Linux",
    href: "/download/linux",
    note: "Packages for common distributions.",
  },
};

export type DeviceKindId =
  | "android"
  | "android_tv"
  | "mac"
  | "windows"
  | "linux"
  | "iphone"
  | "ipad"
  | "apple_tv"
  | "samsung_tv"
  | "lg_tv";

export interface DeviceKind {
  id: DeviceKindId;
  label: string;
  short: string;
  os: DeviceOs;
  /** `app`: runs an Extend app and shows a pairing code. `host`: paired through a computer. */
  via: "app" | "host";
  /** For `host` kinds: which paired devices can be the host. */
  host?: "mac" | "computer";
  download?: Platform;
  /** What the Carbon does before entering the code, or before picking the host. */
  guide: string[];
  /** The device's own setup, shown as a preview; live status comes from the setup endpoint. */
  setup: string[];
  canDo: string;
  goodToKnow?: string;
  icon: "phone" | "tv" | "laptop" | "monitor" | "tablet";
}

export const DEVICE_KINDS: DeviceKind[] = [
  {
    id: "android",
    label: "Android phone or tablet",
    short: "Android",
    os: "android",
    via: "app",
    download: "android",
    icon: "phone",
    guide: [
      "Download Silicon Extend for Android on the phone or tablet, and install it.",
      "Open the app. It shows a pairing code and never asks you to log in.",
      "Enter that code below. It changes every 5 minutes, so use the one on screen now.",
    ],
    setup: [
      "Turn on Developer options. The app shows exactly where.",
      "Turn on wireless debugging, and let the app pair with it.",
      "Allow the app to show notifications and to stay running in the background.",
    ],
    canDo:
      "Open any app, see the screen, tap, type, scroll, swipe, press back, home and recent apps, read notifications, install apps, take screenshots and recordings, read device logs, and use Android debugging.",
    goodToKnow:
      "The device needs to be on Wi-Fi (any network). After it restarts, wireless debugging turns off and the app asks you to turn it back on.",
  },
  {
    id: "android_tv",
    label: "Android TV, Google TV or Fire TV",
    short: "Android TV",
    os: "android_tv",
    via: "app",
    download: "android-tv",
    icon: "tv",
    guide: [
      "Install Silicon Extend TV on the TV.",
      "Open it. The pairing code is shown large on the TV.",
      "Enter that code below.",
    ],
    setup: [
      "Turn on Developer options and network debugging. The app shows exactly where.",
      "Approve the “Allow debugging” prompt on the TV.",
    ],
    canDo:
      "Open any app, press any remote button, see the screen, install apps, take screenshots, read device logs, use Android debugging, and show a link, image, video or text full screen.",
    goodToKnow:
      "A small badge in a corner of the TV shows which Silicon is using it. Stop it from the TV app or from here.",
  },
  {
    id: "mac",
    label: "Mac",
    short: "Mac",
    os: "macos",
    via: "app",
    download: "mac",
    icon: "laptop",
    guide: [
      "Download Silicon Extend for Mac and move it to Applications.",
      "Open it from the menu bar. It shows a pairing code.",
      "Enter that code below.",
    ],
    setup: [
      "Allow Accessibility when asked. The app opens the right settings page.",
      "Allow Screen Recording when asked.",
    ],
    canDo:
      "Open any app, see and use any window and the menu bar, click, type, scroll, use the clipboard, take screenshots and recordings, and use the terminal.",
    goodToKnow:
      "A Mac can't be used while it is locked or asleep. The Silicon uses the real mouse and keyboard. A paired Mac is also what iPhones, iPads and Apple TVs pair through.",
  },
  {
    id: "windows",
    label: "Windows computer",
    short: "Windows",
    os: "windows",
    via: "app",
    download: "windows",
    icon: "monitor",
    guide: [
      "Download Silicon Extend for Windows and run the installer.",
      "Open it from the system tray. It shows a pairing code.",
      "Enter that code below.",
    ],
    setup: ["Allow it when Windows asks."],
    canDo:
      "Open any app, see and use any window, click, type, scroll, use the clipboard, take screenshots and recordings, and use the terminal.",
    goodToKnow:
      "A Windows computer can't be used while it is locked. Admin prompts always need you.",
  },
  {
    id: "linux",
    label: "Linux computer",
    short: "Linux",
    os: "linux",
    via: "app",
    download: "linux",
    icon: "monitor",
    guide: [
      "Download Silicon Extend for Linux and install it.",
      "Open it. It shows a pairing code.",
      "Enter that code below.",
    ],
    setup: [
      "On newer desktops, approve screen sharing and remote control once when asked.",
    ],
    canDo:
      "Open any app, see and use any window, click, type, scroll, take screenshots and recordings, and use the terminal.",
    goodToKnow:
      "A Linux computer can't be used while it is locked. A computer without a screen, like a server, only gets the terminal.",
  },
  {
    id: "iphone",
    label: "iPhone",
    short: "iPhone",
    os: "ios",
    via: "host",
    host: "mac",
    icon: "phone",
    guide: [
      "There is no app to install on the iPhone. Extend puts a small helper on it through your paired Mac.",
      "Pick the paired Mac the iPhone will connect through. It needs to be online.",
      "Keep the iPhone and a cable nearby for the first setup.",
    ],
    setup: [
      "Plug the iPhone into the Mac once, and tap Trust on the iPhone.",
      "Turn on Developer Mode (Settings, then Privacy & Security). The iPhone restarts.",
      "Extend puts its helper on the iPhone. After that the cable isn't needed while both are on the same Wi-Fi.",
    ],
    canDo:
      "Open any app, see the screen, tap, type, swipe, scroll, and take screenshots and recordings.",
    goodToKnow:
      "The iPhone must be near its Mac (same Wi-Fi or plugged in), awake and unlocked. A Silicon can't approve payments or Face ID.",
  },
  {
    id: "ipad",
    label: "iPad",
    short: "iPad",
    os: "ipados",
    via: "host",
    host: "mac",
    icon: "tablet",
    guide: [
      "There is no app to install on the iPad. Extend puts a small helper on it through your paired Mac.",
      "Pick the paired Mac the iPad will connect through. It needs to be online.",
      "Keep the iPad and a cable nearby for the first setup.",
    ],
    setup: [
      "Plug the iPad into the Mac once, and tap Trust on the iPad.",
      "Turn on Developer Mode (Settings, then Privacy & Security). The iPad restarts.",
      "Extend puts its helper on the iPad. After that the cable isn't needed while both are on the same Wi-Fi.",
    ],
    canDo:
      "Open any app, see the screen, tap, type, swipe, scroll, and take screenshots and recordings.",
    goodToKnow:
      "The iPad must be near its Mac (same Wi-Fi or plugged in), awake and unlocked. A Silicon can't approve payments or Face ID.",
  },
  {
    id: "apple_tv",
    label: "Apple TV",
    short: "Apple TV",
    os: "tvos",
    via: "host",
    host: "mac",
    icon: "tv",
    guide: [
      "Pick a paired Mac on the same network as the Apple TV.",
      "Turn the Apple TV on. During setup it shows a 4-digit code you enter here.",
    ],
    setup: ["Enter the 4-digit code the Apple TV shows."],
    canDo: "Open apps, press remote buttons, and show pictures and videos on the screen.",
    goodToKnow: "The Apple TV must stay on the same network as its Mac.",
  },
  {
    id: "samsung_tv",
    label: "Samsung TV",
    short: "Samsung TV",
    os: "samsung_tv",
    via: "host",
    host: "computer",
    icon: "tv",
    guide: [
      "Pick a paired computer on the same network as the TV.",
      "Turn the TV on. It asks you to allow the connection once.",
    ],
    setup: ["Approve the connection on the TV."],
    canDo: "Open apps, press remote buttons, and open links.",
    goodToKnow:
      "A Silicon can't see what is on this TV's screen. The TV must stay on the same network as its computer.",
  },
  {
    id: "lg_tv",
    label: "LG TV",
    short: "LG TV",
    os: "lg_tv",
    via: "host",
    host: "computer",
    icon: "tv",
    guide: [
      "Pick a paired computer on the same network as the TV.",
      "Turn the TV on. It asks you to accept the connection once.",
    ],
    setup: ["Accept the connection on the TV."],
    canDo: "Open apps, press remote buttons, and open links.",
    goodToKnow:
      "A Silicon can't see what is on this TV's screen. The TV must stay on the same network as its computer.",
  },
];

export function deviceKind(id: string | undefined): DeviceKind | undefined {
  return DEVICE_KINDS.find((k) => k.id === id);
}

export const OS_LABEL: Record<DeviceOs, string> = {
  android: "Android",
  android_tv: "Android TV",
  macos: "macOS",
  windows: "Windows",
  linux: "Linux",
  ios: "iOS",
  ipados: "iPadOS",
  tvos: "tvOS",
  samsung_tv: "Samsung TV",
  lg_tv: "LG TV",
};

/** Devices refresh this often while the tab is visible. */
export const POLL_MS = 5000;
/** Setup steps refresh this often while the wizard waits on the device. */
export const SETUP_POLL_MS = 2000;

/**
 * Everything the website needs to know that is not fetched from Extend: where the API is,
 * where the Extend apps download from, and how each kind of device is added.
 *
 * The Extend apps link to `/download/<platform>` (extend-agent's config.rs does too), and each
 * page offers the files of the latest GitHub release under stable names. A platform whose `files`
 * is null isn't published yet. Change them here and nowhere else.
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
  docs: "https://extend.teamofsilicons.com/docs",
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

/** Assets of the latest GitHub release, by stable name. release.yml builds versioned artifacts; the release
 * procedure copies them to these names and writes SHA256SUMS when it creates the GitHub release. */
export const RELEASES_URL = "https://github.com/teamofsilicons/silicon-extend/releases/latest";
export const releaseAsset = (name: string) => `${RELEASES_URL}/download/${name}`;
export const CHECKSUMS = releaseAsset("SHA256SUMS");

export interface DownloadFile {
  label: string;
  name: string;
}

export interface Download {
  platform: Platform;
  app: string;
  href: string;
  note: string;
  /** null: not published yet. */
  files: DownloadFile[] | null;
}

const ANDROID_APK: DownloadFile[] = [{ label: "Android app (.apk)", name: "Silicon-Extend-android.apk" }];

export const DOWNLOADS: Record<Platform, Download> = {
  android: {
    platform: "android",
    app: "Silicon Extend for Android",
    href: "/download/android",
    note: "Install the .apk on the phone or tablet. Android asks once to allow installs from your browser.",
    files: ANDROID_APK,
  },
  "android-tv": {
    platform: "android-tv",
    app: "Silicon Extend TV",
    href: "/download/android-tv",
    note: "For Android TV, Google TV and Fire TV. The same .apk as for phones: install it on the TV itself.",
    files: ANDROID_APK,
  },
  mac: {
    platform: "mac",
    app: "Silicon Extend for Mac",
    href: "/download/mac",
    note: "A menu bar app for Macs with Apple silicon. Unzip it and move Silicon Extend to Applications.",
    files: [{ label: "Mac, Apple silicon (.zip)", name: "Silicon-Extend-macos-arm64.zip" }],
  },
  windows: {
    platform: "windows",
    app: "Silicon Extend for Windows",
    href: "/download/windows",
    note: "A system tray app. Unzip it and run extend-agent.exe. This is a preview: it isn't signed yet, so Windows SmartScreen warns before it runs.",
    files: [
      { label: "Windows x64 (.zip)", name: "Silicon-Extend-windows-x64.zip" },
      { label: "Windows on Arm (.zip)", name: "Silicon-Extend-windows-arm64.zip" },
    ],
  },
  linux: {
    platform: "linux",
    app: "Silicon Extend for Linux",
    href: "/download/linux",
    note: "A .deb for Ubuntu 24.04, Debian 13 and newer, or a tarball for other distributions with glibc 2.39 or later.",
    files: [
      { label: "Debian/Ubuntu x64 (.deb)", name: "silicon-extend_amd64.deb" },
      { label: "Debian/Ubuntu arm64 (.deb)", name: "silicon-extend_arm64.deb" },
      { label: "Linux x64 (.tar.gz)", name: "silicon-extend-linux-x64.tar.gz" },
      { label: "Linux arm64 (.tar.gz)", name: "silicon-extend-linux-arm64.tar.gz" },
    ],
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
      "Download Silicon Extend for Windows, unzip it and run extend-agent.exe.",
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
      "The iPhone must be near its Mac (same Wi-Fi or plugged in), awake and unlocked. A Silicon can't approve payments or Face ID. The iPhone shows “Automation Running” while Extend sets it up and while a Silicon is working on it (Apple shows that on every automated iPhone). It goes away when the Silicon's session ends, or about a minute after its last action.",
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
      "The iPad must be near its Mac (same Wi-Fi or plugged in), awake and unlocked. A Silicon can't approve payments or Face ID. The iPad shows “Automation Running” while Extend sets it up and while a Silicon is working on it (Apple shows that on every automated iPad). It goes away when the Silicon's session ends, or about a minute after its last action.",
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

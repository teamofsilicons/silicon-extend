export const LINKS = {repository:"https://github.com/teamofsilicons/silicon-extend"};
export type Platform =
  | "android"
  | "android-tv"
  | "mac"
  | "windows"
  | "linux";

export interface DownloadRelease {
  version: string;
  url: string;
  assetsUrl: string;
}

/** Desktop downloads retain the stable names in the latest full app release. Android patches
 * have their own tag so publishing an APK does not replace the desktop release or its checksums. */
const DESKTOP_RELEASE: DownloadRelease = {
  version: "1.1.0",
  url: `${LINKS.repository}/releases/latest`,
  assetsUrl: `${LINKS.repository}/releases/latest/download`,
};
const ANDROID_RELEASE: DownloadRelease = {
  version: "1.1.2",
  url: `${LINKS.repository}/releases/tag/android-v1.1.2`,
  assetsUrl: `${LINKS.repository}/releases/download/android-v1.1.2`,
};
export const releaseAsset = (release: DownloadRelease, name: string) => `${release.assetsUrl}/${name}`;

export interface DownloadFile {
  label: string;
  name: string;
}

export interface Download {
  platform: Platform;
  app: string;
  href: string;
  note: string;
  release: DownloadRelease;
  /** null: not published yet. */
  files: DownloadFile[] | null;
}

const ANDROID_APK: DownloadFile[] = [{ label: "Android app (.apk)", name: "Silicon-Extend-Android-1.1.2.apk" }];

export const DOWNLOADS: Record<Platform, Download> = {
  android: {
    platform: "android",
    release: ANDROID_RELEASE,
    app: "Silicon Extend for Android",
    href: "/download/android",
    note: "Install the .apk on the phone or tablet. Android asks once to allow installs from your browser.",
    files: ANDROID_APK,
  },
  "android-tv": {
    platform: "android-tv",
    release: ANDROID_RELEASE,
    app: "Silicon Extend TV",
    href: "/download/android-tv",
    note: "For Android TV, Google TV and Fire TV. The same .apk as for phones: install it on the TV itself.",
    files: ANDROID_APK,
  },
  mac: {
    platform: "mac",
    release: DESKTOP_RELEASE,
    app: "Silicon Extend for Mac",
    href: "/download/mac",
    note: "A menu bar app for Macs with Apple silicon. Unzip it and move Silicon Extend to Applications.",
    files: [{ label: "Mac, Apple silicon (.zip)", name: "Silicon-Extend-macos-arm64.zip" }],
  },
  windows: {
    platform: "windows",
    release: DESKTOP_RELEASE,
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
    release: DESKTOP_RELEASE,
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


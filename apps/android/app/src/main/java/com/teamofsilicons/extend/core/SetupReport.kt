package com.teamofsilicons.extend.core

import android.Manifest
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import androidx.core.app.NotificationManagerCompat
import com.teamofsilicons.extend.a11y.ExtendAccessibilityService
import com.teamofsilicons.extend.adb.DebuggingAfterRestart
import com.teamofsilicons.extend.adb.DebuggingPath
import com.teamofsilicons.extend.config.Config
import com.teamofsilicons.extend.config.DeviceInfo
import com.teamofsilicons.extend.driver.Capabilities as C
import com.teamofsilicons.extend.driver.ScreenshotPath
import com.teamofsilicons.extend.driver.TvRemoteKeys
import com.teamofsilicons.extend.notif.ExtendNotificationListener
import com.teamofsilicons.extend.protocol.MissingCapability
import com.teamofsilicons.extend.protocol.Setup
import com.teamofsilicons.extend.protocol.SetupStep

/**
 * One setup step as the app shows it: the wire step plus the settings page its button opens, and
 * [find]: the system screens its "Can't find it?" list looks for (null: the step has none).
 */
data class SetupItem(val step: SetupStep, val required: Boolean, val open: SettingsTarget?, val actionLabel: String?, val find: FindHelp? = null)

/**
 * Everything setup and capabilities depend on, read from the device in one place
 * ([SetupSignals.read]) so [SetupReport.build] is pure and testable for every Android version.
 */
data class SetupSignals(
    val sdk: Int,
    /** Android's version name ("9"), for help text. */
    val release: String,
    val tv: Boolean,
    val fire: Boolean,
    val pkg: String,
    /** What Settings calls the app. */
    val label: String,
    /** What the app calls itself. */
    val app: String,
    /** The notification listener's flattened component. */
    val listener: String,
    val a11yConnected: Boolean,
    val a11yEnabled: Boolean,
    /** How long the service has been on in Settings without Android starting it (0 when it runs or is off). */
    val a11yStartingMs: Long = 0,
    val listenerConnected: Boolean,
    val listenerGranted: Boolean,
    val postGranted: Boolean,
    val batteryOk: Boolean,
    /** Android offers the "let this app run in the background" request on this device. */
    val batteryRequestResolvable: Boolean,
    val devOptions: Boolean,
    val adbWifi: Boolean,
    val adbOn: Boolean,
    val adbConnected: Boolean,
    val adbLastError: String?,
    val afterRestart: DebuggingAfterRestart.Status,
    val dpadSupported: Boolean,
) {
    companion object {
        /** When the service was first seen on in Settings but not running, for [a11yStartingMs]. */
        @Volatile private var a11yStartingSince: Long? = null

        fun read(context: Context, config: Config): SetupSignals {
            val pkg = context.packageName
            val sdk = Build.VERSION.SDK_INT
            val nm = context.getSystemService(NotificationManager::class.java)
            val listener = ExtendNotificationListener.component(context)
            val adb = com.teamofsilicons.extend.Extend.get(context).adb
            val tv = DeviceInfo.isTv(context, config)
            fun global(name: String) = runCatching { Settings.Global.getInt(context.contentResolver, name, 0) == 1 }.getOrDefault(false)
            val a11yConnected = ExtendAccessibilityService.instance != null
            val a11yEnabled = ExtendAccessibilityService.isEnabledInSettings(context)
            val now = android.os.SystemClock.elapsedRealtime()
            if (!a11yEnabled || a11yConnected) a11yStartingSince = null
            else if (a11yStartingSince == null) a11yStartingSince = now
            val startingSince = a11yStartingSince
            return SetupSignals(
                sdk = sdk,
                release = Build.VERSION.RELEASE ?: "$sdk",
                tv = tv,
                fire = DeviceInfo.isFireTv(context),
                pkg = pkg,
                label = DeviceInfo.systemLabel(context),
                app = DeviceInfo.appName(tv),
                listener = listener.flattenToString(),
                a11yConnected = a11yConnected,
                a11yEnabled = a11yEnabled,
                a11yStartingMs = startingSince?.let { now - it } ?: 0,
                listenerConnected = ExtendNotificationListener.instance != null,
                listenerGranted = runCatching {
                    // NotificationManager.isNotificationListenerAccessGranted is Android 8.1+.
                    if (Build.VERSION.SDK_INT >= 27) nm.isNotificationListenerAccessGranted(listener)
                    else pkg in NotificationManagerCompat.getEnabledListenerPackages(context)
                }.getOrDefault(false),
                postGranted = Build.VERSION.SDK_INT < 33 || context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED,
                batteryOk = runCatching { context.getSystemService(PowerManager::class.java).isIgnoringBatteryOptimizations(pkg) }.getOrDefault(false),
                batteryRequestResolvable = runCatching {
                    Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$pkg")).resolveActivity(context.packageManager)
                }.getOrNull() != null,
                devOptions = global(Settings.Global.DEVELOPMENT_SETTINGS_ENABLED),
                adbWifi = global("adb_wifi_enabled"),
                adbOn = global(Settings.Global.ADB_ENABLED),
                adbConnected = adb.connected,
                adbLastError = adb.lastError,
                afterRestart = DebuggingAfterRestart.status(context, adb),
                dpadSupported = ExtendAccessibilityService.dpadSupported,
            )
        }
    }
}

/** What this device can do right now, and what the Carbon still has to allow. */
data class SetupReport(
    val items: List<SetupItem>,
    val capabilities: List<String>,
    val missing: List<MissingCapability>,
    /** Whether a restart turned Wireless debugging off after it was connected (the app then asks the Carbon). */
    val debuggingAfterRestart: DebuggingAfterRestart.Status = DebuggingAfterRestart.Status.NONE,
) {
    val setup: Setup
        get() {
            val required = items.filter { it.required }.map { it.step }
            val state = when {
                required.all { it.status == "done" } -> "complete"
                required.any { it.status == "needs_carbon" || it.status == "failed" } -> "needs_carbon"
                else -> "in_progress"
            }
            return Setup(state, items.map { it.step })
        }

    /**
     * An optional step waits on the Carbon (Wireless debugging after a restart). It never holds
     * setup back (the state comes from the required steps only); the app points the Carbon at it.
     */
    val optionalNeedsCarbon: Boolean
        get() = items.any { !it.required && it.step.status == "needs_carbon" }

    companion object {
        fun compute(context: Context, config: Config): SetupReport = build(SetupSignals.read(context, config))

        /** Setup steps and capabilities for [s]: which steps, their words and pages, and what works, by Android version. */
        fun build(s: SetupSignals): SetupReport {
            val tv = s.tv
            val fire = s.fire
            val label = s.label
            val app = s.app
            fun status(done: Boolean, pending: Boolean = false) = when {
                done -> "done"
                pending -> "in_progress"
                else -> "needs_carbon"
            }
            fun target(page: SettingsPage) = SettingsRoutes.target(page, s.sdk, tv, fire, s.pkg, label, s.listener, s.devOptions)
            // Restricted settings (a sideloaded app's accessibility and notification access) exist from Android 13.
            val restricted = s.sdk >= 33
            val items = ArrayList<SetupItem>()

            // 1. Accessibility: the one permission everything else rests on.
            val a11yHelp = when {
                fire -> "Settings › Accessibility › $label › turn it on."
                // The button can land in a maker's own menu too, so point at the list of other screens rather than promise Android's page.
                tv && s.sdk < 30 -> "Settings › Device Preferences › Accessibility › $label › Enable. " +
                    "If this TV's own settings menu doesn't list Accessibility, try the button; if that doesn't reach it either, " +
                    "select “${FindHelp.buttonLabel(tv = true)}”."
                tv -> "Settings › System (or Device Preferences) › Accessibility › $label › Enable." +
                    if (restricted) " If Android says the setting is restricted: Settings › Apps › $label › Allow restricted settings, then try again." else ""
                restricted -> "Settings › Accessibility › Downloaded apps (or Installed apps) › $label › Use $label. " +
                    "If Android says it's a restricted setting: Settings › Apps › $label › ⋮ (top right) › Allow restricted settings, then try again."
                else -> "Settings › Accessibility › $label (under Downloaded apps or Installed services on some phones) › On."
            }
            items += SetupItem(
                SetupStep(
                    "accessibility",
                    if (tv) "Allow $app to control this TV" else "Allow $app to control the screen",
                    status(s.a11yConnected, pending = s.a11yEnabled),
                    help = a11yHelp,
                    error = a11yStuck(s),
                ),
                required = true,
                open = target(SettingsPage.ACCESSIBILITY),
                actionLabel = "Open Accessibility settings",
                find = FindHelp.accessibility(label),
            )

            // 2. Notifications (phones: the in-use notification with Stop). A runtime permission from Android 13.
            if (!tv && s.sdk >= 33) {
                items += SetupItem(
                    SetupStep(
                        "notifications",
                        "Allow notifications, so you always see when a Silicon is using this device",
                        status(s.postGranted),
                        help = "Tap Allow when asked, or Settings › Apps › $label › Notifications › Allow.",
                    ),
                    required = true,
                    open = null, // requested in-app with the runtime permission prompt
                    actionLabel = "Allow notifications",
                )
            }

            // 3. Background: stay connected with the screen off.
            if (!tv || (!s.batteryOk && s.batteryRequestResolvable)) {
                val where = if (s.sdk >= 31) "Settings › Apps › $label › App battery usage (Battery) › Unrestricted."
                else "Settings › Apps & notifications › Special app access › Battery optimisation › All apps › $label › Don't optimise."
                items += SetupItem(
                    SetupStep(
                        "background",
                        "Let $app stay connected in the background",
                        status(s.batteryOk),
                        help = "Tap Allow when asked to let the app run in the background, or $where",
                    ),
                    required = true,
                    open = target(SettingsPage.BATTERY),
                    actionLabel = "Allow background use",
                )
            }

            // 4. Notification access (phones): lets a Silicon read notifications.
            if (!tv) {
                val where = when {
                    s.sdk >= 31 -> "Settings › Notifications › Device & app notifications (Notification read, reply & control) › $label › Allow."
                    else -> "Settings › Apps & notifications › Special app access › Notification access › $label › Allow."
                }
                items += SetupItem(
                    SetupStep(
                        "notification_access",
                        "Let Silicons read this phone's notifications",
                        status(s.listenerConnected, pending = s.listenerGranted),
                        help = where + if (restricted) " If it's a restricted setting, allow restricted settings first (Settings › Apps › $label › ⋮)." else "",
                    ),
                    required = true,
                    open = target(SettingsPage.NOTIFICATION_ACCESS),
                    actionLabel = "Open notification access",
                )
            }

            // Android debugging adds installation, logs and recording (and, before Android 11,
            // screenshots; on TVs before Android 13, remote buttons).
            val adds = buildList {
                add("installation"); add("logs")
                if (!tv) add("recording")
                if (s.sdk < ScreenshotPath.ACCESSIBILITY_SDK) add("screenshots")
                if (tv && !s.dpadSupported) add("remote buttons")
            }
            val optionalNote = " Then connect Android debugging below to enable ${adds.dropLast(1).joinToString(", ")} and ${adds.last()}."
            items += SetupItem(
                SetupStep(
                    "developer_options",
                    "Turn on Developer options",
                    if (s.devOptions) "done" else "todo",
                    help = when {
                        fire -> "Settings › My Fire TV › About › select the device name 7 times, then go back: Developer options appears under My Fire TV."
                        tv && s.sdk < 30 -> "On the About screen the button opens (Settings › Device Preferences › About), select Build 7 times, " +
                            "until the TV says you are a developer. Developer options then appear under Device Preferences."
                        tv -> "Settings › System (or Device Preferences) › About › select Android TV OS build (Build) 7 times."
                        s.sdk < 28 -> "Settings › System › About phone › tap Build number 7 times, then enter your PIN."
                        else -> "Settings › About phone › tap Build number 7 times, then enter your PIN."
                    } + optionalNote,
                ),
                required = false,
                open = target(SettingsPage.ABOUT),
                actionLabel = "Open About",
                find = FindHelp.developerOptions(tv),
            )
            val debuggingFind = FindHelp.debugging(tv, s.sdk)
            val restart = restartStep(s.afterRestart, tv, s.adbLastError, target(SettingsPage.WIRELESS_DEBUGGING), debuggingFind)
            items += when {
                restart != null -> restart
                DebuggingPath.mode(s.sdk) == DebuggingPath.Mode.NETWORK -> SetupItem(
                    SetupStep(
                        DebuggingPath.stepKey(tv, s.sdk),
                        "Turn on network debugging",
                        // A phone's `adb tcpip` is invisible to apps: it counts once Extend is connected.
                        if (s.adbConnected || (tv && s.devOptions && s.adbOn)) "done" else "todo",
                        help = DebuggingPath.legacyStepHelp(tv, fire, s.release),
                    ),
                    required = false,
                    open = target(SettingsPage.DEVELOPER_OPTIONS),
                    actionLabel = "Open Developer options",
                    find = debuggingFind,
                )
                tv -> SetupItem(
                    SetupStep(
                        "network_debugging",
                        "Turn on network debugging",
                        if (s.devOptions && (s.adbOn || s.adbWifi)) "done" else "todo",
                        help = when {
                            fire -> "Settings › My Fire TV › Developer options › ADB debugging › On."
                            else -> "Settings › System (or Device Preferences) › Developer options › Network debugging (or USB debugging / Wireless debugging) › On. " +
                                "Approve \"Allow debugging\" when the TV asks."
                        } + optionalNote,
                    ),
                    required = false,
                    open = target(SettingsPage.DEVELOPER_OPTIONS),
                    actionLabel = "Open Developer options",
                    find = debuggingFind,
                )
                else -> SetupItem(
                    SetupStep(
                        "wireless_debugging",
                        "Turn on wireless debugging",
                        if (s.adbWifi) "done" else "todo",
                        help = "Settings › System › Developer options › Wireless debugging › On (needs Wi-Fi). It turns off when the phone restarts; the app then asks you to turn it back on." + optionalNote,
                    ),
                    required = false,
                    open = target(SettingsPage.WIRELESS_DEBUGGING),
                    actionLabel = "Open Wireless debugging",
                    find = debuggingFind,
                )
            }

            val a11yReason = "Turn on $label in accessibility settings: $a11yHelp"
            val (caps, missing) = capabilities(s, a11yReason, items.firstOrNull { it.step.key == "notification_access" }?.step?.help)
            return SetupReport(items, caps, missing, s.afterRestart)
        }

        /** How long the service may be on without running before the step says why. */
        const val A11Y_STUCK_MS = 15_000L

        /**
         * Android 8 and 9 can leave an accessibility service that was running when its app was
         * updated switched on but never started again (seen on the Android 8.0 and 9 emulators;
         * turning it off and on through Settings didn't help there, a restart did). Android 10
         * rebinds it. Null unless that is what this looks like.
         */
        fun a11yStuck(s: SetupSignals): String? {
            if (s.sdk >= 29 || !s.a11yEnabled || s.a11yConnected || s.a11yStartingMs < A11Y_STUCK_MS) return null
            val noun = if (s.tv) "TV" else "device"
            return "Accessibility is on for ${s.label}, but Android hasn't started it. On Android ${s.release} this can happen after " +
                "${s.app} is updated: restart this $noun and it starts by itself."
        }

        /**
         * Capabilities: exactly what works right now on this Android version, and why the rest is
         * missing. [notificationHelp] is the notification-access step's help (phones).
         */
        fun capabilities(s: SetupSignals, a11yReason: String, notificationHelp: String?): Pair<List<String>, List<MissingCapability>> {
            val tv = s.tv
            val caps = ArrayList<String>()
            val missing = ArrayList<MissingCapability>()
            fun adbReason(what: String) =
                if (s.afterRestart == DebuggingAfterRestart.Status.NONE) DebuggingPath.missingReason(what, tv, s.fire, s.sdk) else C.afterRestartReason(what, tv)

            val a11yCaps = if (tv) {
                listOf(C.SCREEN_READ, C.INPUT_TEXT, C.NAV_SYSTEM, C.APPS_LAUNCH, C.LINKS, C.ALERTS, C.DISPLAY, C.REPLAY)
            } else {
                listOf(C.SCREEN_READ, C.INPUT_TOUCH, C.INPUT_TEXT, C.INPUT_KEYBOARD, C.NAV_SYSTEM, C.APPS_LAUNCH, C.LINKS, C.ALERTS, C.CLIPBOARD, C.REPLAY)
            }
            for (c in a11yCaps) if (s.a11yConnected) caps += c else missing += MissingCapability(c, a11yReason)

            // Screenshots: accessibility from Android 11, Android debugging's screencap on any version.
            when (ScreenshotPath.choose(s.sdk, s.a11yConnected, s.adbConnected)) {
                ScreenshotPath.ACCESSIBILITY, ScreenshotPath.ADB -> caps += C.SCREEN_CAPTURE
                ScreenshotPath.NONE -> missing += MissingCapability(
                    C.SCREEN_CAPTURE,
                    when {
                        s.sdk >= ScreenshotPath.ACCESSIBILITY_SDK -> a11yReason
                        s.afterRestart != DebuggingAfterRestart.Status.NONE -> C.afterRestartReason("Taking screenshots", tv)
                        else -> DebuggingPath.screenshotReason(tv, s.fire, s.sdk, s.release)
                    },
                )
            }
            if (tv) {
                // Through Android debugging every remote button works on any TV; accessibility
                // covers them (except Menu) from Android 13.
                val reason = TvRemoteKeys.missingReason(
                    s.dpadSupported, s.a11yConnected, s.adbConnected, a11yReason, s.release, DebuggingPath.legacySwitch(tv = true, fire = s.fire),
                )
                if (reason == null) caps += C.INPUT_REMOTE else missing += MissingCapability(C.INPUT_REMOTE, reason)
            }
            caps += C.APPS_LIST
            caps += C.TAKEOVER
            if (!tv) {
                if (s.listenerConnected) caps += C.NOTIFICATIONS
                else missing += MissingCapability(C.NOTIFICATIONS, "Allow notification access: ${notificationHelp.orEmpty()}")
                if (s.adbConnected) caps += C.SCREEN_RECORD
                else missing += MissingCapability(C.SCREEN_RECORD, adbReason("Screen recording"))
            }
            for ((cap, what) in listOf(C.APPS_INSTALL to "Installing apps", C.LOGS to "Reading device logs", C.ADB to "Running adb commands")) {
                if (s.adbConnected) caps += cap else missing += MissingCapability(cap, adbReason(what))
            }
            val full = if (tv) C.ANDROID_TV_FULL else C.ANDROID_FULL
            return full.filter { it in caps } to missing.filter { it.capability in full }.sortedBy { full.indexOf(it.capability) }
        }

        /**
         * The debugging step while [status] says a restart turned Wireless debugging off (null when
         * it didn't). The step itself is `needs_carbon` (then `in_progress` while Extend reconnects),
         * so `hello`/`setup_progress` carry it and `GET /devices/{id}/setup` lists it until debugging
         * is back or the Carbon disconnects it in the app. It stays optional, like every debugging
         * step: setup keeps its state from the required steps, so the device stays `ready` and
         * Silicons keep using it through accessibility; only the debugging capabilities are missing,
         * each with the after-restart reason. Only Wireless debugging (Android 11+) goes off with a
         * restart this way, so [status] is NONE on older versions.
         */
        fun restartStep(status: DebuggingAfterRestart.Status, tv: Boolean, lastError: String?, open: SettingsTarget?, find: FindHelp? = null): SetupItem? {
            val key = if (tv) "network_debugging" else "wireless_debugging"
            val noun = if (tv) "TV" else "phone"
            val path = "Settings › System${if (tv) " (or Device Preferences)" else ""} › Developer options › Wireless debugging › On (needs Wi-Fi)"
            val step = when (status) {
                DebuggingAfterRestart.Status.NONE -> return null
                DebuggingAfterRestart.Status.OFF -> SetupStep(
                    key,
                    "Turn wireless debugging back on",
                    "needs_carbon",
                    help = "This $noun restarted, and Android turns wireless debugging off when it restarts. Silicons can still read and control the screen, " +
                        "but until it is back on they can't install apps, read device logs, record the screen or run adb commands here. $path. Extend reconnects by itself. " +
                        "To stop using Android debugging instead, tap Disconnect Android debugging in the Extend app on this $noun.",
                )
                DebuggingAfterRestart.Status.RECONNECTING -> SetupStep(
                    key,
                    "Turn wireless debugging back on",
                    "in_progress",
                    help = "Wireless debugging is on again; Extend is reconnecting to it. If this doesn't finish within a minute, open Extend on this $noun " +
                        "and pair Android debugging again below.",
                    error = lastError?.let { "Last try: ${it.trimEnd('.')}." },
                )
            }
            return SetupItem(step, required = false, open = open, actionLabel = "Open Wireless debugging", find = find)
        }
    }
}

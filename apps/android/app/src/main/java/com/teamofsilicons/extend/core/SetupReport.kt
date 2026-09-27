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
import com.teamofsilicons.extend.a11y.ExtendAccessibilityService
import com.teamofsilicons.extend.adb.DebuggingAfterRestart
import com.teamofsilicons.extend.config.Config
import com.teamofsilicons.extend.config.DeviceInfo
import com.teamofsilicons.extend.driver.Capabilities as C
import com.teamofsilicons.extend.driver.TvRemoteKeys
import com.teamofsilicons.extend.notif.ExtendNotificationListener
import com.teamofsilicons.extend.protocol.MissingCapability
import com.teamofsilicons.extend.protocol.Setup
import com.teamofsilicons.extend.protocol.SetupStep

/** One setup step as the app shows it: the wire step plus how to get there. */
data class SetupItem(val step: SetupStep, val required: Boolean, val open: (() -> Intent)?, val actionLabel: String?)

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
        fun compute(context: Context, config: Config): SetupReport {
            val tv = DeviceInfo.isTv(context, config)
            val fire = DeviceInfo.isFireTv(context)
            val pkg = context.packageName
            val a11yConnected = ExtendAccessibilityService.instance != null
            val a11yEnabled = ExtendAccessibilityService.isEnabledInSettings(context)
            val listenerConnected = ExtendNotificationListener.instance != null
            val nm = context.getSystemService(NotificationManager::class.java)
            val listenerGranted = runCatching { nm.isNotificationListenerAccessGranted(ExtendNotificationListener.component(context)) }.getOrDefault(false)
            val postGranted = Build.VERSION.SDK_INT < 33 ||
                context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
            val pm = context.getSystemService(PowerManager::class.java)
            val batteryOk = pm.isIgnoringBatteryOptimizations(pkg)
            val devOptions = Settings.Global.getInt(context.contentResolver, Settings.Global.DEVELOPMENT_SETTINGS_ENABLED, 0) == 1
            val adbWifi = Settings.Global.getInt(context.contentResolver, "adb_wifi_enabled", 0) == 1
            val adbOn = Settings.Global.getInt(context.contentResolver, Settings.Global.ADB_ENABLED, 0) == 1
            val adb = com.teamofsilicons.extend.Extend.get(context).adb
            val adbConnected = adb.connected
            val afterRestart = DebuggingAfterRestart.status(context, adb)
            // What Settings calls the app, and what the app calls itself.
            val label = DeviceInfo.systemLabel(context)
            val app = DeviceInfo.appName(tv)

            fun status(done: Boolean, pending: Boolean = false) = when {
                done -> "done"
                pending -> "in_progress"
                else -> "needs_carbon"
            }
            val appDetails = { Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:$pkg")) }
            val items = ArrayList<SetupItem>()

            // 1. Accessibility: the one permission everything else rests on.
            val a11yHelp = when {
                fire -> "Settings › Accessibility › $label › turn it on."
                tv -> "Settings › System (or Device Preferences) › Accessibility › $label › Enable. " +
                    "If Android says the setting is restricted: Settings › Apps › $label › Allow restricted settings, then try again."
                else -> "Settings › Accessibility › Downloaded apps (or Installed apps) › $label › Use $label. " +
                    "If Android says it's a restricted setting: Settings › Apps › $label › ⋮ (top right) › Allow restricted settings, then try again."
            }
            items += SetupItem(
                SetupStep(
                    "accessibility",
                    if (tv) "Allow $app to control this TV" else "Allow $app to control the screen",
                    status(a11yConnected, pending = a11yEnabled),
                    help = a11yHelp,
                ),
                required = true,
                open = { Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS) },
                actionLabel = "Open Accessibility settings",
            )

            // 2. Notifications (phones: the in-use notification with Stop).
            if (!tv && Build.VERSION.SDK_INT >= 33) {
                items += SetupItem(
                    SetupStep(
                        "notifications",
                        "Allow notifications, so you always see when a Silicon is using this device",
                        status(postGranted),
                        help = "Tap Allow when asked, or Settings › Apps › $label › Notifications › Allow.",
                    ),
                    required = true,
                    open = null, // requested in-app with the runtime permission prompt
                    actionLabel = "Allow notifications",
                )
            }

            // 3. Background: stay connected with the screen off.
            val batteryIntent = Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$pkg"))
            val batteryResolvable = batteryIntent.resolveActivity(context.packageManager) != null
            if (!tv || (!batteryOk && batteryResolvable)) {
                items += SetupItem(
                    SetupStep(
                        "background",
                        "Let $app stay connected in the background",
                        status(batteryOk),
                        help = "Tap Allow when asked to let the app run in the background, or Settings › Apps › $label › " +
                            "App battery usage (Battery) › Unrestricted.",
                    ),
                    required = true,
                    open = if (batteryResolvable) ({ batteryIntent }) else appDetails,
                    actionLabel = "Allow background use",
                )
            }

            // 4. Notification access (phones): lets a Silicon read notifications.
            if (!tv) {
                items += SetupItem(
                    SetupStep(
                        "notification_access",
                        "Let Silicons read this phone's notifications",
                        status(listenerConnected, pending = listenerGranted),
                        help = "Settings › Notifications › Device & app notifications (Notification read, reply & control) › $label › Allow. " +
                            "If it's a restricted setting, allow restricted settings first (Settings › Apps › $label › ⋮).",
                    ),
                    required = true,
                    open = {
                        Intent(Settings.ACTION_NOTIFICATION_LISTENER_DETAIL_SETTINGS).putExtra(
                            Settings.EXTRA_NOTIFICATION_LISTENER_COMPONENT_NAME,
                            ExtendNotificationListener.component(context).flattenToString(),
                        )
                    },
                    actionLabel = "Open notification access",
                )
            }

            // ADB augments accessibility with installation, logs and recording.
            val optionalNote = " Then connect Android debugging below to enable installation, logs and recording."
            items += SetupItem(
                SetupStep(
                    "developer_options",
                    "Turn on Developer options",
                    if (devOptions) "done" else "todo",
                    help = when {
                        fire -> "Settings › My Fire TV › About › select the device name 7 times, then go back: Developer options appears under My Fire TV."
                        tv -> "Settings › System (or Device Preferences) › About › select Android TV OS build (Build) 7 times."
                        else -> "Settings › About phone › tap Build number 7 times, then enter your PIN."
                    } + optionalNote,
                ),
                required = false,
                open = { Intent(Settings.ACTION_DEVICE_INFO_SETTINGS) },
                actionLabel = "Open About",
            )
            val restartStep = restartStep(afterRestart, tv, adb.lastError) { DebuggingAfterRestart.wirelessDebuggingIntent(context) }
            items += when {
                restartStep != null -> restartStep
                tv -> SetupItem(
                    SetupStep(
                        "network_debugging",
                        "Turn on network debugging",
                        if (devOptions && (adbOn || adbWifi)) "done" else "todo",
                        help = when {
                            fire -> "Settings › My Fire TV › Developer options › ADB debugging › On."
                            else -> "Settings › System (or Device Preferences) › Developer options › Network debugging (or USB debugging / Wireless debugging) › On. " +
                                "Approve \"Allow debugging\" when the TV asks."
                        } + optionalNote,
                    ),
                    required = false,
                    open = { Intent(Settings.ACTION_APPLICATION_DEVELOPMENT_SETTINGS) },
                    actionLabel = "Open Developer options",
                )
                else -> SetupItem(
                    SetupStep(
                        "wireless_debugging",
                        "Turn on wireless debugging",
                        if (adbWifi) "done" else "todo",
                        help = "Settings › System › Developer options › Wireless debugging › On (needs Wi-Fi). It turns off when the phone restarts; the app then asks you to turn it back on." + optionalNote,
                    ),
                    required = false,
                    open = { DebuggingAfterRestart.wirelessDebuggingIntent(context) },
                    actionLabel = "Open Wireless debugging",
                )
            }

            // Capabilities: exactly what works right now.
            val caps = ArrayList<String>()
            val missing = ArrayList<MissingCapability>()
            val a11yReason = "Turn on $label in accessibility settings: $a11yHelp"
            val a11yCaps = if (tv) {
                listOf(C.SCREEN_READ, C.SCREEN_CAPTURE, C.INPUT_TEXT, C.NAV_SYSTEM, C.APPS_LAUNCH, C.LINKS, C.ALERTS, C.DISPLAY, C.REPLAY)
            } else {
                listOf(
                    C.SCREEN_READ, C.SCREEN_CAPTURE, C.INPUT_TOUCH, C.INPUT_TEXT, C.INPUT_KEYBOARD, C.NAV_SYSTEM, C.APPS_LAUNCH,
                    C.LINKS, C.ALERTS, C.CLIPBOARD, C.REPLAY,
                )
            }
            for (c in a11yCaps) if (a11yConnected) caps += c else missing += MissingCapability(c, a11yReason)
            if (tv) {
                // Through Android debugging every remote button works on any TV; accessibility
                // covers them (except Menu) from Android 13.
                val reason = TvRemoteKeys.missingReason(
                    ExtendAccessibilityService.dpadSupported, a11yConnected, adbConnected, a11yReason, Build.VERSION.RELEASE ?: "${Build.VERSION.SDK_INT}",
                )
                if (reason == null) caps += C.INPUT_REMOTE else missing += MissingCapability(C.INPUT_REMOTE, reason)
            }
            caps += C.APPS_LIST
            caps += C.TAKEOVER
            if (!tv) {
                if (listenerConnected) caps += C.NOTIFICATIONS
                else missing += MissingCapability(C.NOTIFICATIONS, "Allow notification access: " + items.first { it.step.key == "notification_access" }.step.help)
                if (adbConnected) caps += C.SCREEN_RECORD
                else missing += MissingCapability(C.SCREEN_RECORD, if (afterRestart == DebuggingAfterRestart.Status.NONE) C.RECORD_REASON else C.afterRestartReason("Screen recording", tv))
            }
            for ((cap, what) in listOf(C.APPS_INSTALL to "Installing apps", C.LOGS to "Reading device logs", C.ADB to "Running adb commands")) {
                if (adbConnected) caps += cap
                else missing += MissingCapability(cap, if (afterRestart == DebuggingAfterRestart.Status.NONE) C.adbReason(what) else C.afterRestartReason(what, tv))
            }

            val full = if (tv) C.ANDROID_TV_FULL else C.ANDROID_FULL
            return SetupReport(
                items,
                full.filter { it in caps },
                missing.filter { it.capability in full }.sortedBy { full.indexOf(it.capability) },
                afterRestart,
            )
        }

        /**
         * The debugging step while [status] says a restart turned Wireless debugging off (null when
         * it didn't). The step itself is `needs_carbon` (then `in_progress` while Extend reconnects),
         * so `hello`/`setup_progress` carry it and `GET /devices/{id}/setup` lists it until debugging
         * is back or the Carbon disconnects it in the app. It stays optional, like every debugging
         * step: setup keeps its state from the required steps, so the device stays `ready` and
         * Silicons keep using it through accessibility; only the debugging capabilities are missing,
         * each with the after-restart reason.
         */
        fun restartStep(status: DebuggingAfterRestart.Status, tv: Boolean, lastError: String?, open: () -> Intent): SetupItem? {
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
            return SetupItem(step, required = false, open = open, actionLabel = "Open Wireless debugging")
        }
    }
}

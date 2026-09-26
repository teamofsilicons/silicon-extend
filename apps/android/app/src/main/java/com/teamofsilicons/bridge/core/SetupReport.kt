package com.teamofsilicons.bridge.core

import android.Manifest
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import com.teamofsilicons.bridge.a11y.BridgeAccessibilityService
import com.teamofsilicons.bridge.config.Config
import com.teamofsilicons.bridge.config.DeviceInfo
import com.teamofsilicons.bridge.driver.Capabilities as C
import com.teamofsilicons.bridge.notif.BridgeNotificationListener
import com.teamofsilicons.bridge.protocol.MissingCapability
import com.teamofsilicons.bridge.protocol.Setup
import com.teamofsilicons.bridge.protocol.SetupStep

/** One setup step as the app shows it: the wire step plus how to get there. */
data class SetupItem(val step: SetupStep, val required: Boolean, val open: (() -> Intent)?, val actionLabel: String?)

/** What this device can do right now, and what the Carbon still has to allow. */
data class SetupReport(
    val items: List<SetupItem>,
    val capabilities: List<String>,
    val missing: List<MissingCapability>,
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

    companion object {
        fun compute(context: Context, config: Config): SetupReport {
            val tv = DeviceInfo.isTv(context, config)
            val fire = DeviceInfo.isFireTv(context)
            val pkg = context.packageName
            val a11yConnected = BridgeAccessibilityService.instance != null
            val a11yEnabled = BridgeAccessibilityService.isEnabledInSettings(context)
            val listenerConnected = BridgeNotificationListener.instance != null
            val nm = context.getSystemService(NotificationManager::class.java)
            val listenerGranted = runCatching { nm.isNotificationListenerAccessGranted(BridgeNotificationListener.component(context)) }.getOrDefault(false)
            val postGranted = Build.VERSION.SDK_INT < 33 ||
                context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
            val pm = context.getSystemService(PowerManager::class.java)
            val batteryOk = pm.isIgnoringBatteryOptimizations(pkg)
            val devOptions = Settings.Global.getInt(context.contentResolver, Settings.Global.DEVELOPMENT_SETTINGS_ENABLED, 0) == 1
            val adbWifi = Settings.Global.getInt(context.contentResolver, "adb_wifi_enabled", 0) == 1
            val adbOn = Settings.Global.getInt(context.contentResolver, Settings.Global.ADB_ENABLED, 0) == 1

            fun status(done: Boolean, pending: Boolean = false) = when {
                done -> "done"
                pending -> "in_progress"
                else -> "needs_carbon"
            }
            val appDetails = { Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:$pkg")) }
            val items = ArrayList<SetupItem>()

            // 1. Accessibility: the one permission everything else rests on.
            val a11yHelp = when {
                fire -> "Settings › Accessibility › Silicon Bridge › turn it on."
                tv -> "Settings › System (or Device Preferences) › Accessibility › Silicon Bridge › Enable. " +
                    "If Android says the setting is restricted: Settings › Apps › Silicon Bridge › Allow restricted settings, then try again."
                else -> "Settings › Accessibility › Downloaded apps (or Installed apps) › Silicon Bridge › Use Silicon Bridge. " +
                    "If Android says it's a restricted setting: Settings › Apps › Silicon Bridge › ⋮ (top right) › Allow restricted settings, then try again."
            }
            items += SetupItem(
                SetupStep(
                    "accessibility",
                    if (tv) "Allow Silicon Bridge to control the TV" else "Allow Silicon Bridge to control the screen",
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
                        help = "Tap Allow when asked, or Settings › Apps › Silicon Bridge › Notifications › Allow.",
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
                        "Let Silicon Bridge stay connected in the background",
                        status(batteryOk),
                        help = "Tap Allow when asked to let the app run in the background, or Settings › Apps › Silicon Bridge › " +
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
                        help = "Settings › Notifications › Device & app notifications (Notification read, reply & control) › Silicon Bridge › Allow. " +
                            "If it's a restricted setting, allow restricted settings first (Settings › Apps › Silicon Bridge › ⋮).",
                    ),
                    required = true,
                    open = {
                        Intent(Settings.ACTION_NOTIFICATION_LISTENER_DETAIL_SETTINGS).putExtra(
                            Settings.EXTRA_NOTIFICATION_LISTENER_COMPONENT_NAME,
                            BridgeNotificationListener.component(context).flattenToString(),
                        )
                    },
                    actionLabel = "Open notification access",
                )
            }

            // 5–6. Developer options and wireless/network debugging. Optional in this version:
            // only Android debugging (adb, install, logs) needs them, and that bridge isn't built yet.
            val optionalNote = " Optional for now: only Android debugging (adb, installing apps, device logs) needs it, and this version of the app doesn't use it yet."
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
            items += if (tv) {
                SetupItem(
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
            } else {
                SetupItem(
                    SetupStep(
                        "wireless_debugging",
                        "Turn on wireless debugging",
                        if (adbWifi) "done" else "todo",
                        help = "Settings › System › Developer options › Wireless debugging › On (needs Wi-Fi). It turns off when the phone restarts." + optionalNote,
                    ),
                    required = false,
                    open = { Intent(Settings.ACTION_APPLICATION_DEVELOPMENT_SETTINGS) },
                    actionLabel = "Open Developer options",
                )
            }

            // Capabilities: exactly what works right now.
            val caps = ArrayList<String>()
            val missing = ArrayList<MissingCapability>()
            val a11yReason = "Turn on Silicon Bridge in accessibility settings: $a11yHelp"
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
                when {
                    !BridgeAccessibilityService.dpadSupported -> missing += MissingCapability(
                        C.INPUT_REMOTE,
                        "Remote buttons need Android 13 or later (this TV runs Android ${Build.VERSION.RELEASE}); Android debugging will cover older TVs in a later version.",
                    )
                    a11yConnected -> caps += C.INPUT_REMOTE
                    else -> missing += MissingCapability(C.INPUT_REMOTE, a11yReason)
                }
            }
            caps += C.APPS_LIST
            caps += C.TAKEOVER
            if (!tv) {
                if (listenerConnected) caps += C.NOTIFICATIONS
                else missing += MissingCapability(C.NOTIFICATIONS, "Allow notification access: " + items.first { it.step.key == "notification_access" }.step.help)
                missing += MissingCapability(C.SCREEN_RECORD, C.RECORD_REASON)
            }
            missing += MissingCapability(C.APPS_INSTALL, C.adbReason("Installing apps"))
            missing += MissingCapability(C.LOGS, C.adbReason("Reading device logs"))
            missing += MissingCapability(C.ADB, C.adbReason("Android debugging (adb)"))

            val full = if (tv) C.ANDROID_TV_FULL else C.ANDROID_FULL
            return SetupReport(
                items,
                full.filter { it in caps },
                missing.filter { it.capability in full }.sortedBy { full.indexOf(it.capability) },
            )
        }
    }
}

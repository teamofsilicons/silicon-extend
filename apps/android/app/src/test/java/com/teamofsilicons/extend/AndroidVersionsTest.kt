package com.teamofsilicons.extend

import com.teamofsilicons.extend.adb.DebuggingAfterRestart
import com.teamofsilicons.extend.adb.DebuggingPath
import com.teamofsilicons.extend.adb.DebuggingPath.Mode
import com.teamofsilicons.extend.adb.RecordingMuxer
import com.teamofsilicons.extend.core.SettingsPage
import com.teamofsilicons.extend.core.SettingsRoute
import com.teamofsilicons.extend.core.SettingsRoutes
import com.teamofsilicons.extend.core.SetupReport
import com.teamofsilicons.extend.core.SetupSignals
import com.teamofsilicons.extend.driver.Capabilities as C
import com.teamofsilicons.extend.driver.ClipboardActivity
import com.teamofsilicons.extend.driver.ScreenshotPath
import com.teamofsilicons.extend.ui.DebuggingCardCopy
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The app runs on Android 8 (API 26) and later: TVs, TV boxes, projectors and Fire TV sticks often
 * run Android 8–10. Each version-gated decision is checked per SDK level: which Android debugging
 * path, which setup steps and settings pages, which screenshot path and which capabilities.
 */
class AndroidVersionsTest {
    // ───────────── Android debugging ─────────────

    @Test fun android8To10UseNetworkDebuggingOnPort5555AndAndroid11PairsWithACode() {
        for (sdk in 26..29) {
            assertEquals("API $sdk", Mode.NETWORK, DebuggingPath.mode(sdk))
            assertFalse("API $sdk has no Wireless debugging pairing", DebuggingPath.canPair(sdk))
            assertEquals("API $sdk: no discovery, the network debugging port", 5555, DebuggingPath.connectPort(0, sdk))
            assertEquals("API $sdk: a port the Carbon typed stays", 5557, DebuggingPath.connectPort(5557, sdk))
        }
        for (sdk in listOf(30, 33, 36)) {
            assertEquals(Mode.WIRELESS, DebuggingPath.mode(sdk))
            assertTrue(DebuggingPath.canPair(sdk))
            assertEquals("API $sdk discovers the Wireless debugging port", 0, DebuggingPath.connectPort(0, sdk))
            assertEquals("a TV's legacy port still works on Android 11+", 5555, DebuggingPath.connectPort(5555, sdk))
        }
    }

    @Test fun aConnectTheCarbonStartedWaitsForTheAllowUsbDebuggingPrompt() {
        // A TV remote needs longer than 8 s to reach "Allow" on Android's prompt.
        assertEquals(60L, DebuggingPath.connectWaitSeconds(requireTls = false, startedByCarbon = true))
        assertEquals("the reconnect loop never waits on a prompt", 8L, DebuggingPath.connectWaitSeconds(requireTls = false, startedByCarbon = false))
        assertEquals("TLS (Wireless debugging) has no prompt", 8L, DebuggingPath.connectWaitSeconds(requireTls = true, startedByCarbon = true))
    }

    @Test fun theDebuggingStepAndReasonsSayWhatEachVersionNeeds() {
        assertEquals("network_debugging", DebuggingPath.stepKey(tv = false, sdk = 28))
        assertEquals("wireless_debugging", DebuggingPath.stepKey(tv = false, sdk = 30))
        assertEquals("network_debugging", DebuggingPath.stepKey(tv = true, sdk = 28))
        assertEquals("network_debugging", DebuggingPath.stepKey(tv = true, sdk = 34))

        val phone9 = DebuggingPath.missingReason("Installing apps", tv = false, fire = false, sdk = 28)
        assertTrue(phone9, phone9.startsWith("Installing apps needs Android debugging."))
        assertTrue(phone9, phone9.contains("`adb tcpip 5555`"))
        val tv9 = DebuggingPath.missingReason("Reading device logs", tv = true, fire = false, sdk = 28)
        assertTrue(tv9, tv9.contains("Developer options › Network debugging"))
        val fire = DebuggingPath.missingReason("Running adb commands", tv = true, fire = true, sdk = 28)
        assertTrue(fire, fire.contains("My Fire TV › Developer options › ADB debugging"))
        assertEquals(
            "Installing apps needs Android debugging. Turn on Wireless or Network debugging, then connect it in the Extend app's setup.",
            DebuggingPath.missingReason("Installing apps", tv = false, fire = false, sdk = 30),
        )

        val refusedTv = DebuggingPath.refused(5555, tv = true, fire = false, sdk = 28)
        assertTrue(refusedTv, refusedTv.startsWith("Nothing answered on debugging port 5555. Turn on network debugging"))
        assertTrue(DebuggingPath.refused(5555, tv = false, fire = false, sdk = 29).contains("run `adb tcpip 5555`"))
        assertTrue(DebuggingPath.notAnswered(5555, 60, requireTls = false).contains("\"Allow USB debugging?\""))
        assertTrue(DebuggingPath.disconnected(28).contains("Revoke USB debugging authorisations"))
        assertTrue(DebuggingPath.disconnected(30).contains("Wireless debugging settings"))
    }

    @Test fun theDebuggingCardOnAndroid9HasNoWirelessDebuggingWords() {
        val tv = DebuggingCardCopy.text(false, false, false, sdk = 28, tv = true, fire = false, release = "9", port = 5555)
        assertTrue(tv, tv.startsWith("This TV runs Android 9, which has no Wireless debugging."))
        assertTrue(tv, tv.contains("Allow USB debugging?"))
        assertTrue(DebuggingCardCopy.text(true, true, false, sdk = 28, tv = true, fire = false, release = "9", port = 5555).contains("screenshots and remote buttons"))
        val phone = DebuggingCardCopy.text(false, true, false, sdk = 28, tv = false, fire = false, release = "9", port = 5555)
        assertTrue(phone, phone.startsWith("Connected before, but not connected right now; Extend keeps reconnecting to port 5555."))
        assertTrue(phone, phone.contains("`adb tcpip 5555` lasts until the phone restarts"))
        assertEquals(
            "Android 11+ keeps the Wireless debugging words",
            DebuggingCardCopy.text(false, false, false),
            DebuggingCardCopy.text(false, false, false, sdk = 30, tv = false, fire = false, release = "11", port = 0),
        )
    }

    @Test fun turningOnAccessibilityThroughDebuggingKeepsTheOtherServices() {
        val me = "com.teamofsilicons.extend/com.teamofsilicons.extend.a11y.ExtendAccessibilityService"
        val cmd = DebuggingPath.enableAccessibilityCommand(me)
        assertEquals(
            "cur=\$(settings get secure enabled_accessibility_services); case \":\$cur:\" in *:$me:*) ;; " +
                "*) if [ -z \"\$cur\" ] || [ \"\$cur\" = null ]; then new='$me'; else new=\"\$cur\":'$me'; fi; " +
                "settings put secure enabled_accessibility_services \"\$new\" ;; esac; settings put secure accessibility_enabled 1",
            cmd,
        )
        try {
            DebuggingPath.enableAccessibilityCommand("x/y; reboot")
            throw AssertionError("a component with shell characters must be refused")
        } catch (_: IllegalArgumentException) {
        }
    }

    // ───────────── Screenshots ─────────────

    @Test fun screenshotsUseAccessibilityFromAndroid11AndAndroidDebuggingBefore() {
        assertEquals(ScreenshotPath.ACCESSIBILITY, ScreenshotPath.choose(30, a11yConnected = true, adbConnected = true))
        assertEquals(ScreenshotPath.ACCESSIBILITY, ScreenshotPath.choose(36, a11yConnected = true, adbConnected = false))
        assertEquals("accessibility off: debugging's screencap", ScreenshotPath.ADB, ScreenshotPath.choose(34, a11yConnected = false, adbConnected = true))
        for (sdk in 26..29) {
            assertEquals("API $sdk: takeScreenshot doesn't exist", ScreenshotPath.ADB, ScreenshotPath.choose(sdk, a11yConnected = true, adbConnected = true))
            assertEquals("API $sdk without debugging", ScreenshotPath.NONE, ScreenshotPath.choose(sdk, a11yConnected = true, adbConnected = false))
        }
        assertEquals(ScreenshotPath.NONE, ScreenshotPath.choose(36, a11yConnected = false, adbConnected = false))
    }

    // ───────────── Setup steps and capabilities ─────────────

    private fun signals(
        sdk: Int, tv: Boolean = false, fire: Boolean = false, a11y: Boolean = true, adb: Boolean = false,
        listener: Boolean = true, batteryOk: Boolean = true, devOptions: Boolean = false, adbOn: Boolean = false,
    ) = SetupSignals(
        sdk = sdk, release = mapOf(26 to "8.0", 27 to "8.1", 28 to "9", 29 to "10", 30 to "11", 33 to "13", 34 to "14", 36 to "16")[sdk] ?: "$sdk",
        tv = tv, fire = fire, pkg = "com.teamofsilicons.extend", label = if (tv) "Silicon Extend TV" else "Silicon Extend",
        app = if (tv) "Silicon Extend TV" else "Silicon Extend", listener = "com.teamofsilicons.extend/.notif.ExtendNotificationListener",
        a11yConnected = a11y, a11yEnabled = a11y, listenerConnected = listener, listenerGranted = listener, postGranted = true,
        batteryOk = batteryOk, batteryRequestResolvable = true, devOptions = devOptions, adbWifi = false, adbOn = adbOn,
        adbConnected = adb, adbLastError = null, afterRestart = DebuggingAfterRestart.Status.NONE, dpadSupported = sdk >= 33,
    )

    private fun SetupReport.keys() = items.map { it.step.key }

    @Test fun aPhoneOnAndroid9HasNoNotificationPermissionStepAndANetworkDebuggingStep() {
        val r = SetupReport.build(signals(28))
        assertEquals(listOf("accessibility", "background", "notification_access", "developer_options", "network_debugging"), r.keys())
        assertEquals("complete", r.setup.state)
        val debugging = r.items.last()
        assertFalse(debugging.required)
        assertEquals("todo", debugging.step.status)
        assertEquals(SettingsPage.DEVELOPER_OPTIONS, debugging.open?.page)
        assertTrue(debugging.step.help!!, debugging.step.help!!.contains("`adb tcpip 5555`"))
        assertFalse("restricted settings are Android 13+", r.items.first().step.help!!.contains("restricted"))
        assertTrue(r.items.first { it.step.key == "notification_access" }.step.help!!.startsWith("Settings › Apps & notifications › Special app access › Notification access"))
        assertEquals("done", SetupReport.build(signals(28, adb = true)).items.last().step.status)
        assertEquals("the Android 8.0 About screen is under System", true,
            SetupReport.build(signals(26)).items.first { it.step.key == "developer_options" }.step.help!!.startsWith("Settings › System › About phone"))
    }

    @Test fun aPhoneOnAndroid13KeepsTheNotificationPermissionAndWirelessDebugging() {
        val r = SetupReport.build(signals(33))
        assertEquals(listOf("accessibility", "notifications", "background", "notification_access", "developer_options", "wireless_debugging"), r.keys())
        assertEquals(SettingsPage.WIRELESS_DEBUGGING, r.items.last().open?.page)
        assertTrue(r.items.first().step.help!!.contains("Allow restricted settings"))
    }

    @Test fun aTvOnAndroid9SelectsBuildOnTheAboutScreenTheButtonOpens() {
        val r = SetupReport.build(signals(28, tv = true))
        assertEquals(listOf("accessibility", "developer_options", "network_debugging"), r.keys())
        val dev = r.items.first { it.step.key == "developer_options" }
        assertTrue(dev.step.help!!, dev.step.help!!.startsWith("On the About screen the button opens (Settings › Device Preferences › About), select Build 7 times"))
        assertTrue("debugging also adds screenshots and remote buttons here", dev.step.help!!.endsWith("installation, logs, screenshots and remote buttons."))
        assertEquals(SettingsPage.ABOUT, dev.open?.page)
        assertEquals(SettingsRoute(component = "com.android.tv.settings" to "com.android.tv.settings.about.AboutActivity"), dev.open!!.routes.first())
        val net = r.items.last()
        assertTrue(net.step.help!!, net.step.help!!.contains("This TV runs Android 9, which has no Wireless debugging."))
        assertEquals("todo", net.step.status)
        assertEquals("a TV shows network debugging on as soon as Android has it on", "done",
            SetupReport.build(signals(28, tv = true, devOptions = true, adbOn = true)).items.last().step.status)
        // The button can land in a maker's own menu: the help points at the list of other screens, not at Android's page.
        val a11yHelp = r.items.first().step.help!!
        assertTrue(a11yHelp, a11yHelp.endsWith("try the button; if that doesn't reach it either, select “Can't find it? Other screens on this TV”."))
        assertFalse(a11yHelp, a11yHelp.contains("opens Android's page directly"))
    }

    @Test fun onAndroid9AnAccessibilityServiceAndroidNeverStartedSaysToRestart() {
        fun stuck(sdk: Int, ms: Long, tv: Boolean = true) =
            SetupReport.build(signals(sdk, tv = tv, a11y = false).copy(a11yEnabled = true, a11yStartingMs = ms)).items.first().step
        val step = stuck(28, 20_000)
        assertEquals("in_progress", step.status)
        assertEquals(
            "Accessibility is on for Silicon Extend TV, but Android hasn't started it. On Android 9 this can happen after Silicon Extend TV is updated: " +
                "restart this TV and it starts by itself.",
            step.error,
        )
        assertNull("starting for a moment is normal", stuck(28, 5_000).error)
        assertNull("Android 10 rebinds it", stuck(29, 60_000).error)
        assertTrue(stuck(26, 60_000, tv = false).error!!.contains("restart this device"))
    }

    @Test fun onAndroid9ScreenshotsNeedAndroidDebugging() {
        val phone = SetupReport.build(signals(28))
        assertTrue(C.SCREEN_READ in phone.capabilities)
        assertFalse(C.SCREEN_CAPTURE in phone.capabilities)
        val reason = phone.missing.first { it.capability == C.SCREEN_CAPTURE }.reason
        assertTrue(reason, reason.startsWith("Screenshots through accessibility need Android 11; this phone runs Android 9."))
        assertTrue(reason, reason.contains("`adb tcpip 5555`"))
        val connected = SetupReport.build(signals(28, adb = true))
        assertTrue(C.SCREEN_CAPTURE in connected.capabilities)
        assertTrue(connected.missing.none { it.capability == C.SCREEN_CAPTURE })
        // Android 11+: accessibility screenshots, with or without debugging.
        assertTrue(C.SCREEN_CAPTURE in SetupReport.build(signals(30)).capabilities)
        // Accessibility off on Android 11+: screenshots still work through debugging.
        assertTrue(C.SCREEN_CAPTURE in SetupReport.build(signals(34, a11y = false, adb = true)).capabilities)
        assertFalse(C.SCREEN_CAPTURE in SetupReport.build(signals(34, a11y = false, adb = false)).capabilities)
    }

    @Test fun aTvOnAndroid9GetsRemoteButtonsAndScreenshotsOnlyThroughDebugging() {
        val off = SetupReport.build(signals(28, tv = true))
        assertEquals(
            listOf(C.SCREEN_READ, C.INPUT_TEXT, C.NAV_SYSTEM, C.APPS_LAUNCH, C.APPS_LIST, C.ALERTS, C.REPLAY, C.TAKEOVER, C.DISPLAY, C.LINKS),
            off.capabilities,
        )
        assertEquals(
            listOf(C.SCREEN_CAPTURE, C.INPUT_REMOTE, C.APPS_INSTALL, C.LOGS, C.ADB),
            off.missing.map { it.capability },
        )
        val on = SetupReport.build(signals(28, tv = true, adb = true))
        assertEquals(C.ANDROID_TV_FULL, on.capabilities)
        assertTrue(on.missing.isEmpty())
    }

    @Test fun capabilitiesPerAndroidVersionOnAPhoneWithAccessibilityAndNotificationAccess() {
        for (sdk in listOf(26, 27, 28, 29)) {
            val r = SetupReport.build(signals(sdk))
            assertEquals("API $sdk", listOf(C.SCREEN_CAPTURE, C.SCREEN_RECORD, C.APPS_INSTALL, C.LOGS, C.ADB), r.missing.map { it.capability })
            assertEquals("API $sdk with debugging: everything", C.ANDROID_FULL, SetupReport.build(signals(sdk, adb = true)).capabilities)
        }
        for (sdk in listOf(30, 33, 36)) {
            val r = SetupReport.build(signals(sdk))
            assertEquals("API $sdk", listOf(C.SCREEN_RECORD, C.APPS_INSTALL, C.LOGS, C.ADB), r.missing.map { it.capability })
        }
    }

    // ───────────── Settings pages ─────────────

    private fun components(routes: List<SettingsRoute>) = routes.map { it.component?.second ?: it.action }

    @Test fun onATvAndroidTvSettingsOwnPageComesFirstThenTheActionThenPhoneSettingsThenTheMainScreen() {
        val routes = SettingsRoutes.routes(SettingsPage.ACCESSIBILITY, sdk = 28, tv = true, fire = false, pkg = "p")
        assertEquals(
            listOf(
                "com.android.tv.settings.system.AccessibilityActivity",
                "com.android.tv.settings.oemlink.AccessibilitySettingsActivity",
                "android.settings.ACCESSIBILITY_SETTINGS",
                "com.android.settings.Settings\$AccessibilitySettingsActivity",
                "android.settings.SETTINGS",
                "com.android.tv.settings.MainSettings",
                "com.android.settings.Settings",
            ),
            components(routes),
        )
        val dev = components(SettingsRoutes.routes(SettingsPage.DEVELOPER_OPTIONS, sdk = 28, tv = true, fire = false, pkg = "p"))
        assertEquals("com.android.tv.settings.system.development.DevelopmentActivity", dev.first())
        assertEquals(listOf("android.settings.APPLICATION_DEVELOPMENT_SETTINGS", "com.android.settings.APPLICATION_DEVELOPMENT_SETTINGS"), dev.subList(1, 3))
        val about = components(SettingsRoutes.routes(SettingsPage.ABOUT, sdk = 28, tv = true, fire = false, pkg = "p"))
        assertEquals(listOf("com.android.tv.settings.about.AboutActivity", "com.android.tv.settings.device.DeviceInfoSettingsActivity", "android.settings.DEVICE_INFO_SETTINGS"), about.take(3))
    }

    @Test fun aPhoneAndAFireTvStartWithTheStandardAction() {
        val phone = components(SettingsRoutes.routes(SettingsPage.ACCESSIBILITY, sdk = 28, tv = false, fire = false, pkg = "p"))
        assertEquals("android.settings.ACCESSIBILITY_SETTINGS", phone.first())
        assertTrue(phone.none { it!!.startsWith("com.android.tv.settings.system") })
        val fire = components(SettingsRoutes.routes(SettingsPage.ACCESSIBILITY, sdk = 28, tv = true, fire = true, pkg = "p"))
        assertEquals("Fire TV's own settings answer the standard action; no Android TV or phone pages",
            listOf("android.settings.ACCESSIBILITY_SETTINGS", "android.settings.SETTINGS", "com.android.tv.settings.MainSettings", "com.android.settings.Settings"), fire)
    }

    @Test fun everyPageEndsAtTheMainSettingsScreenWithoutRepeats() {
        for (page in SettingsPage.entries) for (sdk in listOf(26, 28, 29, 30, 34)) for ((tv, fire) in listOf(false to false, true to false, true to true)) {
            val routes = SettingsRoutes.routes(page, sdk, tv, fire, "p", listener = "p/.L")
            assertEquals("$page $sdk tv=$tv fire=$fire", routes.distinct(), routes)
            assertTrue("$page $sdk", SettingsRoute("android.settings.SETTINGS") in routes)
            assertTrue("$page $sdk: the main screen is the last resort", routes.indexOf(SettingsRoute("android.settings.SETTINGS")) >= routes.size - 3)
        }
    }

    @Test fun versionOnlyPagesAreLeftOutBeforeTheirVersion() {
        val access28 = SettingsRoutes.routes(SettingsPage.NOTIFICATION_ACCESS, sdk = 28, tv = false, fire = false, pkg = "p", listener = "p/.L")
        assertEquals("the per-app page is Android 11+", "android.settings.ACTION_NOTIFICATION_LISTENER_SETTINGS", access28.first().action)
        val access30 = SettingsRoutes.routes(SettingsPage.NOTIFICATION_ACCESS, sdk = 30, tv = false, fire = false, pkg = "p", listener = "p/.L")
        assertEquals(SettingsRoute(SettingsRoutes.ACTION_NOTIFICATION_LISTENER_DETAIL, extras = mapOf(SettingsRoutes.EXTRA_NOTIFICATION_LISTENER_COMPONENT to "p/.L")), access30.first())

        val wireless28 = SettingsRoutes.routes(SettingsPage.WIRELESS_DEBUGGING, sdk = 28, tv = false, fire = false, pkg = "p")
        assertTrue("no Wireless debugging page before Android 11", wireless28.none { it.action == SettingsRoutes.ACTION_QS_TILE_PREFERENCES })
        assertEquals("android.settings.APPLICATION_DEVELOPMENT_SETTINGS", wireless28.first().action)
        val wireless30 = SettingsRoutes.routes(SettingsPage.WIRELESS_DEBUGGING, sdk = 30, tv = false, fire = false, pkg = "p", devOptions = true)
        assertEquals(SettingsRoutes.ACTION_QS_TILE_PREFERENCES, wireless30.first().action)
        assertEquals("com.android.settings", wireless30.first().pkg)
        val beforeDevOptions = SettingsRoutes.routes(SettingsPage.WIRELESS_DEBUGGING, sdk = 30, tv = false, fire = false, pkg = "p", devOptions = false)
        assertTrue("the page only opens once Developer options are on", beforeDevOptions.none { it.action == SettingsRoutes.ACTION_QS_TILE_PREFERENCES })

        val battery = SettingsRoutes.routes(SettingsPage.BATTERY, sdk = 28, tv = true, fire = false, pkg = "p")
        assertEquals(SettingsRoute(SettingsRoutes.ACTION_REQUEST_IGNORE_BATTERY, data = "package:p"), battery[0])
        assertEquals(SettingsRoute(SettingsRoutes.ACTION_IGNORE_BATTERY), battery[1])
        assertEquals(SettingsRoute(component = "com.android.tv.settings" to "com.android.tv.settings.device.apps.AppManagementActivity", data = "package:p"), battery[2])
    }

    @Test fun whenNothingOpensTheMessageNamesTheSettingAndWhereItUsuallyIs() {
        val target = SettingsRoutes.target(SettingsPage.ACCESSIBILITY, sdk = 28, tv = true, fire = false, pkg = "p", label = "Silicon Extend TV")
        assertEquals(
            "This TV didn't let Extend open Accessibility settings. It is usually at Settings › Device Preferences › Accessibility › Silicon Extend TV. " +
                "If this TV's own settings menu doesn't show it, ask its maker how to reach Android's Accessibility settings. " +
                "Or connect Android debugging below and select \"Turn on accessibility through debugging\".",
            target.unavailableMessage(tv = true),
        )
        assertTrue("a phone taps", target.unavailableMessage(tv = false).endsWith("tap \"Turn on accessibility through debugging\"."))
        assertTrue(DebuggingCardCopy.TURN_ON_ACCESSIBILITY in target.unavailableMessage(tv = true))
        val about = SettingsRoutes.target(SettingsPage.ABOUT, sdk = 28, tv = true, fire = false, pkg = "p", label = "x").unavailableMessage(tv = true)
        assertTrue(about, about.startsWith("This TV didn't let Extend open the About screen. It is usually at Settings › Device Preferences › About."))
        assertEquals("Settings › Device Preferences › About", SettingsRoutes.where(SettingsPage.ABOUT, 28, tv = true, fire = false, label = "x"))
        assertEquals("Settings › My Fire TV › Developer options", SettingsRoutes.where(SettingsPage.DEVELOPER_OPTIONS, 28, tv = true, fire = true, label = "x"))
    }

    // ───────────── Clipboard ─────────────

    @Test fun onlyAndroid10AndLaterNeedFocusToReadTheClipboard() {
        for (sdk in 26..28) assertFalse("API $sdk: any app may read it", ClipboardActivity.needsFocus(sdk))
        for (sdk in listOf(29, 30, 36)) assertTrue("API $sdk", ClipboardActivity.needsFocus(sdk))
    }

    // ───────────── Recording before Android 9 ─────────────

    @Test fun beforeAndroid9TheFrameBufferGrowsUntilAFrameFits() {
        // MediaExtractor.getSampleSize is Android 9+; older versions grow the buffer on each "too small".
        assertEquals(2 * 1024 * 1024, RecordingMuxer.grownCapacity(1024 * 1024))
        assertEquals(32 * 1024 * 1024, RecordingMuxer.grownCapacity(24 * 1024 * 1024))
        assertNull("no frame is larger than 32 MiB", RecordingMuxer.grownCapacity(32 * 1024 * 1024))
    }
}

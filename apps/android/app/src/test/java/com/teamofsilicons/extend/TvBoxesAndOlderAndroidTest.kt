package com.teamofsilicons.extend

import com.teamofsilicons.extend.adb.DebuggingAfterRestart
import com.teamofsilicons.extend.config.DeviceInfo
import com.teamofsilicons.extend.core.SetupReport
import com.teamofsilicons.extend.core.SetupSignals
import com.teamofsilicons.extend.display.DisplayActivity
import com.teamofsilicons.extend.driver.Capabilities as C
import com.teamofsilicons.extend.driver.ForegroundWait
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * TVs, TV boxes and projectors, which often run Android 8–12: which devices count as TVs, how long
 * the app waits for its own screens after Home, what a TV without Android debugging says about its
 * remote, and why `display show --url` is refused on a device without a web view.
 */
class TvBoxesAndOlderAndroidTest {
    @Test fun aBoxOnThePhoneBuildWithoutATouchscreenIsATv() {
        val touch = "android.hardware.touchscreen"
        assertTrue("Android TV", DeviceInfo.looksLikeTv(false, setOf("android.software.leanback")))
        assertTrue("TV UI mode", DeviceInfo.looksLikeTv(true, setOf(touch)))
        assertTrue("Fire TV", DeviceInfo.looksLikeTv(false, setOf("amazon.hardware.fire_tv")))
        assertTrue("a TV box or projector with its maker's launcher", DeviceInfo.looksLikeTv(false, emptySet()))
        assertFalse("a phone or tablet", DeviceInfo.looksLikeTv(false, setOf(touch)))
        for (computerOrCar in listOf("android.hardware.type.pc", "org.chromium.arc", "android.hardware.type.automotive", "android.hardware.type.watch")) {
            assertFalse(computerOrCar, DeviceInfo.looksLikeTv(false, setOf(computerOrCar)))
        }
    }

    @Test fun upToAndroid11TheWaitCoversTheFiveSecondAppSwitchWindowAfterHome() {
        // Seen on the Android 8.0 and 9 emulators: `display show` right after `home` came to the
        // front about 5 s later, after a 4 s wait had already reported a failure.
        for (sdk in 26..30) {
            assertEquals("API $sdk display/clipboard", 6_500L, ForegroundWait.ms(sdk, 4_000))
            assertEquals("API $sdk open", 6_500L, ForegroundWait.ms(sdk, 5_000))
            assertTrue(ForegroundWait.ms(sdk, 4_000) > ForegroundWait.APP_SWITCH_DELAY_MS)
        }
        for (sdk in listOf(31, 33, 36)) {
            assertEquals("API $sdk keeps its usual wait", 4_000L, ForegroundWait.ms(sdk, 4_000))
            assertEquals(5_000L, ForegroundWait.ms(sdk, 5_000))
        }
        assertEquals("6.5 s", ForegroundWait.seconds(6_500))
        assertEquals("4 s", ForegroundWait.seconds(4_000))
    }

    @Test fun anAndroid9TvWithoutDebuggingSaysWhatTheRemoteNeedsAndWhatStillWorks() {
        val tv9 = SetupReport.build(tv(28))
        assertFalse("no D-pad through accessibility before Android 13", C.INPUT_REMOTE in tv9.capabilities)
        assertTrue(C.NAV_SYSTEM in tv9.capabilities)
        assertTrue(C.DISPLAY in tv9.capabilities)
        val reason = tv9.missing.first { it.capability == C.INPUT_REMOTE }.reason
        assertTrue(reason, reason.startsWith("Remote buttons on this TV (Android 9) need Android debugging"))
        assertTrue(reason, reason.contains("Settings › Device Preferences › Developer options › Network debugging"))
        assertTrue(reason, reason.contains("the back and home commands work"))

        val fire = SetupReport.build(tv(28, fire = true)).missing.first { it.capability == C.INPUT_REMOTE }.reason
        assertTrue(fire, fire.contains("Settings › My Fire TV › Developer options › ADB debugging › On"))

        assertTrue("with Android debugging every button is a key press", C.INPUT_REMOTE in SetupReport.build(tv(28, adb = true)).capabilities)
    }

    @Test fun aPageNeedsAWebViewAndTheRefusalSaysWhatWorksInstead() {
        val m = DisplayActivity.NO_WEBVIEW
        assertTrue(m, m.startsWith("This device has no web view"))
        for (alternative in listOf("display show --image", "--text", "open <url>")) assertTrue(m, m.contains(alternative))
    }

    private fun tv(sdk: Int, fire: Boolean = false, adb: Boolean = false) = SetupSignals(
        sdk = sdk, release = mapOf(26 to "8.0", 28 to "9", 29 to "10", 31 to "12")[sdk] ?: "$sdk",
        tv = true, fire = fire, pkg = "com.teamofsilicons.extend", label = "Silicon Extend TV", app = "Silicon Extend TV",
        listener = "com.teamofsilicons.extend/.notif.ExtendNotificationListener",
        a11yConnected = true, a11yEnabled = true, listenerConnected = false, listenerGranted = false, postGranted = true,
        batteryOk = true, batteryRequestResolvable = false, devOptions = false, adbWifi = false, adbOn = false,
        adbConnected = adb, adbLastError = null, afterRestart = DebuggingAfterRestart.Status.NONE, dpadSupported = sdk >= 33,
    )
}

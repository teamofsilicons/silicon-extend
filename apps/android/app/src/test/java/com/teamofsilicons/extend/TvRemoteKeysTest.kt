package com.teamofsilicons.extend

import com.teamofsilicons.extend.driver.CommandParser
import com.teamofsilicons.extend.driver.TvRemoteKeys
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/** `tv-remote` through Android debugging: real key events, so Menu and TVs before Android 13 work. */
class TvRemoteKeysTest {
    @Test fun everyButtonButPowerIsAnAndroidKeyCode() {
        assertEquals(
            mapOf(
                "up" to 19, "down" to 20, "left" to 21, "right" to 22, "select" to 23, "back" to 4, "home" to 3,
                "menu" to 82, "play-pause" to 85, "volume-up" to 24, "volume-down" to 25, "mute" to 164,
            ),
            TvRemoteKeys.KEYCODES,
        )
        assertEquals("every button the command takes is mapped, except power", CommandParser.TV_BUTTONS.toSet() - "power", TvRemoteKeys.KEYCODES.keys)
    }

    @Test fun pressesAndHoldsBecomeInputKeyevent() {
        assertEquals("input keyevent 82", TvRemoteKeys.command("menu", longPress = false, durationMs = null, sdk = 30))
        assertEquals("input keyevent 19", TvRemoteKeys.command("up", false, null, 31))
        assertEquals("input keyevent --longpress 23", TvRemoteKeys.command("select", true, null, 34))
        assertEquals("input keyevent --duration 1500 3", TvRemoteKeys.command("home", true, 1500, 33))
        // Android 12 and older have no --duration: a long press is Android's own long-press time.
        assertEquals("input keyevent --longpress 3", TvRemoteKeys.command("home", true, 1500, 31))
    }

    @Test fun powerIsRefusedWithItsReason() {
        try {
            TvRemoteKeys.command("power", false, null, 34)
            fail("power must be refused")
        } catch (e: IllegalArgumentException) {
            assertEquals(TvRemoteKeys.POWER_REFUSAL, e.message)
        }
        assertTrue(TvRemoteKeys.POWER_REFUSAL.startsWith("The Extend app won't press power"))
    }

    @Test fun remoteButtonsAreReportedWhenEitherPathWorks() {
        val a11y = "Turn on Silicon Extend TV in accessibility settings: Settings › Accessibility › Silicon Extend TV › Enable."
        // Android 12 TV: only Android debugging can press the D-pad.
        assertNull(TvRemoteKeys.missingReason(dpadSupported = false, a11yConnected = true, adbConnected = true, a11yReason = a11y, androidRelease = "12"))
        assertEquals(
            "Remote buttons on this TV (Android 12) need Android debugging: turn on network debugging in Developer options, then connect Android debugging in the Extend app's setup.",
            TvRemoteKeys.missingReason(false, true, false, a11y, "12"),
        )
        // Android 13+: accessibility or Android debugging.
        assertNull(TvRemoteKeys.missingReason(true, true, false, a11y, "14"))
        assertNull(TvRemoteKeys.missingReason(true, false, true, a11y, "14"))
        assertEquals("$a11y Or connect Android debugging in the Extend app's setup.", TvRemoteKeys.missingReason(true, false, false, a11y, "14"))
    }
}

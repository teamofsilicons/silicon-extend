package com.teamofsilicons.extend

import com.teamofsilicons.extend.a11y.KeepScreenOn
import com.teamofsilicons.extend.core.AddPairMessages
import com.teamofsilicons.extend.core.AwakeUi
import com.teamofsilicons.extend.core.Link
import com.teamofsilicons.extend.core.Links
import com.teamofsilicons.extend.core.PairUi
import com.teamofsilicons.extend.core.SessionUi
import com.teamofsilicons.extend.core.SetupRetry
import com.teamofsilicons.extend.core.TakeoverUi
import com.teamofsilicons.extend.core.UiState
import com.teamofsilicons.extend.driver.ClickFallback
import com.teamofsilicons.extend.driver.ClickFallback.Method
import com.teamofsilicons.extend.net.ApiException
import com.teamofsilicons.extend.protocol.Member
import com.teamofsilicons.extend.ui.SHARED_DEVICE_NOTE
import com.teamofsilicons.extend.ui.pairedToLine
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Several Carbons on one device, keeping the screen on, clicks on TVs and setup retries. */
class MultiCarbonTest {
    private val alice = PairUi("a1", "Living room TV", Member("carbon", "c:alice", "Alice"), firstPair = true)
    private val bob = PairUi("b2", "Family TV", Member("carbon", "c:bob"), firstPair = false)

    @Test fun thePairedScreenNamesEveryCarbonAndTheirNameForTheDevice() {
        assertEquals("Paired to Alice (c:alice)", pairedToLine(listOf(alice)))
        assertEquals("Paired to c:alice (Living room TV) · c:bob (Family TV)", pairedToLine(listOf(alice, bob)))
        assertEquals("Paired · device a1", pairedToLine(listOf(PairUi("a1"))))
        assertTrue(SHARED_DEVICE_NOTE.startsWith("Silicons any Carbon gives access to can use this whole device, including what others leave on it"))
    }

    @Test fun theConnectionSumsUpItsPairs() {
        assertEquals(Link.CONNECTED to null, Links.overall(listOf(alice.copy(link = Link.CONNECTED), bob.copy(link = Link.OFFLINE))))
        val (link, detail) = Links.overall(listOf(alice.copy(link = Link.CONNECTED), bob.copy(link = Link.SUPERSEDED)))
        assertEquals("what needs the Carbon comes first", Link.SUPERSEDED, link)
        assertEquals("Another connection took over c:bob's pair on this device. Tap Reconnect to use this one.", detail)
        assertEquals(Link.OFFLINE, Links.overall(listOf(alice.copy(link = Link.OFFLINE, linkDetail = "Reconnecting in 4 s"))).first)
        assertEquals("Another connection for this device took over. Tap Reconnect to use this one.", Links.overall(listOf(alice.copy(link = Link.SUPERSEDED))).second)
    }

    @Test fun addingAPairExplainsAFinalRefusal() {
        val old = ApiException(404, "not_found", "No such endpoint: POST /api/v1/device/enrollments")
        assertTrue(AddPairMessages.final(old))
        assertEquals(
            "The Extend service this TV uses is too old for this. Pairing with another Carbon needs Silicon Extend 1.1 on the service.",
            AddPairMessages.refused(old, tv = true),
        )
        val limit = ApiException(409, "conflict", "This device is paired to 8 Carbons, the most Extend allows.")
        assertTrue(AddPairMessages.final(limit))
        assertEquals("This device is paired to 8 Carbons, the most Extend allows.", AddPairMessages.refused(limit, tv = false))
        assertFalse("a rate limit is waited out", AddPairMessages.final(ApiException(429, "rate_limited", "Slow down", retryAfterS = 30)))
        assertFalse(AddPairMessages.final(java.io.IOException("offline")))
    }

    private fun state(session: SessionUi? = SessionUi("si:chef", "a3f", "x"), awake: AwakeUi? = AwakeUi(true), takeover: TakeoverUi? = null) =
        UiState(session = session, awake = awake, takeover = takeover)

    @Test fun theScreenStaysOnOnlyWhileASiliconWorksOnAnAwakeDevice() {
        assertTrue(KeepScreenOn.wanted(state()))
        assertFalse("no session", KeepScreenOn.wanted(state(session = null)))
        assertFalse("not awake: never turn a screen on", KeepScreenOn.wanted(state(awake = AwakeUi(false, "locked"))))
        assertFalse("awake unknown", KeepScreenOn.wanted(state(awake = null)))
        assertFalse("stopping", KeepScreenOn.wanted(state(session = SessionUi("si:chef", "a3f", "x", stopping = true))))
        assertFalse("the Carbon is at the device", KeepScreenOn.wanted(state(takeover = TakeoverUi("a3f", "Approve", null))))
    }

    @Test fun aClickTheScreenIgnoredFallsBackToSelectOrATap() {
        assertEquals(listOf(Method.ADB_SELECT, Method.ACCESSIBILITY_SELECT, Method.GESTURE_TAP), ClickFallback.order(tv = true, adbConnected = true, focused = true, dpadSupported = true))
        assertEquals("a TV before Android 13 without debugging: only a tap is left", listOf(Method.GESTURE_TAP), ClickFallback.order(tv = true, adbConnected = false, focused = true, dpadSupported = false))
        assertEquals(listOf(Method.ACCESSIBILITY_SELECT, Method.GESTURE_TAP), ClickFallback.order(tv = true, adbConnected = false, focused = true, dpadSupported = true))
        assertEquals("an element that won't take focus is tapped", listOf(Method.ADB_TAP, Method.GESTURE_TAP), ClickFallback.order(tv = true, adbConnected = true, focused = false, dpadSupported = true))
        assertEquals(listOf(Method.ADB_TAP, Method.GESTURE_TAP), ClickFallback.order(tv = false, adbConnected = true, focused = false, dpadSupported = true))
        assertEquals(listOf(Method.GESTURE_TAP), ClickFallback.order(tv = false, adbConnected = false, focused = false, dpadSupported = true))
        assertEquals("input keyevent 23", ClickFallback.adbCommand(Method.ADB_SELECT, 10, 20, 28))
        assertEquals("input tap 640 360", ClickFallback.adbCommand(Method.ADB_TAP, 640, 360, 28))
        assertEquals("input tap 0 5", ClickFallback.adbCommand(Method.ADB_TAP, -3, 5, 34))
    }

    @Test fun aRetryRunsTheNamedFailedStepOrEveryOne() {
        val failed = listOf("accessibility", "wireless_debugging")
        assertEquals(failed, SetupRetry.select(failed, null))
        assertEquals(listOf("wireless_debugging"), SetupRetry.select(failed, "wireless_debugging"))
        assertEquals("a step that hasn't failed runs nothing", emptyList<String>(), SetupRetry.select(failed, "background"))
        SetupRetry.start(listOf("wireless_debugging"), now = 1_000)
        assertEquals(setOf("wireless_debugging"), SetupRetry.active(now = 2_000))
        assertEquals("a retry that never reports back stops showing", emptySet<String>(), SetupRetry.active(now = 1_000 + SetupRetry.SHOW_MS))
        SetupRetry.start(listOf("accessibility"), now = 0)
        SetupRetry.finished(listOf("accessibility"))
        assertEquals(emptySet<String>(), SetupRetry.active(now = 1))
    }
}

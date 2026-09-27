package com.teamofsilicons.extend

import com.teamofsilicons.extend.adb.DebuggingAfterRestart
import com.teamofsilicons.extend.adb.DebuggingAfterRestart.Status
import com.teamofsilicons.extend.core.SetupItem
import com.teamofsilicons.extend.core.SetupReport
import com.teamofsilicons.extend.driver.Capabilities
import com.teamofsilicons.extend.protocol.DeviceFrame
import com.teamofsilicons.extend.protocol.Frames
import com.teamofsilicons.extend.protocol.SetupStep
import com.teamofsilicons.extend.service.DebuggingOffMessage
import com.teamofsilicons.extend.ui.DebuggingCardCopy
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * UNDERSTANDING.md: "After the device restarts, wireless debugging turns off, and the app asks the
 * Carbon to turn it back on." The setup step then needs the Carbon, which Extend (and the website)
 * learn from `hello` / `setup_progress`; the device stays ready, since only debugging is missing.
 */
class DebuggingAfterRestartTest {
    private fun status(
        enabled: Boolean = true, tls: Boolean = true, connected: Boolean = false,
        lastBoot: Int? = 6, boot: Int = 7, wirelessOn: Boolean = false,
    ) = DebuggingAfterRestart.status(enabled, tls, connected, lastBoot, boot, wirelessOn)

    @Test fun onlyARestartSinceDebuggingLastConnectedAsksTheCarbon() {
        assertEquals(Status.OFF, status())
        assertEquals(Status.RECONNECTING, status(wirelessOn = true))
        assertEquals("connected now", Status.NONE, status(connected = true))
        assertEquals("never connected", Status.NONE, status(lastBoot = null))
        assertEquals("no restart since it connected", Status.NONE, status(lastBoot = 7))
        assertEquals("the Carbon disconnected it in the app", Status.NONE, status(enabled = false))
        assertEquals("a TV's legacy port survives restarts", Status.NONE, status(tls = false))
        assertEquals("Android doesn't count boots", Status.NONE, status(boot = -1))
    }

    private val required = listOf(
        SetupItem(SetupStep("accessibility", "Allow Silicon Extend to control the screen", "done"), true, null, null),
        SetupItem(SetupStep("notifications", "Allow notifications", "done"), true, null, null),
    )

    @Test fun theStepNeedsTheCarbonInHelloButTheDeviceStaysReady() {
        val step = SetupReport.restartStep(Status.OFF, tv = false, lastError = null, open = null)!!
        assertEquals("wireless_debugging", step.step.key)
        assertEquals("needs_carbon", step.step.status)
        assertEquals("Open Wireless debugging", step.actionLabel)
        assertTrue(step.step.help!!, step.step.help!!.startsWith("This phone restarted, and Android turns wireless debugging off when it restarts."))
        assertTrue(step.step.help!!, step.step.help!!.contains("Silicons can still read and control the screen"))
        assertTrue(step.step.help!!.contains("Settings › System › Developer options › Wireless debugging › On"))

        // Debugging only adds installation, logs, recording and adb. A restart turning it off must
        // not hold setup back: Extend marks a device `ready` only while setup is `complete`, and a
        // device that isn't ready refuses new sessions (device_not_ready) although accessibility
        // still works.
        assertFalse("the after-restart step must not block the device", step.required)
        val report = SetupReport(required + step, emptyList(), emptyList(), Status.OFF)
        assertEquals("complete", report.setup.state)
        assertTrue("the app points the Carbon at the step", report.optionalNeedsCarbon)

        val hello = Frames.encode(
            DeviceFrame.Hello(appVersion = "1.0.0", os = "android", capabilities = emptyList(), missing = emptyList(), setup = report.setup),
        )
        assertTrue(hello, hello.contains("\"state\":\"complete\""))
        assertTrue(hello, hello.contains("\"key\":\"wireless_debugging\",\"title\":\"Turn wireless debugging back on\",\"status\":\"needs_carbon\""))
        val progress = Frames.encode(DeviceFrame.SetupProgress(report.setup))
        assertTrue(progress, progress.contains("\"status\":\"needs_carbon\""))
    }

    @Test fun aRequiredStepStillHoldsSetupBack() {
        // The after-restart step is optional, but the required steps keep deciding the state.
        val step = SetupReport.restartStep(Status.OFF, tv = false, lastError = null, open = null)!!
        val a11yOff = SetupItem(SetupStep("accessibility", "Allow Silicon Extend to control the screen", "needs_carbon"), true, null, null)
        val report = SetupReport(listOf(a11yOff, required[1], step), emptyList(), emptyList(), Status.OFF)
        assertEquals("needs_carbon", report.setup.state)
    }

    @Test fun reconnectingIsInProgressWithTheLastError() {
        val step = SetupReport.restartStep(Status.RECONNECTING, tv = false, lastError = "Could not find this device's debugging port.", open = null)!!
        assertEquals("in_progress", step.step.status)
        assertEquals("Last try: Could not find this device's debugging port.", step.step.error)
        assertFalse(step.required)
        val report = SetupReport(required + step, emptyList(), emptyList(), Status.RECONNECTING)
        assertEquals("complete", report.setup.state)
        assertFalse("reconnecting needs nothing from the Carbon", report.optionalNeedsCarbon)
        val tv = SetupReport.restartStep(Status.OFF, tv = true, lastError = null, open = null)!!
        assertEquals("network_debugging", tv.step.key)
        assertTrue(tv.step.help!!.startsWith("This TV restarted"))
    }

    @Test fun whileDebuggingIsOffAfterARestartTheAppOffersTheDisconnectTheStepNames() {
        // The step tells the Carbon they can stop using debugging with this button, so the card
        // must offer it while debugging is paired but not connected (it used to appear only once
        // connected).
        val step = SetupReport.restartStep(Status.OFF, tv = false, lastError = null, open = null)!!
        assertTrue(step.step.help!!, step.step.help!!.contains("tap ${DebuggingCardCopy.DISCONNECT} in the Extend app"))
        assertTrue(DebuggingCardCopy.offersDisconnect(connected = false, enabled = true))
        assertTrue(DebuggingCardCopy.offersDisconnect(connected = true, enabled = true))
        assertFalse("never paired: nothing to disconnect", DebuggingCardCopy.offersDisconnect(connected = false, enabled = false))

        val off = DebuggingCardCopy.text(connected = false, enabled = true, wirelessDebuggingOff = true)
        assertTrue(off, off.startsWith("Paired, but not connected: Wireless debugging is off (Android turns it off when this device restarts)."))
        assertTrue(off, off.contains("Turn it back on in Developer options and Extend reconnects by itself."))
        val reconnecting = DebuggingCardCopy.text(connected = false, enabled = true, wirelessDebuggingOff = false)
        assertTrue(reconnecting, reconnecting.startsWith("Paired, but not connected right now; Extend keeps reconnecting."))
        assertTrue(DebuggingCardCopy.text(connected = false, enabled = false, wirelessDebuggingOff = true).startsWith("Enable Wireless debugging"))
    }

    @Test fun withoutARestartTheStepStaysOptional() {
        assertNull(SetupReport.restartStep(Status.NONE, tv = false, lastError = null, open = null))
        val optional = SetupItem(SetupStep("wireless_debugging", "Turn on wireless debugging", "todo"), false, null, null)
        assertEquals("complete", SetupReport(required + optional, emptyList(), emptyList()).setup.state)
    }

    @Test fun theNotificationAndMissingReasonsSayWhatHappenedWhyAndWhatToDo() {
        val m = DebuggingOffMessage.of(tv = false)
        assertEquals("Turn wireless debugging back on", m.title)
        assertEquals("This phone restarted, which turned wireless debugging off.", m.text)
        assertTrue(m.bigText, m.bigText.contains("Silicons can still read and control the screen, but until it is back on they can't install apps, read device logs, record the screen or run adb commands here"))
        assertTrue(m.bigText.contains("Tap to open Wireless debugging and turn it on; Extend reconnects by itself."))
        assertEquals("Open Wireless debugging", m.action)
        val reason = Capabilities.afterRestartReason("Installing apps", tv = false)
        assertEquals(
            "Installing apps needs Android debugging, and Android turned Wireless debugging off when this phone restarted. " +
                "The Carbon turns it back on in Settings › System › Developer options › Wireless debugging; Extend then reconnects by itself.",
            reason,
        )
    }
}

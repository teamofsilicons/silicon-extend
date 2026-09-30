package com.teamofsilicons.extend

import com.teamofsilicons.extend.driver.ClickFallback.Method
import com.teamofsilicons.extend.driver.Cmd
import com.teamofsilicons.extend.driver.CommandFailure
import com.teamofsilicons.extend.driver.CommandParser
import com.teamofsilicons.extend.driver.CoordinateTap
import com.teamofsilicons.extend.driver.Target
import com.teamofsilicons.extend.driver.TextInputErrors
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.*
import org.junit.Test
import java.io.EOFException

class InputRoutingTest {
    private class Input {
        val sent = ArrayList<String>()
        var adbResult: () -> Int = { 0 }
        var gestureResult = true
        suspend fun execute(command: Cmd.Click, connected: Boolean) = CoordinateTap.execute(command, connected,
            adbTap = { x, y -> sent += "adb $x $y"; adbResult() },
            gestureTap = { x, y -> sent += "gesture $x $y"; gestureResult },
        )
    }

    @Test fun plainClickAndPressUseTheConnectedDebuggingTransportExactlyOnce() = runBlocking {
        for (verb in listOf("click", "press")) {
            val input = Input()
            val command = CommandParser.parse(verb, listOf("450", "615")) as Cmd.Click
            assertEquals(Method.ADB_TAP, input.execute(command, connected = true))
            assertEquals(listOf("adb 450 615"), input.sent)
        }
    }

    @Test fun devicesWithoutDebuggingKeepTheAccessibilityTap() = runBlocking {
        val input = Input()
        assertEquals(Method.GESTURE_TAP, input.execute(Cmd.Click(Target.Point(450, 615)), connected = false))
        assertEquals(listOf("gesture 450 615"), input.sent)
    }

    @Test fun aLostAdbAcknowledgementNeverDuplicatesAnAlreadySentTap() = runBlocking {
        val input = Input().apply { adbResult = { throw EOFException("Tap was sent but shell lost its exit frame") } }
        val error = try {
            input.execute(Cmd.Click(Target.Point(450, 615)), connected = true)
            fail("An unconfirmed tap must fail")
            throw AssertionError()
        } catch (e: CommandFailure) { e }
        assertEquals(listOf("adb 450 615"), input.sent)
        assertEquals(CommandFailure.ACTION_FAILED, error.code)
        assertEquals("unconfirmed", error.details!!.jsonObject["delivery"]!!.jsonPrimitive.content)
        assertFalse(error.details!!.jsonObject["replayed"]!!.jsonPrimitive.boolean)
        assertTrue(error.message!!.contains("Inspect the screen before retrying"))
    }

    @Test fun aNonzeroAdbExitDoesNotReplayThroughAccessibility() = runBlocking {
        val input = Input().apply { adbResult = { 1 } }
        val error = try {
            input.execute(Cmd.Click(Target.Point(450, 615)), connected = true)
            fail("A rejected shell command must fail")
            throw AssertionError()
        } catch (e: CommandFailure) { e }
        assertEquals(listOf("adb 450 615"), input.sent)
        assertEquals(1, error.details!!.jsonObject["exitCode"]!!.jsonPrimitive.int)
    }

    @Test fun cancellationDoesNotSendAnotherTapOrBecomeADeviceFailure() = runBlocking {
        val cancelled = CancellationException("session ended")
        val input = Input().apply { adbResult = { throw cancelled } }
        try {
            input.execute(Cmd.Click(Target.Point(450, 615)), connected = true)
            fail("Cancellation must propagate")
        } catch (e: CancellationException) { assertSame(cancelled, e) }
        assertEquals(listOf("adb 450 615"), input.sent)
    }

    @Test fun refsRepeatedPressesAndHoldsStayWithTheirExistingExecutor() = runBlocking {
        for (command in listOf(
            Cmd.Click(Target.Ref("@e2")),
            Cmd.Click(Target.Point(450, 615), count = 2),
            Cmd.Click(Target.Point(450, 615), holdMs = 500),
        )) {
            val input = Input()
            assertNull(input.execute(command, connected = true))
            assertTrue(input.sent.isEmpty())
        }
    }

    @Test fun aVisibleKeyboardWithHiddenFocusDoesNotClaimFocusIsMissing() {
        for (connected in listOf(false, true)) {
            val error = TextInputErrors.missingFocusedField(keyboardVisible = true, adbConnected = connected)
            assertEquals("text_input_unavailable", error.code)
            val details = error.details!!.jsonObject
            assertTrue(details["keyboardVisible"]!!.jsonPrimitive.boolean)
            assertFalse(details["focusedFieldObservable"]!!.jsonPrimitive.boolean)
            assertFalse(details["inputSent"]!!.jsonPrimitive.boolean)
            assertEquals(connected, details["adbConnected"]!!.jsonPrimitive.boolean)
            assertTrue(error.message!!.contains("No text was sent"))
            assertFalse(error.message!!.contains("No text field has input focus"))
            assertTrue(error.message!!.contains(if (connected) "debugging is connected" else "Extend app's setup"))
        }
    }

    @Test fun noKeyboardRetainsTheOrdinaryFocusError() {
        val error = TextInputErrors.missingFocusedField(keyboardVisible = false, adbConnected = false)
        assertEquals("text_input_not_focused", error.code)
        assertEquals("No text field has input focus. Focus one first (press @ref or focus @ref), or use fill @ref \"text\".", error.message)
    }
}

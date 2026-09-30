package com.teamofsilicons.extend

import com.teamofsilicons.extend.driver.AdbTextInput
import com.teamofsilicons.extend.driver.CommandFailure
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import java.io.EOFException
import java.io.File
import java.io.IOException
import java.util.concurrent.TimeUnit

class AdbTextInputTest {
    private class Input {
        val commands = ArrayList<String>()
        var result: () -> Int = { 0 }
        suspend fun send(text: String, delay: Long? = null, keyboard: Boolean = true, connected: Boolean = true) =
            AdbTextInput.execute(text, delay, keyboard, connected) { command -> commands += command; result() }
    }

    @Test fun aHiddenSearchFieldReceivesOneInputAndReportsItAsUnverified() = runBlocking {
        val input = Input()
        val outcome = input.send("NH1 Bowls")
        assertEquals(listOf("'input' 'text' 'NH1%sBowls'"), input.commands)
        val output = outcome.output.jsonObject
        assertEquals("adb_text", output["method"]!!.jsonPrimitive.content)
        assertEquals(9, output["chars"]!!.jsonPrimitive.int)
        assertTrue(output["inputDispatched"]!!.jsonPrimitive.boolean)
        assertFalse(output["verified"]!!.jsonPrimitive.boolean)
        assertEquals("unavailable", output["verification"]!!.jsonPrimitive.content)
        assertTrue(output["warning"]!!.jsonPrimitive.content.contains("Inspect the screen before retrying"))
        assertFalse("result must not leak typed text", outcome.toString().contains("NH1 Bowls"))
    }

    @Test fun shellMetacharactersRemainOneLiteralTextArgument() = runBlocking {
        assumeTrue(File("/bin/sh").isFile)
        val input = Input()
        val text = "NH1 Bowls'; printf INJECTED; # ${'$'}(printf SUBSTITUTED) `printf BACKTICKS` \"${'$'}EXTEND_TEST_VALUE\" & | < > \\ 50%"
        input.send(text)
        // Exercise a real shell's argument parser; the fixture only records input's arguments.
        val process = ProcessBuilder("/bin/sh", "-c", "input() { printf '%s\\000' \"${'$'}@\"; }\n${input.commands.single()}")
            .redirectErrorStream(true).apply { environment()["EXTEND_TEST_VALUE"] = "EXPANDED" }.start()
        assertTrue("shell fixture timed out", process.waitFor(5, TimeUnit.SECONDS))
        val output = process.inputStream.readBytes().toString(Charsets.UTF_8)
        assertEquals(0, process.exitValue())
        assertEquals("text\u0000${text.replace(" ", "%s")}\u0000", output)
    }

    @Test fun unsupportedTextAndPacingAreRejectedBeforeAnyInput() = runBlocking {
        for ((text, delay, reason) in listOf(
            Triple("café", null, "unsupported_characters"),
            Triple("नमस्ते", null, "unsupported_characters"),
            Triple("line\nfeed", null, "unsupported_characters"),
            Triple("tabs\there", null, "unsupported_characters"),
            Triple("literal %s", null, "literal_percent_s"),
            Triple("NH1 Bowls", 25L, "delay_not_supported"),
        )) {
            val input = Input()
            val error = try {
                input.send(text, delay)
                fail("Unsupported input must be rejected")
                throw AssertionError()
            } catch (e: CommandFailure) { e }
            assertTrue(input.commands.isEmpty())
            assertEquals("text_input_unsupported", error.code)
            assertEquals(reason, error.details!!.jsonObject["reason"]!!.jsonPrimitive.content)
            assertFalse(error.details!!.jsonObject["inputSent"]!!.jsonPrimitive.boolean)
        }
    }

    @Test fun missingKeyboardOrDebuggingKeepsTheExistingErrorsWithoutSending() = runBlocking {
        for ((keyboard, connected, code) in listOf(
            Triple(false, true, "text_input_not_focused"),
            Triple(false, false, "text_input_not_focused"),
            Triple(true, false, "text_input_unavailable"),
        )) {
            val input = Input()
            val error = try {
                input.send("NH1 Bowls", keyboard = keyboard, connected = connected)
                fail("Input needs a visible keyboard and connected debugging")
                throw AssertionError()
            } catch (e: CommandFailure) { e }
            assertEquals(code, error.code)
            assertTrue(input.commands.isEmpty())
        }
    }

    @Test fun transportErrorsNeverReplayTextWhichMayAlreadyHaveArrived() = runBlocking {
        for (failure in listOf(IOException("connection dropped"), EOFException("input sent but exit frame lost"))) {
            val input = Input().apply { result = { throw failure } }
            val error = try {
                input.send("NH1 Bowls")
                fail("Unconfirmed input must fail")
                throw AssertionError()
            } catch (e: CommandFailure) { e }
            assertEquals(1, input.commands.size)
            assertEquals(CommandFailure.ACTION_FAILED, error.code)
            assertEquals("unconfirmed", error.details!!.jsonObject["delivery"]!!.jsonPrimitive.content)
            assertFalse(error.details!!.jsonObject["replayed"]!!.jsonPrimitive.boolean)
            assertTrue(error.message!!.contains("Text may already have been entered"))
        }
    }

    @Test fun aNonzeroExitDoesNotSendASecondInput() = runBlocking {
        val input = Input().apply { result = { 1 } }
        val error = try {
            input.send("NH1 Bowls")
            fail("A failed shell command must fail")
            throw AssertionError()
        } catch (e: CommandFailure) { e }
        assertEquals(1, input.commands.size)
        assertEquals(1, error.details!!.jsonObject["exitCode"]!!.jsonPrimitive.int)
    }

    @Test fun cancellationPropagatesWithoutReplayingInput() = runBlocking {
        val cancelled = CancellationException("session ended after dispatch")
        val input = Input().apply { result = { throw cancelled } }
        try {
            input.send("NH1 Bowls")
            fail("Cancellation must propagate")
        } catch (e: CancellationException) { assertSame(cancelled, e) }
        assertEquals(1, input.commands.size)
    }
}

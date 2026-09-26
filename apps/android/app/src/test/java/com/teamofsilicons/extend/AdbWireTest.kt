package com.teamofsilicons.extend

import com.teamofsilicons.extend.adb.AdbWire
import org.junit.Assert.*
import org.junit.Test
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.IOException
import java.nio.file.Files

class AdbWireTest {
    private fun frame(channel: Int, data: ByteArray) = byteArrayOf(channel.toByte()) + AdbWire.intBytes(data.size) + data
    @Test fun binaryOutputAndExitCode() {
        val payload = byteArrayOf(0, -1, 13, 10, 65)
        val frames = frame(1, payload) + frame(2, "error".toByteArray()) + frame(3, byteArrayOf(9))
        val result = AdbWire.shell(frames.inputStream())
        assertArrayEquals(payload, result.stdout)
        assertEquals("error", result.stderr.toString(Charsets.UTF_8))
        assertEquals(9, result.exitCode)
    }
    @Test fun truncatedAndOversizedResultsFail() {
        for (bytes in listOf(byteArrayOf(), byteArrayOf(1, 4, 0), frame(1, ByteArray(20)), frame(3, byteArrayOf()))) {
            try { AdbWire.shell(bytes.inputStream(), 10); fail("must refuse invalid frames") } catch (_: IOException) { }
        }
    }
    @Test fun shellArgumentsStayLiteral() {
        assertEquals("'a' 'two words' 'it'\"'\"'s' '\$(touch /tmp/bad)'", AdbWire.argv(listOf("a", "two words", "it's", "\$(touch /tmp/bad)")))
    }
    @Test fun syncPacketLengthIsLittleEndian() {
        val output = ByteArrayOutputStream()
        AdbWire.packet(output, "DATA", ByteArray(258))
        assertArrayEquals(byteArrayOf(68, 65, 84, 65, 2, 1, 0, 0), output.toByteArray().take(8).toByteArray())
    }

    @Test fun longOutputSpillsToAFileAndKeepsOnlyTheInlineLimitInMemory() {
        val dir = Files.createTempDirectory("adbwire").toFile()
        try {
            val big = ByteArray(700_000) { (it % 251).toByte() }
            val frames = big.toList().chunked(65_536).fold(ByteArray(0)) { acc, part -> acc + frame(1, part.toByteArray()) } +
                frame(2, "warning".toByteArray()) + frame(3, byteArrayOf(0))
            val spilled = mutableListOf<String>()
            val result = AdbWire.capture(frames.inputStream(), { name -> spilled += name; File(dir, "$name.txt") }, inlineLimit = 256 * 1024)
            assertEquals(0, result.exitCode)
            assertEquals(256 * 1024, result.stdout.inline.size)
            assertArrayEquals(big.copyOf(256 * 1024), result.stdout.inline)
            assertTrue(result.stdout.truncated)
            assertEquals(700_000L, result.stdout.total)
            assertArrayEquals("The spill file holds the whole stream", big, result.stdout.file!!.readBytes())
            assertEquals("warning", result.stderr.text)
            assertFalse(result.stderr.truncated)
            assertEquals(listOf("stdout"), spilled)
        } finally { dir.deleteRecursively() }
    }

    @Test fun outputBeyondTheTotalLimitFailsWithAnExplanationAndLeavesNoFile() {
        val dir = Files.createTempDirectory("adbwire").toFile()
        try {
            val frames = frame(1, ByteArray(60_000)) + frame(1, ByteArray(60_000)) + frame(3, byteArrayOf(0))
            val error = assertThrows(AdbWire.OutputTooLarge::class.java) {
                AdbWire.capture(frames.inputStream(), { File(dir, it) }, inlineLimit = 1000, totalLimit = 100_000)
            }
            assertTrue(error.message!!.contains("adb pull"))
            assertEquals(0, dir.listFiles()!!.size)
        } finally { dir.deleteRecursively() }
    }

    @Test fun shortOutputStaysInline() {
        val frames = frame(1, "hello\n".toByteArray()) + frame(3, byteArrayOf(7))
        val result = AdbWire.capture(frames.inputStream(), { error("no spill expected") })
        assertEquals("hello\n", result.stdout.text)
        assertEquals(7, result.exitCode)
        assertNull(result.stdout.file)
    }
}

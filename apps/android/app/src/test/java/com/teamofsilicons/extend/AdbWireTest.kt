package com.teamofsilicons.extend

import com.teamofsilicons.extend.adb.AdbWire
import org.junit.Assert.*
import org.junit.Test
import java.io.ByteArrayOutputStream
import java.io.IOException

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
}

package com.teamofsilicons.extend.adb

import java.io.ByteArrayOutputStream
import java.io.EOFException
import java.io.InputStream
import java.io.IOException
import java.io.OutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

/** ADB shell-v2 frames retain the remote exit code and keep binary stdout intact. */
object AdbWire {
    const val MAX_OUTPUT = 32 * 1024 * 1024
    data class Result(val stdout: ByteArray, val stderr: ByteArray, val exitCode: Int) {
        val text: String get() = (stdout + stderr).toString(Charsets.UTF_8)
    }
    fun quote(value: String): String = "'" + value.replace("'", "'\"'\"'") + "'"
    fun argv(values: List<String>): String = values.joinToString(" ", transform = ::quote)
    fun intBytes(n: Int): ByteArray = ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN).putInt(n).array()
    fun int(bytes: ByteArray): Int = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).int
    fun exact(input: InputStream, size: Int): ByteArray {
        if (size < 0 || size > MAX_OUTPUT) throw IOException("Invalid ADB frame length: $size")
        val bytes = ByteArray(size)
        var offset = 0
        while (offset < size) {
            val n = input.read(bytes, offset, size - offset)
            if (n < 0) throw EOFException("ADB stream ended before its result")
            offset += n
        }
        return bytes
    }
    fun shell(input: InputStream, limit: Int = MAX_OUTPUT): Result {
        val out = ByteArrayOutputStream()
        val err = ByteArrayOutputStream()
        while (true) {
            val channel = input.read()
            if (channel < 0) throw EOFException("ADB shell closed without an exit status")
            val size = int(exact(input, 4))
            if (size < 0 || size > limit - out.size() - err.size()) throw IOException("ADB output exceeds $limit bytes")
            val bytes = exact(input, size)
            when (channel) {
                1 -> out.write(bytes)
                2 -> err.write(bytes)
                3 -> {
                    if (size != 1) throw IOException("Invalid ADB exit status")
                    return Result(out.toByteArray(), err.toByteArray(), bytes[0].toInt() and 255)
                }
                else -> throw IOException("Unexpected ADB shell channel $channel")
            }
        }
    }
    fun packet(out: OutputStream, id: String, bytes: ByteArray) {
        require(id.length == 4)
        out.write(id.toByteArray(Charsets.US_ASCII)); out.write(intBytes(bytes.size)); out.write(bytes); out.flush()
    }
    fun syncReply(input: InputStream): Pair<String, Int> = exact(input, 4).toString(Charsets.US_ASCII) to int(exact(input, 4))
}

package com.teamofsilicons.extend.adb

import java.io.ByteArrayOutputStream
import java.io.EOFException
import java.io.File
import java.io.InputStream
import java.io.IOException
import java.io.OutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

/** ADB shell-v2 frames retain the remote exit code and keep binary stdout intact. */
object AdbWire {
    /** Upper bound for Extend's own short helper commands, which are read into memory. */
    const val MAX_OUTPUT = 8 * 1024 * 1024
    /** Output of a Silicon's command kept inline in the result, per stream (as the computer `terminal` command does). */
    const val INLINE_LIMIT = 256 * 1024
    /** A Silicon's command may write this much in total; the rest of it goes to files that are uploaded. */
    const val SPILL_LIMIT = 256L * 1024 * 1024

    data class Result(val stdout: ByteArray, val stderr: ByteArray, val exitCode: Int) {
        val text: String get() = (stdout + stderr).toString(Charsets.UTF_8)
    }

    /** One output stream of a Silicon's command: its first [INLINE_LIMIT] bytes, and all of it in [file] when longer. */
    class Stream(val inline: ByteArray, val total: Long, val file: File?) {
        val truncated: Boolean get() = file != null
        val text: String get() = inline.toString(Charsets.UTF_8)
    }
    class Captured(val stdout: Stream, val stderr: Stream, val exitCode: Int)

    /** Output or a file went over one of Extend's limits; Android debugging itself is fine. */
    open class LimitExceeded(message: String) : IOException(message)

    /** The command wrote more than [SPILL_LIMIT] bytes. */
    class OutputTooLarge(limit: Long) : LimitExceeded(
        "The command wrote more than ${limit / (1024 * 1024)} MiB, more than Extend returns for one command, so it was stopped. " +
            "Redirect its output to a file on the device (for example `> /data/local/tmp/out.txt`) and fetch it with " +
            "`extend adb pull`, which takes files up to ${LocalAdb.PULL_LIMIT / (1024 * 1024)} MiB; split a larger file first " +
            "(`split -b 200m`).",
    )

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

    /**
     * Reads a Silicon's shell command without holding its whole output in memory: each stream keeps
     * [inlineLimit] bytes inline and, once longer, is written in full to a file from [spill]
     * (called with "stdout" or "stderr"). More than [totalLimit] bytes in all fails with [OutputTooLarge].
     */
    fun capture(
        input: InputStream,
        spill: (String) -> File,
        inlineLimit: Int = INLINE_LIMIT,
        totalLimit: Long = SPILL_LIMIT,
    ): Captured {
        val sinks = arrayOf(Sink("stdout", inlineLimit, spill), Sink("stderr", inlineLimit, spill))
        try {
            var total = 0L
            while (true) {
                val channel = input.read()
                if (channel < 0) throw EOFException("ADB shell closed without an exit status")
                val size = int(exact(input, 4))
                if (size < 0 || size > 1024 * 1024) throw IOException("Invalid ADB frame length: $size")
                val bytes = exact(input, size)
                when (channel) {
                    1, 2 -> {
                        total += size
                        if (total > totalLimit) throw OutputTooLarge(totalLimit)
                        sinks[channel - 1].write(bytes)
                    }
                    3 -> {
                        if (size != 1) throw IOException("Invalid ADB exit status")
                        return Captured(sinks[0].finish(), sinks[1].finish(), bytes[0].toInt() and 255)
                    }
                    else -> throw IOException("Unexpected ADB shell channel $channel")
                }
            }
        } catch (e: Throwable) {
            sinks.forEach { it.discard() }
            throw e
        }
    }

    private class Sink(private val name: String, private val inlineLimit: Int, private val spill: (String) -> File) {
        private val inline = ByteArrayOutputStream()
        private var file: File? = null
        private var output: OutputStream? = null
        private var total = 0L

        fun write(bytes: ByteArray) {
            if (output == null && inline.size() + bytes.size > inlineLimit) {
                val f = spill(name)
                file = f
                output = f.outputStream().buffered().also { it.write(inline.toByteArray()) }
            }
            output?.write(bytes)
            val room = inlineLimit - inline.size()
            if (room > 0) inline.write(bytes, 0, minOf(room, bytes.size))
            total += bytes.size
        }

        fun finish(): Stream {
            output?.close()
            return Stream(inline.toByteArray(), total, file)
        }

        fun discard() {
            runCatching { output?.close() }
            file?.delete()
        }
    }

    fun packet(out: OutputStream, id: String, bytes: ByteArray) {
        require(id.length == 4)
        out.write(id.toByteArray(Charsets.US_ASCII)); out.write(intBytes(bytes.size)); out.write(bytes); out.flush()
    }
    fun syncReply(input: InputStream): Pair<String, Int> = exact(input, 4).toString(Charsets.US_ASCII) to int(exact(input, 4))
}

package com.teamofsilicons.extend.adb

import android.media.MediaCodec
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.MediaMuxer
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import java.io.File
import java.nio.ByteBuffer

/** Combines finalized H.264 segments without decoding or loading an entire video in memory. */
internal object RecordingMuxer {
    data class Segment(val file: File, val maximumDurationUs: Long)

    fun segment(file: File, timing: String): Segment {
        val values = timing.trim().split(Regex("\\s+"))
        require(values.size == 3) { "Recording segment has no native timing evidence" }
        val elapsed = ((values[1].toBigDecimal() - values[0].toBigDecimal()) * 1_000_000.toBigDecimal()).toLong()
        val capSeconds = values[2].toLong()
        require(elapsed > 0 && capSeconds in 1..180) { "Invalid recording segment timing" }
        return Segment(file, minOf(elapsed, capSeconds * 1_000_000))
    }

    suspend fun combine(parts: List<Segment>, output: File) {
        require(parts.isNotEmpty()) { "Android produced no recording segments" }
        val muxer = MediaMuxer(output.absolutePath, MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4)
        var started = false
        var format: MediaFormat? = null
        var track = -1
        var offsetUs = 0L
        var buffer = ByteBuffer.allocateDirect(1024 * 1024)
        try {
            for (part in parts) {
                currentCoroutineContext().ensureActive()
                val extractor = MediaExtractor()
                try {
                    extractor.setDataSource(part.file.absolutePath)
                    val video = (0 until extractor.trackCount).firstOrNull {
                        extractor.getTrackFormat(it).getString(MediaFormat.KEY_MIME) == "video/avc"
                    } ?: error("Android recording segment has no H.264 video")
                    val next = extractor.getTrackFormat(video)
                    if (format == null) {
                        format = next
                        track = muxer.addTrack(next)
                        muxer.start()
                        started = true
                    } else {
                        for (key in listOf(MediaFormat.KEY_WIDTH, MediaFormat.KEY_HEIGHT)) {
                            require(format.getInteger(key) == next.getInteger(key)) { "Recording dimensions changed between segments" }
                        }
                        for (key in listOf("csd-0", "csd-1")) {
                            require(format.getByteBuffer(key) == next.getByteBuffer(key)) { "Recording encoder format changed between segments" }
                        }
                    }
                    extractor.selectTrack(video)
                    val firstUs = extractor.sampleTime
                    require(firstUs >= 0) { "Android recording segment contains no frames" }
                    var lastUs = -1L
                    while (extractor.sampleTime >= 0) {
                        currentCoroutineContext().ensureActive()
                        val size = extractor.sampleSize
                        require(size in 1..(32L * 1024 * 1024)) { "Invalid recording frame size" }
                        if (size > buffer.capacity()) buffer = ByteBuffer.allocateDirect(size.toInt())
                        buffer.clear()
                        val read = extractor.readSampleData(buffer, 0)
                        require(read == size.toInt()) { "Could not read a complete recording frame" }
                        val timeUs = extractor.sampleTime - firstUs
                        if (timeUs >= part.maximumDurationUs) break
                        require(timeUs > lastUs) { "Recording timestamps are not increasing" }
                        val flags = if (extractor.sampleFlags and MediaExtractor.SAMPLE_FLAG_SYNC != 0) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0
                        val info = MediaCodec.BufferInfo().apply { set(0, read, offsetUs + timeUs, flags) }
                        muxer.writeSampleData(track, buffer, info)
                        lastUs = timeUs
                        extractor.advance()
                    }
                    val durationUs = minOf(next.getLong(MediaFormat.KEY_DURATION) - firstUs, part.maximumDurationUs)
                    require(durationUs > lastUs) { "Recording segment has an invalid duration" }
                    offsetUs += durationUs
                } finally { extractor.release() }
            }
            // Sparse screen updates have long frame gaps. Preserve the declared final sample
            // duration instead of letting the muxer repeat the preceding gap at the tail.
            val end = MediaCodec.BufferInfo().apply { set(0, 0, offsetUs, MediaCodec.BUFFER_FLAG_END_OF_STREAM) }
            muxer.writeSampleData(track, buffer, end)
            muxer.stop()
            started = false
            require(output.length() in 1..RecordingScript.MAX_BYTES) { "Recording exceeded the 1 GiB limit" }
        } catch (error: Exception) {
            output.delete()
            throw error
        } finally {
            if (started) runCatching { muxer.stop() }
            muxer.release()
        }
    }
}

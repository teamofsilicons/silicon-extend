package com.teamofsilicons.extend.adb

import android.media.MediaCodec
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.MediaMuxer
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import java.io.File
import java.io.IOException
import java.nio.ByteBuffer

/**
 * Combines finalized H.264 segments without decoding or loading a whole video in memory.
 *
 * Segments are appended one at a time, so the caller can pull, append and delete each source
 * before the next. Segments whose picture size or encoder parameters differ (for example after
 * the screen rotated) can't share one H.264 track: the caller starts a new output file for them.
 */
internal object RecordingMuxer {
    /** A problem with one source segment. The rest of the recording can still be saved. */
    class BadSegment(message: String) : Exception(message)

    /** A problem writing the output. Nothing about the sources is wrong, so a retry can succeed. */
    class OutputFailure(message: String, cause: Throwable? = null) : IOException(message, cause)

    /** An opened source segment, positioned at its first frame. */
    class Source internal constructor(internal val extractor: MediaExtractor, val format: MediaFormat, val firstUs: Long) : AutoCloseable {
        /** The container's duration relative to the first frame, when it declares one. */
        val containerUs: Long? get() =
            if (format.containsKey(MediaFormat.KEY_DURATION)) format.getLong(MediaFormat.KEY_DURATION) - firstUs else null
        override fun close() = extractor.release()
    }

    fun open(file: File): Source {
        val extractor = MediaExtractor()
        try {
            try { extractor.setDataSource(file.absolutePath) }
            catch (e: IOException) { throw BadSegment("it could not be read as MP4 (${e.message ?: "unreadable"}); the recorder was probably stopped before it finished writing") }
            val video = (0 until extractor.trackCount).firstOrNull {
                extractor.getTrackFormat(it).getString(MediaFormat.KEY_MIME) == "video/avc"
            } ?: throw BadSegment("it has no H.264 video track")
            val format = extractor.getTrackFormat(video)
            extractor.selectTrack(video)
            val firstUs = extractor.sampleTime
            if (firstUs < 0) throw BadSegment("it contains no frames")
            return Source(extractor, format, firstUs)
        } catch (e: Exception) {
            extractor.release()
            throw e
        }
    }

    /** Two segments can share one output track only with the same picture size and encoder parameters. */
    fun compatible(a: MediaFormat, b: MediaFormat): Boolean =
        listOf(MediaFormat.KEY_WIDTH, MediaFormat.KEY_HEIGHT).all { a.getInteger(it) == b.getInteger(it) } &&
            listOf("csd-0", "csd-1").all { key -> a.byteBufferOrNull(key) == b.byteBufferOrNull(key) }

    private fun MediaFormat.byteBufferOrNull(key: String): ByteBuffer? = if (containsKey(key)) getByteBuffer(key) else null

    fun size(format: MediaFormat): String = "${format.getInteger(MediaFormat.KEY_WIDTH)}x${format.getInteger(MediaFormat.KEY_HEIGHT)}"

    /** What appending one segment did. */
    data class Appended(val frames: Int, val skippedFrames: Int, val estimated: Boolean, val cutOff: String?)

    /** One output MP4: consecutive compatible segments on one timeline. */
    class Writer(val output: File, private val format: MediaFormat) {
        private val muxer: MediaMuxer
        private val track: Int
        private var started = false
        private var baseUs: Long? = null
        /** Where the last appended segment ends on this file's timeline. */
        var endUs = 0L
            private set
        var segments = 0
            private set
        private var buffer = ByteBuffer.allocateDirect(1024 * 1024)

        init {
            try {
                muxer = MediaMuxer(output.absolutePath, MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4)
                track = muxer.addTrack(format)
                muxer.start()
                started = true
            } catch (e: Exception) {
                throw OutputFailure("Could not create ${output.name}: ${e.message}", e)
            }
        }

        fun accepts(next: MediaFormat) = compatible(format, next)

        /** Appends [source]; the caller still closes it. */
        suspend fun append(source: Source, timing: RecordingTimeline.Timing?): Appended {
            val offset = RecordingTimeline.offset(timing, baseUs, endUs)
            if (baseUs == null && timing != null) baseUs = RecordingTimeline.base(timing, offset)
            val limit = RecordingTimeline.frameLimitUs(timing)
            val extractor = source.extractor
            var last = -1L
            var frames = 0
            var skipped = 0
            var cutOff: String? = null
            while (extractor.sampleTime >= 0) {
                currentCoroutineContext().ensureActive()
                val size = extractor.sampleSize
                if (size !in 1..(32L * 1024 * 1024)) { cutOff = "a frame had an invalid size"; break }
                if (size > buffer.capacity()) buffer = ByteBuffer.allocateDirect(size.toInt())
                buffer.clear()
                val read = extractor.readSampleData(buffer, 0)
                if (read != size.toInt()) { cutOff = "a frame could not be read completely"; break }
                val relative = extractor.sampleTime - source.firstUs
                if (relative >= limit) {
                    // Past the segment's wall-clock end: it would overlap the next segment.
                    skipped++
                    extractor.advance()
                    continue
                }
                if (relative <= last) { skipped++; extractor.advance(); continue }
                val flags = if (extractor.sampleFlags and MediaExtractor.SAMPLE_FLAG_SYNC != 0) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0
                val info = MediaCodec.BufferInfo().apply { set(0, read, offset + relative, flags) }
                try { muxer.writeSampleData(track, buffer, info) }
                catch (e: Exception) { throw OutputFailure("Could not write ${output.name}: ${e.message}", e) }
                last = relative
                frames++
                extractor.advance()
            }
            val placement = RecordingTimeline.place(timing, offset, last, source.containerUs)
            endUs = placement.offsetUs + placement.durationUs
            segments++
            return Appended(frames, skipped, placement.estimated, cutOff)
        }

        /** Ends the file at the last segment's end and returns its duration in µs. */
        fun finish(): Long {
            try {
                // Sparse screen updates have long frame gaps. End the file at the segment's
                // wall-clock end instead of letting the muxer repeat the preceding gap.
                val end = MediaCodec.BufferInfo().apply { set(0, 0, endUs, MediaCodec.BUFFER_FLAG_END_OF_STREAM) }
                muxer.writeSampleData(track, buffer, end)
                muxer.stop()
                started = false
            } catch (e: Exception) {
                throw OutputFailure("Could not finish ${output.name}: ${e.message}", e)
            } finally {
                release()
            }
            if (output.length() !in 1..RecordingScript.MAX_BYTES) throw OutputFailure("${output.name} exceeded the 1 GiB limit")
            return endUs
        }

        /** Releases the muxer without finishing; the output is deleted. */
        fun abandon() {
            release()
            output.delete()
        }

        private var released = false
        private fun release() {
            if (released) return
            released = true
            if (started) runCatching { muxer.stop() }
            runCatching { muxer.release() }
        }
    }
}

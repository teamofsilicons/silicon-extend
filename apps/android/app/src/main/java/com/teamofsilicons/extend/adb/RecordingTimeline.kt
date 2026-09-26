package com.teamofsilicons.extend.adb

/**
 * Places native screenrecord segments on one timeline using wall-clock evidence.
 *
 * screenrecord writes a frame only when the screen changes and never writes an end-of-stream
 * sample, so a segment's container duration says little about how long it ran: a still screen
 * gives a one-frame segment whose duration is 0, and a burst of frames followed by a still tail
 * gives a duration that ends shortly after the burst. The recording supervisor reads
 * `/proc/uptime` just before it starts each screenrecord and just after that process exits, and
 * writes both to the segment's `.timing` file. Each segment therefore starts on the timeline at
 * its wall-clock start and lasts its wall-clock length. Gaps while screenrecord restarts stay on
 * the timeline, so video time keeps matching the device's log time.
 *
 * A segment's first frame is placed at the moment its screenrecord started. The real first frame
 * arrives after screenrecord's start-up (usually well under a second), so each segment's frames
 * can show slightly early. That error does not add up across segments.
 */
internal object RecordingTimeline {
    /** One segment's `.timing` evidence: uptime in µs just before screenrecord started and after it exited. */
    data class Timing(val startUs: Long, val endUs: Long) {
        val elapsedUs: Long get() = endUs - startUs
    }

    /** Where a segment sits on its output file's timeline. */
    data class Placement(val offsetUs: Long, val durationUs: Long, val estimated: Boolean)

    /** One frame at 30 fps: the length given to a final frame when nothing better is known. */
    const val FRAME_US = 33_333L

    /** A capture is bounded to 30 minutes; allow for start-up and suspend before calling timing implausible. */
    private const val MAX_SEGMENT_US = (1800L + 120L) * 1_000_000

    /** Parses "start end [cap]" seconds from `/proc/uptime`; null when the evidence is missing or implausible. */
    fun parse(text: String?): Timing? {
        val values = text?.trim()?.split(Regex("\\s+"))?.filter { it.isNotEmpty() } ?: return null
        if (values.size !in 2..3) return null
        val start = micros(values[0]) ?: return null
        val end = micros(values[1]) ?: return null
        return Timing(start, end).takeIf { start >= 0 && it.elapsedUs in 1..MAX_SEGMENT_US }
    }

    private fun micros(seconds: String): Long? =
        seconds.toBigDecimalOrNull()?.let { (it * 1_000_000.toBigDecimal()).toLong() }

    /**
     * Where a segment starts on its group's timeline.
     *
     * @param baseUs uptime (µs) that is time 0 of this output file, or null when not known yet
     * @param previousEndUs where the previous segment of this output file ends (0 for the first)
     */
    fun offset(timing: Timing?, baseUs: Long?, previousEndUs: Long): Long {
        if (timing == null || baseUs == null) return previousEndUs
        return maxOf(timing.startUs - baseUs, previousEndUs)
    }

    /** The base that places [timing] at [previousEndUs], for a file whose base isn't known yet. */
    fun base(timing: Timing, previousEndUs: Long): Long = timing.startUs - previousEndUs

    /** Frames at or after this time (relative to the segment's first frame) would overlap the next segment. */
    fun frameLimitUs(timing: Timing?): Long = timing?.elapsedUs ?: Long.MAX_VALUE

    /**
     * How long a segment lasts once its frames are known.
     *
     * @param lastFrameUs the last frame written, relative to the segment's first frame (-1 for none)
     * @param containerUs the source container's duration relative to its first frame, when it has one
     */
    fun place(timing: Timing?, offsetUs: Long, lastFrameUs: Long, containerUs: Long?): Placement {
        if (timing != null && timing.elapsedUs > lastFrameUs) return Placement(offsetUs, timing.elapsedUs, false)
        val fallback = maxOf(containerUs?.takeIf { it > lastFrameUs } ?: 0L, lastFrameUs + FRAME_US)
        return Placement(offsetUs, fallback, true)
    }
}

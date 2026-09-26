package com.teamofsilicons.extend

import com.teamofsilicons.extend.adb.RecordingTimeline
import com.teamofsilicons.extend.adb.RecordingTimeline.Timing
import org.junit.Assert.*
import org.junit.Test

/** Segment placement from wall-clock evidence (screenrecord writes frames only when the screen changes). */
class RecordingTimelineTest {
    private val s = 1_000_000L

    @Test fun parsesUptimeEvidence() {
        assertEquals(Timing(100_250_000, 280_010_000), RecordingTimeline.parse("100.25 280.01 180"))
        assertEquals(Timing(1 * s, 3 * s), RecordingTimeline.parse(" 1.00 3.00\n"))
        for (bad in listOf(null, "", "12.0", "3.0 1.0 180", "a b c", "1 1 180", "0 99999 180")) {
            assertNull("must reject $bad", RecordingTimeline.parse(bad))
        }
    }

    @Test fun aSingleFrameSegmentLastsItsWallClockTime() {
        // A still screen: one keyframe, container duration 0 (MPEG4Writer without end-of-stream).
        val timing = Timing(10 * s, 12 * s + 500_000)
        val offset = RecordingTimeline.offset(timing, null, 0)
        val placement = RecordingTimeline.place(timing, offset, lastFrameUs = 0, containerUs = 0)
        assertEquals(0L, placement.offsetUs)
        assertEquals(2_500_000L, placement.durationUs)
        assertFalse(placement.estimated)
    }

    @Test fun aBurstThenStillSegmentIsNotShortened() {
        // Frames 16 ms apart until 120 s, then a still screen until the 180 s rollover.
        val timing = Timing(100 * s, 280 * s)
        val placement = RecordingTimeline.place(timing, 0, lastFrameUs = 120 * s, containerUs = 120 * s + 16_000)
        assertEquals(180 * s, placement.durationUs)
    }

    @Test fun aLongFinalGapDoesNotInflateTheSegment() {
        // Without an end-of-stream sample the last frame repeats the previous 44 s gap.
        val timing = Timing(0, 60 * s)
        val placement = RecordingTimeline.place(timing, 0, lastFrameUs = 59 * s, containerUs = 103 * s)
        assertEquals(60 * s, placement.durationUs)
    }

    @Test fun laterSegmentsKeepTheirWallClockPlaceIncludingRestartGaps() {
        val first = Timing(100 * s, 280 * s)
        var base: Long? = null
        val o1 = RecordingTimeline.offset(first, base, 0)
        base = RecordingTimeline.base(first, o1)
        val end1 = o1 + RecordingTimeline.place(first, o1, 120 * s, null).durationUs
        val second = Timing(280 * s + 300_000, 300 * s)
        val o2 = RecordingTimeline.offset(second, base, end1)
        assertEquals("The 0.3 s restart gap stays on the timeline", 180 * s + 300_000, o2)
        val end2 = o2 + RecordingTimeline.place(second, o2, 0, 0).durationUs
        assertEquals("The file ends at the capture's wall-clock end", 200 * s, end2)
    }

    @Test fun frameLimitIsTheSegmentsWallClockLength() {
        assertEquals(2 * s, RecordingTimeline.frameLimitUs(Timing(1 * s, 3 * s)))
        assertEquals(Long.MAX_VALUE, RecordingTimeline.frameLimitUs(null))
    }

    @Test fun missingTimingIsEstimatedFromFramesAndContinuesTheTimeline() {
        val offset = RecordingTimeline.offset(null, 7 * s, previousEndUs = 180 * s)
        assertEquals(180 * s, offset)
        val fromContainer = RecordingTimeline.place(null, offset, lastFrameUs = 5 * s, containerUs = 5 * s + 200_000)
        assertEquals(5 * s + 200_000, fromContainer.durationUs)
        assertTrue(fromContainer.estimated)
        // One frame and a zero container duration still gets a positive length after its frame.
        val single = RecordingTimeline.place(null, offset, lastFrameUs = 0, containerUs = 0)
        assertEquals(RecordingTimeline.FRAME_US, single.durationUs)
        assertTrue(single.durationUs > 0)
    }
}

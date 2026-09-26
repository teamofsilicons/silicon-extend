package com.teamofsilicons.bridge

import com.teamofsilicons.bridge.net.Backoff
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.random.Random

class BackoffTest {
    @Test
    fun ceilingDoublesFromOneSecondToSixty() {
        val b = Backoff()
        assertEquals(listOf(1_000L, 2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000), (0..7).map { b.ceilingMs(it) })
        assertEquals(60_000L, b.ceilingMs(64)) // no overflow
    }

    @Test
    fun delaysStayWithinOneToSixtySecondsWithJitter() {
        val b = Backoff(random = Random(7))
        val delays = (0 until 200).map { b.next() }
        assertTrue(delays.all { it in 1_000..60_000 })
        // Full jitter: later attempts spread across the whole range, not pinned to the cap.
        val late = delays.drop(10)
        assertTrue(late.distinct().size > 50)
        assertTrue(late.any { it < 30_000 } && late.any { it > 30_000 })
    }

    @Test
    fun attemptNeverExceedsCeiling() {
        val b = Backoff(random = Random(1))
        repeat(12) { attempt ->
            val ceiling = b.ceilingMs()
            val d = b.next()
            assertTrue("attempt $attempt: $d > $ceiling", d <= maxOf(ceiling, 1_000))
        }
    }

    @Test
    fun resetStartsOver() {
        val b = Backoff(random = Random(3))
        repeat(9) { b.next() }
        assertEquals(9, b.attempt)
        b.reset()
        assertEquals(0, b.attempt)
        assertEquals(1_000L, b.next()) // ceiling 1 s, floor 1 s
    }
}

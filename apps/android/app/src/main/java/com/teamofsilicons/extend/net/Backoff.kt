package com.teamofsilicons.extend.net

import kotlin.math.min
import kotlin.random.Random

/**
 * Reconnect delays: exponential from [baseMs] up to [capMs] with full jitter
 * (`random(0, min(cap, base * 2^attempt))`), never shorter than [floorMs] so a flapping
 * connection can't spin. Defaults follow the device protocol: 1 s to 60 s.
 */
class Backoff(
    private val baseMs: Long = 1_000,
    private val capMs: Long = 60_000,
    private val floorMs: Long = 1_000,
    private val random: Random = Random.Default,
) {
    var attempt: Int = 0
        private set

    /** The ceiling the next delay is drawn under. */
    fun ceilingMs(forAttempt: Int = attempt): Long {
        val shift = min(forAttempt, 30)
        val exp = baseMs * (1L shl shift)
        return min(capMs, if (exp <= 0) capMs else exp)
    }

    /** The delay before the next attempt; advances the attempt counter. */
    fun next(): Long {
        val ceiling = ceilingMs()
        attempt++
        val drawn = if (ceiling <= 0) 0 else random.nextLong(0, ceiling + 1)
        return drawn.coerceIn(floorMs, capMs)
    }

    /** Call once a connection is established. */
    fun reset() {
        attempt = 0
    }
}

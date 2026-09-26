package com.teamofsilicons.extend.adb

/**
 * How long the paired app waits before trying to reconnect Android debugging again.
 *
 * Each try may run mDNS discovery for up to 5 seconds, so failed tries back off from 15 seconds
 * to 15 minutes instead of repeating every 15 seconds for days after a reboot turned Wireless
 * debugging off. A network change, Wireless debugging being switched on, or the Carbon opening
 * the app wakes the loop early and starts the back-off again.
 */
class AdbReconnectPolicy(private val firstMs: Long = 15_000, private val maxMs: Long = 15 * 60_000) {
    private var failures = 0

    /** The wait after a failed try. */
    fun afterFailure(): Long {
        val wait = (firstMs shl minOf(failures, 20)).coerceAtMost(maxMs)
        failures++
        return wait
    }

    /** The wait while nothing needs reconnecting. */
    fun idle(): Long {
        failures = 0
        return firstMs
    }

    /** Something changed that may let a reconnect succeed: the next failure starts the back-off again. */
    fun reset() {
        failures = 0
    }
}

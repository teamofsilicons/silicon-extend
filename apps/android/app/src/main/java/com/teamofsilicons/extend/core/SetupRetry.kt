package com.teamofsilicons.extend.core

import android.os.SystemClock
import java.util.concurrent.ConcurrentHashMap

/**
 * Running a failed setup step again: from the step's Retry button in this app, or when Extend sends
 * `setup_retry` (the website's Retry, `extend device setup --retry`). The app advertises this in
 * `hello.features` ("setup_retry").
 *
 * Android's failed steps are the ones something on the device tried and couldn't do: reconnecting
 * to Android debugging the Carbon connected, and an accessibility service Android turned on but
 * never started. A retry marks the step in progress ([active]), runs it again at once (a debugging
 * reconnect right now, a fresh look at accessibility), and the next `setup_progress` reports how it
 * went. A step that is only waiting for the Carbon (a switch in Settings) has nothing to retry.
 */
object SetupRetry {
    const val FAILED = "failed"

    /** How long a retried step shows as in progress at most, if its attempt never reports back. */
    const val SHOW_MS = 30_000L

    private val until = ConcurrentHashMap<String, Long>()

    /** Steps being retried now. */
    fun active(now: Long = SystemClock.elapsedRealtime()): Set<String> {
        until.entries.removeIf { it.value <= now }
        return until.keys.toSet()
    }

    fun start(keys: Collection<String>, now: Long = SystemClock.elapsedRealtime()) {
        for (k in keys) until[k] = now + SHOW_MS
    }

    /** The attempt for [keys] finished (whatever it found). */
    fun finished(keys: Collection<String>) {
        keys.forEach(until::remove)
    }

    /** The keys of Android debugging's step, whichever this device uses. */
    val DEBUGGING_KEYS = setOf("wireless_debugging", "network_debugging")

    /**
     * Which steps a retry of [step] (null: every failed step) runs, from [failed] (the failed
     * steps' keys). An unknown or healthy step runs nothing: the app just reports again.
     */
    fun select(failed: List<String>, step: String?): List<String> =
        if (step == null) failed else failed.filter { it == step }
}

package com.teamofsilicons.extend.core

/**
 * Which Silicon sessions this device keeps while its socket to Extend is down.
 *
 * Extend keeps a session alive while its device is briefly offline and announces it again
 * (`session_started`) when the device reconnects. So a dropped socket must not end a session's
 * recordings, logs or snapshot references while Extend may still keep the session. A session known
 * when the socket dropped is kept until one of these:
 * - it is announced again: it continues ([confirmed]);
 * - after reconnecting, `GET /api/v1/device` shows another session or none in use: it ended while
 *   the device was away ([reconcile]);
 * - [graceMs] passes without either: Extend has certainly ended it for being offline ([expired]).
 */
class SessionRetention(private val graceMs: Long = GRACE_MS) {
    private val deadlines = HashMap<String, Long>()

    /** The socket dropped: every known session must be confirmed within the grace period. */
    @Synchronized fun disconnected(known: Collection<String>, nowMs: Long) {
        for (id in known) deadlines.putIfAbsent(id, nowMs + graceMs)
    }

    /** Extend announced [id] again: it continues. */
    @Synchronized fun confirmed(id: String) {
        deadlines.remove(id)
    }

    /** The session ended some other way; stop tracking it. */
    @Synchronized fun forget(id: String) {
        deadlines.remove(id)
    }

    /**
     * Extend's current view after a reconnect: every unconfirmed session other than [active] has
     * ended. [among]: only these sessions (the ones of the pair that reconnected; the others wait
     * for their own pair's connection).
     */
    @Synchronized fun reconcile(active: String?, among: Collection<String>? = null): List<String> {
        val ended = deadlines.keys.filter { it != active && (among == null || it in among) }
        if (among == null) deadlines.clear() else among.forEach(deadlines::remove)
        return ended
    }

    /** Sessions whose grace period has passed without confirmation. */
    @Synchronized fun expired(nowMs: Long): List<String> {
        val ended = deadlines.filterValues { it <= nowMs }.keys.toList()
        ended.forEach(deadlines::remove)
        return ended
    }

    @Synchronized fun nextDeadline(): Long? = deadlines.values.minOrNull()

    @Synchronized fun pending(): Set<String> = deadlines.keys.toSet()

    companion object {
        // Extend's timing, mirrored from crates/extend-protocol/src/lib.rs (SessionRetentionTest
        // checks them against that file).
        /** Seconds between Extend's pings on the device socket. */
        const val HEARTBEAT_S = 15L
        /** Seconds without an answer before Extend counts the device as offline. */
        const val OFFLINE_AFTER_S = 45L
        /** Seconds Extend keeps a session after it last had contact with its offline device. */
        const val SESSION_OFFLINE_GRACE_S = 120L
        /** Extend checks offline sessions every 2 s and compares whole seconds (`> 120`). */
        const val SCHEDULER_S = 3L

        /**
         * The longest Extend can keep a session after this device lost contact. Extend only notices
         * a silent device when a ping finds no answer for more than [OFFLINE_AFTER_S] (pings come
         * every [HEARTBEAT_S]), records that moment as the device's last contact, and ends the
         * session [SESSION_OFFLINE_GRACE_S] later on a scheduler pass: 45 + 15 + 120 + 3 = 183 s.
         * This device notices a drop no earlier than Extend last heard from it, so counting from
         * its own notice is safe.
         */
        const val EXTEND_KEEPS_MS = (OFFLINE_AFTER_S + HEARTBEAT_S + SESSION_OFFLINE_GRACE_S + SCHEDULER_S) * 1000

        /** How long this device keeps a disconnected session: Extend's worst case plus margin, 4 minutes. */
        const val GRACE_MS = 240_000L
    }
}

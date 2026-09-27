package com.teamofsilicons.extend.core

import java.time.Instant

/**
 * The wake requests this device holds, and what it may show of each (UNDERSTANDING.md, Waking a
 * device). Pure, for JVM tests.
 *
 * Each Carbon sees only their own side: while a Silicon given access by another Carbon (or from
 * another Team) is using the device, a request from this side must not show who asked or why, not
 * on the lock screen, not in the app, and not in what the Silicon using it can read. The service
 * leaves those fields out when it knows; the app also hides them itself from the moment a session
 * starts ([redacted]), before that session's first command runs, because the service's corrected
 * frames can arrive later on another pair's connection.
 */
object WakeRequests {
    /** What the device may show of one request. [silicon] and [reason] are null when hidden. */
    data class Line(val wakeId: String, val silicon: String?, val reason: String?, val expiresAt: String)

    /** Adds [w], or replaces the request with its id: a frame without Silicon and reason replaces those too. */
    fun upsert(list: List<WakeUi>, w: WakeUi): List<WakeUi> {
        val at = list.indexOfFirst { it.wakeId == w.wakeId }
        return if (at < 0) list + w else list.toMutableList().also { it[at] = w }
    }

    fun remove(list: List<WakeUi>, wakeId: String): List<WakeUi> = list.filter { it.wakeId != wakeId }

    /** Requests of one pair go when that pair ends. */
    fun removePair(list: List<WakeUi>, pairId: String): List<WakeUi> = list.filter { it.pairId != pairId }

    /** Requests still open at [nowMs] (an unreadable expiry is kept; the service ends it anyway). */
    fun open(list: List<WakeUi>, nowMs: Long): List<WakeUi> = list.filter { (millis(it.expiresAt) ?: Long.MAX_VALUE) > nowMs }

    /** When the next request expires, or null. */
    fun nextExpiry(list: List<WakeUi>): Long? = list.mapNotNull { millis(it.expiresAt) }.minOrNull()

    /**
     * Whether [w] is shown without its Silicon and reason: the service already left them out, or a
     * session runs whose side differs from the request's. A session whose side isn't known (one
     * learnt from `GET /api/v1/device` after a reconnect) hides every request: better too little
     * than another side's Silicon and reason.
     */
    fun redacted(w: WakeUi, session: SessionUi?): Boolean =
        w.siliconId.isNullOrBlank() || (session != null && (session.side == null || w.side != session.side))

    /** Every open request as the device may show it, newest first. */
    fun lines(list: List<WakeUi>, session: SessionUi?): List<Line> =
        list.sortedByDescending { millis(it.createdAt) ?: 0 }.map { w ->
            if (redacted(w, session)) Line(w.wakeId, null, null, w.expiresAt) else Line(w.wakeId, w.siliconId, w.reason, w.expiresAt)
        }

    private fun millis(ts: String): Long? = runCatching { Instant.parse(ts).toEpochMilli() }.getOrNull()
}

/**
 * The words of the one wake notification a phone or tablet shows for every open request: the full
 * version, and the [publicTitle] and [publicText] the lock screen shows when it hides notification
 * content (the Silicon's name, never its reason).
 */
data class WakeNotice(
    val title: String,
    val text: String,
    val bigText: String,
    val publicTitle: String,
    val publicText: String,
) {
    companion object {
        const val HIDDEN_TITLE = "A Silicon asked to use this"
        const val TING = "Its Carbon was told through Ting."

        /** [noun]: "phone" or "tablet". Null when there is nothing to show. */
        fun of(lines: List<WakeRequests.Line>, noun: String): WakeNotice? {
            if (lines.isEmpty()) return null
            val named = lines.filter { it.silicon != null }
            if (named.isEmpty()) {
                val title = "$HIDDEN_TITLE $noun"
                val text = if (lines.size == 1) TING else "${lines.size} requests. $TING"
                return WakeNotice(title, text, text, title, text)
            }
            val first = named.first()
            val others = lines.size - 1
            val title = if (others == 0) "${first.silicon} asks to use this $noun"
            else "${first.silicon} and $others more ask to use this $noun"
            val unlock = "Unlock this $noun to let ${if (others == 0) "it" else "them"} know it's awake."
            val big = lines.joinToString("\n") { l ->
                if (l.silicon == null) "A Silicon: $TING" else "${l.silicon}: ${l.reason ?: "no reason given"}"
            } + "\n" + unlock
            return WakeNotice(
                title = title,
                text = first.reason ?: unlock,
                bigText = big,
                publicTitle = title,
                publicText = "Unlock this $noun to see why.",
            )
        }
    }
}

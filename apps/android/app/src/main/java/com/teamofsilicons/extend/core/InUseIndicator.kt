package com.teamofsilicons.extend.core

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * What the device itself shows while a Silicon uses it (UNDERSTANDING.md, Always visible): the
 * TV's badge at the bottom centre, the phone's in-use notification with Stop.
 *
 * - Shown (the default): it names the Silicon for [SHOW_MS] when a Silicon starts using the device
 *   (every new session, so again when a different Silicon starts), then hides by itself.
 * - Hidden: it never names the Silicon.
 * - Either way, a Silicon waiting for the Carbon (a takeover) stays up until the Carbon answers,
 *   and the app's own screen always shows who is using the device, with Stop.
 *
 * One setting per physical device, shared by every Carbon's pair of it: `in_use_indicator` in
 * `GET /api/v1/device`, changed with `PATCH /api/v1/device` (any pair's credential).
 */
object InUseIndicator {
    const val SHOWN = "shown"
    const val HIDDEN = "hidden"

    /** How long the badge or notification names the Silicon after its session starts. */
    const val SHOW_MS = 10_000L

    /** Only "hidden" hides it: a value from a newer service shows it, the safe side for the people around the device. */
    fun shows(value: String?): Boolean = value != HIDDEN

    fun value(shown: Boolean): String = if (shown) SHOWN else HIDDEN

    enum class Show {
        /** Nothing names the Silicon. */
        NONE,
        /** The first [SHOW_MS] of a session: the Silicon's id (badge) or the in-use notification with Stop. */
        ANNOUNCE,
        /** A Silicon is waiting for the Carbon: shown until they answer, whatever the setting. */
        WAITING,
    }

    /** What the device shows for [state] right now. */
    fun show(state: UiState): Show {
        if (state.takeover != null) return Show.WAITING
        val session = state.session ?: return Show.NONE
        if (!state.indicatorShown) return Show.NONE
        val a = state.announce ?: return Show.NONE
        return if (a.showing && a.sessionId == session.sessionId && a.pairId == session.pairId) Show.ANNOUNCE else Show.NONE
    }

    /** The key under which a session counts as announced: pair and session, since session ids are short. */
    fun key(session: SessionUi): String = "${session.pairId.orEmpty()}/${session.sessionId}"
}

/** The session the device last announced; [showing] while its [InUseIndicator.SHOW_MS] run. */
data class AnnounceUi(val sessionId: String, val siliconId: String, val showing: Boolean, val pairId: String? = null)

/**
 * Starts and ends a session's [InUseIndicator.SHOW_MS]. [onSession] is called with every change of
 * the session (it ignores re-reads of the same one). A session already announced before the app's
 * process was killed ([announced], kept across process death) isn't announced again when the app
 * comes back and reads it from Extend.
 */
class InUseAnnouncer(
    private val scope: CoroutineScope,
    private val update: ((UiState) -> UiState) -> Unit,
    private val announced: () -> String?,
    private val remember: (String?) -> Unit,
    private val showMs: Long = InUseIndicator.SHOW_MS,
) {
    private var job: Job? = null
    private var current: String? = null

    @Synchronized
    fun onSession(session: SessionUi?) {
        val key = session?.let { InUseIndicator.key(it) }
        if (key == current) return
        current = key
        job?.cancel()
        job = null
        if (session == null) {
            update { it.copy(announce = null) }
            return
        }
        val id = session.sessionId
        if (key == announced()) {
            update { it.copy(announce = AnnounceUi(id, session.siliconId, showing = false, pairId = session.pairId)) }
            return
        }
        remember(key)
        update { it.copy(announce = AnnounceUi(id, session.siliconId, showing = true, pairId = session.pairId)) }
        job = scope.launch {
            delay(showMs)
            update { s -> s.announce?.takeIf { it.sessionId == id && it.pairId == session.pairId }?.let { s.copy(announce = it.copy(showing = false, pairId = session.pairId)) } ?: s }
        }
    }
}

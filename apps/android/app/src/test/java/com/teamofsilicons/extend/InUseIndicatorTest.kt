package com.teamofsilicons.extend

import com.teamofsilicons.extend.core.*
import com.teamofsilicons.extend.ui.InUseBadge
import com.teamofsilicons.extend.a11y.KeepScreenOn
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.*
import org.junit.Assert.*
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class InUseIndicatorTest {
    @Test fun hidesAfterTenSecondsWithoutEndingTheSessionOrScreenHold() = runTest {
        val session = SessionUi("si:chef", "s1", "now", pairId = "p1")
        var state = UiState(isTv = true, session = session, awake = AwakeUi(true))
        var remembered: String? = null
        val announcer = InUseAnnouncer(backgroundScope, { state = it(state) }, { remembered }, { remembered = it })
        announcer.onSession(session)
        runCurrent()
        assertNotNull(InUseBadge.from(state))
        advanceTimeBy(9_999)
        assertNotNull(InUseBadge.from(state))
        announcer.onSession(session.copy(carbon = "c:alice")) // refresh must not restart the clock
        advanceTimeBy(1)
        runCurrent()
        assertNull(InUseBadge.from(state))
        assertEquals(session, state.session)
        assertTrue(KeepScreenOn.wanted(state))
        // A process restart or reconnect does not announce the same session again.
        val restarted = InUseAnnouncer(backgroundScope, { state = it(state) }, { remembered }, { remembered = it })
        restarted.onSession(session)
        assertNull(InUseBadge.from(state))
        // Session ids can be reused in a different pair/world; this is a new announcement.
        val next = session.copy(pairId = "p2")
        state = state.copy(session = next)
        restarted.onSession(next)
        assertNotNull(InUseBadge.from(state))
        state = state.copy(session = null)
        restarted.onSession(null)
        assertNull(InUseBadge.from(state))
        assertFalse(KeepScreenOn.wanted(state))
    }

    @Test fun hiddenSettingSuppressesAnnouncementsButNotRequestsForHelp() = runTest {
        val session = SessionUi("si:chef", "s1", "now")
        var state = UiState(isTv = true, session = session, indicatorShown = false)
        val announcer = InUseAnnouncer(backgroundScope, { state = it(state) }, { null }, {})
        announcer.onSession(session)
        assertNull(InUseBadge.from(state))
        state = state.copy(takeover = TakeoverUi("s1", "Please sign in", null))
        runCurrent()
        advanceTimeBy(60_000)
        runCurrent()
        assertEquals(InUseIndicator.Show.WAITING, InUseIndicator.show(state))
        assertTrue(InUseBadge.from(state)!!.waiting)
        state = state.copy(takeover = null)
        assertEquals(InUseIndicator.Show.NONE, InUseIndicator.show(state))
    }
}

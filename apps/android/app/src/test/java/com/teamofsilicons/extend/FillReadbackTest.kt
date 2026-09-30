package com.teamofsilicons.extend

import com.teamofsilicons.extend.driver.FillReadback
import com.teamofsilicons.extend.driver.FillReadback.Field
import com.teamofsilicons.extend.driver.FillReadback.Identity
import com.teamofsilicons.extend.driver.FillReadback.Result
import com.teamofsilicons.extend.driver.FillReadback.State
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test

class FillReadbackTest {
    private val original = Identity("node-1", 42, "example.app", "android.widget.EditText", "example.app:id/search")
    private fun field(text: String? = "", hint: Boolean = false, handle: String = "node-1") =
        Field(original.copy(handle = handle), text, hintShowing = hint)

    private class Probe {
        var clock = 0L
        var captures = 0
        var refreshes = 0
        val prompts = FillReadback.Prompts()
        fun verify(
            before: Field, expected: String, budget: Long = 1_000,
            capture: (Int) -> List<Field>? = { listOf(before.copy(text = expected, hintShowing = false)) },
            refresh: (Field, Int) -> Field? = { f, _ -> f },
        ): Result = runBlocking {
            FillReadback.verify(before, expected, prompts, budget, { clock },
                capture = { capture(++captures) }, refresh = { refresh(it, ++refreshes) }, pause = { clock += it })
        }
    }

    @Test fun exactFreshValueIsVerifiedWithoutAnExtraDelay() {
        val p = Probe()
        assertEquals(State.VERIFIED, p.verify(field(), "query").state)
        assertEquals(1, p.captures)
        assertEquals(0L, p.clock)
    }

    @Test fun replacedNodeIsResolvedByUniqueStableIdNotItsOldTextOrBounds() {
        val p = Probe()
        assertEquals(State.VERIFIED, p.verify(field("Search", true), "query", capture = { listOf(field("query", handle = "replacement")) }).state)
    }

    @Test fun aFailedRefreshCanRecoverFromAFreshReplacement() {
        val p = Probe()
        val result = p.verify(field(), "query", capture = { n -> listOf(field("query", handle = if (n == 1) "node-1" else "replacement")) },
            refresh = { f, n -> if (n == 1) null else f })
        assertEquals(State.VERIFIED, result.state)
        assertEquals(2, p.captures)
        assertEquals(75L, p.clock)
    }

    @Test fun delayedValueSettlesDuringReadOnlyPolling() {
        val p = Probe()
        assertEquals(State.VERIFIED, p.verify(field(), "query", capture = { n -> listOf(field(if (n < 3) "que" else "query")) }).state)
        assertEquals(3, p.captures)
    }

    @Test fun aRealMismatchAndAnUnchangedPreviousRealValueRemainFailures() {
        for (actual in listOf("wrong", "previous")) {
            val p = Probe()
            val result = p.verify(field("previous"), "query", capture = { listOf(field(actual)) })
            assertEquals(State.MISMATCH, result.state)
            assertEquals(actual.length, result.actualLength)
            assertEquals(1_000L, p.clock)
        }
    }

    @Test fun aPersistentInitialPromptIsUnverifiedOnFirstAndRepeatedFills() {
        val p = Probe()
        val prompt = "Type to search restaurants or dishes"
        val after = field(prompt)
        val first = p.verify(field(prompt, hint = true), "first query", capture = { listOf(after) })
        assertEquals(Result(State.UNAVAILABLE, "accessibility_prompt"), first)
        assertEquals(first, p.verify(after, "second query", capture = { listOf(after) }))
        // The literal accessibility prompt is not trustworthy value evidence even when requested.
        assertEquals(first, p.verify(after, prompt, capture = { listOf(after) }))
        val replacement = after.copy(identity = original.copy(handle = "replacement"))
        assertEquals(first, p.verify(replacement, "third query", capture = { listOf(replacement) }))
    }

    @Test fun promptProvenanceIsClearedWhenTheFieldExposesARealValue() {
        val p = Probe()
        assertEquals(State.UNAVAILABLE, p.verify(field("Search here", true), "first", capture = { listOf(field("Search here")) }).state)
        assertEquals(State.VERIFIED, p.verify(field("Search here"), "first").state)
        assertEquals(State.MISMATCH, p.verify(field("first"), "second", capture = { listOf(field("Search here")) }).state)
    }

    @Test fun anEmptyFieldStillShowingItsHintIsNotClaimedAsAcceptedButUnverified() {
        val p = Probe()
        val before = field("Search here", hint = true)
        assertEquals(State.MISMATCH, p.verify(before, "query", capture = { listOf(before) }).state)
        assertEquals(State.VERIFIED, p.verify(before, "", capture = { listOf(before) }).state)
    }

    @Test fun duplicateReplacementIdsCannotVerifyOrInheritPromptProvenance() {
        val p = Probe()
        val result = p.verify(field("Search here", true), "query", capture = { listOf(field("query", handle = "a"), field("query", handle = "b")) })
        assertEquals(Result(State.UNCONFIRMED, "field_missing_or_ambiguous"), result)
        assertEquals(0, p.refreshes)
    }

    @Test fun otherWindowsPackagesAndKeyboardsCannotStandInForTheTarget() {
        val candidates = listOf(field("query").copy(identity = original.copy(windowId = 43)),
            field("query").copy(identity = original.copy(packageName = "other.app")),
            field("query").copy(applicationWindow = false), field("query").copy(visible = false), field("query").copy(editable = false))
        val p = Probe()
        assertEquals(State.UNCONFIRMED, p.verify(field(), "query", capture = { candidates }).state)
        assertEquals(0, p.refreshes)
    }

    @Test fun aMissingFinalObservationDoesNotReuseAnEarlierMismatch() {
        val p = Probe()
        assertEquals(Result(State.UNCONFIRMED, "capture_unavailable"), p.verify(field(), "query", capture = { n -> if (n == 1) listOf(field("que")) else null }))
    }

    @Test fun failedRefreshNeverConfirmsTheStaleObject() {
        val p = Probe()
        assertEquals(Result(State.UNCONFIRMED, "field_refresh_failed"), p.verify(field(), "query", refresh = { _, _ -> null }))
    }

    @Test fun remainingBudgetCapsPollingAndExpiredBudgetDoesNotCapture() {
        val p = Probe()
        assertEquals(State.MISMATCH, p.verify(field(), "query", budget = 110, capture = { listOf(field("old")) }).state)
        assertEquals(110L, p.clock)
        assertEquals(2, p.captures)
        assertEquals(Result(State.UNCONFIRMED, "readback_budget_exhausted"), p.verify(field(), "query", budget = 0))
        assertEquals(2, p.captures)
    }

    @Test fun passwordsRemainExplicitlyUnverifiedWithoutReadingTheirValue() {
        val p = Probe()
        assertEquals(Result(State.UNAVAILABLE, "password"), p.verify(field().copy(password = true), "secret"))
        assertEquals(0, p.captures)
    }

    @Test fun promptHistoryIsBounded() {
        val prompts = FillReadback.Prompts()
        repeat(65) { n -> prompts.remember(original.copy(handle = "node-$n", resourceId = "id-$n"), "prompt-$n") }
        val first = field().copy(identity = original.copy(handle = "node-0", resourceId = "id-0"))
        val last = field().copy(identity = original.copy(handle = "node-64", resourceId = "id-64"))
        assertNull(prompts.find(first, listOf(first)))
        assertEquals("prompt-64", prompts.find(last, listOf(last)))
    }
}

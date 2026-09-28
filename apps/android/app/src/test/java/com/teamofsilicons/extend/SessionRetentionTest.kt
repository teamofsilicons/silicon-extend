package com.teamofsilicons.extend

import com.teamofsilicons.extend.core.SessionRetention
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import java.io.File

/** A dropped socket keeps sessions (and their recordings) as long as Extend does. */
class SessionRetentionTest {
    @Test fun aBriefDropKeepsTheSessionWhenItIsAnnouncedAgain() {
        val r = SessionRetention(graceMs = 150_000)
        r.disconnected(setOf("s1"), nowMs = 1_000)
        r.confirmed("s1") // session_started again after the reconnect
        assertEquals(emptyList<String>(), r.expired(nowMs = 1_000_000))
        assertEquals(emptyList<String>(), r.reconcile(active = "s1"))
    }

    @Test fun nothingEndsBeforeTheGracePeriod() {
        val r = SessionRetention(graceMs = 150_000)
        r.disconnected(setOf("s1", "s2"), nowMs = 0)
        assertEquals(emptyList<String>(), r.expired(nowMs = 149_999))
        assertEquals(setOf("s1", "s2"), r.expired(nowMs = 150_000).toSet())
        assertNull(r.nextDeadline())
    }

    @Test fun afterAReconnectOnlyTheActiveSessionSurvives() {
        val r = SessionRetention()
        r.disconnected(setOf("old", "current"), nowMs = 0)
        assertEquals(listOf("old"), r.reconcile(active = "current"))
        assertEquals(emptyList<String>(), r.reconcile(active = null))
    }

    @Test fun aPairsReconnectReconcilesOnlyItsOwnSessions() {
        val r = SessionRetention()
        r.disconnected(setOf("a-old", "b-live"), nowMs = 0)
        // Pair A reconnected and has nothing in use; pair B's session waits for pair B.
        assertEquals(listOf("a-old"), r.reconcile(active = null, among = setOf("a-old")))
        assertEquals(setOf("b-live"), r.pending())
        assertEquals(emptyList<String>(), r.reconcile(active = "b-live", among = setOf("b-live")))
        assertEquals(emptySet<String>(), r.pending())
    }

    @Test fun aSessionThatEndedWhileOfflineIsEndedOnReconnect() {
        val r = SessionRetention()
        r.disconnected(setOf("s1"), nowMs = 0)
        assertEquals(listOf("s1"), r.reconcile(active = null))
    }

    @Test fun repeatedDropsKeepTheFirstDeadline() {
        val r = SessionRetention(graceMs = 100)
        r.disconnected(setOf("s1"), nowMs = 0)
        r.disconnected(setOf("s1"), nowMs = 90)
        assertEquals(100L, r.nextDeadline())
    }

    /**
     * The verifier's outage: the network went off at 0 and the device noticed at once; Extend
     * noticed the silent device at +52 s and would end the session 120 s after that, at about
     * +175 s. The network came back at +170 s and Extend announced the session again, so the
     * device must still hold it then (the earlier 150 s grace had already discarded it).
     */
    @Test fun anOutageExtendStillToleratesKeepsTheSession() {
        val r = SessionRetention()
        r.disconnected(setOf("a00"), nowMs = 0)
        assertEquals(emptyList<String>(), r.expired(nowMs = 170_000))
        r.confirmed("a00")
        assertEquals(emptyList<String>(), r.reconcile(active = "a00"))
    }

    /** Worst case on Extend's side: last contact at 0, noticed at 60 s, ended at 60 + 121 + 2 s. */
    @Test fun graceOutlastsTheLongestExtendKeepsAnOfflineSession() {
        assertEquals(183_000L, SessionRetention.EXTEND_KEEPS_MS)
        assertTrue(SessionRetention.GRACE_MS >= SessionRetention.EXTEND_KEEPS_MS + 30_000)
        val r = SessionRetention()
        r.disconnected(setOf("s1"), nowMs = 0)
        assertEquals(emptyList<String>(), r.expired(nowMs = SessionRetention.EXTEND_KEEPS_MS))
        assertEquals(listOf("s1"), r.expired(nowMs = SessionRetention.GRACE_MS))
    }

    /** The mirrored timing must match the service's protocol crate, or the grace above is wrong. */
    @Test fun mirroredTimingMatchesTheProtocolCrate() {
        val root = generateSequence(File("").absoluteFile) { it.parentFile }
            .firstOrNull { File(it, "crates/extend-protocol/src/lib.rs").isFile }
        assumeTrue("Runs inside the Extend repository", root != null)
        val lib = File(root, "crates/extend-protocol/src/lib.rs").readText()
        fun constant(name: String): Long =
            Regex("""pub const $name: \w+ = ([0-9_]+);""").find(lib)?.groupValues?.get(1)?.replace("_", "")?.toLong()
                ?: error("$name is missing from crates/extend-protocol/src/lib.rs")
        assertEquals(SessionRetention.HEARTBEAT_S, constant("HEARTBEAT_S"))
        assertEquals(SessionRetention.OFFLINE_AFTER_S, constant("OFFLINE_AFTER_S"))
        assertEquals(SessionRetention.SESSION_OFFLINE_GRACE_S, constant("SESSION_OFFLINE_GRACE_S"))
        val scheduler = File(root, "crates/extend-service/src/scheduler.rs").readText()
        val tick = Regex("""interval\(Duration::from_secs\((\d+)\)\)""").find(scheduler)?.groupValues?.get(1)?.toLong()
        assertNotNull("Extend's scheduler interval moved", tick)
        assertTrue("Extend's scheduler runs every $tick s", tick!! + 1 <= SessionRetention.SCHEDULER_S)
    }
}

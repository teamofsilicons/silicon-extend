package com.teamofsilicons.extend

import com.teamofsilicons.extend.driver.CommandJobs
import com.teamofsilicons.extend.protocol.CommandError
import com.teamofsilicons.extend.protocol.DeviceFrame
import com.teamofsilicons.extend.protocol.ServiceFrame
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.JsonNull
import org.junit.After
import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.CopyOnWriteArrayList

/** Every command answers once, also when a Stop, a cancel frame or a debugging disconnect stops it. */
class CommandJobsTest {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val jobs = CommandJobs(scope)
    private val sent = CopyOnWriteArrayList<DeviceFrame.Result>()
    @After fun stop() = scope.cancel()

    private fun frame(id: String, session: String = "s1", command: String = "wait") =
        ServiceFrame.Command(id = id, sessionId = session, command = command, args = emptyList(), timeoutMs = 30_000)

    private fun awaitSent(n: Int) = runBlocking { withTimeout(5000) { while (sent.size < n) kotlinx.coroutines.delay(10) } }

    @Test fun aRunningCommandStoppedBySessionEndAnswersSessionEnded() {
        val started = CompletableDeferred<Unit>()
        jobs.submit(frame("c1"), { started.complete(Unit); awaitCancellation() }, { sent += it })
        runBlocking { withTimeout(5000) { started.await() } }
        jobs.cancelSession("s1", CommandError("session_ended", "The session ended."))
        awaitSent(1)
        assertEquals("c1", sent[0].id)
        assertFalse(sent[0].ok)
        assertEquals("session_ended", sent[0].error!!.code)
    }

    @Test fun aDroppedPairConnectionStopsOnlyItsOwnCommands() {
        val startedA = CompletableDeferred<Unit>()
        val startedB = CompletableDeferred<Unit>()
        val finishB = CompletableDeferred<Unit>()
        jobs.submit(frame("a", session = "sA"), { startedA.complete(Unit); awaitCancellation() }, { sent += it }, pairId = "pairA")
        jobs.submit(frame("b", session = "sB"), { startedB.complete(Unit); finishB.await(); ok("b") }, { sent += it }, pairId = "pairB")
        runBlocking { withTimeout(5000) { startedA.await(); startedB.await() } }
        jobs.cancelPair("pairA")
        runBlocking { withTimeout(5000) { while (jobs.running > 1) kotlinx.coroutines.delay(10) } }
        assertTrue("pair A's socket is gone: its command can't answer", sent.isEmpty())
        finishB.complete(Unit)
        awaitSent(1)
        assertEquals("pair B's command answers on its own socket", "b", sent.single().id)
    }

    @Test fun aQueuedCommandAlsoAnswers() {
        val lock = Mutex()
        val holding = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        jobs.submit(frame("first"), { lock.withLock { holding.complete(Unit); release.await(); ok("first") } }, { sent += it })
        runBlocking { holding.await() }
        jobs.submit(frame("queued"), { lock.withLock { ok("queued") } }, { sent += it })
        jobs.cancel("queued", CommandError("cancelled", "Extend cancelled this command."))
        awaitSent(1)
        assertEquals("queued", sent[0].id)
        assertEquals("cancelled", sent[0].error!!.code)
        release.complete(Unit)
        awaitSent(2)
        assertTrue(sent[1].ok)
    }

    @Test fun disconnectingDebuggingOnlyStopsDebuggingCommands() {
        val started = CompletableDeferred<Unit>()
        jobs.submit(frame("snap", command = "snapshot"), { started.await(); ok("snap") }, { sent += it })
        jobs.submit(frame("rec", command = "record"), { awaitCancellation() }, { sent += it })
        jobs.cancelCommands(setOf("adb", "record"), CommandError("device_not_ready", "Android debugging was disconnected."))
        awaitSent(1)
        assertEquals("rec", sent[0].id)
        assertEquals("device_not_ready", sent[0].error!!.code)
        started.complete(Unit)
        awaitSent(2)
        assertEquals("snap", sent[1].id)
        assertTrue(sent[1].ok)
    }

    @Test fun aLostSocketStopsCommandsWithoutAnswers() {
        val started = CompletableDeferred<Unit>()
        jobs.submit(frame("c1"), { started.complete(Unit); awaitCancellation() }, { sent += it })
        runBlocking { started.await() }
        jobs.cancelAll()
        runBlocking { withTimeout(5000) { while (jobs.running > 0) kotlinx.coroutines.delay(10) } }
        assertTrue(sent.isEmpty())
    }

    @Test fun aCommandAnswersOnlyOnce() {
        jobs.submit(frame("c1"), { ok("c1") }, { sent += it })
        awaitSent(1)
        jobs.cancel("c1", CommandError("cancelled", "late"))
        Thread.sleep(100)
        assertEquals(1, sent.size)
        assertTrue(sent[0].ok)
    }

    private fun ok(id: String) = DeviceFrame.Result(id, true, JsonNull, "done", null, emptyList())
}

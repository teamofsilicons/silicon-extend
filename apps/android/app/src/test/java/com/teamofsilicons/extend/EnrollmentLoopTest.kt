package com.teamofsilicons.extend

import com.teamofsilicons.extend.core.EnrollmentLoop
import com.teamofsilicons.extend.core.EnrollmentPort
import com.teamofsilicons.extend.core.PairingMessages
import com.teamofsilicons.extend.core.PairingUi
import com.teamofsilicons.extend.net.ApiException
import com.teamofsilicons.extend.net.Backoff
import com.teamofsilicons.extend.net.Connection
import com.teamofsilicons.extend.net.SocketEvent
import com.teamofsilicons.extend.protocol.EnrollmentCreated
import com.teamofsilicons.extend.protocol.EnrollmentFrame
import com.teamofsilicons.extend.protocol.EnrollmentState
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException
import java.time.Instant
import java.time.ZoneOffset
import kotlin.random.Random

/**
 * The unpaired app's enrollment loop against a scripted service, in virtual time. The regression:
 * an enrollment whose socket was refused at once (401/404) used to be followed by a new enrollment
 * immediately, in a tight loop that exhausted the service's 60-per-hour enrollment limit.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class EnrollmentLoopTest {
    /** Full jitter always drawing its ceiling, so waits are exactly 1, 2, 4 … 60 s. */
    private object Ceiling : Random() {
        override fun nextBits(bitCount: Int): Int = 0
        override fun nextLong(from: Long, until: Long): Long = until - 1
    }

    private class FakeConnection(vararg events: SocketEvent) : Connection {
        override val events = Channel<SocketEvent>(Channel.UNLIMITED).apply { events.forEach { trySend(it) } }
        override fun send(text: String) = true
        override fun close(code: Int, reason: String) {}
    }

    /** A scripted service. Each create is timed; creating again within a second fails the test at once. */
    private inner class Service(val scope: TestScope) : EnrollmentPort {
        val creates = ArrayList<Long>()
        var createFailure: ((Int) -> Exception?) = { null }
        /** What the socket of the n-th enrollment (0-based) does. */
        var socket: (Int) -> FakeConnection = { refused() }
        var pollAnswer: (Int) -> EnrollmentState? = { throw ApiException(404, "enrollment_not_found", "This enrollment no longer exists.") }
        val discarded = ArrayList<String>()

        override suspend fun create(): EnrollmentCreated {
            val now = scope.testScheduler.currentTime
            val n = creates.size
            creates += now
            if (n > 0 && now - creates[n - 1] < 1_000) {
                throw AssertionError("enrollment ${n + 1} was created ${now - creates[n - 1]} ms after the one before (tight loop)")
            }
            createFailure(n)?.let { throw it }
            return EnrollmentCreated("e$n", "ens_$n", "abc12$n", Instant.ofEpochMilli(BASE + now + 300_000).toString())
        }

        override suspend fun poll(e: EnrollmentCreated): EnrollmentState? = pollAnswer(e.enrollmentId.drop(1).toInt())
        override fun connect(e: EnrollmentCreated): Connection = socket(e.enrollmentId.drop(1).toInt())
        override fun discard(e: EnrollmentCreated) { discarded += e.enrollmentId }
    }

    /** The service's answer to the enrollment socket of an enrollment it no longer has. */
    private fun refused() = FakeConnection(
        SocketEvent.Failed(
            java.net.ProtocolException("Expected HTTP 101 response but was '404 Not Found'"),
            404,
            ApiException(404, "enrollment_not_found", "This enrollment no longer exists (it was discarded, expired, or already paired).", "Start a new enrollment with POST /api/v1/enrollments."),
        ),
    )

    private class Harness(val loop: EnrollmentLoop, val netWake: Channel<Unit>, val ui: () -> PairingUi)

    private fun TestScope.harness(service: Service, pace: Backoff = Backoff(random = Ceiling)): Harness {
        val wake = Channel<Unit>(Channel.CONFLATED)
        var ui = PairingUi()
        val loop = EnrollmentLoop(
            port = service,
            netWake = wake,
            ui = { f -> ui = f(ui) },
            serviceUrl = { "http://10.0.2.2:8480" },
            pace = pace,
            clock = { BASE + testScheduler.currentTime },
            zone = ZoneOffset.UTC,
            log = { _, _ -> },
        )
        return Harness(loop, wake, { ui })
    }

    private fun gaps(times: List<Long>) = times.zipWithNext { a, b -> b - a }

    @Test fun anEnrollmentRefusedAtOnceIsNotReplacedAtOnce() = runTest {
        val service = Service(this)
        val h = harness(service)
        val job = launch { h.loop.run() }
        advanceTimeBy(5 * 60_000L)
        job.cancel()
        // 1 + 2 + 4 + 8 + 16 + 32 + 60 + 60 + 60 + 60 s ≈ 5 min: ten enrollments, not millions.
        assertEquals(listOf(1_000L, 2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000, 60_000), gaps(service.creates).take(9))
        assertTrue("${service.creates.size} enrollments in 5 minutes", service.creates.size <= 11)
        val ui = h.ui()
        assertNull("the dead code isn't shown", ui.code)
        assertTrue(ui.error!!, ui.error!!.startsWith("That pairing code stopped working before this device could use it (HTTP 404: This enrollment"))
        assertFalse(ui.error!!.contains("Can't reach"))
    }

    @Test fun theBackOffResetsOnlyOnceAnEnrollmentSocketHasOpened() = runTest {
        val service = Service(this)
        service.socket = { n ->
            // The fourth enrollment's socket opens (then drops, and the enrollment turns out gone).
            if (n == 3) FakeConnection(SocketEvent.Open, SocketEvent.Closed(1006, "dropped"))
            else refused()
        }
        val h = harness(service)
        val job = launch { h.loop.run() }
        advanceTimeBy(30_000L)
        job.cancel()
        assertEquals(listOf(1_000L, 2_000, 4_000, 1_000, 2_000), gaps(service.creates).take(5))
    }

    @Test fun rateLimitedWaitsWhatTheServiceAsksAndSaysSo() = runTest {
        val service = Service(this)
        service.createFailure = { n ->
            if (n == 0) ApiException(429, "rate_limited", "Too many new enrollments from this address; the limit is 60 per 60 minutes.", "Retry in 754 seconds.", 754)
            else null
        }
        service.socket = { FakeConnection(SocketEvent.Open) }
        val h = harness(service)
        val job = launch { h.loop.run() }
        runCurrent()
        val error = h.ui().error!!
        assertTrue(error, error.startsWith("Extend is limiting new pairing codes from this network, so it hasn't given this device one yet (Too many new enrollments"))
        assertTrue(error, error.contains("in 12 min 34 s (at "))
        assertFalse("Extend was reachable", error.contains("Can't reach"))
        // A network change doesn't cut a rate-limit wait short.
        h.netWake.trySend(Unit)
        advanceTimeBy(753_000L)
        assertEquals(1, service.creates.size)
        advanceTimeBy(2_000L)
        assertEquals(listOf(0L, 754_000L), service.creates)
        assertEquals("ABC121", h.ui().code)
        assertNull(h.ui().error)
        job.cancel()
    }

    @Test fun aRateLimitWithoutRetryAfterStillWaitsAMinute() = runTest {
        val service = Service(this)
        service.createFailure = { n -> if (n == 0) ApiException(429, null, "HTTP 429 from POST /api/v1/enrollments") else null }
        service.socket = { FakeConnection(SocketEvent.Open) }
        val h = harness(service)
        val job = launch { h.loop.run() }
        advanceTimeBy(59_000L)
        assertEquals(1, service.creates.size)
        advanceTimeBy(2_000L)
        assertEquals(listOf(0L, 60_000L), service.creates)
        job.cancel()
    }

    @Test fun anUnreachableServiceSaysSoAndRetriesWhenTheNetworkReturns() = runTest {
        val service = Service(this)
        service.createFailure = { n -> if (n < 3) IOException("Failed to connect to /10.0.2.2:8480") else null }
        service.socket = { FakeConnection(SocketEvent.Open) }
        val h = harness(service)
        val job = launch { h.loop.run() }
        advanceTimeBy(1_500L + 2_500L)
        assertEquals(listOf(0L, 1_000L, 3_000L), service.creates)
        val error = h.ui().error!!
        assertTrue(error, error.startsWith("Can't reach Extend at 10.0.2.2:8480 (Failed to connect to /10.0.2.2:8480). Check this device's internet connection."))
        // The third wait is 4 s; the network coming back ends it early (but never under the service's answer).
        advanceTimeBy(500L)
        h.netWake.trySend(Unit)
        runCurrent()
        assertEquals(4, service.creates.size)
        assertEquals(4_500L, service.creates.last())
        job.cancel()
    }

    @Test fun aServiceErrorIsNotCalledUnreachable() = runTest {
        val service = Service(this)
        service.createFailure = { n -> if (n == 0) ApiException(503, "service_unavailable", "Postgres is unavailable right now: timeout.", "Retry in a moment.") else null }
        service.socket = { FakeConnection(SocketEvent.Open) }
        val h = harness(service)
        val job = launch { h.loop.run() }
        runCurrent()
        assertEquals(
            "Extend refused a new pairing code (HTTP 503 service_unavailable: Postgres is unavailable right now: timeout). Retry in a moment. " +
                "The app asks again in 1 s. If it keeps failing, report it with this message.",
            h.ui().error,
        )
        job.cancel()
    }

    @Test fun pairsAndAbandonedEnrollmentsAreDiscarded() = runTest {
        val service = Service(this)
        service.socket = { FakeConnection(SocketEvent.Open, SocketEvent.Text("""{"type":"paired","device_id":"7c1e09ab","device_credential":"dev_x"}""")) }
        val h = harness(service)
        var paired: EnrollmentFrame.Paired? = null
        launch { paired = h.loop.run() }
        runCurrent()
        assertEquals("7c1e09ab", paired?.deviceId)
        assertEquals("dev_x", paired?.deviceCredential)

        val waiting = Service(this)
        waiting.socket = { FakeConnection(SocketEvent.Open) }
        val job = launch { harness(waiting).loop.run() }
        runCurrent()
        job.cancel()
        runCurrent()
        assertEquals(listOf("e0"), waiting.discarded)
    }

    @Test fun durationsReadNaturally() {
        assertEquals("8 s", PairingMessages.duration(7_200))
        assertEquals("89 s", PairingMessages.duration(89_000))
        assertEquals("2 min 5 s", PairingMessages.duration(125_000))
        assertEquals("12 min", PairingMessages.duration(720_000))
        assertEquals("1 h 2 min", PairingMessages.duration(3_720_000))
        assertEquals("in 45 s", PairingMessages.whenNext(45_000, 0, ZoneOffset.UTC))
        assertEquals("in 12 min 34 s (at 00:12)", PairingMessages.whenNext(754_000, 0, ZoneOffset.UTC))
    }

    companion object {
        private val BASE = Instant.parse("2026-09-27T10:00:00Z").toEpochMilli()
    }
}

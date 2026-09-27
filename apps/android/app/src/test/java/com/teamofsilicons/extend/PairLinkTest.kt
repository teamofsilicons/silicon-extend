package com.teamofsilicons.extend

import com.teamofsilicons.extend.core.Link
import com.teamofsilicons.extend.core.LinkHost
import com.teamofsilicons.extend.core.LinkPort
import com.teamofsilicons.extend.core.PairLink
import com.teamofsilicons.extend.core.PairSupervisor
import com.teamofsilicons.extend.net.Backoff
import com.teamofsilicons.extend.net.Connection
import com.teamofsilicons.extend.net.SocketEvent
import com.teamofsilicons.extend.protocol.DeviceFrame
import com.teamofsilicons.extend.protocol.ServiceFrame
import com.teamofsilicons.extend.security.StoredPair
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.random.Random

/**
 * One connection per Carbon's pair: each link keeps its own state, ends only with its own pair,
 * and handles its frames one at a time, in order.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class PairLinkTest {
    private object Ceiling : Random() {
        override fun nextBits(bitCount: Int): Int = 0
        override fun nextLong(from: Long, until: Long): Long = until - 1
    }

    private class FakeConnection : Connection {
        override val events = Channel<SocketEvent>(Channel.UNLIMITED)
        val sent = ArrayList<String>()
        var closed = false
        override fun send(text: String): Boolean = !closed && sent.add(text)
        override fun close(code: Int, reason: String) { closed = true }
        fun open() = apply { events.trySend(SocketEvent.Open) }
        fun frame(json: String) = apply { events.trySend(SocketEvent.Text(json)) }
        fun closedWith(code: Int) = apply { events.trySend(SocketEvent.Closed(code, "")) }
    }

    /** Connections per credential, in the order they are opened. */
    private class Port(vararg scripts: Pair<String, List<FakeConnection>>) : LinkPort {
        val queues = scripts.associate { (cred, conns) -> cred to ArrayDeque(conns) }
        val opened = ArrayList<String>()
        override fun connect(credential: String): Connection {
            opened += credential
            return queues[credential]?.removeFirstOrNull() ?: FakeConnection()
        }
    }

    private class Host(val onFrame: suspend (PairLink, ServiceFrame) -> PairLink.Exit? = { _, _ -> null }) : LinkHost {
        val log = ArrayList<String>()
        val states = HashMap<String, Link>()
        override fun opened(link: PairLink) { log += "opened ${link.deviceId}"; link.send(DeviceFrame.Pong(0)) }
        override suspend fun handle(link: PairLink, frame: ServiceFrame): PairLink.Exit? {
            log += "frame ${link.deviceId} ${frame.javaClass.simpleName}"
            return onFrame(link, frame)
        }
        override fun closed(link: PairLink) { log += "closed ${link.deviceId}" }
        override fun state(link: PairLink, state: Link, detail: String?) { states[link.deviceId] = state }
    }

    private fun TestScope.link(pair: StoredPair, port: LinkPort, host: LinkHost) =
        PairLink(pair, port, host, clock = { testScheduler.currentTime }, backoff = Backoff(random = Ceiling))

    @Test fun unpairedOnOneLinkKeepsTheOthers() = runTest {
        val a = FakeConnection().open().closedWith(4401)
        val b = FakeConnection().open().frame("""{"type":"ping","nonce":1}""")
        val port = Port("edc_a" to listOf(a), "edc_b" to listOf(b))
        val host = Host { l, f -> if (f is ServiceFrame.Ping) { l.send(DeviceFrame.Pong(f.nonce)); null } else null }
        val ended = ArrayList<String>()
        val sup = PairSupervisor(backgroundScope, { p -> link(p, port, host) }, { ended += it.deviceId })
        sup.sync(listOf(StoredPair("a1", "edc_a"), StoredPair("b2", "edc_b")))
        runCurrent()
        assertEquals(listOf("a1"), ended)
        assertEquals("only b2 still runs", listOf("b2"), sup.links().map { it.deviceId })
        assertTrue(sup.link("b2")!!.connected)
        assertFalse("b2's socket is untouched", b.closed)
        assertTrue("b2 answered on its own socket", b.sent.any { it.contains("\"nonce\":1") })
        assertTrue(host.log.contains("closed a1"))
        assertFalse(host.log.contains("closed b2"))
        assertEquals(Link.CONNECTED, host.states["b2"])
    }

    @Test fun eachLinkUsesItsOwnCredentialAndHello() = runTest {
        val port = Port("edc_a" to listOf(FakeConnection().open()), "edc_b" to listOf(FakeConnection().open()))
        val host = Host()
        val sup = PairSupervisor(backgroundScope, { p -> link(p, port, host) }, {})
        sup.sync(listOf(StoredPair("a1", "edc_a"), StoredPair("b2", "edc_b")))
        runCurrent()
        assertEquals(listOf("edc_a", "edc_b"), port.opened)
        assertEquals(listOf("opened a1", "opened b2"), host.log.filter { it.startsWith("opened") })
        // Dropping a pair from the stored list stops just its link.
        sup.sync(listOf(StoredPair("b2", "edc_b")))
        runCurrent()
        assertEquals(listOf("b2"), sup.links().map { it.deviceId })
    }

    @Test fun supersededWaitsForReconnect() = runTest {
        val first = FakeConnection().open().frame("""{"type":"superseded"}""")
        val second = FakeConnection().open()
        val port = Port("edc_a" to listOf(first, second))
        val host = Host { _, f -> if (f is ServiceFrame.Superseded) PairLink.Exit.SUPERSEDED else null }
        val l = link(StoredPair("a1", "edc_a"), port, host)
        backgroundScope.launch { l.run() }
        runCurrent()
        assertEquals(Link.SUPERSEDED, host.states["a1"])
        advanceTimeBy(600_000)
        assertEquals("it never reconnects by itself", 1, port.opened.size)
        l.reconnect()
        runCurrent()
        assertEquals(2, port.opened.size)
        assertEquals(Link.CONNECTED, host.states["a1"])
    }

    @Test fun aDroppedSocketReconnectsWithBackoff() = runTest {
        val port = Port("edc_a" to listOf(FakeConnection().open().closedWith(1011), FakeConnection().open()))
        val host = Host()
        val l = link(StoredPair("a1", "edc_a"), port, host)
        backgroundScope.launch { l.run() }
        runCurrent()
        assertEquals(Link.OFFLINE, host.states["a1"])
        advanceTimeBy(1_001)
        runCurrent()
        assertEquals(2, port.opened.size)
        assertEquals(Link.CONNECTED, host.states["a1"])
    }

    @Test fun aSessionsFramesAreHandledInOrder() = runTest {
        // session_started must be fully handled (another side's wake requests hidden) before the
        // session's first command reaches the executor, even when that takes a while.
        val conn = FakeConnection().open()
            .frame("""{"type":"session_started","target":null,"session_id":"a3f","silicon_id":"si:chef","since":"x","side":"s1"}""")
            .frame("""{"type":"command","id":"c1","session_id":"a3f","command":"notifications","timeout_ms":30000,"upload_ids":[]}""")
        val order = ArrayList<String>()
        val host = Host { _, f ->
            when (f) {
                is ServiceFrame.SessionStarted -> { delay(500); order += "redacted" }
                is ServiceFrame.Command -> order += "command"
                else -> {}
            }
            null
        }
        val l = link(StoredPair("a1", "edc_a"), Port("edc_a" to listOf(conn)), host)
        backgroundScope.launch { l.run() }
        advanceTimeBy(1_000)
        assertEquals(listOf("redacted", "command"), order)
    }

    @Test fun unpairStopsTheLinkForGood() = runTest {
        val conn = FakeConnection().open()
        val port = Port("edc_a" to listOf(conn))
        val ended = ArrayList<String>()
        val host = Host()
        val sup = PairSupervisor(backgroundScope, { p -> link(p, port, host) }, { ended += it.deviceId })
        sup.sync(listOf(StoredPair("a1", "edc_a")))
        runCurrent()
        sup.link("a1")!!.unpair()
        runCurrent()
        assertTrue(conn.closed)
        assertEquals(listOf("a1"), ended)
        assertEquals(1, port.opened.size)
    }

    @Test fun aQuietConnectionIsRemadeWhenTheScreenComesOn() = runTest {
        val first = FakeConnection().open()
        val port = Port("edc_a" to listOf(first, FakeConnection().open()))
        val host = Host()
        val l = link(StoredPair("a1", "edc_a"), port, host)
        backgroundScope.launch { l.run() }
        runCurrent()
        l.kick()
        runCurrent()
        assertEquals("heard from just now: kept", 1, port.opened.size)
        advanceTimeBy(PairLink.QUIET_MS + 1)
        l.kick()
        runCurrent()
        assertTrue(first.closed)
        assertEquals("re-made at once, without the back-off", 2, port.opened.size)
    }
}

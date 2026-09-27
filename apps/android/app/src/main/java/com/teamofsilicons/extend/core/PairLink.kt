package com.teamofsilicons.extend.core

import com.teamofsilicons.extend.net.ApiException
import com.teamofsilicons.extend.net.Backoff
import com.teamofsilicons.extend.net.Connection
import com.teamofsilicons.extend.net.SocketEvent
import com.teamofsilicons.extend.protocol.DeviceFrame
import com.teamofsilicons.extend.protocol.Frames
import com.teamofsilicons.extend.protocol.MissingCapability
import com.teamofsilicons.extend.protocol.ServiceFrame
import com.teamofsilicons.extend.protocol.Setup
import com.teamofsilicons.extend.security.StoredPair
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.selects.select
import kotlinx.coroutines.withTimeoutOrNull
import java.time.ZoneId

/** What a [PairLink] needs from the service; [ConnectionManager] wires the real socket, tests fake it. */
fun interface LinkPort {
    /** Opens `/api/v1/device/connect` with this pair's credential. */
    fun connect(credential: String): Connection
}

/** What a [PairLink] tells the app around it, and asks of it. */
interface LinkHost {
    /** The socket opened: send hello and awake on it, and re-read the pair. */
    fun opened(link: PairLink)

    /**
     * A frame arrived. Frames of one connection are handled one at a time, in order, so what this
     * does before returning happens before the next frame (a session's commands follow its
     * `session_started`). A non-null answer ends the connection that way.
     */
    suspend fun handle(link: PairLink, frame: ServiceFrame): PairLink.Exit?

    /** The socket is gone: commands that came on it can no longer answer. */
    fun closed(link: PairLink)

    /** The connection's state for the pair's row ([detail]: what to show beside it). */
    fun state(link: PairLink, state: Link, detail: String?)
}

/**
 * One Carbon's pair of this device, connected to Extend with that pair's own credential: exactly
 * a 1.0 device connection (docs/device-protocol.md), one per pair. It reconnects with back-off, and
 * ends only when the pair does ([run] returns: `unpaired`, close 4401, or [unpair]). When another
 * connection took the pair over (`superseded`), or the app is too old, it waits for [reconnect].
 */
class PairLink(
    val pair: StoredPair,
    private val port: LinkPort,
    private val host: LinkHost,
    private val clock: () -> Long = System::currentTimeMillis,
    private val backoff: Backoff = Backoff(),
    private val zone: ZoneId = ZoneId.systemDefault(),
) {
    enum class Exit { UNPAIRED, SUPERSEDED, UPGRADE_REQUIRED }

    val deviceId: String get() = pair.deviceId
    val credential: String get() = pair.credential

    private val netWake = Channel<Unit>(Channel.CONFLATED)
    private val manualWake = Channel<Unit>(Channel.CONFLATED)
    /** Ends the current connection's loop at once ([unpair], [kick], [reconnect]), whatever the socket still reports. */
    private val interrupt = Channel<Unit>(Channel.CONFLATED)
    @Volatile private var connection: Connection? = null
    @Volatile private var unpairRequested = false
    /** Set by [kick]: reconnect without waiting out the back-off. */
    @Volatile private var hurry = false

    /** When a frame last arrived (the service pings every 15 s), by [clock]. */
    @Volatile var lastHeardAt: Long = 0
        private set

    /** Whether the socket is open (hello can go). */
    @Volatile var connected: Boolean = false
        private set

    /** What this connection last told the service, so only changes are sent again. */
    @Volatile var sentCaps: Pair<List<String>, List<MissingCapability>>? = null
    @Volatile var sentSetup: Setup? = null

    /** Runs until this pair ends. */
    suspend fun run() {
        while (currentCoroutineContext().isActive) {
            when (runSocket()) {
                Exit.UNPAIRED -> return
                Exit.SUPERSEDED -> {
                    host.state(this, Link.SUPERSEDED, null)
                    if (waitForCarbon()) return
                }
                Exit.UPGRADE_REQUIRED -> {
                    host.state(this, Link.UPGRADE_REQUIRED, null)
                    if (waitForCarbon()) return
                }
            }
        }
    }

    /** Waits for Reconnect; true when the pair ended meanwhile. */
    private suspend fun waitForCarbon(): Boolean {
        manualWake.receive()
        return unpairRequested
    }

    private suspend fun runSocket(): Exit {
        while (currentCoroutineContext().isActive) {
            if (unpairRequested) return Exit.UNPAIRED
            host.state(this, Link.CONNECTING, null)
            lastHeardAt = clock()
            interrupt.tryReceive()
            val sock = port.connect(credential)
            connection = sock
            var exit: Exit? = null
            var limited: ApiException? = null
            try {
                loop@ while (true) {
                    val ev = select<SocketEvent?> {
                        sock.events.onReceive { it }
                        interrupt.onReceive { null }
                    }
                    if (ev == null) {
                        com.teamofsilicons.extend.Extend.log("pair $deviceId: connection ended by the app (${if (unpairRequested) "pair ended" else "reconnecting"})")
                        break@loop
                    }
                    lastHeardAt = clock()
                    when (ev) {
                        SocketEvent.Open -> {
                            backoff.reset()
                            hurry = false
                            connected = true
                            sentCaps = null
                            sentSetup = null
                            host.state(this, Link.CONNECTED, null)
                            host.opened(this)
                        }
                        is SocketEvent.Text -> {
                            exit = host.handle(this, Frames.decodeService(ev.text))
                            if (exit != null) break@loop
                        }
                        is SocketEvent.Closed -> {
                            exit = when (ev.code) {
                                Frames.CLOSE_UNAUTHORIZED -> Exit.UNPAIRED
                                Frames.CLOSE_SUPERSEDED -> Exit.SUPERSEDED
                                Frames.CLOSE_UPGRADE_REQUIRED -> Exit.UPGRADE_REQUIRED
                                else -> null
                            }
                            com.teamofsilicons.extend.Extend.log("pair $deviceId: socket closed ${ev.code} ${ev.reason}")
                            break@loop
                        }
                        is SocketEvent.Failed -> {
                            exit = when (ev.httpStatus) {
                                401 -> Exit.UNPAIRED
                                426 -> Exit.UPGRADE_REQUIRED
                                else -> null
                            }
                            if (ev.httpStatus == 429 || ev.refusal?.rateLimited == true) limited = ev.refusal ?: ApiException(429, "rate_limited", "HTTP 429")
                            com.teamofsilicons.extend.Extend.log("pair $deviceId: socket failed (${ev.httpStatus})", ev.error)
                            break@loop
                        }
                    }
                }
            } finally {
                connected = false
                connection = null
                sock.close()
                // Running commands of this pair stop; sessions keep their recordings and logs through
                // a brief outage, as Extend keeps the sessions themselves (CommandExecutor.connectionLost).
                host.closed(this)
            }
            if (unpairRequested) return Exit.UNPAIRED
            if (exit != null) return exit
            if (limited != null) {
                // Extend answered: it is limiting connections, so the network coming back can't help.
                val wait = limited.retryAfterS?.let { (it * 1000).coerceIn(1_000, EnrollmentLoop.MAX_RETRY_AFTER_MS) }
                    ?: maxOf(backoff.next(), EnrollmentLoop.RATE_LIMIT_FALLBACK_MS)
                host.state(this, Link.OFFLINE, "Extend is limiting connections from this network; reconnecting ${PairingMessages.whenNext(wait, clock(), zone)}")
                delay(wait)
                continue
            }
            if (hurry) {
                hurry = false
                continue
            }
            val wait = backoff.next()
            host.state(this, Link.OFFLINE, "Reconnecting in ${(wait + 999) / 1000} s")
            withTimeoutOrNull(wait) {
                select<Unit> {
                    netWake.onReceive {}
                    manualWake.onReceive {}
                }
            }
        }
        return Exit.UNPAIRED
    }

    /** Sends a frame on this pair's connection; false when it isn't connected. */
    fun send(frame: DeviceFrame): Boolean = connection?.takeIf { connected }?.send(Frames.encode(frame)) ?: false

    /** Reconnect now: after "superseded" or "update required", or at the Carbon's request. */
    fun reconnect() {
        hurry = true
        val c = connection
        if (c != null) {
            interrupt.trySend(Unit)
            c.close()
        } else {
            // Waiting for the Carbon (superseded, too old) or out the back-off.
            manualWake.trySend(Unit)
        }
    }

    /** The network came back: stop waiting out the back-off. */
    fun networkBack() {
        netWake.trySend(Unit)
    }

    /**
     * The screen came on: a connection that has heard nothing for [QUIET_MS] (the service pings
     * every 15 s) may be dead after the device slept, so it is re-made at once.
     */
    fun kick(quietMs: Long = QUIET_MS) {
        if (connection != null && clock() - lastHeardAt > quietMs) {
            hurry = true
            interrupt.trySend(Unit)
            connection?.close()
        } else {
            netWake.trySend(Unit)
        }
    }

    /** The pair ended (Revoke pair, or `GET /api/v1/device` said 401): [run] returns. */
    fun unpair() {
        unpairRequested = true
        manualWake.trySend(Unit)
        netWake.trySend(Unit)
        interrupt.trySend(Unit)
        connection?.close()
    }

    companion object {
        /** How long a connection may stay silent before a screen-on re-makes it. */
        const val QUIET_MS = 20_000L
    }
}

/**
 * Runs one [PairLink] per stored pair: starts links for new pairs, stops the links of pairs that
 * went, and reports a link whose pair ended on its own ([ended]), leaving every other link alone.
 */
class PairSupervisor(
    private val scope: CoroutineScope,
    private val newLink: (StoredPair) -> PairLink,
    private val ended: (PairLink) -> Unit,
) {
    private class Running(val link: PairLink, val job: Job)

    private val running = LinkedHashMap<String, Running>()

    /** Makes the running links match [pairs]. */
    @Synchronized
    fun sync(pairs: List<StoredPair>) {
        val wanted = pairs.associateBy { it.deviceId }
        for ((id, r) in running.entries.toList()) {
            if (wanted[id] != r.link.pair) {
                running.remove(id)
                r.job.cancel()
            }
        }
        for (p in pairs) {
            if (p.deviceId in running) continue
            val link = newLink(p)
            val job = scope.launch {
                while (true) {
                    try {
                        link.run()
                        break
                    } catch (e: CancellationException) {
                        throw e
                    } catch (e: Exception) {
                        // A bug handling one frame must not end this pair's connection for good.
                        com.teamofsilicons.extend.Extend.log("pair ${link.deviceId}: connection loop failed; restarting it", e)
                        delay(RESTART_MS)
                    }
                }
                // The pair ended by itself (unpaired): forget it, unless it was already replaced.
                if (forget(link)) ended(link)
            }
            running[p.deviceId] = Running(link, job)
        }
    }

    @Synchronized
    private fun forget(link: PairLink): Boolean {
        val r = running[link.deviceId] ?: return false
        if (r.link !== link) return false
        running.remove(link.deviceId)
        return true
    }

    /** Stops [deviceId]'s link without reporting it (the app ended the pair itself). */
    @Synchronized
    fun stop(deviceId: String) {
        running.remove(deviceId)?.let { it.link.unpair(); it.job.cancel() }
    }

    @Synchronized
    fun stopAll() {
        for (r in running.values) r.job.cancel()
        running.clear()
    }

    @Synchronized
    fun link(deviceId: String?): PairLink? = deviceId?.let { running[it]?.link }

    @Synchronized
    fun links(): List<PairLink> = running.values.map { it.link }

    private companion object {
        const val RESTART_MS = 5_000L
    }
}

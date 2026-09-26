package com.teamofsilicons.bridge.core

import android.net.ConnectivityManager
import android.net.Network
import com.teamofsilicons.bridge.Bridge
import com.teamofsilicons.bridge.config.DeviceInfo
import com.teamofsilicons.bridge.net.ApiException
import com.teamofsilicons.bridge.net.Backoff
import com.teamofsilicons.bridge.net.Socket
import com.teamofsilicons.bridge.net.SocketEvent
import com.teamofsilicons.bridge.protocol.DeviceFrame
import com.teamofsilicons.bridge.protocol.EnrollmentCreate
import com.teamofsilicons.bridge.protocol.EnrollmentCreated
import com.teamofsilicons.bridge.protocol.EnrollmentFrame
import com.teamofsilicons.bridge.protocol.EnrollmentState
import com.teamofsilicons.bridge.protocol.Frames
import com.teamofsilicons.bridge.protocol.MissingCapability
import com.teamofsilicons.bridge.protocol.ServiceFrame
import com.teamofsilicons.bridge.protocol.Setup
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeoutOrNull
import java.time.Instant
import kotlin.coroutines.coroutineContext

/**
 * Owns the app's connection to Bridge: the enrollment (pairing code) while unpaired, then the
 * device socket, kept open with exponential backoff. Runs inside the foreground service.
 */
class ConnectionManager(private val bridge: Bridge) {
    private enum class Exit { UNPAIRED, SUPERSEDED, UPGRADE_REQUIRED, RESTART }

    private var mainJob: Job? = null
    private var watcherJob: Job? = null
    @Volatile private var socket: Socket? = null
    private val netWake = Channel<Unit>(Channel.CONFLATED)
    private val manualWake = Channel<Unit>(Channel.CONFLATED)
    private val sendLock = Mutex()
    private var sentCaps: Pair<List<String>, List<MissingCapability>>? = null
    private var sentSetup: Setup? = null
    @Volatile private var pendingUnpair = false

    private val config get() = bridge.config

    fun start() {
        if (mainJob?.isActive == true) return
        mainJob = bridge.scope.launch { runLoop() }
        if (watcherJob?.isActive != true) {
            watcherJob = bridge.scope.launch {
                while (isActive) {
                    recomputeSetup()
                    delay(3_000)
                }
            }
        }
        registerNetworkCallback()
    }

    /** Reconnect now (after "superseded", "update required", or a changed service URL). */
    fun reconnect() {
        val old = mainJob
        mainJob = null
        old?.cancel()
        socket?.close()
        socket = null
        start()
    }

    private var networkRegistered = false
    private fun registerNetworkCallback() {
        if (networkRegistered) return
        networkRegistered = true
        runCatching {
            bridge.context.getSystemService(ConnectivityManager::class.java).registerDefaultNetworkCallback(
                object : ConnectivityManager.NetworkCallback() {
                    override fun onAvailable(network: Network) {
                        netWake.trySend(Unit)
                    }
                },
            )
        }
    }

    private suspend fun runLoop() {
        while (coroutineContext.isActive) {
            val credential = bridge.secrets.readCredential()
            if (credential == null) {
                bridge.update {
                    it.copy(phase = Phase.UNPAIRED, device = null, session = null, takeover = null, deviceId = null, environment = null)
                }
                enroll()
                continue
            }
            bridge.update { it.copy(phase = Phase.PAIRED, deviceId = config.deviceId, environment = config.environment, pairing = PairingUi()) }
            when (runDevice(credential)) {
                Exit.UNPAIRED -> forgetPairLocally()
                Exit.SUPERSEDED -> {
                    bridge.update { it.copy(link = Link.SUPERSEDED, linkDetail = "Another connection for this device took over. Tap Reconnect to use this one.") }
                    manualWake.receive()
                }
                Exit.UPGRADE_REQUIRED -> {
                    bridge.update { it.copy(link = Link.UPGRADE_REQUIRED, linkDetail = "This version of Silicon Bridge is too old. Install the latest from bridge.teamofsilicons.com.") }
                    manualWake.receive()
                }
                Exit.RESTART -> {}
            }
        }
    }

    // ───────────── Enrollment ─────────────

    private suspend fun enroll() {
        val backoff = Backoff()
        while (coroutineContext.isActive) {
            val created = try {
                bridge.api.createEnrollment(
                    EnrollmentCreate(
                        os = bridge.os,
                        osVersion = DeviceInfo.osVersion,
                        model = DeviceInfo.model,
                        appVersion = DeviceInfo.APP_VERSION,
                    ),
                )
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                val wait = backoff.next()
                bridge.update {
                    it.copy(pairing = it.pairing.copy(live = false, error = "Can't reach Bridge (${describe(e)}). Retrying in ${wait / 1000} s."))
                }
                Bridge.log("enrollment create failed", e)
                withTimeoutOrNull(wait) { netWake.receive() }
                continue
            }
            backoff.reset()
            bridge.update { it.copy(pairing = PairingUi(created.pairingCode.uppercase(), created.codeExpiresAt, live = false, error = null)) }
            val paired = try {
                followEnrollment(created)
            } catch (e: CancellationException) {
                // Abandoned (restart, new service URL): tell the service so its code stops working.
                bridge.scope.launch { runCatching { bridge.api.discardEnrollment(created.enrollmentId, created.enrollmentSecret) } }
                throw e
            } ?: continue
            bridge.secrets.writeCredential(paired.deviceCredential)
            config.deviceId = paired.deviceId
            config.environment = paired.environment
            sentCaps = null
            sentSetup = null
            bridge.update {
                it.copy(phase = Phase.PAIRED, deviceId = paired.deviceId, environment = paired.environment, pairing = PairingUi())
            }
            Bridge.log("paired as ${paired.deviceId}")
            return
        }
    }

    /** Follows one enrollment until it pairs (the result) or is gone (null). */
    private suspend fun followEnrollment(e: EnrollmentCreated): EnrollmentFrame.Paired? {
        val backoff = Backoff()
        var expiresAt = e.codeExpiresAt
        while (coroutineContext.isActive) {
            val sock = Socket.open(
                bridge.api.client,
                config.webSocketUrl("/api/v1/enrollments/${e.enrollmentId}/connect"),
                "Bridge-Enrollment ${e.enrollmentSecret}",
            )
            var gone = false
            try {
                loop@ while (true) {
                    val untilExpiry = millisUntil(expiresAt)?.plus(5_000)?.coerceAtLeast(5_000) ?: 60_000
                    val ev = withTimeoutOrNull(untilExpiry) { sock.events.receive() }
                    if (ev == null) {
                        // The code expired without a rotation frame: ask for the current one.
                        when (val st = pollEnrollment(e)) {
                            is PollResult.Paired -> return st.paired
                            PollResult.Gone -> { gone = true; break@loop }
                            is PollResult.Waiting -> expiresAt = st.expiresAt
                            PollResult.Unknown -> break@loop
                        }
                        continue
                    }
                    when (ev) {
                        SocketEvent.Open -> {
                            backoff.reset()
                            bridge.update { it.copy(pairing = it.pairing.copy(live = true, error = null)) }
                        }
                        is SocketEvent.Text -> when (val f = Frames.decodeEnrollment(ev.text)) {
                            is EnrollmentFrame.Code -> {
                                expiresAt = f.codeExpiresAt
                                bridge.update { it.copy(pairing = it.pairing.copy(code = f.pairingCode.uppercase(), expiresAt = f.codeExpiresAt, live = true)) }
                            }
                            is EnrollmentFrame.Paired -> return f
                            is EnrollmentFrame.Ping -> sock.send(Frames.encode(DeviceFrame.Pong(f.nonce)))
                            is EnrollmentFrame.Unknown -> Bridge.log("enrollment socket: ignored frame ${f.type}: ${f.problem ?: ""}")
                        }
                        is SocketEvent.Closed -> break@loop
                        is SocketEvent.Failed -> {
                            if (ev.httpStatus == 401 || ev.httpStatus == 404) gone = true
                            Bridge.log("enrollment socket failed (${ev.httpStatus})", ev.error)
                            break@loop
                        }
                    }
                }
            } finally {
                sock.close()
            }
            bridge.update { it.copy(pairing = it.pairing.copy(live = false)) }
            if (gone) return null
            // The socket dropped: maybe we paired meanwhile, maybe the enrollment is gone.
            when (val st = pollEnrollment(e)) {
                is PollResult.Paired -> return st.paired
                PollResult.Gone -> return null
                is PollResult.Waiting -> {
                    expiresAt = st.expiresAt
                    bridge.update { it.copy(pairing = it.pairing.copy(code = st.code, expiresAt = st.expiresAt)) }
                }
                PollResult.Unknown -> {}
            }
            withTimeoutOrNull(backoff.next()) { netWake.receive() }
        }
        return null
    }

    private sealed interface PollResult {
        data class Paired(val paired: EnrollmentFrame.Paired) : PollResult
        data class Waiting(val code: String, val expiresAt: String) : PollResult
        data object Gone : PollResult
        data object Unknown : PollResult
    }

    private suspend fun pollEnrollment(e: EnrollmentCreated): PollResult = try {
        when (val st = bridge.api.getEnrollment(e.enrollmentId, e.enrollmentSecret)) {
            is EnrollmentState.Paired -> PollResult.Paired(EnrollmentFrame.Paired(st.deviceId, st.deviceCredential, st.environment))
            is EnrollmentState.Waiting -> PollResult.Waiting(st.pairingCode.uppercase(), st.codeExpiresAt)
            null -> PollResult.Unknown
        }
    } catch (ex: ApiException) {
        if (ex.status == 401 || ex.status == 404) PollResult.Gone else PollResult.Unknown
    } catch (ex: CancellationException) {
        throw ex
    } catch (_: Exception) {
        PollResult.Unknown
    }

    // ───────────── The device socket ─────────────

    private suspend fun runDevice(credential: String): Exit {
        val backoff = Backoff()
        pendingUnpair = false
        while (coroutineContext.isActive) {
            if (pendingUnpair || bridge.secrets.readCredential() == null) return Exit.UNPAIRED
            bridge.update { it.copy(link = Link.CONNECTING, linkDetail = null) }
            val sock = Socket.open(bridge.api.client, config.webSocketUrl("/api/v1/device/connect"), "Bridge-Device $credential")
            socket = sock
            var exit: Exit? = null
            try {
                loop@ for (ev in sock.events) {
                    when (ev) {
                        SocketEvent.Open -> {
                            backoff.reset()
                            bridge.update { it.copy(link = Link.CONNECTED, linkDetail = null) }
                            sentCaps = null
                            sentSetup = null
                            sendHello(force = true)
                            bridge.scope.launch { refreshDevice(credential) }
                        }
                        is SocketEvent.Text -> {
                            exit = handle(Frames.decodeService(ev.text), sock)
                            if (exit != null) break@loop
                        }
                        is SocketEvent.Closed -> {
                            exit = when (ev.code) {
                                Frames.CLOSE_UNAUTHORIZED -> Exit.UNPAIRED
                                Frames.CLOSE_SUPERSEDED -> Exit.SUPERSEDED
                                Frames.CLOSE_UPGRADE_REQUIRED -> Exit.UPGRADE_REQUIRED
                                else -> null
                            }
                            Bridge.log("device socket closed ${ev.code} ${ev.reason}")
                            break@loop
                        }
                        is SocketEvent.Failed -> {
                            exit = when (ev.httpStatus) {
                                401 -> Exit.UNPAIRED
                                426 -> Exit.UPGRADE_REQUIRED
                                else -> null
                            }
                            Bridge.log("device socket failed (${ev.httpStatus})", ev.error)
                            break@loop
                        }
                    }
                }
            } finally {
                socket = null
                sock.close()
                bridge.executor.cancelAll()
            }
            if (pendingUnpair) return Exit.UNPAIRED
            if (exit != null) return exit
            val wait = backoff.next()
            bridge.update { it.copy(link = Link.OFFLINE, linkDetail = "Reconnecting in ${(wait + 999) / 1000} s") }
            withTimeoutOrNull(wait) { netWake.receive() }
        }
        return Exit.RESTART
    }

    private fun handle(frame: ServiceFrame, sock: Socket): Exit? {
        when (frame) {
            is ServiceFrame.Ping -> sock.send(Frames.encode(DeviceFrame.Pong(frame.nonce)))
            is ServiceFrame.Command -> {
                bridge.update { it.copy(lastCommand = frame.command) }
                bridge.executor.submit(frame) { result -> send(result) }
            }
            is ServiceFrame.Cancel -> bridge.executor.cancel(frame.id)
            is ServiceFrame.SessionStarted -> {
                bridge.executor.beginSession(frame.sessionId)
                bridge.update { it.copy(session = SessionUi(frame.siliconId, frame.sessionId, frame.since)) }
            }
            is ServiceFrame.SessionEnded -> {
                bridge.executor.endSession(frame.sessionId)
                bridge.update { s ->
                    if (s.session == null || s.session.sessionId == frame.sessionId) s.copy(session = null, takeover = null) else s
                }
            }
            is ServiceFrame.Takeover -> bridge.update { it.copy(takeover = TakeoverUi(frame.sessionId, frame.reason, frame.expiresAt)) }
            is ServiceFrame.TakeoverEnded -> bridge.update { it.copy(takeover = null) }
            ServiceFrame.Refresh -> bridge.secrets.readCredential()?.let { c -> bridge.scope.launch { refreshDevice(c) } }
            is ServiceFrame.Environment -> {
                config.environment = frame.environment
                bridge.update { it.copy(environment = frame.environment) }
            }
            is ServiceFrame.Unpaired -> {
                Bridge.log("unpaired by the service: ${frame.reason}")
                return Exit.UNPAIRED
            }
            ServiceFrame.Superseded -> return Exit.SUPERSEDED
            is ServiceFrame.Unknown -> Bridge.log("device socket: ignored frame ${frame.type}: ${frame.problem ?: ""}")
        }
        return null
    }

    /** Sends a frame on the device socket; false when it isn't connected. */
    fun send(frame: DeviceFrame): Boolean = socket?.send(Frames.encode(frame)) ?: false

    private suspend fun refreshDevice(credential: String) {
        try {
            val d = bridge.api.device(credential)
            config.environment = d.environment
            bridge.update {
                it.copy(
                    device = d,
                    deviceId = d.deviceId,
                    environment = d.environment,
                    session = d.inUse?.let { u -> SessionUi(u.siliconId, u.sessionId, u.since) },
                    takeover = d.takeover?.let { t -> TakeoverUi(t.sessionId, t.reason, t.expiresAt) },
                )
            }
        } catch (e: ApiException) {
            Bridge.log("GET /api/v1/device failed: ${e.status} ${e.message}")
            if (e.status == 401) {
                pendingUnpair = true
                socket?.close()
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            Bridge.log("GET /api/v1/device failed", e)
        }
    }

    // ───────────── Setup and capabilities ─────────────

    /** Recomputes setup and capabilities; tells the service when they changed. */
    fun recomputeSetup() {
        bridge.scope.launch {
            sendLock.withLock {
                val report = runCatching { SetupReport.compute(bridge.context, config) }.getOrNull() ?: return@withLock
                bridge.update { it.copy(report = report, isTv = bridge.isTv) }
                if (socket == null) return@withLock
                val caps = report.capabilities to report.missing
                if (caps != sentCaps) {
                    sendHello(force = true, report)
                } else if (report.setup != sentSetup) {
                    if (send(DeviceFrame.SetupProgress(report.setup))) sentSetup = report.setup
                }
            }
        }
    }

    private fun sendHello(force: Boolean, given: SetupReport? = null) {
        val report = given ?: SetupReport.compute(bridge.context, config)
        if (!force && (report.capabilities to report.missing) == sentCaps) return
        val hello = DeviceFrame.Hello(
            appVersion = DeviceInfo.APP_VERSION,
            os = bridge.os,
            osVersion = DeviceInfo.osVersion,
            model = DeviceInfo.model,
            agentDeviceVersion = null,
            capabilities = report.capabilities,
            missing = report.missing,
            setup = report.setup,
        )
        if (send(hello)) {
            sentCaps = report.capabilities to report.missing
            sentSetup = report.setup
        }
        bridge.update { it.copy(report = report) }
    }

    // ───────────── What the Carbon does ─────────────

    /** Stop: ends the Silicon's session. */
    fun stopSession() {
        val session = bridge.state.value.session ?: return
        bridge.update { it.copy(session = session.copy(stopping = true)) }
        bridge.scope.launch {
            if (!send(DeviceFrame.Stop)) {
                val cred = bridge.secrets.readCredential()
                if (cred != null) runCatching { bridge.api.stop(cred) }.onFailure { Bridge.log("POST /device/stop failed", it) }
            }
            delay(10_000)
            bridge.update { s -> if (s.session?.sessionId == session.sessionId && s.session.stopping) s.copy(session = null) else s }
        }
    }

    /** Done on a takeover. */
    fun takeoverDone() {
        send(DeviceFrame.TakeoverDone)
        bridge.update { it.copy(takeover = null) }
    }

    /** Revoke pair. Throws with a message the UI can show when the service refused. */
    suspend fun revokePair() {
        val cred = bridge.secrets.readCredential()
        if (cred != null) {
            try {
                bridge.api.revoke(cred)
            } catch (e: ApiException) {
                if (e.status != 401 && e.status != 404) throw e
            }
        }
        // Hand over to the running loop rather than restarting it, so exactly one new
        // enrollment starts.
        val waitingForCarbon = bridge.state.value.link.let { it == Link.SUPERSEDED || it == Link.UPGRADE_REQUIRED }
        pendingUnpair = true
        forgetPairLocally()
        socket?.close()
        netWake.trySend(Unit)
        if (waitingForCarbon) manualWake.trySend(Unit)
        if (mainJob?.isActive != true) start()
    }

    private fun forgetPairLocally() {
        bridge.secrets.clearCredential()
        config.clearPair()
        sentCaps = null
        sentSetup = null
        bridge.executor.cancelAll()
        bridge.update {
            it.copy(
                phase = Phase.UNPAIRED, device = null, deviceId = null, session = null, takeover = null,
                environment = null, pairing = PairingUi(), link = Link.CONNECTING, linkDetail = null,
            )
        }
    }

    private fun millisUntil(timestamp: String): Long? =
        runCatching { Instant.parse(timestamp).toEpochMilli() - System.currentTimeMillis() }.getOrNull()

    private fun describe(e: Exception): String = when (e) {
        is ApiException -> "HTTP ${e.status}${e.code?.let { " $it" } ?: ""}: ${e.message}"
        else -> e.message ?: e.javaClass.simpleName
    }
}

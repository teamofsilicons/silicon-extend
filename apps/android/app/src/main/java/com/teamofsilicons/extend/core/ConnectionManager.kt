package com.teamofsilicons.extend.core

import android.net.ConnectivityManager
import android.net.Network
import com.teamofsilicons.extend.Extend
import com.teamofsilicons.extend.config.DeviceInfo
import com.teamofsilicons.extend.net.ApiException
import com.teamofsilicons.extend.net.Backoff
import com.teamofsilicons.extend.net.Connection
import com.teamofsilicons.extend.net.Socket
import com.teamofsilicons.extend.net.SocketEvent
import com.teamofsilicons.extend.protocol.DeviceFrame
import com.teamofsilicons.extend.protocol.EnrollmentCreate
import com.teamofsilicons.extend.protocol.EnrollmentCreated
import com.teamofsilicons.extend.protocol.EnrollmentState
import com.teamofsilicons.extend.protocol.Frames
import com.teamofsilicons.extend.protocol.MissingCapability
import com.teamofsilicons.extend.protocol.ServiceFrame
import com.teamofsilicons.extend.protocol.Setup
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.coroutines.coroutineContext

/**
 * Owns the app's connection to Extend: the enrollment (pairing code) while unpaired, then the
 * device socket, kept open with exponential backoff. Runs inside the foreground service.
 */
class ConnectionManager(private val extend: Extend) {
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

    private val config get() = extend.config

    fun start() {
        if (mainJob?.isActive == true) return
        mainJob = extend.scope.launch { runLoop() }
        if (watcherJob?.isActive != true) {
            watcherJob = extend.scope.launch {
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
            extend.context.getSystemService(ConnectivityManager::class.java).registerDefaultNetworkCallback(
                object : ConnectivityManager.NetworkCallback() {
                    override fun onAvailable(network: Network) {
                        netWake.trySend(Unit)
                        extend.wakeAdbReconnect()
                    }
                },
            )
        }
    }

    private suspend fun runLoop() {
        while (coroutineContext.isActive) {
            val credential = extend.secrets.readCredential()
            if (credential == null) {
                extend.update {
                    it.copy(phase = Phase.UNPAIRED, device = null, session = null, takeover = null, deviceId = null, environment = null)
                }
                enroll()
                continue
            }
            extend.update { it.copy(phase = Phase.PAIRED, deviceId = config.deviceId, environment = config.environment, pairing = PairingUi()) }
            when (runDevice(credential)) {
                Exit.UNPAIRED -> forgetPairLocally()
                Exit.SUPERSEDED -> {
                    extend.update { it.copy(link = Link.SUPERSEDED, linkDetail = "Another connection for this device took over. Tap Reconnect to use this one.") }
                    manualWake.receive()
                }
                Exit.UPGRADE_REQUIRED -> {
                    extend.update { it.copy(link = Link.UPGRADE_REQUIRED, linkDetail = "This version of ${DeviceInfo.appName(extend.isTv)} is too old. Install the latest from extend.teamofsilicons.com.") }
                    manualWake.receive()
                }
                Exit.RESTART -> {}
            }
        }
    }

    // ───────────── Enrollment ─────────────

    private val enrollmentPort = object : EnrollmentPort {
        override suspend fun create(): EnrollmentCreated = extend.api.createEnrollment(
            EnrollmentCreate(
                os = extend.os,
                osVersion = DeviceInfo.osVersion,
                model = DeviceInfo.model,
                appVersion = DeviceInfo.APP_VERSION,
            ),
        )

        override suspend fun poll(e: EnrollmentCreated): EnrollmentState? = extend.api.getEnrollment(e.enrollmentId, e.enrollmentSecret)

        override fun connect(e: EnrollmentCreated): Connection = Socket.open(
            extend.api.client,
            config.webSocketUrl("/api/v1/enrollments/${e.enrollmentId}/connect"),
            "Extend-Enrollment ${e.enrollmentSecret}",
        )

        override fun discard(e: EnrollmentCreated) {
            extend.scope.launch { runCatching { extend.api.discardEnrollment(e.enrollmentId, e.enrollmentSecret) } }
        }
    }

    private suspend fun enroll() {
        val paired = EnrollmentLoop(
            port = enrollmentPort,
            netWake = netWake,
            ui = { f -> extend.update { it.copy(pairing = f(it.pairing)) } },
            serviceUrl = { config.serviceUrl },
        ).run()
        extend.secrets.writeCredential(paired.deviceCredential)
        config.deviceId = paired.deviceId
        config.environment = paired.environment
        sentCaps = null
        sentSetup = null
        extend.update {
            it.copy(phase = Phase.PAIRED, deviceId = paired.deviceId, environment = paired.environment, pairing = PairingUi())
        }
        Extend.log("paired as ${paired.deviceId}")
    }

    // ───────────── The device socket ─────────────

    private suspend fun runDevice(credential: String): Exit {
        val backoff = Backoff()
        pendingUnpair = false
        while (coroutineContext.isActive) {
            if (pendingUnpair || extend.secrets.readCredential() == null) return Exit.UNPAIRED
            extend.update { it.copy(link = Link.CONNECTING, linkDetail = null) }
            val sock = Socket.open(extend.api.client, config.webSocketUrl("/api/v1/device/connect"), "Extend-Device $credential")
            socket = sock
            var exit: Exit? = null
            var limited: ApiException? = null
            try {
                loop@ for (ev in sock.events) {
                    when (ev) {
                        SocketEvent.Open -> {
                            backoff.reset()
                            extend.update { it.copy(link = Link.CONNECTED, linkDetail = null) }
                            sentCaps = null
                            sentSetup = null
                            sendHello(force = true)
                            extend.scope.launch { refreshDevice(credential) }
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
                            Extend.log("device socket closed ${ev.code} ${ev.reason}")
                            break@loop
                        }
                        is SocketEvent.Failed -> {
                            exit = when (ev.httpStatus) {
                                401 -> Exit.UNPAIRED
                                426 -> Exit.UPGRADE_REQUIRED
                                else -> null
                            }
                            if (ev.httpStatus == 429 || ev.refusal?.rateLimited == true) limited = ev.refusal ?: ApiException(429, "rate_limited", "HTTP 429")
                            Extend.log("device socket failed (${ev.httpStatus})", ev.error)
                            break@loop
                        }
                    }
                }
            } finally {
                socket = null
                sock.close()
                // Running commands stop; sessions keep their recordings and logs through a brief
                // outage, as Extend keeps the sessions themselves (CommandExecutor.connectionLost).
                extend.executor.connectionLost()
            }
            if (pendingUnpair) return Exit.UNPAIRED
            if (exit != null) return exit
            if (limited != null) {
                // Extend answered: it is limiting connections, so the network coming back can't help.
                val wait = limited.retryAfterS?.let { (it * 1000).coerceIn(1_000, EnrollmentLoop.MAX_RETRY_AFTER_MS) }
                    ?: maxOf(backoff.next(), EnrollmentLoop.RATE_LIMIT_FALLBACK_MS)
                extend.update {
                    it.copy(link = Link.OFFLINE, linkDetail = "Extend is limiting connections from this network; reconnecting ${PairingMessages.whenNext(wait, System.currentTimeMillis(), java.time.ZoneId.systemDefault())}")
                }
                delay(wait)
                continue
            }
            val wait = backoff.next()
            extend.update { it.copy(link = Link.OFFLINE, linkDetail = "Reconnecting in ${(wait + 999) / 1000} s") }
            withTimeoutOrNull(wait) { netWake.receive() }
        }
        return Exit.RESTART
    }

    private fun handle(frame: ServiceFrame, sock: Socket): Exit? {
        when (frame) {
            is ServiceFrame.Ping -> sock.send(Frames.encode(DeviceFrame.Pong(frame.nonce)))
            is ServiceFrame.Command -> {
                extend.update { it.copy(lastCommand = frame.command) }
                extend.executor.submit(frame) { result -> send(result) }
            }
            is ServiceFrame.Cancel -> extend.executor.cancel(frame.id)
            is ServiceFrame.SessionStarted -> {
                extend.executor.beginSession(frame.sessionId)
                extend.update { it.copy(session = SessionUi(frame.siliconId, frame.sessionId, frame.since)) }
            }
            is ServiceFrame.SessionEnded -> {
                extend.executor.endSession(frame.sessionId)
                extend.update { s ->
                    if (s.session == null || s.session.sessionId == frame.sessionId) s.copy(session = null, takeover = null) else s
                }
            }
            is ServiceFrame.Takeover -> extend.update { it.copy(takeover = TakeoverUi(frame.sessionId, frame.reason, frame.expiresAt)) }
            is ServiceFrame.TakeoverEnded -> extend.update { it.copy(takeover = null) }
            ServiceFrame.Refresh -> extend.secrets.readCredential()?.let { c -> extend.scope.launch { refreshDevice(c) } }
            is ServiceFrame.Environment -> {
                config.environment = frame.environment
                extend.update { it.copy(environment = frame.environment) }
            }
            is ServiceFrame.Unpaired -> {
                Extend.log("unpaired by the service: ${frame.reason}")
                return Exit.UNPAIRED
            }
            ServiceFrame.Superseded -> return Exit.SUPERSEDED
            is ServiceFrame.Unknown -> Extend.log("device socket: ignored frame ${frame.type}: ${frame.problem ?: ""}")
        }
        return null
    }

    /** Sends a frame on the device socket; false when it isn't connected. */
    fun send(frame: DeviceFrame): Boolean = socket?.send(Frames.encode(frame)) ?: false

    private suspend fun refreshDevice(credential: String) {
        try {
            val d = extend.api.device(credential)
            config.environment = d.environment
            // Sessions that ended while the socket was down are no longer in use.
            extend.executor.reconcile(d.inUse?.sessionId)
            extend.update {
                it.copy(
                    device = d,
                    deviceId = d.deviceId,
                    environment = d.environment,
                    session = d.inUse?.let { u -> SessionUi(u.siliconId, u.sessionId, u.since) },
                    takeover = d.takeover?.let { t -> TakeoverUi(t.sessionId, t.reason, t.expiresAt) },
                )
            }
        } catch (e: ApiException) {
            Extend.log("GET /api/v1/device failed: ${e.status} ${e.message}")
            if (e.status == 401) {
                pendingUnpair = true
                socket?.close()
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            Extend.log("GET /api/v1/device failed", e)
        }
    }

    // ───────────── Setup and capabilities ─────────────

    /** Recomputes setup and capabilities; tells the service when they changed. */
    fun recomputeSetup() {
        extend.scope.launch {
            sendLock.withLock {
                val report = runCatching { SetupReport.compute(extend.context, config) }.getOrNull() ?: return@withLock
                extend.update { it.copy(report = report, isTv = extend.isTv) }
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
        val report = given ?: SetupReport.compute(extend.context, config)
        if (!force && (report.capabilities to report.missing) == sentCaps) return
        val hello = DeviceFrame.Hello(
            appVersion = DeviceInfo.APP_VERSION,
            os = extend.os,
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
        extend.update { it.copy(report = report) }
    }

    // ───────────── What the Carbon does ─────────────

    /** Stop: ends the Silicon's session. */
    fun stopSession() {
        val session = extend.state.value.session ?: return
        extend.executor.endSession(session.sessionId)
        extend.update { it.copy(session = session.copy(stopping = true)) }
        extend.scope.launch {
            if (!send(DeviceFrame.Stop)) {
                val cred = extend.secrets.readCredential()
                if (cred != null) runCatching { extend.api.stop(cred) }.onFailure { Extend.log("POST /device/stop failed", it) }
            }
            delay(10_000)
            extend.update { s -> if (s.session?.sessionId == session.sessionId && s.session.stopping) s.copy(session = null) else s }
        }
    }

    /** Done on a takeover. */
    fun takeoverDone() {
        send(DeviceFrame.TakeoverDone)
        extend.update { it.copy(takeover = null) }
    }

    /** Revoke pair. Throws with a message the UI can show when the service refused. */
    suspend fun revokePair() {
        val cred = extend.secrets.readCredential()
        if (cred != null) {
            try {
                extend.api.revoke(cred)
            } catch (e: ApiException) {
                if (e.status != 401 && e.status != 404) throw e
            }
        }
        // Hand over to the running loop rather than restarting it, so exactly one new
        // enrollment starts.
        val waitingForCarbon = extend.state.value.link.let { it == Link.SUPERSEDED || it == Link.UPGRADE_REQUIRED }
        pendingUnpair = true
        forgetPairLocally()
        socket?.close()
        netWake.trySend(Unit)
        if (waitingForCarbon) manualWake.trySend(Unit)
        if (mainJob?.isActive != true) start()
    }

    private fun forgetPairLocally() {
        extend.secrets.clearCredential()
        config.clearPair()
        sentCaps = null
        sentSetup = null
        extend.executor.forgetAll()
        extend.update {
            it.copy(
                phase = Phase.UNPAIRED, device = null, deviceId = null, session = null, takeover = null,
                environment = null, pairing = PairingUi(), link = Link.CONNECTING, linkDetail = null,
            )
        }
    }

}

package com.teamofsilicons.extend.core

import android.net.ConnectivityManager
import android.net.Network
import com.teamofsilicons.extend.Extend
import com.teamofsilicons.extend.config.DeviceInfo
import com.teamofsilicons.extend.net.ApiException
import com.teamofsilicons.extend.net.Connection
import com.teamofsilicons.extend.net.Socket
import com.teamofsilicons.extend.protocol.DeviceFrame
import com.teamofsilicons.extend.protocol.EnrollmentCreate
import com.teamofsilicons.extend.protocol.EnrollmentCreated
import com.teamofsilicons.extend.protocol.EnrollmentState
import com.teamofsilicons.extend.protocol.Frames
import com.teamofsilicons.extend.protocol.ServiceFrame
import com.teamofsilicons.extend.security.StoredPair
import com.teamofsilicons.extend.service.WakeNotifications
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.distinctUntilChangedBy
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.coroutineContext

/**
 * Owns the app's connections to Extend. Runs inside the foreground service.
 *
 * - Unpaired: the first enrollment (pairing code) of this device.
 * - Paired: one [PairLink] per Carbon who paired the device, each a connection with that pair's
 *   own credential ([PairSupervisor]). Ending one pair (Revoke pair here, Remove on the website,
 *   or its lifetime) never touches the others; after the last one the app shows a pairing code.
 * - "Pair with another Carbon": a new code for this same device, asked for with a live pair's
 *   credential, so another Carbon can pair it to their own account.
 *
 * It is still one device: one Silicon at a time whichever pair its session runs through, one
 * awake state for every connection, and one wake notification for every request.
 */
class ConnectionManager(private val extend: Extend) : LinkHost {
    private var mainJob: Job? = null
    private var watcherJob: Job? = null
    private var addJob: Job? = null
    private var expiryJob: Job? = null
    private val pairsChanged = Channel<Unit>(Channel.CONFLATED)
    /** Network came back, for whichever enrollment is waiting (never two at once). */
    private val enrollWake = Channel<Unit>(Channel.CONFLATED)
    private val sendLock = Mutex()
    private val wakeLock = Mutex()
    private val indicatorLock = Mutex()
    private var announceJob: Job? = null

    /** The 10 s during which the badge or notification names a Silicon that just started. */
    private val announcer = InUseAnnouncer(
        extend.scope,
        update = { f -> extend.update(f) },
        announced = { extend.config.announcedSession },
        remember = { extend.config.announcedSession = it },
    )
    private val supervisor = PairSupervisor(extend.scope, ::newLink, ::pairEnded)

    /** The app process's `awake` run, and the sequence number shared by every connection. */
    private val awakeRun = UUID.randomUUID().toString()
    private val awakeSeq = AtomicLong(0)

    /** Wake requests already answered with `wake_request_shown`. */
    private val shownWakes = ConcurrentHashMap.newKeySet<String>()
    /** A wake request marked `alert` arrived and the notification hasn't sounded for it yet. */
    @Volatile private var alertPending = false

    private val config get() = extend.config

    /** Screen and lock broadcasts; the foreground service starts and stops it. */
    val wakefulness = Wakefulness.Watcher(extend.context, { extend.isTv }, ::awakeChanged, ::screenOn)

    fun start() {
        if (announceJob?.isActive != true) {
            announceJob = extend.scope.launch {
                extend.state.map { it.session }.distinctUntilChangedBy { s -> s?.let { InUseIndicator.key(it) } }.collect { announcer.onSession(it) }
            }
        }
        if (mainJob?.isActive == true) return
        mainJob = extend.scope.launch { supervise() }
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

    /** Starts again from the stored pairs (after the service URL changed, or the pairs were forgotten). */
    fun reconnect() {
        val old = mainJob
        mainJob = null
        old?.cancel()
        supervisor.stopAll()
        cancelAddPair()
        start()
    }

    /** Reconnect one Carbon's pair (after "superseded" or "update required"). */
    fun reconnect(deviceId: String) {
        supervisor.link(deviceId)?.reconnect()
    }

    /** Forgets every pair on this device without telling the service (debug builds and the developer settings). */
    fun forgetPairsLocally() {
        extend.secrets.clearPairs()
        config.clearPairs()
    }

    private var networkRegistered = false
    private fun registerNetworkCallback() {
        if (networkRegistered) return
        networkRegistered = true
        runCatching {
            extend.context.getSystemService(ConnectivityManager::class.java).registerDefaultNetworkCallback(
                object : ConnectivityManager.NetworkCallback() {
                    override fun onAvailable(network: Network) {
                        enrollWake.trySend(Unit)
                        supervisor.links().forEach { it.networkBack() }
                        extend.wakeAdbReconnect()
                    }
                },
            )
        }
    }

    // ───────────── The pairs ─────────────

    private suspend fun supervise() {
        while (coroutineContext.isActive) {
            val pairs = extend.secrets.pairs { config.deviceId }
            if (pairs.isEmpty()) {
                unpairedLocally()
                val paired = enrollFirst()
                extend.secrets.addPair(StoredPair(paired.deviceId, paired.deviceCredential))
                config.pairIds = listOf(paired.deviceId)
                config.environment = paired.environment
                Extend.log("paired as ${paired.deviceId}")
                continue
            }
            val ids = pairs.map { it.deviceId }
            if (config.pairIds != ids) config.pairIds = ids
            extend.update { s ->
                val known = s.pairs.associateBy { it.deviceId }
                val rows = ids.map { known[it] ?: PairUi(it) }
                val (link, detail) = Links.overall(rows)
                s.copy(phase = Phase.PAIRED, pairs = rows, link = link, linkDetail = detail, environment = config.environment, pairing = PairingUi())
            }
            supervisor.sync(pairs)
            pairsChanged.receive()
        }
    }

    /** No pair is left: the pairing screen. */
    private suspend fun unpairedLocally() {
        supervisor.stopAll()
        cancelAddPair()
        extend.executor.forgetAll()
        config.clearPairs()
        shownWakes.clear()
        extend.update {
            it.copy(
                phase = Phase.UNPAIRED, pairs = emptyList(), session = null, takeover = null, environment = null,
                pairing = PairingUi(), link = Link.CONNECTING, linkDetail = null, wakeRequests = emptyList(), addingPair = null,
            )
        }
        wakeChanged(wait = false)
    }

    private fun newLink(pair: StoredPair): PairLink = PairLink(
        pair,
        port = { credential -> Socket.open(extend.api.client, config.webSocketUrl("/api/v1/device/connect"), "Extend-Device $credential") },
        host = this,
    )

    /** Extend ended this pair (`unpaired`, or its credential stopped working). The others carry on. */
    private fun pairEnded(link: PairLink) {
        Extend.log("pair ${link.deviceId} ended")
        extend.scope.launch {
            forgetLocally(link.deviceId)
            pairsChanged.trySend(Unit)
        }
    }

    private suspend fun forgetLocally(deviceId: String) {
        extend.secrets.removePair(deviceId)
        config.clearPair(deviceId)
        extend.executor.forgetPair(deviceId)
        shownWakes.removeAll(extend.state.value.wakeRequests.filter { it.pairId == deviceId }.map { it.wakeId }.toSet())
        extend.update { s ->
            val rows = s.pairs.filter { it.deviceId != deviceId }
            val (link, detail) = Links.overall(rows)
            val through = s.session?.pairId == deviceId
            s.copy(
                pairs = rows, link = link, linkDetail = detail,
                session = if (through) null else s.session,
                takeover = if (through) null else s.takeover,
                wakeRequests = WakeRequests.removePair(s.wakeRequests, deviceId),
            )
        }
        wakeChanged(wait = false)
    }

    /** The credential of the pair [deviceId], for its uploads. */
    fun credentialFor(deviceId: String): String? =
        supervisor.link(deviceId)?.credential ?: runCatching { extend.secrets.credentialFor(deviceId) }.getOrNull()

    // ───────────── Enrollment ─────────────

    private inner class EnrollmentPortBase(private val create: suspend () -> EnrollmentCreated) : EnrollmentPort {
        override suspend fun create(): EnrollmentCreated = create.invoke()

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

    private suspend fun enrollFirst() = EnrollmentLoop(
        port = EnrollmentPortBase {
            extend.api.createEnrollment(
                EnrollmentCreate(os = extend.os, osVersion = DeviceInfo.osVersion, model = DeviceInfo.model, appVersion = DeviceInfo.APP_VERSION),
            )
        },
        netWake = enrollWake,
        ui = { f -> extend.update { it.copy(pairing = f(it.pairing)) } },
        serviceUrl = { config.serviceUrl },
    ).run()

    // ───────────── Pair with another Carbon ─────────────

    /** Opens "Pair with another Carbon": the shared-device note comes first. */
    fun startAddPair() {
        extend.update { if (it.phase == Phase.PAIRED && it.addingPair == null) it.copy(addingPair = AddPairUi()) else it }
    }

    /** The Carbon read the note and continues: get a code. */
    fun showAddPairCode() {
        if (addJob?.isActive == true) return
        extend.update { it.copy(addingPair = AddPairUi(showingCode = true)) }
        addJob = extend.scope.launch { addPair() }
    }

    fun cancelAddPair() {
        addJob?.cancel()
        addJob = null
        extend.update { it.copy(addingPair = null) }
    }

    private suspend fun addPair() {
        val loop = EnrollmentLoop(
            port = EnrollmentPortBase {
                val credential = supervisor.links().firstOrNull { it.connected }?.credential
                    ?: extend.secrets.pairs().firstOrNull()?.credential
                    ?: throw ApiException(401, "unauthorized", "This device has no pair left to add another Carbon with.")
                extend.api.createPairEnrollment(credential)
            },
            netWake = enrollWake,
            ui = { f -> extend.update { s -> s.copy(addingPair = s.addingPair?.let { a -> a.copy(pairing = f(a.pairing)) }) } },
            serviceUrl = { config.serviceUrl },
            giveUp = { e -> AddPairMessages.final(e) },
        )
        try {
            val paired = loop.run()
            extend.secrets.addPair(StoredPair(paired.deviceId, paired.deviceCredential))
            config.pairIds = (config.pairIds + paired.deviceId).distinct()
            Extend.log("paired with another Carbon as ${paired.deviceId}")
            extend.update { s ->
                val rows = if (s.pair(paired.deviceId) != null) s.pairs else s.pairs + PairUi(paired.deviceId)
                s.copy(addingPair = null, pairs = rows)
            }
            pairsChanged.trySend(Unit)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            Extend.log("pair with another Carbon: refused", e)
            extend.update { s -> s.copy(addingPair = (s.addingPair ?: AddPairUi(showingCode = true)).copy(error = AddPairMessages.refused(e, extend.isTv))) }
        } finally {
            addJob = null
        }
    }

    // ───────────── What each pair's connection says ─────────────

    override fun opened(link: PairLink) {
        sendHello(link, SetupReport.compute(extend.context, config))
        link.send(awakeFrame(wakefulness.forConnect()))
        extend.scope.launch {
            // A change the Carbon made here while this device was offline goes first, so the
            // re-read below doesn't undo it.
            if (config.inUseIndicatorPending) pushIndicator(config.inUseIndicatorShown)
            refreshPair(link)
        }
    }

    override fun closed(link: PairLink) {
        extend.executor.connectionLost(link.deviceId)
    }

    override fun state(link: PairLink, state: Link, detail: String?) {
        val why = detail ?: if (state == Link.UPGRADE_REQUIRED) {
            "This version of ${DeviceInfo.appName(extend.isTv)} is too old. Install the latest from extend.teamofsilicons.com."
        } else null
        extend.update { s ->
            val rows = s.pairs.map { if (it.deviceId == link.deviceId) it.copy(link = state, linkDetail = why) else it }
            val (overall, overallDetail) = Links.overall(rows)
            s.copy(pairs = rows, link = overall, linkDetail = overallDetail)
        }
    }

    override suspend fun handle(link: PairLink, frame: ServiceFrame): PairLink.Exit? {
        when (frame) {
            is ServiceFrame.Ping -> link.send(DeviceFrame.Pong(frame.nonce))
            is ServiceFrame.Command -> {
                extend.update { it.copy(lastCommand = frame.command) }
                extend.executor.submit(frame, link.deviceId) { result -> link.send(result) }
            }
            is ServiceFrame.Cancel -> extend.executor.cancel(frame.id)
            is ServiceFrame.SessionStarted -> sessionStarted(link, frame)
            is ServiceFrame.SessionEnded -> {
                extend.executor.endSession(frame.sessionId)
                extend.update { s ->
                    if (s.session == null || s.session.sessionId == frame.sessionId) s.copy(session = null, takeover = null) else s
                }
                // Another side's requests may show in full again.
                wakeChanged(wait = false)
            }
            is ServiceFrame.Takeover -> extend.update { it.copy(takeover = TakeoverUi(frame.sessionId, frame.reason, frame.expiresAt)) }
            is ServiceFrame.TakeoverEnded -> extend.update { it.copy(takeover = null) }
            ServiceFrame.Refresh -> extend.scope.launch { refreshPair(link) }
            is ServiceFrame.Environment -> {
                config.environment = frame.environment
                extend.update { it.copy(environment = frame.environment) }
            }
            is ServiceFrame.Unpaired -> {
                Extend.log("pair ${link.deviceId} unpaired by the service: ${frame.reason}")
                return PairLink.Exit.UNPAIRED
            }
            ServiceFrame.Superseded -> return PairLink.Exit.SUPERSEDED
            is ServiceFrame.WakeRequest -> wakeRequest(link, frame)
            is ServiceFrame.WakeRequestEnded -> {
                extend.update { it.copy(wakeRequests = WakeRequests.remove(it.wakeRequests, frame.wakeId)) }
                wakeChanged(wait = false)
            }
            is ServiceFrame.SetupRetry -> {
                if (frame.target != null) Extend.log("setup_retry for ${frame.target}: this device carries no other device")
                else retrySetup(frame.step)
            }
            is ServiceFrame.Unknown -> Extend.log("pair ${link.deviceId}: ignored frame ${frame.type}: ${frame.problem ?: ""}")
        }
        return null
    }

    /**
     * A session started through [link]. Before this returns, and so before any command of the
     * session (which follows on the same connection) reaches the executor, every wake request of
     * another side is hidden in the app and in the notification.
     */
    private suspend fun sessionStarted(link: PairLink, frame: ServiceFrame.SessionStarted) {
        if (frame.target != null) {
            Extend.log("session_started for ${frame.target}: this device carries no other device")
            return
        }
        extend.executor.beginSession(frame.sessionId, link.deviceId)
        extend.update { s ->
            val carbon = s.pair(link.deviceId)?.owner?.id
            s.copy(session = SessionUi(frame.siliconId, frame.sessionId, frame.since, pairId = link.deviceId, carbon = carbon, side = frame.side))
        }
        wakeChanged(wait = true)
    }

    /** Re-reads [link]'s pair (`GET /api/v1/device`): its name and Carbon, and its session after a reconnect. */
    private suspend fun refreshPair(link: PairLink) {
        try {
            val d = extend.api.device(link.credential)
            if (d.deviceId != link.deviceId) {
                // A 1.0 credential migrated without its device id: store it under the right one.
                Extend.log("pair ${link.deviceId.ifEmpty { "(unnamed)" }} is ${d.deviceId}")
                extend.secrets.renamePair(link.deviceId, d.deviceId)
                config.pairIds = config.pairIds.map { if (it == link.deviceId) d.deviceId else it }.ifEmpty { listOf(d.deviceId) }
                pairsChanged.trySend(Unit)
                return
            }
            config.environment = d.environment
            // One setting for the whole device: whichever pair read it last is right. A 1.0
            // service leaves it out, and a change still on its way to Extend wins.
            val indicator = d.inUseIndicator?.takeIf { !config.inUseIndicatorPending }?.let { InUseIndicator.shows(it) }
            if (indicator != null && indicator != config.inUseIndicatorShown) {
                Extend.log("in-use indicator is ${InUseIndicator.value(indicator)} (from Extend)")
                config.inUseIndicatorShown = indicator
            }
            // Sessions of this pair that ended while its connection was down are no longer in use.
            extend.executor.reconcile(d.inUse?.sessionId, link.deviceId)
            extend.update { s ->
                val rows = s.pairs.map {
                    if (it.deviceId == d.deviceId) it.copy(name = d.name, owner = d.owner, firstPair = d.firstPair, instanceId = d.instanceId) else it
                }
                val instances = rows.mapNotNull { it.instanceId }.distinct()
                if (instances.size > 1) Extend.log("this app holds pairs of ${instances.size} devices (${instances.joinToString()}); each keeps working")
                val current = s.session
                val session = when {
                    d.inUse != null -> SessionUi(
                        d.inUse.siliconId, d.inUse.sessionId, d.inUse.since,
                        stopping = current != null && current.sessionId == d.inUse.sessionId && current.stopping,
                        pairId = d.deviceId, carbon = d.owner.id,
                        // GET /api/v1/device carries no side: keep the one session_started gave.
                        side = current?.takeIf { it.sessionId == d.inUse.sessionId }?.side,
                    )
                    current?.pairId == d.deviceId -> null
                    else -> current
                }
                val takeover = when {
                    d.takeover != null -> TakeoverUi(d.takeover.sessionId, d.takeover.reason, d.takeover.expiresAt)
                    current?.pairId == d.deviceId -> null
                    else -> s.takeover
                }
                s.copy(
                    pairs = rows, environment = d.environment, session = session, takeover = takeover,
                    indicatorShown = indicator ?: s.indicatorShown,
                )
            }
            wakeChanged(wait = false)
        } catch (e: ApiException) {
            Extend.log("GET /api/v1/device for pair ${link.deviceId} failed: ${e.status} ${e.message}")
            if (e.status == 401) link.unpair()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            Extend.log("GET /api/v1/device for pair ${link.deviceId} failed", e)
        }
    }

    // ───────────── Awake and wake requests ─────────────

    private fun awakeFrame(r: Wakefulness.Reading) =
        DeviceFrame.Awake(r.awake, r.sleepState, r.inputSeen, awakeRun, awakeSeq.incrementAndGet())

    /** The device woke, locked or slept: every connection says so, and an awake device has no wake requests left. */
    private fun awakeChanged(r: Wakefulness.Reading) {
        extend.update { s -> s.copy(awake = r.ui, wakeRequests = if (r.awake) emptyList() else s.wakeRequests) }
        for (link in supervisor.links()) link.send(awakeFrame(r))
        if (r.awake) extend.scope.launch { wakeChanged(wait = false) }
    }

    /** The screen came on: re-make any connection that went quiet while it was off. */
    private fun screenOn() {
        supervisor.links().forEach { it.kick() }
    }

    private suspend fun wakeRequest(link: PairLink, frame: ServiceFrame.WakeRequest) {
        if (frame.target != null) {
            Extend.log("wake_request for ${frame.target}: this device carries no other device")
            return
        }
        val w = WakeUi(frame.wakeId, link.deviceId, frame.siliconId, frame.reason, frame.side, frame.createdAt, frame.expiresAt)
        extend.update { it.copy(wakeRequests = WakeRequests.upsert(it.wakeRequests, w)) }
        if (frame.alert) alertPending = true
        wakeChanged(wait = false)
        if (shownWakes.add(frame.wakeId)) {
            val showing = WakeNotifications.showing(extend.context, extend.isTv, DeviceInfo.noun(extend.context, extend.isTv))
            link.send(DeviceFrame.WakeRequestShown(frame.wakeId, showing.shown, showing.note))
        }
        // Already awake (the request crossed the device waking): say so again, so Extend ends it.
        if (extend.state.value.awake?.awake == true) link.send(awakeFrame(wakefulness.forConnect()))
    }

    /**
     * Brings the wake notification and the expiry timer in line with the state. [wait]: return only
     * once Android shows the new notification (a session just started).
     */
    private suspend fun wakeChanged(wait: Boolean): Unit = wakeLock.withLock {
        val now = System.currentTimeMillis()
        extend.update { s -> WakeRequests.open(s.wakeRequests, now).let { if (it.size == s.wakeRequests.size) s else s.copy(wakeRequests = it) } }
        val s = extend.state.value
        if (!s.isTv) {
            val alert = alertPending
            alertPending = false
            val lines = WakeRequests.lines(s.wakeRequests, s.session)
            if (lines.isNotEmpty() || wait) Extend.log("wake notification: ${lines.size} request(s), ${lines.count { it.silicon == null }} hidden${if (s.session != null) " (a session runs)" else ""}")
            WakeNotifications.render(extend.context, lines, DeviceInfo.noun(extend.context, false), alert, wait)
        }
        expiryJob?.cancel()
        val next = WakeRequests.nextExpiry(s.wakeRequests)
        if (next != null) {
            expiryJob = extend.scope.launch {
                delay((next - System.currentTimeMillis()).coerceAtLeast(0) + 500)
                wakeChanged(wait = false)
            }
        }
    }

    // ───────────── Setup and capabilities ─────────────

    /** Recomputes setup and capabilities; tells each connection's service side when they changed. */
    fun recomputeSetup() {
        extend.scope.launch {
            sendLock.withLock {
                val report = runCatching { SetupReport.compute(extend.context, config) }.getOrNull() ?: return@withLock
                extend.update { it.copy(report = report, isTv = extend.isTv) }
                for (link in supervisor.links()) {
                    if (!link.connected) continue
                    if ((report.capabilities to report.missing) != link.sentCaps) {
                        sendHello(link, report)
                    } else if (report.setup != link.sentSetup) {
                        if (link.send(DeviceFrame.SetupProgress(report.setup))) link.sentSetup = report.setup
                    }
                }
            }
        }
    }

    private fun sendHello(link: PairLink, report: SetupReport) {
        val hello = DeviceFrame.Hello(
            appVersion = DeviceInfo.APP_VERSION,
            os = extend.os,
            osVersion = DeviceInfo.osVersion,
            model = DeviceInfo.model,
            engineVersion = null,
            capabilities = report.capabilities,
            missing = report.missing,
            setup = report.setup,
            features = Frames.FEATURES,
        )
        if (link.send(hello)) {
            link.sentCaps = report.capabilities to report.missing
            link.sentSetup = report.setup
        }
        extend.update { it.copy(report = report) }
    }

    /**
     * Runs failed setup steps again (the step's Retry button, or `setup_retry` from Extend): [step],
     * or every failed step when null. The steps show as in progress, and the next reports say how
     * the attempt went.
     */
    fun retrySetup(step: String?) {
        extend.scope.launch {
            val report = runCatching { SetupReport.compute(extend.context, config) }.getOrNull()
            val keys = SetupRetry.select(report?.failed?.map { it.key }.orEmpty(), step)
            if (keys.isEmpty()) {
                Extend.log("setup retry${step?.let { " of $it" } ?: ""}: nothing has failed")
                recomputeSetup()
                return@launch
            }
            Extend.log("setup retry: ${keys.joinToString()}")
            SetupRetry.start(keys)
            recomputeSetup()
            if (keys.any { it in SetupRetry.DEBUGGING_KEYS }) extend.retryAdbNow()
            // Nothing runs for accessibility: Android starts the service by itself, or after a
            // restart. The fresh look below reports what it found.
            val others = keys.filter { it !in SetupRetry.DEBUGGING_KEYS }
            if (others.isNotEmpty()) {
                delay(1_000)
                SetupRetry.finished(others)
                recomputeSetup()
            }
        }
    }

    // ───────────── What the Carbon does ─────────────

    /**
     * The Carbon turned the in-use badge or notification on or off here. It takes effect on this
     * device at once, then goes to Extend for every pair of it (`PATCH /api/v1/device`); while
     * this device is offline it waits for the next connection.
     */
    fun setInUseIndicator(shown: Boolean) {
        config.inUseIndicatorShown = shown
        config.inUseIndicatorPending = true
        Extend.log("in-use indicator set to ${InUseIndicator.value(shown)} on this device")
        extend.update { it.copy(indicatorShown = shown, indicatorNote = null) }
        extend.scope.launch { pushIndicator(shown) }
    }

    /** Sends the Carbon's choice to Extend with the first pair credential that works; says what happened when it didn't. */
    private suspend fun pushIndicator(shown: Boolean) = indicatorLock.withLock {
        if (config.inUseIndicatorShown != shown || !config.inUseIndicatorPending) return@withLock
        val noun = DeviceInfo.noun(extend.context, extend.isTv)
        val credentials = (supervisor.links().sortedByDescending { it.connected }.map { it.credential } +
            runCatching { extend.secrets.pairs().map { it.credential } }.getOrDefault(emptyList())).distinct()
        var note: String? = "Saved on this $noun. Extend hears about it when this $noun is back online."
        for (credential in credentials) {
            try {
                val now = extend.api.setInUseIndicator(credential, InUseIndicator.value(shown))
                // A newer toggle may have been queued while this request was in flight.
                if (config.inUseIndicatorShown != shown) return@withLock
                config.inUseIndicatorPending = false
                note = null
                // Extend may know better (another Carbon changed it at the same moment).
                val applied = now?.let { InUseIndicator.shows(it) } ?: shown
                if (applied != shown) {
                    config.inUseIndicatorShown = applied
                    extend.update { it.copy(indicatorShown = applied) }
                }
                break
            } catch (e: CancellationException) {
                throw e
            } catch (e: ApiException) {
                Extend.log("PATCH /api/v1/device (in_use_indicator) refused: ${e.status} ${e.code} ${e.message}")
                if (config.inUseIndicatorShown != shown) return@withLock
                when {
                    e.status == 401 -> continue
                    e.status == 404 || e.status == 405 || e.status == 501 -> {
                        // A 1.0 service: this device keeps the choice for itself.
                        config.inUseIndicatorPending = false
                        note = "Saved on this $noun only: the Extend service it uses is too old to show this setting on the website."
                    }
                    e.status == 429 || e.status >= 500 -> Unit
                    else -> {
                        config.inUseIndicatorPending = false
                        config.inUseIndicatorShown = !shown
                        extend.update { it.copy(indicatorShown = !shown) }
                        note = (e.message?.trim()?.trimEnd('.')?.takeIf { it.isNotEmpty() && !it.startsWith("HTTP ") } ?: "Extend didn't accept the change (HTTP ${e.status})") + "."
                    }
                }
                break
            } catch (e: Exception) {
                Extend.log("PATCH /api/v1/device (in_use_indicator) failed", e)
                break
            }
        }
        extend.update { it.copy(indicatorNote = note) }
    }

    /** Stop: ends the Silicon's session, whichever Carbon's pair it came through. */
    fun stopSession() {
        val session = extend.state.value.session ?: return
        extend.executor.endSession(session.sessionId)
        extend.update { it.copy(session = session.copy(stopping = true)) }
        extend.scope.launch {
            // On the session's own pair first; Extend stops the device's session from any of them.
            val holder = supervisor.link(session.pairId)
            val sent = holder?.send(DeviceFrame.Stop) == true || supervisor.links().any { it.send(DeviceFrame.Stop) }
            if (!sent) {
                val credential = holder?.credential ?: supervisor.links().firstOrNull()?.credential ?: extend.secrets.pairs().firstOrNull()?.credential
                if (credential != null) runCatching { extend.api.stop(credential) }.onFailure { Extend.log("POST /device/stop failed", it) }
            }
            delay(10_000)
            extend.update { s -> if (s.session?.sessionId == session.sessionId && s.session.stopping) s.copy(session = null) else s }
        }
    }

    /** Done on a takeover. */
    fun takeoverDone() {
        val holder = supervisor.link(extend.state.value.session?.pairId)
        if (holder?.send(DeviceFrame.TakeoverDone) != true) supervisor.links().any { it.send(DeviceFrame.TakeoverDone) }
        extend.update { it.copy(takeover = null) }
    }

    /**
     * Revoke pair, for one Carbon: removes this device from that Carbon's account and ends access
     * for their Silicons. The other Carbons' pairs carry on. Throws with a message the UI can show
     * when the service refused.
     */
    suspend fun revokePair(deviceId: String) {
        val credential = credentialFor(deviceId)
        if (credential != null) {
            try {
                extend.api.revoke(credential)
            } catch (e: ApiException) {
                if (e.status != 401 && e.status != 404) throw e
            }
        }
        supervisor.stop(deviceId)
        forgetLocally(deviceId)
        pairsChanged.trySend(Unit)
        if (mainJob?.isActive != true) start()
    }
}

/** What "Pair with another Carbon" says when Extend refuses it. */
object AddPairMessages {
    /** Refusals that asking again can't change. */
    fun final(e: Exception): Boolean = e is ApiException && e.status in setOf(401, 403, 404, 409, 426)

    fun refused(e: Exception, tv: Boolean): String {
        val noun = if (tv) "TV" else "device"
        if (e !is ApiException) return "Couldn't get a code for another Carbon: ${e.message ?: "no reason given"}. Check this $noun's connection and try again."
        val message = e.message?.trim()?.trimEnd('.')?.takeIf { it.isNotEmpty() && !it.startsWith("HTTP ") }
        return when (e.status) {
            404 -> "The Extend service this $noun uses is too old for this. Pairing with another Carbon needs Silicon Extend 1.1 on the service."
            409 -> (message ?: "This $noun is paired to as many Carbons as Extend allows") + "."
            426 -> "This version of the Extend app is too old for this. Install the latest from extend.teamofsilicons.com."
            401 -> "This $noun's pair no longer works, so it can't add another Carbon."
            else -> (message ?: "Extend refused a code for another Carbon (HTTP ${e.status})") + "."
        }
    }
}

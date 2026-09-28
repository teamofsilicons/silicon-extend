package com.teamofsilicons.extend.core

import com.teamofsilicons.extend.Extend
import com.teamofsilicons.extend.net.ApiException
import com.teamofsilicons.extend.net.Backoff
import com.teamofsilicons.extend.net.Connection
import com.teamofsilicons.extend.net.SocketEvent
import com.teamofsilicons.extend.protocol.DeviceFrame
import com.teamofsilicons.extend.protocol.EnrollmentCreated
import com.teamofsilicons.extend.protocol.EnrollmentFrame
import com.teamofsilicons.extend.protocol.EnrollmentState
import com.teamofsilicons.extend.protocol.Frames
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.channels.ReceiveChannel
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withTimeoutOrNull
import java.io.IOException
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/** What the enrollment loop needs from the service; [ConnectionManager] wires the real calls, tests fake them. */
interface EnrollmentPort {
    /** `POST /api/v1/enrollments`, or `POST /api/v1/device/enrollments` for "Pair with another Carbon". */
    suspend fun create(): EnrollmentCreated

    /** `GET /api/v1/enrollments/{id}`; throws [ApiException] when the service refuses. */
    suspend fun poll(e: EnrollmentCreated): EnrollmentState?

    /** Opens `/api/v1/enrollments/{id}/connect`. */
    fun connect(e: EnrollmentCreated): Connection

    /** `DELETE /api/v1/enrollments/{id}`, without waiting (the enrollment was abandoned). */
    fun discard(e: EnrollmentCreated)
}

/**
 * The unpaired app: gets a pairing code, follows its rotations on the enrollment socket, and
 * returns the credential once a Carbon claims the code (docs/device-protocol.md section 1).
 *
 * Pacing, so a broken or refusing service is never hammered:
 * - One [pace] back-off spans every enrollment of this run. It is reset only when an enrollment
 *   socket actually opens, so an enrollment that is refused at once (401/404) is followed by a new
 *   one only after `pace.next()`, growing to 60 s, never immediately.
 * - `429 rate_limited` waits the service's `retry_after_s` (or at least [RATE_LIMIT_FALLBACK_MS]
 *   without one), and a network coming back does not cut that wait short.
 * - Only a failure to reach the service wakes early when the network returns ([netWake]).
 * - A refusal [giveUp] names (an old service, the pair limit) ends the loop: [run] throws it, since
 *   asking again can't help.
 */
class EnrollmentLoop(
    private val port: EnrollmentPort,
    private val netWake: ReceiveChannel<Unit>,
    private val ui: ((PairingUi) -> PairingUi) -> Unit,
    private val serviceUrl: () -> String,
    private val pace: Backoff = Backoff(),
    private val clock: () -> Long = System::currentTimeMillis,
    private val zone: ZoneId = ZoneId.systemDefault(),
    private val log: (String, Throwable?) -> Unit = { m, e -> Extend.log(m, e) },
    private val giveUp: (Exception) -> Boolean = { false },
) {
    /** How following one enrollment ended. */
    private sealed interface Followed {
        data class Paired(val paired: EnrollmentFrame.Paired) : Followed
        /** Its code no longer works. [opened]: its socket had opened at least once. */
        data class Gone(val opened: Boolean, val status: Int?, val refusal: ApiException?) : Followed
    }

    /** What `GET /api/v1/enrollments/{id}` said. */
    private sealed interface Polled {
        data class Paired(val paired: EnrollmentFrame.Paired) : Polled
        data class Waiting(val code: String, val expiresAt: String) : Polled
        data class Gone(val status: Int, val refusal: ApiException) : Polled
        data object Unknown : Polled
    }

    suspend fun run(): EnrollmentFrame.Paired {
        while (true) {
            currentCoroutineContext().ensureActive()
            val created = try {
                port.create()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                if (giveUp(e)) throw e
                val wait = waitAfterCreateFailure(e)
                ui { it.copy(code = null, expiresAt = null, live = false, error = PairingMessages.createFailed(e, wait, serviceUrl(), clock(), zone)) }
                log("enrollment create failed", e)
                sleep(wait, wakeOnNetwork = e !is ApiException)
                continue
            }
            ui { PairingUi(created.pairingCode.uppercase(), created.codeExpiresAt, live = false, error = null) }
            val outcome = try {
                follow(created)
            } catch (e: CancellationException) {
                // Abandoned (restart, new service URL): tell the service so its code stops working.
                port.discard(created)
                throw e
            }
            when (outcome) {
                is Followed.Paired -> return outcome.paired
                is Followed.Gone -> {
                    // Its code no longer works. A socket that never opened proves nothing about the
                    // service, so the next enrollment waits its turn in the back-off.
                    val limited = outcome.refusal?.takeIf { it.rateLimited }
                    val wait = limited?.let { rateLimitWait(it) } ?: pace.next()
                    val error = if (outcome.opened && limited == null) null
                    else PairingMessages.enrollmentRefused(outcome.status, outcome.refusal, wait, clock(), zone)
                    ui { PairingUi(code = null, expiresAt = null, live = false, error = error) }
                    log("enrollment ${created.enrollmentId} is gone (HTTP ${outcome.status ?: "-"}, socket ${if (outcome.opened) "had opened" else "never opened"}); next in ${wait} ms", null)
                    sleep(wait, wakeOnNetwork = false)
                }
            }
        }
    }

    private fun waitAfterCreateFailure(e: Exception): Long = when {
        e is ApiException && e.rateLimited -> rateLimitWait(e)
        // Too old to pair: retrying soon can't help; an update restarts the app anyway.
        e is ApiException && (e.code == "upgrade_required" || e.status == 426) -> maxOf(pace.next(), UPGRADE_WAIT_MS)
        else -> pace.next()
    }

    private fun rateLimitWait(e: ApiException): Long =
        e.retryAfterS?.let { (it * 1000).coerceIn(1_000, MAX_RETRY_AFTER_MS) } ?: maxOf(pace.next(), RATE_LIMIT_FALLBACK_MS)

    private suspend fun sleep(ms: Long, wakeOnNetwork: Boolean) {
        if (wakeOnNetwork) withTimeoutOrNull(ms) { netWake.receive() } else delay(ms)
    }

    /** Follows one enrollment until it pairs (the result) or is gone. */
    private suspend fun follow(e: EnrollmentCreated): Followed {
        val retry = Backoff()
        var expiresAt = e.codeExpiresAt
        var opened = false
        while (true) {
            currentCoroutineContext().ensureActive()
            val sock = port.connect(e)
            var gone: Followed.Gone? = null
            var limited: ApiException? = null
            try {
                loop@ while (true) {
                    val untilExpiry = millisUntil(expiresAt)?.plus(5_000)?.coerceAtLeast(5_000) ?: 60_000
                    val ev = withTimeoutOrNull(untilExpiry) { sock.events.receive() }
                    if (ev == null) {
                        // The code expired without a rotation frame: ask for the current one.
                        when (val st = poll(e)) {
                            is Polled.Paired -> return Followed.Paired(st.paired)
                            is Polled.Gone -> { gone = Followed.Gone(opened, st.status, st.refusal); break@loop }
                            is Polled.Waiting -> expiresAt = st.expiresAt
                            Polled.Unknown -> break@loop
                        }
                        continue
                    }
                    when (ev) {
                        SocketEvent.Open -> {
                            opened = true
                            pace.reset()
                            retry.reset()
                            ui { it.copy(live = true, error = null) }
                        }
                        is SocketEvent.Text -> when (val f = Frames.decodeEnrollment(ev.text)) {
                            is EnrollmentFrame.Code -> {
                                expiresAt = f.codeExpiresAt
                                ui { it.copy(code = f.pairingCode.uppercase(), expiresAt = f.codeExpiresAt, live = true) }
                            }
                            is EnrollmentFrame.Paired -> return Followed.Paired(f)
                            is EnrollmentFrame.Ping -> sock.send(Frames.encode(DeviceFrame.Pong(f.nonce)))
                            is EnrollmentFrame.Unknown -> log("enrollment socket: ignored frame ${f.type}: ${f.problem ?: ""}", null)
                        }
                        is SocketEvent.Closed -> break@loop
                        is SocketEvent.Failed -> {
                            when {
                                ev.httpStatus == 401 || ev.httpStatus == 404 -> gone = Followed.Gone(opened, ev.httpStatus, ev.refusal)
                                ev.httpStatus == 429 || ev.refusal?.rateLimited == true -> limited = ev.refusal ?: ApiException(429, "rate_limited", "HTTP 429")
                            }
                            log("enrollment socket failed (${ev.httpStatus})", ev.error)
                            break@loop
                        }
                    }
                }
            } finally {
                sock.close()
            }
            ui { it.copy(live = false) }
            gone?.let { return it }
            if (limited != null) {
                // The service is reachable and limiting: wait as long as it asks, then reconnect.
                val wait = rateLimitWait(limited)
                ui { it.copy(error = PairingMessages.socketLimited(limited, wait, clock(), zone)) }
                delay(wait)
                continue
            }
            // The socket dropped: maybe we paired meanwhile, maybe the enrollment is gone.
            when (val st = poll(e)) {
                is Polled.Paired -> return Followed.Paired(st.paired)
                is Polled.Gone -> return Followed.Gone(opened, st.status, st.refusal)
                is Polled.Waiting -> {
                    expiresAt = st.expiresAt
                    ui { it.copy(code = st.code, expiresAt = st.expiresAt) }
                }
                Polled.Unknown -> {}
            }
            sleep(retry.next(), wakeOnNetwork = true)
        }
    }

    private suspend fun poll(e: EnrollmentCreated): Polled = try {
        when (val st = port.poll(e)) {
            is EnrollmentState.Paired -> Polled.Paired(EnrollmentFrame.Paired(st.deviceId, st.deviceCredential, st.environment))
            is EnrollmentState.Waiting -> Polled.Waiting(st.pairingCode.uppercase(), st.codeExpiresAt)
            null -> Polled.Unknown
        }
    } catch (ex: ApiException) {
        if (ex.status == 401 || ex.status == 404) Polled.Gone(ex.status, ex) else Polled.Unknown
    } catch (ex: CancellationException) {
        throw ex
    } catch (_: Exception) {
        Polled.Unknown
    }

    private fun millisUntil(timestamp: String): Long? =
        runCatching { Instant.parse(timestamp).toEpochMilli() - clock() }.getOrNull()

    companion object {
        /** A 429 without `retry_after_s` waits at least this long. */
        const val RATE_LIMIT_FALLBACK_MS = 60_000L
        /** The longest wait a `retry_after_s` can ask for (the service's window is an hour). */
        const val MAX_RETRY_AFTER_MS = 3_600_000L
        /** After `upgrade_required`, ask again this rarely (updating the app restarts it anyway). */
        const val UPGRADE_WAIT_MS = 15 * 60_000L
    }
}

/** The pairing screen's error lines: what happened, why, and what to do. */
object PairingMessages {
    /** `8 s`, `2 min 5 s`, `1 h 2 min`. */
    fun duration(ms: Long): String {
        val s = (ms + 999) / 1000
        return when {
            s < 90 -> "$s s"
            s < 3600 -> "${s / 60} min${if (s % 60 != 0L) " ${s % 60} s" else ""}"
            else -> "${s / 3600} h${if ((s % 3600) / 60 != 0L) " ${(s % 3600) / 60} min" else ""}"
        }
    }

    /** "in 8 s", or "in 12 min (at 14:32)" when the wait is long enough to outlive the screen's first read. */
    fun whenNext(waitMs: Long, now: Long, zone: ZoneId): String {
        val base = "in ${duration(waitMs)}"
        if (waitMs < 90_000) return base
        val at = DateTimeFormatter.ofPattern("HH:mm").withZone(zone).format(Instant.ofEpochMilli(now + waitMs))
        return "$base (at $at)"
    }

    private fun clean(text: String?) = text?.trim()?.trimEnd('.')?.takeIf { it.isNotEmpty() }

    fun createFailed(e: Exception, waitMs: Long, serviceUrl: String, now: Long, zone: ZoneId): String {
        val next = whenNext(waitMs, now, zone)
        if (e !is ApiException) {
            val host = serviceUrl.substringAfter("://").substringBefore('/')
            val why = clean(e.message) ?: e.javaClass.simpleName
            val network = e is IOException
            return "Can't reach Extend at $host ($why). " +
                (if (network) "Check this device's internet connection. " else "") +
                "The app asks for a pairing code again $next."
        }
        if (e.rateLimited) {
            return "Extend is limiting new pairing codes from this network, so it hasn't given this device one yet" +
                (clean(e.message)?.let { " ($it)" } ?: "") + ". " +
                "Nothing to do: keep the app open and it asks again by itself $next."
        }
        if (e.code == "upgrade_required" || e.status == 426) {
            return "This version of Silicon Extend is too old to pair" + (clean(e.message)?.let { ": $it" } ?: "") + ". " +
                "Install the latest from extend.teamofsilicons.com. The app asks again $next."
        }
        return "Extend refused a new pairing code (HTTP ${e.status}${e.code?.let { " $it" } ?: ""}: ${clean(e.message) ?: "no reason given"})." +
            (clean(e.hint)?.let { " $it." } ?: "") +
            " The app asks again $next. If it keeps failing, report it with this message."
    }

    fun enrollmentRefused(status: Int?, refusal: ApiException?, waitMs: Long, now: Long, zone: ZoneId): String {
        val next = whenNext(waitMs, now, zone)
        if (refusal?.rateLimited == true) {
            return "Extend is limiting pairing connections from this network" + (clean(refusal.message)?.let { " ($it)" } ?: "") + ". " +
                "Nothing to do: keep the app open and it gets a new code by itself $next."
        }
        val why = refusal?.let { clean(it.message) } ?: when (status) {
            401 -> "the service no longer accepts this code's secret"
            404 -> "the service no longer knows this code"
            else -> "the service ended it"
        }
        return "That pairing code stopped working before this device could use it (HTTP ${status ?: "error"}: $why). " +
            "The app gets a new code by itself $next; enter the new code on the website."
    }

    fun socketLimited(e: ApiException, waitMs: Long, now: Long, zone: ZoneId): String =
        "Extend is limiting connections from this network" + (clean(e.message)?.let { " ($it)" } ?: "") + ". " +
            "This code still works; the app reconnects by itself ${whenNext(waitMs, now, zone)}."
}

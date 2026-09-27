package com.teamofsilicons.extend.driver

import com.teamofsilicons.extend.protocol.CommandError
import com.teamofsilicons.extend.protocol.DeviceFrame
import com.teamofsilicons.extend.protocol.Frames
import com.teamofsilicons.extend.protocol.ServiceFrame
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonNull
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean

/**
 * The commands in flight. Each answers with exactly one `result` (docs/device-protocol.md: "A
 * failed command still answers `result`"), also when it is cancelled with a reason, so the Silicon
 * isn't left waiting for its whole timeout. Only commands cancelled because the socket is gone
 * don't answer: nothing could carry the answer.
 */
class CommandJobs(private val scope: CoroutineScope) {
    private class Entry(val job: Job, val sessionId: String, val command: String, val pairId: String)

    private val entries = ConcurrentHashMap<String, Entry>()
    private val reasons = ConcurrentHashMap<String, CommandError>()

    /** [pairId]: the pair whose connection the command came on, and which carries its answer. */
    fun submit(frame: ServiceFrame.Command, run: suspend () -> DeviceFrame.Result, send: (DeviceFrame.Result) -> Unit, pairId: String = "") {
        val answered = AtomicBoolean(false)
        val job = scope.launch(start = CoroutineStart.LAZY) {
            val result = run()
            if (answered.compareAndSet(false, true)) send(Frames.fitResult(result))
        }
        entries[frame.id] = Entry(job, frame.sessionId, frame.command, pairId)
        job.invokeOnCompletion { cause ->
            entries.remove(frame.id)
            val reason = reasons.remove(frame.id)
            if (cause is CancellationException && reason != null && answered.compareAndSet(false, true)) {
                send(DeviceFrame.Result(frame.id, false, JsonNull, reason.message, reason, emptyList()))
            }
        }
        job.start()
    }

    /** Cancels one command; it answers with [reason] unless it already answered. */
    fun cancel(commandId: String, reason: CommandError) {
        val entry = entries[commandId] ?: return
        reasons.putIfAbsent(commandId, reason)
        entry.job.cancel()
    }

    /** Cancels every command of [sessionId] with [reason]. */
    fun cancelSession(sessionId: String, reason: CommandError) =
        entries.filterValues { it.sessionId == sessionId }.keys.forEach { cancel(it, reason) }

    /** Cancels every command named in [commands] with [reason]. */
    fun cancelCommands(commands: Set<String>, reason: CommandError) =
        entries.filterValues { it.command in commands }.keys.forEach { cancel(it, reason) }

    /** Cancels everything without answers: the socket they would answer on is gone. */
    fun cancelAll() {
        entries.values.forEach { it.job.cancel() }
    }

    /** Cancels, without answers, the commands that came on [pairId]'s connection, which is gone. */
    fun cancelPair(pairId: String) {
        entries.values.filter { it.pairId == pairId }.forEach { it.job.cancel() }
    }

    val running: Int get() = entries.size
}

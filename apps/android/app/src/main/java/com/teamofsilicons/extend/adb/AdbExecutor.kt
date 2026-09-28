package com.teamofsilicons.extend.adb

import android.os.StatFs
import com.teamofsilicons.extend.driver.CommandFailure
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import java.io.File
import java.io.IOException
import java.util.UUID

/**
 * Session-owned recordings, logs and shell processes. Temporary files and processes never belong
 * to another session.
 *
 * A finished recording or log file is kept until it has been uploaded: when an upload fails or
 * runs out of time, the next `record stop` / `logs stop` in the session sends it again.
 */
class AdbExecutor(
    private val adb: LocalAdb,
    /** Short-lived files (pulled files, command output). */
    private val cache: File,
    /** Files that must survive cache trimming: the recovery list, captures and saved output. */
    private val state: File = cache,
    /** Written into each capture directory on the device so a sweep only removes this app's. */
    private val owner: String = "com.teamofsilicons.extend",
    private val freeBytes: (File) -> Long = { dir -> dir.mkdirs(); StatFs(dir.path).availableBytes },
    /** Native screenrecord segment length; tests use a few seconds to exercise rollover. */
    private val segmentSeconds: Int = 180,
    /** Overall capture bound; tests use a few seconds to exercise the automatic stop. */
    private val durationSeconds: Int = 1800,
) {
    /**
     * A file a command produced. A [retained] file stays until [delivered] is called for it, so a
     * failed upload can be retried; other files are deleted after the upload attempt.
     */
    data class Artifact(val file: File, val contentType: String, val kind: String, val retained: Boolean = false)

    /**
     * What a command produced. [output] replaces the default `{message, files}` output, and a
     * non-null, non-zero [exitCode] fails the command after its files are uploaded.
     */
    data class Result(
        val text: String,
        val artifacts: List<Artifact> = emptyList(),
        val output: JsonObject? = null,
        val exitCode: Int? = null,
    )

    private class Capture(val directory: String, val name: String) {
        lateinit var job: Job
        @Volatile var error: String? = null
        /** Output-side failures while saving; a recording that can't be written three times is given up. */
        var outputFailures = 0
    }
    private class LogCapture(val dir: File, val file: File) {
        lateinit var job: Job
        val ready = CompletableDeferred<Unit>()
        @Volatile var error: String? = null
    }
    /** Output that was saved but not yet delivered. */
    private class Saved(val dir: File, val artifacts: MutableList<Artifact>, val text: String)
    private class Session {
        var recording: Capture? = null
        var logs: LogCapture? = null
        var savedRecording: Saved? = null
        var savedLogs: Saved? = null
        /** A Silicon's shell command ran, so its detached processes are ended with the session. */
        var shellUsed = false
    }
    /** Captures a session lost on the device while Extend may still keep the session. */
    private class Lost(var recording: Boolean, var logs: Boolean, val reason: String)

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val sessions = mutableMapOf<String, Session>()
    /** The ids in [sessions], readable without the mutex. */
    private val sessionIndex = java.util.concurrent.ConcurrentHashMap.newKeySet<String>()
    /**
     * Sessions whose captures were discarded by [endSession] with a reason, newest last: if Extend
     * announces such a session again, record stop and logs stop say what happened to the capture.
     */
    private val lost = object : LinkedHashMap<String, Lost>() {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, Lost>?) = size > 32
    }
    private val mutex = Mutex()
    private val recoveryFile = File(state, "pending-recordings.txt")
    private val pending: MutableSet<String> = loadPending()
    private var swept = false
    private var localCleaned = false

    private fun loadPending(): MutableSet<String> {
        // Earlier versions kept the list in the cache directory, which Android may clear.
        val sources = listOf(recoveryFile, File(cache, "pending-recordings.txt")).distinct()
        return sources.filter { it.isFile }.flatMap { it.readLines() }
            .filter { it.matches(REMOTE_DIRECTORY) }.toMutableSet()
    }
    private fun persist() {
        state.mkdirs()
        val next = File(state, "pending-recordings.tmp")
        next.writeText(pending.joinToString("\n"))
        check(next.renameTo(recoveryFile)) { "Could not save Android recording recovery state" }
        File(cache, "pending-recordings.txt").takeIf { it != recoveryFile }?.delete()
    }
    private suspend fun recoverLocked() {
        val active = sessions.values.mapNotNull { it.recording?.directory }.toSet()
        for (dir in pending.toList().filterNot { it in active }) {
            stop(Capture(dir, "recovery"))
            adb.shell("rm -rf ${quote(dir)}")
            pending.remove(dir)
            persist()
        }
        if (!swept) {
            // Best effort: a failed sweep is tried again after the next reconnect.
            swept = try { sweep(active); true } catch (e: IOException) { false }
        }
        if (!localCleaned) {
            // Saved output from an earlier app process belongs to sessions that no longer exist here.
            val live = sessions.values.flatMap { listOfNotNull(it.logs?.dir, it.savedLogs?.dir, it.savedRecording?.dir) }.toSet()
            state.listFiles()?.filter { it.isDirectory && (it.name.startsWith("recording-") || it.name.startsWith("logs-")) && it !in live }
                ?.forEach { it.deleteRecursively() }
            localCleaned = true
        }
    }

    /**
     * Removes capture directories on the device that no session owns, for example when the
     * recovery list was lost or the app was reinstalled. It skips directories of another app,
     * directories changed in the last ten minutes and ones whose recorder still runs.
     */
    private suspend fun sweep(active: Set<String>) {
        val keep = active.joinToString(" ") { quote(it) }
        adb.shell(
            OWNED + "keep=\" $keep \"; for d in /data/local/tmp/silicon-extend-*; do [ -d \"\$d\" ] || continue; " +
                "case \"\$keep\" in *\" '\$d' \"*) continue;; esac; " +
                "if [ -f \"\$d/owner\" ]; then [ \"\$(cat \"\$d/owner\")\" = ${quote(owner)} ] || continue; fi; " +
                "[ -z \"\$(find \"\$d\" -mmin -10 2>/dev/null | head -n 1)\" ] || continue; " +
                "if owned \"\$d/supervisor.pid\" \"\$d\" || owned \"\$d/pid\" \"\$d\"; then continue; fi; " +
                "rm -rf \"\$d\"; done",
            check = false,
        )
    }

    /** Recovers interrupted captures; also call after Android debugging reconnects. */
    suspend fun recover() = mutex.withLock {
        swept = false
        recoverLocked()
    }
    private fun quote(s: String) = AdbWire.quote(s)
    private fun directory() = "/data/local/tmp/silicon-extend-${UUID.randomUUID()}"
    private fun requireAttachment(path: String, attachments: Collection<File>): File =
        attachments.firstOrNull { it.absolutePath == path }?.takeIf { it.isFile }
            ?: throw CommandFailure.invalid("Send the file as a command attachment; local device paths are not accepted as APK inputs.")

    suspend fun execute(sessionId: String, command: AdbCommand, attachments: Collection<File>): Result = mutex.withLock {
        recoverLocked()
        val session = sessions.getOrPut(sessionId) { Session() }
        sessionIndex += sessionId
        when (command) {
            is AdbCommand.Install -> install(requireAttachment(command.path, attachments), command.app, fresh = command.fresh)
            is AdbCommand.Raw -> raw(sessionId, session, command.args, attachments)
            is AdbCommand.Record -> when (command.action) {
                "start" -> {
                    if (session.recording != null) throw CommandFailure.invalid("A recording is already running in this session. Use record stop first.")
                    if (session.savedRecording != null) throw CommandFailure.invalid(
                        "The recording saved by the last record stop hasn't reached Briefcase yet. Run record stop again to receive it, then start a new recording.",
                    )
                    val name = command.name.removeSuffix(".mp4") + ".mp4"
                    val capture = startCapture(name, command.quality == "high")
                    session.recording = capture
                    lost[sessionId]?.recording = false
                    Result("Recording started (up to 30 minutes or 1 GiB). Use record stop to save it.")
                }
                else -> {
                    session.savedRecording?.let { saved ->
                        return@withLock Result("${saved.text} Sending the recording saved by an earlier record stop.", saved.artifacts.toList())
                    }
                    val capture = session.recording ?: throw missing(sessionId, recording = true)
                    val saved = try {
                        collect(capture)
                    } catch (e: Discarded) {
                        session.recording = null
                        throw CommandFailure(CommandFailure.ACTION_FAILED, e.message ?: "The recording could not be saved.")
                    }
                    session.recording = null
                    session.savedRecording = saved
                    Result(saved.text, saved.artifacts.toList())
                }
            }
            is AdbCommand.Logs -> when (command.action) {
                "start" -> {
                    if (session.logs != null) throw CommandFailure.invalid("Logs are already being collected. Use logs stop first.")
                    if (session.savedLogs != null) throw CommandFailure.invalid(
                        "The logs saved by the last logs stop haven't reached Briefcase yet. Run logs stop again to receive them, then start a new capture.",
                    )
                    session.logs = startLogs()
                    lost[sessionId]?.logs = false
                    Result("Collecting device logs (up to 16 MB). Use logs stop to save them.")
                }
                "stop" -> {
                    session.savedLogs?.let { saved ->
                        return@withLock Result("${saved.text} Sending the logs saved by an earlier logs stop.", saved.artifacts.toList())
                    }
                    val capture = session.logs ?: throw missing(sessionId, recording = false)
                    capture.job.cancelAndJoin()
                    session.logs = null
                    if (!capture.file.isFile || capture.file.length() == 0L) {
                        capture.dir.deleteRecursively()
                        throw CommandFailure(
                            CommandFailure.ACTION_FAILED,
                            "Android returned no log lines${capture.error?.let { " (the log stream ended: $it)" } ?: ""}. Run logs start to capture again.",
                        )
                    }
                    val saved = Saved(
                        capture.dir,
                        mutableListOf(Artifact(capture.file, "text/plain", "log", retained = true)),
                        "Saved device logs${capture.error?.let { "; the stream ended early: $it" } ?: ""}.",
                    )
                    session.savedLogs = saved
                    Result(saved.text, saved.artifacts.toList())
                }
                "mark" -> { adb.shell("log -t SiliconExtend -- ${quote(command.label)}"); Result("Log marker added") }
                else -> { adb.shell("logcat -c"); Result("Device log buffers cleared") }
            }
        }
    }

    /** Nothing to stop: either nothing was started, or the device discarded the capture ([lost]). */
    private fun missing(sessionId: String, recording: Boolean): CommandFailure {
        val gone = lost[sessionId]?.takeIf { if (recording) it.recording else it.logs }
            ?: return CommandFailure.invalid(
                if (recording) "No recording in this session. Run record start first."
                else "No logs are being collected in this session. Run logs start first.",
            )
        return CommandFailure(
            CommandFailure.ACTION_FAILED,
            "The ${if (recording) "recording" else "log capture"} in this session was discarded on the device: ${gone.reason} " +
                "It can't be recovered. Run ${if (recording) "record start to record" else "logs start to collect logs"} again.",
        )
    }

    /** A retained artifact reached Briefcase: it no longer needs to be kept. */
    suspend fun delivered(sessionId: String, artifact: Artifact) = mutex.withLock {
        val session = sessions[sessionId]
        for (saved in listOfNotNull(session?.savedRecording, session?.savedLogs)) {
            if (saved.artifacts.remove(artifact) && saved.artifacts.isEmpty()) {
                saved.dir.deleteRecursively()
                if (session?.savedRecording === saved) session.savedRecording = null
                if (session?.savedLogs === saved) session.savedLogs = null
            }
        }
        artifact.file.delete()
    }

    private suspend fun startLogs(): LogCapture {
        val dir = File(state, "logs-${UUID.randomUUID()}").apply { mkdirs() }
        val capture = LogCapture(dir, File(dir, "device.log"))
        val opened = CompletableDeferred<Unit>()
        capture.job = scope.launch {
            try {
                adb.logStream(capture.file, onOpen = { opened.complete(Unit) }, onReady = { capture.ready.complete(Unit) })
                val ended = IllegalStateException("Android's log stream ended before it produced a line")
                opened.completeExceptionally(ended)
                capture.ready.completeExceptionally(ended)
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) {
                capture.error = e.message
                opened.completeExceptionally(e)
                capture.ready.completeExceptionally(e)
            }
        }
        val discard: suspend () -> Unit = { withContext(NonCancellable) { capture.job.cancelAndJoin(); dir.deleteRecursively() } }
        val ready = try {
            withTimeoutOrNull(5000) {
                opened.await()
                // After `logs clear` the buffers are empty and `logcat -T 1` prints nothing until a
                // new line is logged, which may take a long time on an idle TV. The marker
                // guarantees a line and records where this capture began.
                adb.shell("log -t SiliconExtend -- ${quote("Extend log capture started")}")
                capture.ready.await()
            } != null
        } catch (e: CancellationException) {
            discard(); throw e
        } catch (e: Exception) {
            discard()
            throw CommandFailure(
                CommandFailure.ACTION_FAILED,
                "Android's log stream could not start: ${e.message ?: e.javaClass.simpleName}. Check Android debugging in the Extend app on this device, then run logs start again.",
            )
        }
        if (!ready) {
            discard()
            throw CommandFailure(
                CommandFailure.ACTION_FAILED,
                "Android's log stream did not deliver its first line within 5 seconds, so no capture was started. Run logs start again; if it keeps failing, reconnect Android debugging in the Extend app.",
            )
        }
        return capture
    }

    private suspend fun startCapture(name: String, highQuality: Boolean): Capture {
        val dir = directory()
        val capture = Capture(dir, name)
        pending.add(dir)
        persist()
        adb.shell("mkdir -m 700 ${quote(dir)} && echo ${quote(owner)} >${quote("$dir/owner")}")
        val script = File.createTempFile("record-", ".sh", cache.apply { mkdirs() })
        try {
            script.writeText(RecordingScript.create(dir, highQuality, durationSeconds = durationSeconds, segmentSeconds = segmentSeconds))
            adb.push(script, "$dir/capture.sh")
        } finally { script.delete() }
        // The PTY keeps the supervisor tied to this app's ADB stream. Its HUP trap finalizes
        // the current child and prevents another segment after transport or owner loss.
        capture.job = scope.launch {
            try {
                adb.shell("exec sh ${quote("$dir/capture.sh")}", pty = true)
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) { capture.error = e.message }
        }
        try {
            val started = withTimeoutOrNull(5000) {
                while (true) {
                    capture.error?.let { throw CommandFailure(CommandFailure.ACTION_FAILED, "Android's screen recorder could not start: $it. Check Android debugging in the Extend app, then run record start again.") }
                    val ready = adb.shell("p=\$(cat ${quote("$dir/pid")} 2>/dev/null); test -n \"\$p\" && $NAMES_IN_CMDLINE ${quote("$dir/chunk-")} /proc/\$p/cmdline 2>/dev/null", check = false)
                    if (ready.exitCode == 0) break
                    if (capture.job.isCompleted) throw CommandFailure(CommandFailure.ACTION_FAILED, "Android's screen recorder exited without producing a recording. Run record start again; if it keeps failing, restart the device.")
                    delay(100)
                }
                true
            }
            if (started == null) throw CommandFailure(
                CommandFailure.ACTION_FAILED,
                "Android's screen recorder did not start within 5 seconds, so the recording was cancelled. The device may be busy; run record start again.",
            )
            return capture
        } catch (e: Exception) {
            withContext(NonCancellable) { runCatching { withTimeout(12000) { cleanup(capture) } } }
            throw e
        }
    }

    /** Stops the recorder; true when it had to be killed because it didn't finalize in time. */
    private suspend fun stop(capture: Capture): Boolean {
        val dir = quote(capture.directory)
        // A persistent stop marker fences segment rollover before any signal is delivered.
        // The supervisor is preferred: it signals its child once and waits for finalization.
        adb.shell(OWNED + "touch $dir/stop; if owned $dir/supervisor.pid $dir; then kill -2 \"\$p\" 2>/dev/null || true; " +
            "elif owned $dir/pid $dir; then kill -2 \"\$p\" 2>/dev/null || true; fi")
        repeat(80) {
            val running = adb.shell(OWNED + "owned $dir/supervisor.pid $dir || owned $dir/pid $dir", check = false)
            if (running.exitCode != 0) return false
            delay(100)
        }
        adb.shell(OWNED + "for f in $dir/pid $dir/supervisor.pid; do if owned \"\$f\" $dir; then kill -9 \"\$p\" 2>/dev/null || true; fi; done")
        return true
    }
    private suspend fun cleanup(capture: Capture) {
        try { stop(capture) }
        finally { capture.job.cancelAndJoin() }
        adb.shell("rm -rf ${quote(capture.directory)}")
        pending.remove(capture.directory)
        persist()
    }

    /** The recording can't be saved and was deleted; retrying can't help. */
    private class Discarded(message: String) : Exception(message)

    private data class Segment(val index: Int, val name: String, val size: Long, val timing: String?)

    /**
     * Saves a recording: pulls and appends one segment at a time. A segment that can't be read is
     * left out (with a note) instead of failing the rest. Segments with a different picture size
     * go to another file. The device copy is removed only once every output file is written.
     */
    private suspend fun collect(capture: Capture): Saved {
        val forced = stop(capture)
        capture.job.cancelAndJoin()
        val remote = capture.directory
        val listing = adb.shell(
            "cd ${quote(remote)} && for f in chunk-*.mp4; do [ -f \"\$f\" ] || continue; " +
                "echo \"\$f \$(stat -c %s \"\$f\") \$(cat \"\$f.timing\" 2>/dev/null)\"; done",
        ).text
        val segments = listing.lineSequence().mapNotNull { line ->
            val fields = line.trim().split(Regex("\\s+"))
            val name = fields.getOrNull(0)?.takeIf { it.matches(Regex("chunk-[0-9]+\\.mp4")) } ?: return@mapNotNull null
            Segment(name.removePrefix("chunk-").removeSuffix(".mp4").toInt(), name, fields.getOrNull(1)?.toLongOrNull() ?: 0L,
                fields.drop(2).joinToString(" ").ifBlank { null })
        }.sortedBy { it.index }.toList()
        val reason = adb.shell("cat ${quote(remote)}/completed", check = false).text.trim().ifEmpty { "stopped" }

        // Output and the one source being appended share /data with the device copy.
        val needed = segments.sumOf { it.size } + (segments.maxOfOrNull { it.size } ?: 0L) + 64L * 1024 * 1024
        val free = freeBytes(state)
        if (free < needed) throw CommandFailure(
            CommandFailure.ACTION_FAILED,
            "Saving the recording needs about ${mib(needed)} MiB free on this device, and ${mib(free)} MiB is free. " +
                "Free up space, then run record stop again; the recording is kept until then.",
        )

        val dir = File(state, "recording-${UUID.randomUUID()}").apply { mkdirs() }
        val notes = mutableListOf<String>()
        if (forced) notes += "The recorder did not stop within 8 seconds and was force-stopped, so the end of the recording may be missing."
        val writers = mutableListOf<RecordingMuxer.Writer>()
        var current: RecordingMuxer.Writer? = null
        var estimated = 0
        try {
            for (segment in segments) {
                currentCoroutineContext().ensureActive()
                val label = "segment ${segment.index + 1}"
                if (segment.size == 0L) { notes += "Left out $label: it was empty (recording stopped as it began)."; continue }
                val source = File(dir, ".source-${segment.name}")
                try {
                    adb.pull("$remote/${segment.name}", source, RecordingScript.MAX_BYTES)
                    val opened = try { RecordingMuxer.open(source) } catch (e: RecordingMuxer.BadSegment) {
                        notes += "Left out $label: ${e.message}."
                        continue
                    }
                    opened.use { src ->
                        var writer = current
                        if (writer == null || !writer.accepts(src.format)) {
                            val atUs = writer?.endUs
                            writer?.finish()
                            writer = RecordingMuxer.Writer(File(dir, outputName(capture.name, writers.size)), src.format)
                            writers += writer
                            current = writer
                            if (atUs != null) notes += "The screen size changed to ${RecordingMuxer.size(src.format)} after ${clock(atUs)}; the recording continues in ${writer.output.name}."
                        }
                        val timing = RecordingTimeline.parse(segment.timing)
                        val appended = writer.append(src, timing)
                        if (appended.estimated) estimated++
                        appended.cutOff?.let { notes += "$label was cut off ($it); the frames before that were kept." }
                    }
                } finally {
                    source.delete()
                }
            }
            current?.finish()
        } catch (e: CancellationException) {
            writers.forEach { it.abandon() }
            dir.deleteRecursively()
            throw e
        } catch (e: CommandFailure) {
            writers.forEach { it.abandon() }
            dir.deleteRecursively()
            throw e
        } catch (e: Exception) {
            writers.forEach { it.abandon() }
            dir.deleteRecursively()
            val output = e is RecordingMuxer.OutputFailure
            if (output && ++capture.outputFailures >= 3) {
                withContext(NonCancellable) { runCatching { withTimeout(12000) { cleanup(capture) } } }
                throw Discarded("The recording could not be written after three attempts (${e.message?.trimEnd('.')}), so it was discarded; another record stop can't recover it. Run record start to record again.")
            }
            throw CommandFailure(
                CommandFailure.ACTION_FAILED,
                "Could not save the Android recording: ${(e.message ?: e.javaClass.simpleName).trimEnd('.')}. The recording is kept on the device; run record stop again.",
            )
        }
        if (writers.isEmpty()) {
            withContext(NonCancellable) { runCatching { withTimeout(12000) { cleanup(capture) } } }
            dir.deleteRecursively()
            // Each note is a sentence of its own, ending in a full stop: no second one after them.
            throw Discarded(
                "Android produced no usable recording${if (notes.isEmpty()) "." else ": " + notes.joinToString(" ").trimEnd('.') + "."} " +
                    "It was discarded, and another record stop can't recover it. Run record start to record again.",
            )
        }
        // Every output file is written: the device copy is no longer needed.
        val removed = adb.shell("rm -rf ${quote(remote)}", check = false).exitCode == 0
        if (removed) {
            pending.remove(remote)
            persist()
        }
        if (estimated > 0) notes += "$estimated segment(s) had no timing record; their length was estimated from their frames."
        if (segments.size > 1) notes += "Combined ${segments.size} segments; native recorder restarts can leave brief capture gaps."
        val files = writers.joinToString(" and ") { "${it.output.name} (${clock(it.endUs)})" }
        val text = (listOf("Saved $files ($reason).") + notes).joinToString(" ")
        return Saved(dir, writers.map { Artifact(it.output, "video/mp4", "recording", retained = true) }.toMutableList(), text)
    }

    private fun outputName(name: String, index: Int) = if (index == 0) name else name.removeSuffix(".mp4") + "-${index + 1}.mp4"
    private fun mib(bytes: Long) = (bytes + 1024 * 1024 - 1) / (1024 * 1024)
    private fun clock(us: Long): String { val s = us / 1_000_000; return "%d:%02d".format(s / 60, s % 60) }

    private suspend fun install(file: File, app: String?, fresh: Boolean, replace: Boolean = true): Result {
        val dir = directory()
        try {
            adb.shell("mkdir -m 700 ${quote(dir)} && echo ${quote(owner)} >${quote("$dir/owner")}")
            adb.push(file, "$dir/app.apk")
            // Package identity was checked against the attachment before reaching here.
            var removed = false
            if (fresh && app != null) {
                // The device engine's reinstall: remove the app and its data, then install.
                val uninstall = adb.shell(AdbWire.argv(listOf("pm", "uninstall", app)), check = false)
                removed = uninstall.text.lineSequence().any { it.trim() == "Success" }
            }
            val response = adb.shell("pm install ${if (replace) "-r " else ""}${quote("$dir/app.apk")}", check = false)
            val answer = response.text.trim()
            if (!answer.lineSequence().any { it.trim() == "Success" }) throw CommandFailure(
                CommandFailure.ACTION_FAILED,
                "Android refused to install ${app ?: file.name}: ${answer.take(2000).ifEmpty { "exit ${response.exitCode}" }}." +
                    (if (removed) " The previously installed copy and its data were already removed." else "") +
                    " Check that the APK suits this device (Android version, CPU type, signature) and try again.",
            )
            val prefix = when {
                !fresh -> "Installed"
                removed -> "Removed the installed copy and its data, then installed"
                else -> "Installed (it wasn't installed before)"
            }
            return Result("$prefix ${app ?: file.name}: $answer")
        } finally {
            withContext(NonCancellable) { runCatching { withTimeout(5000) { adb.shell("rm -rf ${quote(dir)}") } } }
        }
    }

    /** Runs a Silicon's shell command, tagged with its session so detached processes end with it. */
    private suspend fun captured(sessionId: String, session: Session, command: String, stdoutName: String): Pair<AdbWire.Captured, File> {
        session.shellUsed = true
        val dir = File(cache, UUID.randomUUID().toString()).apply { mkdirs() }
        val result = try {
            adb.shellCapture("export $SESSION_VARIABLE=${quote(sessionId)}; $command") { stream ->
                File(dir, if (stream == "stdout") stdoutName else "stderr.txt")
            }
        } catch (e: Exception) {
            dir.deleteRecursively()
            throw e
        }
        return result to dir
    }

    /** `{stdout, stderr, exit_code}` as the computer `terminal` command reports it, with full output attached when long. */
    private fun shellResult(result: AdbWire.Captured, dir: File): Result {
        val artifacts = listOfNotNull(result.stdout.file, result.stderr.file).map { Artifact(it, "text/plain", "log") }
        if (artifacts.isEmpty()) dir.deleteRecursively()
        val output = buildJsonObject {
            put("stdout", result.stdout.text)
            put("stderr", result.stderr.text)
            put("exit_code", result.exitCode)
            if (result.stdout.truncated || result.stderr.truncated) {
                put("stdout_truncated", result.stdout.truncated)
                put("stderr_truncated", result.stderr.truncated)
            }
        }
        val text = StringBuilder(result.stdout.text)
        if (result.stderr.text.isNotEmpty()) {
            if (text.isNotEmpty() && !text.endsWith("\n")) text.append('\n')
            text.append(result.stderr.text)
        }
        for ((name, stream) in listOf("stdout" to result.stdout, "stderr" to result.stderr)) {
            if (stream.truncated) text.append("\n[$name truncated after ${AdbWire.INLINE_LIMIT / 1024} KiB of ${stream.total} bytes; the full $name is attached as ${stream.file!!.name}]")
        }
        return Result(text.toString(), artifacts, output, result.exitCode)
    }

    private suspend fun raw(sessionId: String, session: Session, args: List<String>, attachments: Collection<File>): Result {
        val verb = args[0]; val rest = args.drop(1)
        fun need(n: Int) { if (rest.size != n) throw CommandFailure.invalid("adb $verb needs $n argument(s)") }
        return when (verb) {
            "shell" -> {
                if (rest.isEmpty()) throw CommandFailure.invalid("Interactive adb shell is not supported; supply a command.")
                // adb shell with a single argument is a shell program; multiple arguments are
                // individual argv values, preserving spaces and preventing unintended expansion.
                val (result, dir) = captured(sessionId, session, if (rest.size == 1) rest[0] else AdbWire.argv(rest), "stdout.txt")
                shellResult(result, dir)
            }
            "exec-out" -> {
                if (rest.isEmpty()) throw CommandFailure.invalid("adb exec-out needs a command.")
                val (result, dir) = captured(sessionId, session, if (rest.size == 1) rest[0] else AdbWire.argv(rest), "adb-output.bin")
                val binary = result.stdout.file ?: File(dir, "adb-output.bin").apply { writeBytes(result.stdout.inline) }
                val artifacts = listOf(Artifact(binary, "application/octet-stream", "other")) +
                    listOfNotNull(result.stderr.file).map { Artifact(it, "text/plain", "log") }
                val output = buildJsonObject {
                    put("stdout_bytes", result.stdout.total)
                    put("stderr", result.stderr.text)
                    put("exit_code", result.exitCode)
                }
                val text = "Saved binary command output (${result.stdout.total} bytes)" +
                    (if (result.stderr.text.isNotEmpty()) "\n" + result.stderr.text else "")
                Result(text, artifacts, output, result.exitCode)
            }
            "logcat" -> {
                // Unbounded streams cannot fit one Extend command; use logs start/stop instead.
                if ("-d" !in rest && "--dump" !in rest && "-c" !in rest) throw CommandFailure.invalid("Use adb logcat -d for a snapshot, or logs start/stop for streaming logs.")
                val (result, dir) = captured(sessionId, session, AdbWire.argv(listOf("logcat") + rest), "stdout.txt")
                shellResult(result, dir)
            }
            "push" -> { need(2); adb.push(requireAttachment(rest[0], attachments), rest[1]); Result("File pushed") }
            "pull" -> {
                need(1)
                val dir = File(cache, UUID.randomUUID().toString()).apply { mkdirs() }
                val name = rest[0].substringAfterLast('/').takeIf { it.isNotBlank() && it != "." && it != ".." } ?: "adb-file"
                val file = File(dir, name)
                try { adb.pull(rest[0], file) } catch (e: Exception) { dir.deleteRecursively(); throw e }
                Result("Pulled $name", listOf(Artifact(file, "application/octet-stream", "other")))
            }
            "install" -> {
                val replace = rest.firstOrNull() == "-r"
                val paths = if (replace) rest.drop(1) else rest
                if (paths.size != 1) throw CommandFailure.invalid("Usage: adb install [-r] <APK attachment>")
                install(requireAttachment(paths[0], attachments), null, fresh = false, replace = replace)
            }
            "uninstall" -> {
                need(1)
                val (result, dir) = captured(sessionId, session, AdbWire.argv(listOf("pm", "uninstall", rest[0])), "stdout.txt")
                shellResult(result, dir)
            }
            "get-state" -> { need(0); Result(if (adb.connected) "device" else "offline") }
            else -> throw CommandFailure.invalid("adb $verb is a host operation or unsupported service. Use shell, exec-out, logcat, push, pull, install, uninstall or get-state on this paired device.")
        }
    }

    /**
     * Ends a session's captures, deletes its saved output and ends the processes its shell commands
     * left running. [lostReason] is given when the session may still continue on Extend's side (the
     * device gave up on it while offline): a later record stop or logs stop in it then explains why
     * its capture is gone.
     */
    suspend fun endSession(id: String, lostReason: String? = null) = mutex.withLock {
        sessionIndex -= id
        val session = sessions.remove(id) ?: return@withLock
        if (lostReason != null) {
            val recording = session.recording != null || session.savedRecording != null
            val logs = session.logs != null || session.savedLogs != null
            if (recording || logs) lost[id] = Lost(recording, logs, lostReason)
        }
        session.logs?.let { it.job.cancelAndJoin(); it.dir.deleteRecursively() }
        session.recording?.let { runCatching { withTimeout(12000) { cleanup(it) } } }
        listOfNotNull(session.savedRecording, session.savedLogs).forEach { it.dir.deleteRecursively() }
        if (session.shellUsed) runCatching { withTimeout(10000) { endProcesses(id) } }
    }

    /**
     * Ends every process that still carries this session's tag in its environment, including ones
     * a Silicon detached with nohup, setsid or `&`. A process that clears its environment escapes.
     */
    private suspend fun endProcesses(sessionId: String) {
        adb.shell(
            "grep -lzxF -- ${quote("$SESSION_VARIABLE=$sessionId")} /proc/[0-9]*/environ 2>/dev/null | " +
                "while read -r f; do p=\${f#/proc/}; kill -9 \"\${p%/environ}\" 2>/dev/null; done; true",
            check = false,
        )
    }

    suspend fun endAll() {
        val ids = mutex.withLock { sessions.keys.toList() }
        ids.forEach { endSession(it) }
    }

    /** Session ids with state here (captures, saved output or tagged processes). Never waits. */
    fun sessionIds(): Set<String> = sessionIndex.toSet()

    companion object {
        /** Environment variable that tags a Silicon's shell processes with its session id. */
        const val SESSION_VARIABLE = "EXTEND_SESSION"
        private val REMOTE_DIRECTORY = Regex("/data/local/tmp/silicon-extend-[a-f0-9-]{36}")
        /**
         * `grep` over a `/proc/<pid>/cmdline` file: true when one of the process's arguments
         * contains the pattern that follows. grep opens the file itself and stops on a read error.
         * Never read it through `tr` from a redirect: when the process exits between the open and
         * the read, toybox `tr` spins at full CPU forever and the command never finishes.
         */
        internal const val NAMES_IN_CMDLINE = "grep -qzF --"
        /**
         * `owned <pid file> <directory>`: the pid in the file is a live process whose command line
         * names the directory, so a recycled PID is never signalled. Sets `p`.
         */
        internal const val OWNED = "owned() { p=\$(cat \"\$1\" 2>/dev/null); case \"\$p\" in ''|*[!0-9]*) return 1;; esac; " +
            "$NAMES_IN_CMDLINE \"\$2\" /proc/\$p/cmdline 2>/dev/null; }; "
    }
}

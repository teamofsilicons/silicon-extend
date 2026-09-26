package com.teamofsilicons.extend.adb

import com.teamofsilicons.extend.driver.CommandFailure
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.delay
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import java.io.File
import java.util.UUID

/** Session-owned recordings and logs. Temporary files and processes never belong to another session. */
class AdbExecutor(private val adb: LocalAdb, private val cache: File) {
    data class Artifact(val file: File, val contentType: String, val kind: String)
    data class Result(val text: String, val artifact: Artifact? = null)
    private class Capture(val directory: String, val name: String) {
        lateinit var job: Job
        @Volatile var error: String? = null
    }
    private class LogCapture(val file: File) {
        lateinit var job: Job
        val ready = CompletableDeferred<Unit>()
        @Volatile var error: String? = null
    }
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private data class Session(var recording: Capture? = null, var logs: LogCapture? = null)
    private val sessions = mutableMapOf<String, Session>()
    private val mutex = Mutex()
    private val recoveryFile = File(cache, "pending-recordings.txt")
    private val pending = recoveryFile.takeIf { it.isFile }?.readLines()?.filter {
        it.matches(Regex("/data/local/tmp/silicon-extend-[a-f0-9-]{36}"))
    }?.toMutableSet() ?: mutableSetOf()
    private fun persist() {
        cache.mkdirs()
        val next = File(cache, "pending-recordings.tmp")
        next.writeText(pending.joinToString("\n"))
        check(next.renameTo(recoveryFile)) { "Could not save Android recording recovery state" }
    }
    private suspend fun recoverLocked() {
        val active = sessions.values.mapNotNull { it.recording?.directory }.toSet()
        for (dir in pending.toList().filterNot { it in active }) {
            stop(Capture(dir, "recovery"))
            adb.shell("rm -rf ${quote(dir)}")
            pending.remove(dir)
            persist()
        }
    }
    suspend fun recover() = mutex.withLock { recoverLocked() }
    private fun quote(s: String) = AdbWire.quote(s)
    private fun directory() = "/data/local/tmp/silicon-extend-${UUID.randomUUID()}"
    private fun requireAttachment(path: String, attachments: Collection<File>): File =
        attachments.firstOrNull { it.absolutePath == path }?.takeIf { it.isFile }
            ?: throw CommandFailure.invalid("Send the file as a command attachment; local device paths are not accepted as APK inputs.")

    suspend fun execute(sessionId: String, command: AdbCommand, attachments: Collection<File>): Result = mutex.withLock {
        recoverLocked()
        val session = sessions.getOrPut(sessionId) { Session() }
        when (command) {
            is AdbCommand.Install -> install(requireAttachment(command.path, attachments), command.app, command.replace)
            is AdbCommand.Raw -> raw(command.args, attachments)
            is AdbCommand.Record -> when (command.action) {
                "start" -> {
                    if (session.recording != null) throw CommandFailure.invalid("A recording is already running in this session. Use record stop first.")
                    val name = command.name.removeSuffix(".mp4") + ".mp4"
                    val capture = startCapture(name, command.quality == "high")
                    session.recording = capture
                    Result("Recording started (up to 30 minutes or 1 GiB). Use record stop to save it.")
                }
                else -> {
                    val capture = session.recording ?: throw CommandFailure.invalid("No recording in this session. Run record start first.")
                    val result = collect(capture)
                    session.recording = null
                    result
                }
            }
            is AdbCommand.Logs -> when (command.action) {
                "start" -> {
                    if (session.logs != null) throw CommandFailure.invalid("Logs are already being collected. Use logs stop first.")
                    val dir = File(cache, UUID.randomUUID().toString()).apply { mkdirs() }
                    val capture = LogCapture(File(dir, "device.log"))
                    capture.job = scope.launch {
                        try {
                            adb.logStream(capture.file) { capture.ready.complete(Unit) }
                            if (!capture.ready.isCompleted) capture.ready.completeExceptionally(IllegalStateException("Android log stream ended before readiness"))
                        }
                        catch (e: CancellationException) { throw e }
                        catch (e: Exception) { capture.error = e.message; capture.ready.completeExceptionally(e) }
                    }
                    try { withTimeout(5000) { capture.ready.await() } }
                    catch (e: Exception) {
                        withContext(NonCancellable) { capture.job.cancelAndJoin(); dir.deleteRecursively() }
                        throw e
                    }
                    session.logs = capture
                    Result("Collecting device logs (up to 16 MB). Use logs stop to save them.")
                }
                "stop" -> {
                    val capture = session.logs ?: throw CommandFailure.invalid("No logs being collected in this session.")
                    capture.job.cancelAndJoin()
                    session.logs = null
                    if (!capture.file.isFile || capture.file.length() == 0L) {
                        capture.file.parentFile?.deleteRecursively()
                        throw CommandFailure(CommandFailure.ACTION_FAILED, capture.error ?: "Android returned no log lines.")
                    }
                    Result("Saved device logs${capture.error?.let { "; stream ended: $it" } ?: ""}", Artifact(capture.file, "text/plain", "log"))
                }
                "mark" -> { adb.shell("log -t SiliconExtend -- ${quote(command.label)}"); Result("Log marker added") }
                else -> { adb.shell("logcat -c"); Result("Device log buffers cleared") }
            }
        }
    }
    private suspend fun startCapture(name: String, highQuality: Boolean): Capture {
        val dir = directory()
        val capture = Capture(dir, name)
        pending.add(dir)
        persist()
        adb.shell("mkdir -m 700 ${quote(dir)}")
        val script = File.createTempFile("record-", ".sh", cache)
        try {
            script.writeText(RecordingScript.create(dir, highQuality))
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
            withTimeout(5000) {
                while (true) {
                    capture.error?.let { throw CommandFailure(CommandFailure.ACTION_FAILED, "Android capture could not start: $it") }
                    val ready = adb.shell("p=\$(cat ${quote("$dir/pid")} 2>/dev/null); test -n \"\$p\" && tr '\\0' ' ' </proc/\$p/cmdline 2>/dev/null | grep -F -- ${quote("$dir/chunk-")} >/dev/null", check = false)
                    if (ready.exitCode == 0) break
                    if (capture.job.isCompleted) throw CommandFailure(CommandFailure.ACTION_FAILED, "Android recorder exited without producing a recording")
                    delay(100)
                }
            }
            return capture
        } catch (e: Exception) {
            withContext(NonCancellable) { runCatching { withTimeout(12000) { cleanup(capture) } } }
            throw e
        }
    }
    private suspend fun stop(capture: Capture) {
        val dir = quote(capture.directory)
        // A persistent stop marker fences segment rollover before any signal is delivered.
        // The supervisor is preferred: it signals its child once and waits for finalization.
        val ownership = "owned() { p=\$(cat \"\$1\" 2>/dev/null); case \"\$p\" in ''|*[!0-9]*) return 1;; esac; " +
            "tr '\\0' ' ' </proc/\$p/cmdline 2>/dev/null | grep -F -- $dir >/dev/null; }; "
        adb.shell(ownership + "touch $dir/stop; if owned $dir/supervisor.pid; then kill -2 \"\$p\" 2>/dev/null || true; " +
            "elif owned $dir/pid; then kill -2 \"\$p\" 2>/dev/null || true; fi")
        repeat(80) {
            val running = adb.shell(ownership + "owned $dir/supervisor.pid || owned $dir/pid", check = false)
            if (running.exitCode != 0) return
            delay(100)
        }
        adb.shell(ownership + "for f in $dir/pid $dir/supervisor.pid; do if owned \"\$f\"; then kill -9 \"\$p\" 2>/dev/null || true; fi; done")
        throw CommandFailure(CommandFailure.ACTION_FAILED, "Android recorder did not finalize within eight seconds")
    }
    private suspend fun cleanup(capture: Capture) {
        try { stop(capture) }
        finally { capture.job.cancelAndJoin() }
        adb.shell("rm -rf ${quote(capture.directory)}")
        pending.remove(capture.directory)
        persist()
    }
    private suspend fun collect(capture: Capture): Result {
        stop(capture)
        capture.job.cancelAndJoin()
        val dir = File(cache, UUID.randomUUID().toString()).apply { mkdirs() }
        val file = File(dir, capture.name)
        try {
            val names = adb.shell("ls ${quote(capture.directory)}/chunk-*.mp4").text.lineSequence()
                .map { it.trim().substringAfterLast('/') }
                .filter { it.matches(Regex("chunk-[0-9]+\\.mp4")) }
                .sortedBy { it.removePrefix("chunk-").removeSuffix(".mp4").toInt() }.toList()
            val parts = names.map { name ->
                val source = File(dir, ".source-$name").also { adb.pull("${capture.directory}/$name", it) }
                RecordingMuxer.segment(source, adb.shell("cat ${quote("${capture.directory}/$name.timing")}").text)
            }
            RecordingMuxer.combine(parts, file)
            val reason = adb.shell("cat ${quote(capture.directory)}/completed", check = false).text.trim()
            adb.shell("rm -rf ${quote(capture.directory)}")
            pending.remove(capture.directory)
            persist()
            parts.forEach { it.file.delete() }
            val note = if (parts.size > 1) " Combined ${parts.size} segments; native recorder restarts can leave brief capture gaps." else ""
            return Result("Saved ${capture.name} ($reason).$note", Artifact(file, "video/mp4", "recording"))
        } catch (e: CancellationException) {
            dir.deleteRecursively()
            throw e
        } catch (e: Exception) {
            dir.deleteRecursively()
            throw CommandFailure(CommandFailure.ACTION_FAILED, "Could not finalize Android recording: ${e.message}. Source segments are retained for retry.")
        }
    }
    private suspend fun install(file: File, app: String?, replace: Boolean): Result {
        val dir = directory()
        try {
            adb.shell("mkdir -m 700 ${quote(dir)}")
            adb.push(file, "$dir/app.apk")
            // Verify package identity before installation. The APK parser runs as shell via
            // dumpsys only after install, so require the package through PackageManager locally
            // in CommandExecutor before reaching here.
            val response = adb.shell("pm install ${if (replace) "-r " else ""}${quote("$dir/app.apk")}")
            if (!response.text.lineSequence().any { it.trim() == "Success" }) throw CommandFailure(CommandFailure.ACTION_FAILED, response.text)
            return Result("Installed ${app ?: file.name}: ${response.text.trim()}")
        } finally {
            withContext(NonCancellable) { runCatching { withTimeout(5000) { adb.shell("rm -rf ${quote(dir)}") } } }
        }
    }
    private suspend fun raw(args: List<String>, attachments: Collection<File>): Result {
        val verb = args[0]; val rest = args.drop(1)
        fun need(n: Int) { if (rest.size != n) throw CommandFailure.invalid("adb $verb needs $n argument(s)") }
        return when (verb) {
            "shell", "exec-out" -> {
                if (rest.isEmpty()) throw CommandFailure.invalid("Interactive adb shell is not supported; supply a command.")
                // adb shell with a single argument is a shell program; multiple arguments are
                // individual argv values, preserving spaces and preventing unintended expansion.
                val result = adb.shell(if (rest.size == 1) rest[0] else AdbWire.argv(rest))
                if (verb == "exec-out") {
                    val dir = File(cache, UUID.randomUUID().toString()).apply { mkdirs() }
                    val file = File(dir, "adb-output.bin").apply { writeBytes(result.stdout) }
                    Result("Saved binary command output", Artifact(file, "application/octet-stream", "other"))
                } else Result(result.text)
            }
            "logcat" -> {
                // Unbounded streams cannot fit one Extend command; use logs start/stop instead.
                if ("-d" !in rest && "--dump" !in rest && "-c" !in rest) throw CommandFailure.invalid("Use adb logcat -d for a snapshot, or logs start/stop for streaming logs.")
                Result(adb.shell(AdbWire.argv(listOf("logcat") + rest)).text)
            }
            "push" -> { need(2); adb.push(requireAttachment(rest[0], attachments), rest[1]); Result("File pushed") }
            "pull" -> {
                need(1)
                val dir = File(cache, UUID.randomUUID().toString()).apply { mkdirs() }
                val name = rest[0].substringAfterLast('/').takeIf { it.isNotBlank() && it != "." && it != ".." } ?: "adb-file"
                val file = File(dir, name)
                try { adb.pull(rest[0], file) } catch (e: Exception) { dir.deleteRecursively(); throw e }
                Result("Pulled $name", Artifact(file, "application/octet-stream", "other"))
            }
            "install" -> {
                val replace = rest.firstOrNull() == "-r"
                val paths = if (replace) rest.drop(1) else rest
                if (paths.size != 1) throw CommandFailure.invalid("Usage: adb install [-r] <APK attachment>")
                install(requireAttachment(paths[0], attachments), null, replace)
            }
            "uninstall" -> { need(1); Result(adb.shell(AdbWire.argv(listOf("pm", "uninstall", rest[0]))).text) }
            "get-state" -> { need(0); Result(if (adb.connected) "device" else "offline") }
            else -> throw CommandFailure.invalid("adb $verb is a host operation or unsupported service. Use shell, exec-out, logcat, push, pull, install, uninstall or get-state on this paired device.")
        }
    }
    suspend fun endSession(id: String) = mutex.withLock {
        val session = sessions.remove(id) ?: return@withLock
        session.logs?.let { it.job.cancelAndJoin(); it.file.parentFile?.deleteRecursively() }
        session.recording?.let { runCatching { withTimeout(12000) { cleanup(it) } } }
    }
    suspend fun endAll() {
        val ids = mutex.withLock { sessions.keys.toList() }
        ids.forEach { endSession(it) }
    }
}

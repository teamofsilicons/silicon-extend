package com.teamofsilicons.extend.driver

import android.accessibilityservice.AccessibilityService
import android.app.ActivityManager
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.media.AudioManager
import android.net.Uri
import android.os.Bundle
import android.os.SystemClock
import android.text.InputType
import android.util.Base64
import android.view.KeyEvent
import android.view.accessibility.AccessibilityNodeInfo
import com.teamofsilicons.extend.Extend
import com.teamofsilicons.extend.a11y.ExtendAccessibilityService
import com.teamofsilicons.extend.core.SessionRetention
import com.teamofsilicons.extend.core.SetupReport
import com.teamofsilicons.extend.display.DisplayActivity
import com.teamofsilicons.extend.net.ApiException
import com.teamofsilicons.extend.notif.ExtendNotificationListener
import com.teamofsilicons.extend.protocol.Attachment
import com.teamofsilicons.extend.protocol.CommandError
import com.teamofsilicons.extend.protocol.DeviceFrame
import com.teamofsilicons.extend.protocol.ProducedFile
import com.teamofsilicons.extend.protocol.ServiceFrame
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import java.io.ByteArrayOutputStream
import java.io.File
import java.time.Instant
import java.util.concurrent.ConcurrentHashMap
import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt
import kotlin.math.sin

/** What a successful command returns: `result.output` and `result.text`. */
data class Outcome(val output: JsonElement, val text: String)

/**
 * Runs agent-device commands on this device through [ExtendAccessibilityService]. Commands run one
 * at a time (as agent-device's daemon serialises a session); refs from `snapshot` are kept per
 * session until the next snapshot.
 */
class CommandExecutor(
    private val extend: Extend,
    /** How long a session is kept while the device socket is down; tests shorten it. */
    retentionGraceMs: Long = SessionRetention.GRACE_MS,
) {
    private class Session {
        var last: Snapshot? = null
        var baseline: Snapshot? = null
    }

    private class Run(val frame: ServiceFrame.Command, val session: Session, val deadline: Long) {
        /** Attachments written to the command's scratch directory, by attachment name. */
        val localFiles = LinkedHashMap<String, File>()
        var scratch: File? = null
        var uploadCursor = 0
        val files = ArrayList<ProducedFile>()
        var depth = 0
        fun remainingMs(): Long = deadline - SystemClock.elapsedRealtime()
    }

    private val sessions = ConcurrentHashMap<String, Session>()
    private val jobs = CommandJobs(extend.scope)
    private val stoppedSessions = ConcurrentHashMap.newKeySet<String>()
    private val retention = SessionRetention(retentionGraceMs)
    private var retentionJob: Job? = null
    private val lock = Mutex()
    private val context get() = extend.context

    /** Extend announced a session (new, or again after a reconnect: it continues with its state). */
    fun beginSession(sessionId: String) {
        stoppedSessions.remove(sessionId)
        retention.confirmed(sessionId)
        sessions.getOrPut(sessionId) { Session() }
    }

    /**
     * The session ended (Extend said so, or the Carbon pressed Stop): its running and queued
     * commands answer `session_ended`, and its recordings, logs and shell processes end.
     * [lostReason] means the device gave up on the session itself (offline too long): should
     * Extend announce it again, record stop and logs stop explain that the capture was discarded.
     */
    fun endSession(sessionId: String, lostReason: String? = null) {
        stoppedSessions.add(sessionId)
        retention.forget(sessionId)
        val reason = CommandError(
            CommandFailure.SESSION_ENDED,
            "The session ended on the device (the Carbon pressed Stop, or Extend ended it) before this command finished, so it was stopped. Start a new session to continue.",
        )
        jobs.cancelSession(sessionId, reason)
        sessions.remove(sessionId)
        extend.scope.launch { extend.adbExecutor.endSession(sessionId, lostReason) }
    }

    /** Extend cancelled one command (it gave up waiting for it). */
    fun cancel(commandId: String) {
        jobs.cancel(commandId, CommandError(CommandFailure.CANCELLED, "Extend cancelled this command."))
    }

    /**
     * The device socket dropped. Running commands can't answer any more, so they stop. Sessions
     * keep their state: Extend keeps them alive while the device is briefly offline and announces
     * them again on reconnect ([SessionRetention]).
     */
    fun connectionLost() {
        jobs.cancelAll()
        retention.disconnected(sessions.keys + extend.adbExecutor.sessionIds(), SystemClock.elapsedRealtime())
        scheduleRetention()
    }

    /** After a reconnect, Extend's current session is [active]: sessions it no longer announces have ended. */
    fun reconcile(active: String?) {
        retention.reconcile(active).forEach(::endSession)
    }

    @Synchronized private fun scheduleRetention() {
        retentionJob?.cancel()
        retentionJob = extend.scope.launch {
            while (true) {
                val next = retention.nextDeadline() ?: break
                delay((next - SystemClock.elapsedRealtime()).coerceAtLeast(0))
                retention.expired(SystemClock.elapsedRealtime()).forEach { endSession(it, OFFLINE_TOO_LONG) }
            }
        }
    }

    /** The device was unpaired: everything stops and every session's state goes. */
    fun forgetAll() {
        jobs.cancelAll()
        retentionJob?.cancel()
        for (id in sessions.keys + retention.pending()) retention.forget(id)
        sessions.clear()
        extend.scope.launch { extend.adbExecutor.endAll() }
    }

    /** The Carbon disconnected Android debugging: commands that need it answer `device_not_ready`. */
    fun cancelAdbCommands() {
        val reason = CommandError(
            CommandFailure.NOT_READY,
            "The Carbon disconnected Android debugging on the device while this command ran, so it was stopped. Ask the Carbon to connect Android debugging in the Extend app, then try again.",
        )
        jobs.cancelCommands(ADB_COMMANDS, reason)
    }

    fun submit(frame: ServiceFrame.Command, send: (DeviceFrame.Result) -> Unit) =
        jobs.submit(frame, { lock.withLock { run(frame) } }, send)

    /** Runs one command frame to its `result`. */
    suspend fun run(frame: ServiceFrame.Command): DeviceFrame.Result {
        if (frame.sessionId in stoppedSessions) return DeviceFrame.Result(
            frame.id, false, JsonNull, "This session has ended.", CommandError(CommandFailure.SESSION_ENDED, "This session has ended. Start a new session to continue."), emptyList(),
        )
        val session = sessions.getOrPut(frame.sessionId) { Session() }
        val budget = (frame.timeoutMs - 750).coerceAtLeast(1_000)
        val run = Run(frame, session, SystemClock.elapsedRealtime() + budget)
        return try {
            if (frame.target != null) {
                throw CommandFailure.unsupported("This Android device doesn't carry other devices; the command was addressed to ${frame.target}.")
            }
            val args = materializeAttachments(run)
            val outcome = withTimeoutOrNull(budget) { runStep(frame.command, args, run) }
                ?: throw CommandFailure(CommandFailure.TIMEOUT, "${frame.command} didn't finish within ${budget} ms (timeout_ms ${frame.timeoutMs}).")
            DeviceFrame.Result(frame.id, true, outcome.output, outcome.text, null, run.files)
        } catch (f: CommandFailure) {
            DeviceFrame.Result(frame.id, false, f.output ?: JsonNull, f.text ?: f.message, CommandError(f.code, f.message ?: f.code, f.details), run.files)
        } catch (c: CancellationException) {
            if (kotlin.coroutines.coroutineContext[Job]?.isActive != true) throw c
            // This command was not cancelled: a time limit inside it escaped as a cancellation.
            // Answer it as a failure instead of leaving the Silicon waiting for its whole timeout.
            Extend.log("command ${frame.command}: an inner time limit escaped", c)
            val message = "${frame.command} hit a time limit inside the device and was stopped (${c.message ?: "timed out"}). Try again; if it keeps happening, reconnect Android debugging in the Extend app."
            DeviceFrame.Result(frame.id, false, JsonNull, message, CommandError(CommandFailure.ACTION_FAILED, message), run.files)
        } catch (e: Exception) {
            Extend.log("command ${frame.command} crashed", e)
            DeviceFrame.Result(
                frame.id, false, JsonNull, e.toString(),
                CommandError("internal_error", "The Android app hit an unexpected error running ${frame.command}: $e"),
                run.files,
            )
        } finally {
            run.scratch?.deleteRecursively()
        }
    }

    /**
     * Writes each attachment into this command's scratch directory and replaces every
     * `attachment:<name>` argument (also `--flag=attachment:<name>`) with the file's local path
     * (docs/device-protocol.md, "Attachments").
     */
    private fun materializeAttachments(run: Run): List<String> {
        val atts = run.frame.attachments
        if (atts.isEmpty()) return run.frame.args
        val dir = File(context.cacheDir, "commands/" + run.frame.id.replace(Regex("[^A-Za-z0-9-]"), "_")).apply { mkdirs() }
        run.scratch = dir
        for (att in atts) {
            val safe = att.name.substringAfterLast('/').replace(Regex("[^A-Za-z0-9._-]"), "_").ifEmpty { "attachment" }
            val file = File(dir, safe)
            val bytes = try {
                Base64.decode(att.contentBase64, Base64.DEFAULT)
            } catch (e: IllegalArgumentException) {
                throw CommandFailure.invalid("Attachment ${att.name} isn't valid base64: ${e.message}")
            }
            file.writeBytes(bytes)
            run.localFiles[att.name] = file
        }
        fun sub(token: String): String {
            val idx = token.indexOf("attachment:")
            if (idx != 0 && !(idx > 0 && token[idx - 1] == '=')) return token
            val name = token.substring(idx + "attachment:".length)
            val file = run.localFiles[name] ?: throw CommandFailure.invalid(
                "$token names an attachment the command doesn't carry (it has: ${run.localFiles.keys.joinToString(", ").ifEmpty { "none" }}).",
            )
            return token.substring(0, idx) + file.absolutePath
        }
        return run.frame.args.map(::sub)
    }

    private suspend fun runStep(command: String, args: List<String>, run: Run): Outcome {
        val cmd = CommandParser.parse(command, args)
        checkCapability(command)
        return exec(cmd, run)
    }

    private fun checkCapability(command: String) {
        val needed = Capabilities.COMMAND_CAPABILITIES[command] ?: return
        val report = SetupReport.compute(context, extend.config)
        if (needed.any { it in report.capabilities }) return
        val reason = report.missing.firstOrNull { it.capability in needed }?.reason
            ?: "${if (extend.isTv) "An Android TV" else "An Android phone or tablet"} doesn't have ${needed.joinToString(" or ")}."
        throw CommandFailure.unsupported("$command needs ${needed.joinToString(" or ")}. $reason")
    }

    private fun a11y(): ExtendAccessibilityService = ExtendAccessibilityService.instance
        ?: throw CommandFailure(
            CommandFailure.NOT_READY,
            "Silicon Extend's accessibility service isn't running. The Carbon turns it on in Settings › Accessibility › ${com.teamofsilicons.extend.config.DeviceInfo.systemLabel(extend.context)}.",
        )

    private fun capture(): Capture = a11y().capture()

    // ───────────── dispatch ─────────────

    private suspend fun exec(cmd: Cmd, run: Run): Outcome = when (cmd) {
        is Cmd.Debug -> debugging(cmd, run)
        is Cmd.Snapshot -> snapshot(cmd, run)
        is Cmd.Get -> get(cmd, run)
        is Cmd.Find -> find(cmd, run)
        is Cmd.Is -> assertIs(cmd)
        is Cmd.Wait -> wait(cmd, run)
        is Cmd.Screenshot -> screenshot(cmd, run)
        is Cmd.Click -> click(cmd, run)
        is Cmd.LongPress -> longPress(cmd, run)
        is Cmd.Fill -> fill(cmd.target, cmd.text, cmd.delayMs, run)
        is Cmd.Type -> type(cmd.text, cmd.delayMs)
        is Cmd.Focus -> focus(cmd.target, run)
        is Cmd.Scroll -> scroll(cmd)
        is Cmd.Swipe -> swipe(cmd)
        is Cmd.Gesture -> gesture(cmd, run)
        Cmd.Back -> global(AccessibilityService.GLOBAL_ACTION_BACK, "back")
        Cmd.Home -> global(AccessibilityService.GLOBAL_ACTION_HOME, "home")
        Cmd.AppSwitcher -> global(AccessibilityService.GLOBAL_ACTION_RECENTS, "app-switcher")
        is Cmd.TvRemote -> tvRemote(cmd)
        is Cmd.Keyboard -> keyboard(cmd)
        is Cmd.Clipboard -> clipboard(cmd)
        is Cmd.Open -> open(cmd)
        is Cmd.Close -> close(cmd)
        is Cmd.Apps -> apps(cmd)
        Cmd.AppState -> appState()
        is Cmd.Alert -> alert(cmd, run)
        Cmd.Notifications -> notifications()
        is Cmd.Display -> display(cmd, run)
        is Cmd.Batch -> steps("batch", Scripts.parseBatch(cmd.stepsJson), run)
        is Cmd.Replay -> replay(cmd, run)
        is Cmd.TestSuite -> testSuite(cmd, run)
    }

    private suspend fun debugging(cmd: Cmd.Debug, run: Run): Outcome {
        val install = cmd.command as? com.teamofsilicons.extend.adb.AdbCommand.Install
        if (install != null) {
            val file = run.localFiles.values.firstOrNull { it.absolutePath == install.path }
                ?: throw CommandFailure.invalid("Send the APK as an attachment.")
            val info = context.packageManager.getPackageArchiveInfo(file.absolutePath, 0)
                ?: throw CommandFailure.invalid("The attachment is not a valid APK.")
            if (info.packageName != install.app) throw CommandFailure.invalid("APK package ${info.packageName} does not match ${install.app}.")
        }
        val sessionId = run.frame.sessionId
        val result = try {
            extend.adbExecutor.execute(sessionId, cmd.command, run.localFiles.values)
        } catch (e: com.teamofsilicons.extend.adb.AdbWire.LimitExceeded) {
            // Android debugging works; the output or file is just too large. The message says what to do.
            throw CommandFailure(CommandFailure.ACTION_FAILED, e.message ?: "The output is larger than Extend accepts.")
        } catch (e: java.io.IOException) {
            throw CommandFailure(
                CommandFailure.ACTION_FAILED,
                "Android debugging failed: ${e.message ?: e.javaClass.simpleName}. If Android debugging disconnected, ask the Carbon to reconnect it in the Extend app.",
            )
        }
        val uploaded = ArrayList<ProducedFile>()
        try {
            for ((i, artifact) in result.artifacts.withIndex()) {
                uploaded += uploadArtifact(run, artifact, result.artifacts.size - i - 1)
                if (artifact.retained) extend.adbExecutor.delivered(sessionId, artifact)
            }
        } finally {
            // Files that aren't kept for a retry go now, uploaded or not.
            result.artifacts.filterNot { it.retained }.forEach { it.file.parentFile?.deleteRecursively() }
        }
        val files = JsonArray(uploaded.map { JsonPrimitive(it.uploadId) })
        val output = result.output?.let { JsonObject(it + ("files" to files)) } ?: buildJsonObject {
            put("message", result.text)
            if (uploaded.isNotEmpty()) put("files", files)
        }
        val exit = result.exitCode
        if (exit != null && exit != 0) {
            val verb = (cmd.command as? com.teamofsilicons.extend.adb.AdbCommand.Raw)?.args?.firstOrNull() ?: run.frame.command
            throw CommandFailure(
                CommandFailure.COMMAND_FAILED,
                "adb $verb exited with code $exit.",
                buildJsonObject { put("exit_code", exit) },
                output,
                result.text.ifEmpty { "adb $verb exited with code $exit." },
            )
        }
        return Outcome(output, result.text)
    }

    /**
     * Uploads one ADB artifact within the command's deadline. [left] files come after it. A
     * retained file stays on the device when this fails, and the message says how to get it.
     */
    private suspend fun uploadArtifact(run: Run, artifact: com.teamofsilicons.extend.adb.AdbExecutor.Artifact, left: Int): ProducedFile {
        val file = artifact.file
        val size = file.length()
        val sizeText = if (size >= 1024 * 1024) "${(size + 1024 * 1024 - 1) / (1024 * 1024)} MiB" else "${(size + 1023) / 1024} KiB"
        val again = when {
            !artifact.retained -> "Run the command again (with a longer --timeout, up to 300000 ms, for a large file)."
            else -> {
                val stop = if (artifact.kind == "recording") "record stop" else "logs stop"
                "The file is kept on the device: run $stop again (with a longer --timeout, up to 300000 ms, for a large file) to send it" +
                    (if (left > 0) " and the $left file(s) after it." else ".")
            }
        }
        val id = run.frame.uploadIds.getOrNull(run.uploadCursor)
            ?: throw CommandFailure(CommandFailure.UPLOAD_FAILED, "The command came with ${run.frame.uploadIds.size} upload slots, all used, so ${file.name} wasn't sent. $again")
        run.uploadCursor++
        val cred = extend.secrets.readCredential()
            ?: throw CommandFailure(CommandFailure.NOT_READY, "This device is no longer paired, so ${file.name} can't be uploaded. Pair it again in the Extend app.")
        val budget = run.remainingMs() - 250
        val sent = try {
            if (budget <= 0) null else withTimeoutOrNull(budget) { extend.api.uploadFile(cred, id, file, artifact.contentType) }
        } catch (e: ApiException) {
            throw CommandFailure(CommandFailure.UPLOAD_FAILED, "Uploading ${file.name} ($sizeText) failed: HTTP ${e.status} ${e.message?.trimEnd('.')}. $again")
        } catch (e: java.io.IOException) {
            throw CommandFailure(CommandFailure.UPLOAD_FAILED, "Uploading ${file.name} ($sizeText) failed: ${(e.message ?: e.javaClass.simpleName).trimEnd('.')}. $again")
        }
        if (sent == null) throw CommandFailure(
            CommandFailure.UPLOAD_FAILED,
            "Uploading ${file.name} ($sizeText) didn't finish within the command's time limit (timeout_ms ${run.frame.timeoutMs}). $again",
        )
        return ProducedFile(id, file.name, artifact.contentType, artifact.kind, size).also { run.files += it }
    }

    // ───────────── seeing the screen ─────────────

    private fun snapshot(cmd: Cmd.Snapshot, run: Run): Outcome {
        val cap = capture()
        val snap = SnapshotEngine.build(cap, cmd.options, run.session.last)
        val previousBaseline = run.session.baseline
        run.session.last = snap
        run.session.baseline = snap
        if (cmd.diff) {
            if (previousBaseline == null) {
                return Outcome(
                    buildJsonObject { put("baselineInitialized", true); put("nodes", snap.nodes.size) },
                    "Baseline initialized (${snap.nodes.size} nodes). Run diff snapshot again after a change.",
                )
            }
            val (text, json) = SnapshotEngine.diff(previousBaseline, snap)
            return Outcome(json, text)
        }
        return Outcome(snap.json(), snap.text())
    }

    private data class Resolved(val node: UiNode?, val x: Int, val y: Int, val ref: String?, val desc: String)

    private fun pointOf(n: UiNode, screen: Bounds): Pair<Int, Int> {
        val visible = if (n.bounds.intersects(screen)) n.bounds.intersect(screen) else n.bounds
        return visible.centerX to visible.centerY
    }

    private fun describe(n: UiNode): String {
        val label = SnapshotEngine.displayLabel(n, n.role)
        return if (label.isNotEmpty()) "${n.role} \"$label\"" else n.role
    }

    private fun resolve(t: Target, run: Run): Resolved {
        val screen = a11y().screenBounds()
        return when (t) {
            is Target.Point -> Resolved(null, t.x, t.y, null, "(${t.x}, ${t.y})")
            is Target.Ref -> {
                val snap = run.session.last ?: throw CommandFailure(
                    CommandFailure.STALE_REF,
                    "Session ${run.frame.sessionId} has no snapshot yet, so ${t.ref} doesn't name anything. Run snapshot first.",
                )
                val sn = snap.ref(t.ref) ?: throw CommandFailure(
                    CommandFailure.STALE_REF,
                    "${t.ref} isn't in the latest snapshot (it has ${if (snap.nodes.isEmpty()) "no refs" else "@e1–@e${snap.nodes.size}"}). Run snapshot again.",
                )
                val live = relocate(sn.node) ?: throw CommandFailure(
                    CommandFailure.STALE_REF,
                    "${t.ref} (${describe(sn.node)}) is no longer on screen. Run snapshot again.",
                )
                val (x, y) = pointOf(live, screen)
                Resolved(live, x, y, t.ref, "${t.ref} ${describe(live)}")
            }
            is Target.Sel -> {
                val cap = capture()
                val matches = Selectors.resolveAll(t.chain, cap.allNodes, cap.screen)
                val n = matches.firstOrNull() ?: throw CommandFailure(
                    CommandFailure.NOT_FOUND,
                    "No element on screen matches ${t.chain.raw}.",
                    buildJsonObject { put("selector", t.chain.raw) },
                )
                val (x, y) = pointOf(n, cap.screen)
                Resolved(n, x, y, refFor(n, run), describe(n))
            }
        }
    }

    /** The live version of a node from an earlier capture, or null when it's gone. */
    private fun relocate(node: UiNode): UiNode? {
        val info = node.handle as? AccessibilityNodeInfo
        if (info != null && runCatching { info.refresh() }.getOrDefault(false)) {
            val r = android.graphics.Rect()
            info.getBoundsInScreen(r)
            val b = Bounds(r.left, r.top, r.right, r.bottom)
            if (b == node.bounds) return node
        }
        // Find the same element again: same class, label and id, closest to where it was.
        val cap = capture()
        return cap.allNodes
            .filter { it.className == node.className && it.label == node.label && it.identifier == node.identifier && it.visibleToUser }
            .minByOrNull { abs(it.bounds.centerX - node.bounds.centerX) + abs(it.bounds.centerY - node.bounds.centerY) }
    }

    private fun refFor(n: UiNode, run: Run): String? =
        run.session.last?.nodes?.firstOrNull { it.node.handle != null && it.node.handle == n.handle }?.let { "@" + it.ref }

    private fun info(n: UiNode?): AccessibilityNodeInfo? = n?.handle as? AccessibilityNodeInfo

    private fun targetJson(r: Resolved, method: String) = buildJsonObject {
        r.ref?.let { put("ref", it) }
        put("x", r.x)
        put("y", r.y)
        put("method", method)
        r.node?.let { put("element", Snapshot.nodeJson(SnapNode(r.ref?.removePrefix("@") ?: "", it, 0, it.role, SnapshotEngine.displayLabel(it, it.role)), withChildren = false)) }
    }

    private fun get(cmd: Cmd.Get, run: Run): Outcome {
        val r = resolve(cmd.target, run)
        val n = r.node ?: throw CommandFailure.invalid("get ${cmd.what} needs an element (@ref or selector), not coordinates")
        return if (cmd.what == "text") {
            val text = Selectors.nodeText(n)
            Outcome(buildJsonObject { r.ref?.let { put("ref", it) }; put("text", text) }, text)
        } else {
            val attrs = Snapshot.nodeJson(SnapNode(r.ref?.removePrefix("@") ?: "", n, 0, n.role, SnapshotEngine.displayLabel(n, n.role)), withChildren = false)
            Outcome(attrs, attrs.entries.joinToString("\n") { (k, v) -> "$k: $v" })
        }
    }

    // ───────────── find ─────────────

    private fun scoreText(value: String?, q: String): Int {
        val n = Selectors.normalizeText(value)
        if (n.isEmpty()) return 0
        if (n == q) return 2
        if (n.contains(q)) return 1
        return 0
    }

    private fun findMatches(locator: String, query: String, cap: Capture): List<UiNode> {
        val q = Selectors.normalizeText(query)
        var best = 0
        val out = ArrayList<UiNode>()
        for (n in cap.allNodes) {
            if (!Selectors.isVisible(n, cap.screen)) continue
            val score = when (locator) {
                "role" -> max(scoreText(Roles.normalizeType(n.className), q), scoreText(n.role, q))
                "label" -> scoreText(n.label, q)
                "value" -> scoreText(n.value, q)
                "id" -> scoreText(n.identifier, q)
                else -> maxOf(scoreText(n.label, q), scoreText(n.value, q), scoreText(n.identifier, q))
            }
            if (score <= 0) continue
            if (score > best) {
                best = score
                out.clear()
            }
            if (score == best) out += n
        }
        return out
    }

    private suspend fun find(cmd: Cmd.Find, run: Run): Outcome {
        var cap = capture()
        var matches = findMatches(cmd.locator, cmd.query, cap)
        if (cmd.action == "wait") {
            val until = SystemClock.elapsedRealtime() + min(cmd.waitMs ?: 10_000, run.remainingMs() - 200)
            while (matches.isEmpty() && SystemClock.elapsedRealtime() < until) {
                delay(300)
                cap = capture()
                matches = findMatches(cmd.locator, cmd.query, cap)
            }
        }
        // Refs: a fresh default snapshot, so every match can be named.
        val snap = SnapshotEngine.build(cap, SnapshotOptions(), run.session.last)
        run.session.last = snap
        fun refOf(n: UiNode): SnapNode? {
            var cur: UiNode? = n
            while (cur != null) {
                snap.nodes.firstOrNull { it.node === cur }?.let { return it }
                cur = cur.parent
            }
            return null
        }
        fun matchJson(n: UiNode) = buildJsonObject {
            refOf(n)?.let { put("ref", "@" + it.ref) }
            put("role", n.role)
            put("label", SnapshotEngine.displayLabel(n, n.role))
            put("rect", Snapshot.rectJson(n.bounds))
        }
        val base = buildJsonObject {
            put("locator", cmd.locator)
            put("query", cmd.query)
            put("matches", matches.size)
        }
        when (cmd.action) {
            "exists" -> {
                if (matches.isEmpty()) throw CommandFailure(CommandFailure.ASSERTION_FAILED, "Nothing on screen matches ${cmd.locator} \"${cmd.query}\".", base)
                return Outcome(base, "Found ${matches.size} match${if (matches.size == 1) "" else "es"} for \"${cmd.query}\".")
            }
            "list" -> {
                val list = JsonArray(matches.map { matchJson(it) })
                val text = if (matches.isEmpty()) "No matches for \"${cmd.query}\"." else matches.joinToString("\n") { n ->
                    val sn = refOf(n)
                    "${sn?.let { "@" + it.ref + " " } ?: ""}[${n.role}] \"${SnapshotEngine.displayLabel(n, n.role)}\""
                }
                return Outcome(JsonObject(base + ("items" to list)), text)
            }
        }
        if (matches.isEmpty()) {
            throw CommandFailure(
                if (cmd.action == "wait") CommandFailure.WAIT_TIMEOUT else CommandFailure.NOT_FOUND,
                "Nothing on screen matches ${cmd.locator} \"${cmd.query}\".",
                JsonObject(base + ("reason" to JsonPrimitive("wait_target_absent"))),
            )
        }
        val chosen = when {
            cmd.pick == "first" -> matches.first()
            cmd.pick == "last" -> matches.last()
            matches.size == 1 -> matches.first()
            // Several matches that all act through the same clickable element aren't ambiguous.
            matches.map { it.nearestClickable() ?: it }.distinct().size == 1 -> matches.first()
            else -> throw CommandFailure(
                CommandFailure.AMBIGUOUS,
                "${matches.size} elements match \"${cmd.query}\"; pick one with --first/--last or a ref:\n" +
                    matches.take(10).joinToString("\n") { n -> "  ${refOf(n)?.let { "@" + it.ref } ?: "-"} [${n.role}] \"${SnapshotEngine.displayLabel(n, n.role)}\"" },
                JsonObject(base + ("candidates" to JsonArray(matches.take(10).map { matchJson(it) }))),
            )
        }
        val sn = refOf(chosen)
        val target: Target = if (sn != null) Target.Ref("@" + sn.ref) else Target.Point(pointOf(chosen, cap.screen).first, pointOf(chosen, cap.screen).second)
        return when (cmd.action) {
            "click" -> click(Cmd.Click(target), run)
            "focus" -> focus(target, run)
            "fill" -> fill(target, cmd.value.orEmpty(), null, run)
            "type" -> {
                focus(target, run)
                type(cmd.value.orEmpty(), null)
            }
            "wait" -> Outcome(JsonObject(base + matchJson(chosen)), "Found \"${cmd.query}\".")
            "get_text" -> Selectors.nodeText(chosen).let { Outcome(buildJsonObject { sn?.let { s -> put("ref", "@" + s.ref) }; put("text", it) }, it) }
            "get_attrs" -> Snapshot.nodeJson(SnapNode(sn?.ref ?: "", chosen, 0, chosen.role, SnapshotEngine.displayLabel(chosen, chosen.role)), false)
                .let { Outcome(it, it.entries.joinToString("\n") { (k, v) -> "$k: $v" }) }
            else -> throw CommandFailure.invalid("Unknown find action ${cmd.action}")
        }
    }

    // ───────────── assertions and waiting ─────────────

    private fun assertIs(cmd: Cmd.Is): Outcome {
        val cap = capture()
        val matches = Selectors.resolveAll(cmd.chain, cap.allNodes, cap.screen)
        val visible = matches.filter { Selectors.isVisible(it, cap.screen) }
        val first = visible.firstOrNull() ?: matches.firstOrNull()
        val (pass, why) = when (cmd.predicate) {
            "exists" -> (matches.isNotEmpty()) to "${matches.size} match(es)"
            "absent" -> (matches.isEmpty()) to "${matches.size} match(es)"
            "visible" -> visible.isNotEmpty() to "${visible.size} visible of ${matches.size} match(es)"
            "hidden" -> visible.isEmpty() to "${visible.size} visible of ${matches.size} match(es)"
            "editable" -> (first?.isEditable == true) to (first?.let { "first match is ${describe(it)}, editable=${it.isEditable}" } ?: "no match")
            "selected" -> (first?.selected == true) to (first?.let { "first match is ${describe(it)}, selected=${it.selected}" } ?: "no match")
            "focused" -> (first?.focused == true) to (first?.let { "first match is ${describe(it)}, focused=${it.focused}" } ?: "no match")
            "text" -> {
                val actual = first?.let { Selectors.nodeText(it) }
                (actual != null && Selectors.normalizeText(actual) == Selectors.normalizeText(cmd.expected)) to
                    (if (actual == null) "no match" else "text is \"$actual\"")
            }
            else -> false to "unknown predicate"
        }
        val out = buildJsonObject {
            put("pass", pass)
            put("predicate", cmd.predicate)
            put("selector", cmd.chain.raw)
            put("matches", matches.size)
            put("reason", why)
        }
        if (!pass) throw CommandFailure(CommandFailure.ASSERTION_FAILED, "is ${cmd.predicate} ${cmd.chain.raw} failed: $why.", out)
        return Outcome(out, "pass: is ${cmd.predicate} ${cmd.chain.raw} ($why)")
    }

    private suspend fun wait(cmd: Cmd.Wait, run: Run): Outcome {
        val started = SystemClock.elapsedRealtime()
        fun budget(requested: Long?) = min(requested ?: 10_000, run.remainingMs() - 250).coerceAtLeast(0)
        suspend fun poll(timeout: Long, test: () -> Boolean): Boolean {
            val until = started + timeout
            while (true) {
                if (runCatching(test).getOrDefault(false)) return true
                if (SystemClock.elapsedRealtime() >= until) return false
                delay(300)
            }
        }
        fun waited() = SystemClock.elapsedRealtime() - started
        return when (cmd) {
            is Cmd.Wait.Duration -> {
                if (cmd.ms > run.remainingMs()) throw CommandFailure.invalid("wait ${cmd.ms} is longer than the command's timeout (${run.frame.timeoutMs} ms); raise --timeout")
                delay(cmd.ms)
                Outcome(buildJsonObject { put("waitedMs", cmd.ms) }, "Waited ${cmd.ms} ms")
            }
            is Cmd.Wait.ForText -> {
                val q = Selectors.normalizeText(cmd.text)
                val ok = poll(budget(cmd.timeoutMs)) {
                    capture().allNodes.any { n -> n.visibleToUser && listOf(n.label, n.value).any { Selectors.normalizeText(it).contains(q) } }
                }
                waitResult(ok, "text \"${cmd.text}\"", waited(), "wait_target_absent")
            }
            is Cmd.Wait.ForTarget -> {
                val test: () -> Boolean = when (val t = cmd.target) {
                    is Target.Ref -> {
                        val sn = run.session.last?.ref(t.ref) ?: throw CommandFailure(CommandFailure.STALE_REF, "wait ${t.ref} needs a snapshot in this session that has ${t.ref}.")
                        val text = Selectors.normalizeText(sn.label.ifEmpty { sn.node.label ?: "" })
                        if (text.isEmpty()) throw CommandFailure.invalid("${t.ref} has no text to wait for")
                        ({ capture().allNodes.any { n -> n.visibleToUser && Selectors.normalizeText(n.label).contains(text) } })
                    }
                    is Target.Sel -> ({ capture().let { c -> Selectors.resolveAll(t.chain, c.allNodes, c.screen).any { Selectors.isVisible(it, c.screen) } } })
                    is Target.Point -> throw CommandFailure.invalid("wait takes milliseconds, text, a ref or a selector, not coordinates")
                }
                waitResult(poll(budget(cmd.timeoutMs), test), "the target", waited(), "wait_target_absent")
            }
            is Cmd.Wait.Absent -> {
                val ok = poll(budget(cmd.timeoutMs)) {
                    capture().let { c -> Selectors.resolveAll(cmd.chain, c.allNodes, c.screen).isEmpty() }
                }
                waitResult(ok, "${cmd.chain.raw} to go away", waited(), "wait_target_present")
            }
        }
    }

    private fun waitResult(ok: Boolean, what: String, waitedMs: Long, reason: String): Outcome {
        val out = buildJsonObject { put("met", ok); put("waitedMs", waitedMs) }
        if (!ok) {
            throw CommandFailure(
                CommandFailure.WAIT_TIMEOUT,
                "Waited $waitedMs ms for $what; it didn't happen.",
                buildJsonObject { put("reason", reason); put("waitedMs", waitedMs) },
            )
        }
        return Outcome(out, "Met after $waitedMs ms: $what")
    }

    // ───────────── acting ─────────────

    private suspend fun click(cmd: Cmd.Click, run: Run): Outcome {
        val r = resolve(cmd.target, run)
        val a = a11y()
        if (r.node != null && cmd.count == 1 && cmd.holdMs == null) {
            val target = r.node.nearestClickable()
            if (target != null && info(target)?.performAction(AccessibilityNodeInfo.ACTION_CLICK) == true) {
                return Outcome(targetJson(r, "accessibility_click"), "Tapped ${r.desc}")
            }
        }
        repeat(cmd.count) { i ->
            if (!a.tap(r.x.toFloat(), r.y.toFloat(), cmd.holdMs ?: 50)) {
                throw CommandFailure(CommandFailure.ACTION_FAILED, "Android cancelled the tap at (${r.x}, ${r.y}), usually because the screen changed or another gesture started.")
            }
            if (i < cmd.count - 1) delay(cmd.intervalMs)
        }
        val times = if (cmd.count > 1) " ${cmd.count} times" else ""
        return Outcome(targetJson(r, "tap"), "Tapped ${r.desc}$times")
    }

    private suspend fun longPress(cmd: Cmd.LongPress, run: Run): Outcome {
        val r = resolve(cmd.target, run)
        if (!a11y().tap(r.x.toFloat(), r.y.toFloat(), cmd.ms)) {
            // A gesture can't reach every node (e.g. on a TV); fall back to the accessibility action.
            var n = r.node
            while (n != null && !n.longClickable) n = n.parent
            if (n == null || info(n)?.performAction(AccessibilityNodeInfo.ACTION_LONG_CLICK) != true) {
                throw CommandFailure(CommandFailure.ACTION_FAILED, "Android cancelled the long press at (${r.x}, ${r.y}).")
            }
            return Outcome(targetJson(r, "accessibility_long_click"), "Long-pressed ${r.desc}")
        }
        return Outcome(targetJson(r, "touch_hold"), "Long-pressed ${r.desc} for ${cmd.ms} ms")
    }

    private fun editableIn(n: UiNode): UiNode? =
        n.walk().firstOrNull { it.isEditable } ?: generateSequence(n.parent) { it.parent }.firstOrNull { it.isEditable }

    private fun setText(info: AccessibilityNodeInfo, text: String): Boolean {
        val args = Bundle().apply { putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, text) }
        val ok = info.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, args)
        if (ok) {
            val sel = Bundle().apply {
                putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_START_INT, text.length)
                putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_END_INT, text.length)
            }
            info.performAction(AccessibilityNodeInfo.ACTION_SET_SELECTION, sel)
        }
        return ok
    }

    private suspend fun typeInto(info: AccessibilityNodeInfo, base: String, text: String, delayMs: Long?) {
        if (delayMs == null || delayMs <= 0) {
            if (!setText(info, base + text)) throw CommandFailure(CommandFailure.ACTION_FAILED, "The field refused the text (ACTION_SET_TEXT failed); it may be read-only or not a standard text field.")
            return
        }
        // Paced: grow the text one character at a time so debounced fields see every step.
        var i = 0
        while (i < text.length) {
            val next = i + Character.charCount(text.codePointAt(i))
            if (!setText(info, base + text.substring(0, next))) throw CommandFailure(CommandFailure.ACTION_FAILED, "The field stopped accepting text after $i characters.")
            i = next
            delay(delayMs)
        }
    }

    private suspend fun fill(target: Target, text: String, delayMs: Long?, run: Run): Outcome {
        val r = resolve(target, run)
        val a = a11y()
        var field = r.node?.let { editableIn(it) }
        if (field == null) {
            // Not a text field itself: tap it and use whatever took input focus.
            if (r.node?.nearestClickable()?.let { info(it)?.performAction(AccessibilityNodeInfo.ACTION_CLICK) } != true) {
                a.tap(r.x.toFloat(), r.y.toFloat())
            }
            delay(350)
        }
        val info = info(field) ?: a.focusedInput() ?: throw CommandFailure(
            CommandFailure.NOT_FOUND,
            "${r.desc} isn't a text field and tapping it didn't focus one.",
        )
        if (!info.isFocused) {
            info.performAction(AccessibilityNodeInfo.ACTION_FOCUS)
            if (!info.isFocused) info.performAction(AccessibilityNodeInfo.ACTION_CLICK)
        }
        typeInto(info, "", text, delayMs)
        delay(120)
        info.refresh()
        val password = info.isPassword
        val now = if (info.isShowingHintText) "" else info.text?.toString().orEmpty()
        if (!password && now != text) {
            throw CommandFailure(
                CommandFailure.ACTION_FAILED,
                "After fill the field holds ${now.length} characters, not the ${text.length} typed; the app may format or limit the input. Check it with get text.",
                buildJsonObject { put("expectedLength", text.length); put("actualLength", now.length) },
            )
        }
        val out = buildJsonObject {
            r.ref?.let { put("ref", it) }
            put("chars", text.length)
            put("verified", !password)
        }
        return Outcome(out, "Filled ${r.desc} [redacted ${text.length} chars]")
    }

    private suspend fun type(text: String, delayMs: Long?): Outcome {
        val a = a11y()
        val info = a.focusedInput() ?: throw CommandFailure(
            "text_input_not_focused",
            "No text field has input focus. Focus one first (press @ref or focus @ref), or use fill @ref \"text\".",
        )
        val existing = if (info.isShowingHintText) "" else info.text?.toString().orEmpty()
        if (info.isPassword && existing.isNotEmpty()) {
            throw CommandFailure(
                CommandFailure.ACTION_FAILED,
                "The focused field is a password field that already has text, which Android hides from accessibility, so type can't append to it. Use fill to replace it.",
            )
        }
        typeInto(info, existing, text, delayMs)
        return Outcome(buildJsonObject { put("chars", text.length) }, "Typed [redacted ${text.length} chars]")
    }

    private suspend fun focus(target: Target, run: Run): Outcome {
        val r = resolve(target, run)
        val n = r.node
        if (n == null) {
            a11y().tap(r.x.toFloat(), r.y.toFloat())
            return Outcome(targetJson(r, "tap"), "Tapped ${r.desc} to focus it")
        }
        val candidate = generateSequence(n) { it.parent }.firstOrNull { it.focusable || it.isEditable } ?: n
        val i = info(candidate)
        var ok = i?.performAction(AccessibilityNodeInfo.ACTION_FOCUS) == true
        if (!ok && candidate.isEditable) ok = i?.performAction(AccessibilityNodeInfo.ACTION_CLICK) == true
        if (!ok) {
            a11y().tap(r.x.toFloat(), r.y.toFloat())
            return Outcome(targetJson(r, "tap"), "Tapped ${r.desc} to focus it")
        }
        return Outcome(targetJson(r, "accessibility_focus"), "Focused ${r.desc}")
    }

    private fun pickScroll(cap: Capture, vertical: Boolean): UiNode? {
        val apps = cap.roots.filter { it.windowType == "application" }.ifEmpty { cap.roots }
        return apps.lastOrNull { root -> root.walk().any { it.scrollable && it.visibleToUser } }
            ?.walk()?.filter { it.scrollable && it.visibleToUser && !it.bounds.isEmpty }
            ?.filter { if (vertical) it.bounds.height >= it.bounds.width / 3 else true }
            ?.maxByOrNull { it.bounds.width.toLong() * it.bounds.height }
    }

    private fun signature(cap: Capture, region: Bounds): List<String> =
        cap.allNodes.filter { it.visibleToUser && it.bounds.intersects(region) && !it.label.isNullOrBlank() }
            .map { "${it.label}@${it.bounds.top},${it.bounds.left}" }

    private suspend fun scroll(cmd: Cmd.Scroll): Outcome {
        val a = a11y()
        val cap = a.capture()
        val vertical = cmd.direction in setOf("up", "down", "top", "bottom")
        val container = pickScroll(cap, vertical)
        var region = (container?.bounds ?: cap.screen).intersect(cap.screen)
        val ime = a.imeBounds()
        var keyboardAvoided = false
        if (vertical && ime != null && ime.top < region.bottom) {
            region = region.copy(bottom = max(region.top, ime.top))
            keyboardAvoided = true
        }
        if (region.height < 120 || region.width < 60) {
            throw CommandFailure(
                CommandFailure.ACTION_FAILED,
                "There's no room to scroll: the keyboard covers the list. Run keyboard dismiss first.",
                buildJsonObject { put("reason", "scroll_keyboard_occludes_surface") },
            )
        }
        if (cmd.direction == "top" || cmd.direction == "bottom") {
            val action = if (cmd.direction == "top") AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD else AccessibilityNodeInfo.ACTION_SCROLL_FORWARD
            var steps = 0
            val i = info(container)
            if (i != null) {
                // Lists that animate their scroll refuse the next step until the animation ends,
                // so a refusal only counts once it repeats after a pause.
                var refusals = 0
                while (steps < 40 && refusals < 2) {
                    if (i.performAction(action)) {
                        steps++
                        refusals = 0
                        delay(120)
                    } else {
                        refusals++
                        delay(400)
                    }
                }
                delay(300)
            } else {
                repeat(6) { swipeFor(if (cmd.direction == "top") "up" else "down", region, (region.height * 0.8f).toInt(), 150); delay(150) }
                steps = 6
            }
            return Outcome(buildJsonObject { put("direction", cmd.direction); put("steps", steps) }, "Scrolled to the ${cmd.direction} ($steps steps)")
        }
        val axis = if (vertical) region.height else region.width
        val distance = min((cmd.pixels ?: ((cmd.fraction ?: 0.5f) * axis).roundToInt()), (axis * 0.8f).roundToInt())
        val before = signature(cap, region)
        swipeFor(cmd.direction, region, distance, 450)
        delay(400)
        val after = runCatching { signature(a.capture(), region) }.getOrNull()
        val movement = when {
            after == null -> "unobserved"
            after != before -> "moved"
            else -> "unchanged"
        }
        val out = buildJsonObject {
            put("direction", cmd.direction)
            put("pixels", distance)
            put("referenceHeight", region.height)
            put("movement", movement)
            if (keyboardAvoided) {
                put("keyboardAvoided", true)
                put("keyboardMinY", region.bottom)
            }
        }
        return Outcome(out, "Scrolled ${cmd.direction} $distance px ($movement)")
    }

    /** A swipe that scrolls content in [direction] (down = reveal what's below: the finger moves up). */
    private suspend fun swipeFor(direction: String, region: Bounds, distance: Int, durationMs: Long) {
        val cx = region.centerX.toFloat()
        val cy = region.centerY.toFloat()
        val h = distance / 2f
        val (from, to) = when (direction) {
            "down" -> (cx to cy + h) to (cx to cy - h)
            "up" -> (cx to cy - h) to (cx to cy + h)
            "right" -> (cx + h to cy) to (cx - h to cy)
            else -> (cx - h to cy) to (cx + h to cy)
        }
        if (!a11y().gesture(listOf(ExtendAccessibilityService.Stroke(listOf(from, to), 0, durationMs)))) {
            throw CommandFailure(CommandFailure.ACTION_FAILED, "Android cancelled the scroll gesture.")
        }
    }

    private suspend fun swipe(cmd: Cmd.Swipe): Outcome {
        val a = a11y()
        repeat(cmd.count) { i ->
            val forward = !cmd.pingPong || i % 2 == 0
            val pts = if (forward) listOf(cmd.x1.toFloat() to cmd.y1.toFloat(), cmd.x2.toFloat() to cmd.y2.toFloat())
            else listOf(cmd.x2.toFloat() to cmd.y2.toFloat(), cmd.x1.toFloat() to cmd.y1.toFloat())
            if (!a.gesture(listOf(ExtendAccessibilityService.Stroke(pts, 0, cmd.durationMs)))) {
                throw CommandFailure(CommandFailure.ACTION_FAILED, "Android cancelled swipe ${i + 1} of ${cmd.count}.")
            }
            if (i < cmd.count - 1 && cmd.pauseMs > 0) delay(cmd.pauseMs)
        }
        return Outcome(
            buildJsonObject { put("from", "${cmd.x1},${cmd.y1}"); put("to", "${cmd.x2},${cmd.y2}"); put("count", cmd.count) },
            "Swiped from (${cmd.x1}, ${cmd.y1}) to (${cmd.x2}, ${cmd.y2})${if (cmd.count > 1) " ${cmd.count} times" else ""}",
        )
    }

    private suspend fun gesture(cmd: Cmd.Gesture, run: Run): Outcome {
        val a = a11y()
        val screen = a.screenBounds()
        fun check(ok: Boolean, what: String) {
            if (!ok) throw CommandFailure(CommandFailure.ACTION_FAILED, "Android cancelled the $what gesture.")
        }
        return when (cmd) {
            is Cmd.Gesture.Pan -> {
                val strokes = (0 until cmd.pointers).map { p ->
                    val off = p * 120f
                    ExtendAccessibilityService.Stroke(
                        listOf(cmd.x + off to cmd.y.toFloat(), cmd.x + off + cmd.dx to (cmd.y + cmd.dy).toFloat()), 0, cmd.durationMs,
                    )
                }
                check(a.gesture(strokes), "pan")
                Outcome(buildJsonObject { put("dx", cmd.dx); put("dy", cmd.dy); put("pointers", cmd.pointers) }, "Panned by (${cmd.dx}, ${cmd.dy})")
            }
            is Cmd.Gesture.Fling -> {
                val d = (cmd.distance ?: 600).toFloat()
                val (dx, dy) = when (cmd.direction) {
                    Direction.UP -> 0f to -d
                    Direction.DOWN -> 0f to d
                    Direction.LEFT -> -d to 0f
                    Direction.RIGHT -> d to 0f
                }
                check(a.gesture(listOf(ExtendAccessibilityService.Stroke(listOf(cmd.x.toFloat() to cmd.y.toFloat(), cmd.x + dx to cmd.y + dy), 0, 90))), "fling")
                Outcome(buildJsonObject { put("direction", cmd.direction.name.lowercase()) }, "Flung ${cmd.direction.name.lowercase()}")
            }
            is Cmd.Gesture.Pinch -> {
                val cx = (cmd.x ?: screen.centerX).toFloat()
                val cy = (cmd.y ?: screen.centerY).toFloat()
                val start = min(screen.width, screen.height) * 0.12f
                val end = (start * cmd.scale).coerceIn(10f, min(screen.width, screen.height) * 0.48f)
                check(
                    a.gesture(
                        listOf(
                            ExtendAccessibilityService.Stroke(listOf(cx - start to cy, cx - end to cy), 0, 450),
                            ExtendAccessibilityService.Stroke(listOf(cx + start to cy, cx + end to cy), 0, 450),
                        ),
                    ),
                    "pinch",
                )
                Outcome(buildJsonObject { put("scale", cmd.scale) }, "Pinched to ${cmd.scale}x")
            }
            is Cmd.Gesture.Rotate -> {
                val cx = (cmd.x ?: screen.centerX).toFloat()
                val cy = (cmd.y ?: screen.centerY).toFloat()
                val radius = min(screen.width, screen.height) * 0.15f
                val steps = 12
                fun arc(startAngle: Double) = (0..steps).map { s ->
                    val ang = startAngle + Math.toRadians(cmd.degrees.toDouble()) * s / steps
                    (cx + radius * cos(ang)).toFloat() to (cy + radius * sin(ang)).toFloat()
                }
                check(a.gesture(listOf(ExtendAccessibilityService.Stroke(arc(0.0), 0, 600), ExtendAccessibilityService.Stroke(arc(Math.PI), 0, 600))), "rotate")
                Outcome(buildJsonObject { put("degrees", cmd.degrees) }, "Rotated ${cmd.degrees}°")
            }
            is Cmd.Gesture.Drag -> {
                val from = resolve(cmd.from, run)
                val to = resolve(cmd.to, run)
                val p0 = from.x.toFloat() to from.y.toFloat()
                val p1 = to.x.toFloat() to to.y.toFloat()
                val phases = buildList {
                    add(ExtendAccessibilityService.Stroke(listOf(p0, p0), 0, cmd.holdMs.coerceAtLeast(1)))
                    add(ExtendAccessibilityService.Stroke(listOf(p0, p1), 0, cmd.moveMs.coerceAtLeast(1)))
                    if (cmd.dropHoldMs > 0) add(ExtendAccessibilityService.Stroke(listOf(p1, p1), 0, cmd.dropHoldMs))
                }
                check(a.continuousStroke(phases), "drag")
                Outcome(buildJsonObject { put("from", from.desc); put("to", to.desc) }, "Dragged ${from.desc} to ${to.desc}")
            }
        }
    }

    private suspend fun global(action: Int, name: String): Outcome {
        val a = a11y()
        if (!a.global(action)) throw CommandFailure(CommandFailure.ACTION_FAILED, "Android refused the $name action.")
        // Let the transition finish, so the next command (e.g. open right after home) isn't
        // overtaken by it: for home, until the launcher is in front.
        if (action == AccessibilityService.GLOBAL_ACTION_HOME) {
            val home = homePackage()
            val until = SystemClock.elapsedRealtime() + 2_500
            while (home != null && a.currentForeground() != home && SystemClock.elapsedRealtime() < until) delay(100)
        }
        delay(300)
        return Outcome(buildJsonObject { put("action", name) }, "Pressed $name")
    }

    private fun homePackage(): String? = runCatching {
        context.packageManager.resolveActivity(Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME), PackageManager.MATCH_DEFAULT_ONLY)
            ?.activityInfo?.packageName
    }.getOrNull()

    private suspend fun tvRemote(cmd: Cmd.TvRemote): Outcome {
        val b = cmd.button
        if (b == "power") throw CommandFailure.unsupported(TvRemoteKeys.POWER_REFUSAL)
        // With Android debugging connected every button is a real key press, so Menu works and
        // so do TVs older than Android 13. Accessibility is the fallback.
        var adbProblem: String? = null
        if (extend.adb.connected) {
            val command = TvRemoteKeys.command(b, cmd.longPress, cmd.durationMs, android.os.Build.VERSION.SDK_INT)
            try {
                val r = extend.adb.shell(command, check = false)
                if (r.exitCode != 0) throw CommandFailure(
                    CommandFailure.ACTION_FAILED,
                    "Android's input command didn't press $b (exit ${r.exitCode}: ${r.text.trim().take(500).ifEmpty { "no output" }}). Try again; if it keeps failing, reconnect Android debugging in the Extend app.",
                )
                return Outcome(
                    buildJsonObject {
                        put("button", b); put("longpress", cmd.longPress); put("method", "adb_keyevent")
                        put("keycode", TvRemoteKeys.KEYCODES.getValue(b))
                    },
                    "${if (cmd.longPress) "Long-pressed" else "Pressed"} $b",
                )
            } catch (e: java.io.IOException) {
                Extend.log("tv-remote $b through Android debugging failed; trying accessibility", e)
                adbProblem = e.message ?: e.javaClass.simpleName
            }
        }
        val a = ExtendAccessibilityService.instance ?: throw CommandFailure(
            CommandFailure.NOT_READY,
            (adbProblem?.let { "Android debugging failed while pressing $b ($it), and " } ?: "") +
                "Silicon Extend's accessibility service isn't running. The Carbon turns it on in Settings › Accessibility › ${com.teamofsilicons.extend.config.DeviceInfo.systemLabel(extend.context)}, or connects Android debugging in the Extend app.",
        )
        val dpad = mapOf(
            "up" to AccessibilityService.GLOBAL_ACTION_DPAD_UP,
            "down" to AccessibilityService.GLOBAL_ACTION_DPAD_DOWN,
            "left" to AccessibilityService.GLOBAL_ACTION_DPAD_LEFT,
            "right" to AccessibilityService.GLOBAL_ACTION_DPAD_RIGHT,
            "select" to AccessibilityService.GLOBAL_ACTION_DPAD_CENTER,
        )
        fun done(method: String) = Outcome(
            buildJsonObject { put("button", b); put("longpress", cmd.longPress); put("method", method) },
            "${if (cmd.longPress) "Long-pressed" else "Pressed"} $b",
        )
        if (cmd.longPress) {
            if (b == "select") {
                val focused = a.focusedInput() ?: a.rootInActiveWindow?.findFocus(AccessibilityNodeInfo.FOCUS_ACCESSIBILITY)
                var n = focused
                while (n != null && !n.isLongClickable) n = n.parent
                if (n?.performAction(AccessibilityNodeInfo.ACTION_LONG_CLICK) == true) return done("accessibility_long_click")
                throw CommandFailure(CommandFailure.ACTION_FAILED, "Nothing focused on screen accepts a long press of select.")
            }
            throw CommandFailure.unsupported(
                "Holding $b needs Android debugging (an accessibility service can't send held keys). Connect Android debugging in the Extend app's setup, or use tv-remote press $b.",
            )
        }
        when (b) {
            in dpad -> {
                if (!ExtendAccessibilityService.dpadSupported) {
                    throw CommandFailure.unsupported(
                        "D-pad buttons need Android debugging on this TV (Android ${android.os.Build.VERSION.RELEASE}; accessibility has D-pad actions only from Android 13). Connect Android debugging in the Extend app's setup.",
                    )
                }
                if (!a.global(dpad.getValue(b))) throw CommandFailure(CommandFailure.ACTION_FAILED, "Android refused the $b button.")
                return done("global_action")
            }
            "back" -> return global(AccessibilityService.GLOBAL_ACTION_BACK, "back").let { done("global_action") }
            "home" -> return global(AccessibilityService.GLOBAL_ACTION_HOME, "home").let { done("global_action") }
            "play-pause" -> {
                val am = context.getSystemService(AudioManager::class.java)
                am.dispatchMediaKeyEvent(KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE))
                am.dispatchMediaKeyEvent(KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE))
                return done("media_key")
            }
            "volume-up", "volume-down", "mute" -> {
                val am = context.getSystemService(AudioManager::class.java)
                val dir = when (b) {
                    "volume-up" -> AudioManager.ADJUST_RAISE
                    "volume-down" -> AudioManager.ADJUST_LOWER
                    else -> AudioManager.ADJUST_TOGGLE_MUTE
                }
                am.adjustSuggestedStreamVolume(dir, AudioManager.USE_DEFAULT_STREAM_TYPE, AudioManager.FLAG_SHOW_UI)
                return done("audio_manager")
            }
            "menu" -> throw CommandFailure.unsupported(
                (adbProblem?.let { "Android debugging failed while pressing menu ($it). " } ?: "") +
                    "The Menu key needs Android debugging (an accessibility service can't send it). Connect Android debugging in the Extend app's setup, then try again.",
            )
        }
        throw CommandFailure.invalid("Unknown remote button $b")
    }

    private suspend fun keyboard(cmd: Cmd.Keyboard): Outcome {
        val a = a11y()
        val visible = a.imeVisible()
        if (cmd.action == "status") {
            val focused = a.focusedInput()
            val type = focused?.let { inputTypeName(it.inputType) }
            val out = buildJsonObject {
                put("visible", visible)
                put("inputType", type?.let { JsonPrimitive(it) } ?: JsonNull)
                put("focusedField", focused != null)
            }
            return Outcome(out, "Keyboard ${if (visible) "visible" else "hidden"}${type?.let { " (input type $it)" } ?: ""}")
        }
        if (!visible) return Outcome(buildJsonObject { put("visible", false); put("dismissed", false) }, "Keyboard already hidden")
        a.global(AccessibilityService.GLOBAL_ACTION_BACK)
        var still = true
        repeat(8) {
            delay(150)
            still = a.imeVisible()
            if (!still) return@repeat
        }
        if (still) throw CommandFailure(CommandFailure.ACTION_FAILED, "Pressed back but the keyboard is still showing; press a visible Done control instead.")
        return Outcome(buildJsonObject { put("visible", false); put("dismissed", true) }, "Keyboard dismissed")
    }

    private fun inputTypeName(t: Int): String {
        val cls = t and InputType.TYPE_MASK_CLASS
        val variation = t and InputType.TYPE_MASK_VARIATION
        return when (cls) {
            InputType.TYPE_CLASS_NUMBER -> if (variation == InputType.TYPE_NUMBER_VARIATION_PASSWORD) "number-password" else "number"
            InputType.TYPE_CLASS_PHONE -> "phone"
            InputType.TYPE_CLASS_DATETIME -> "datetime"
            InputType.TYPE_CLASS_TEXT -> when (variation) {
                InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS, InputType.TYPE_TEXT_VARIATION_WEB_EMAIL_ADDRESS -> "email"
                InputType.TYPE_TEXT_VARIATION_PASSWORD, InputType.TYPE_TEXT_VARIATION_WEB_PASSWORD, InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD -> "password"
                InputType.TYPE_TEXT_VARIATION_URI -> "url"
                else -> "text"
            }
            else -> "unknown"
        }
    }

    private suspend fun clipboard(cmd: Cmd.Clipboard): Outcome {
        val cm = context.getSystemService(ClipboardManager::class.java)
        val write = cmd.write
        if (write != null) {
            withContext(Dispatchers.Main) {
                cm.setPrimaryClip(ClipData.newPlainText("Silicon Extend", write))
            }
            return Outcome(buildJsonObject { put("chars", write.length) }, "Clipboard set [redacted ${write.length} chars]")
        }
        // Android 10+ only lets the focused app read the clipboard: take focus for a moment.
        val text = ClipboardActivity.read(a11y())
        return Outcome(buildJsonObject { put("text", text?.let { JsonPrimitive(it) } ?: JsonNull) }, text ?: "(clipboard is empty)")
    }

    // ───────────── apps ─────────────

    private data class AppEntry(val pkg: String, val label: String, val launchable: Boolean, val system: Boolean, val version: String?)

    private fun launchIntent(pkg: String): Intent? {
        val pm = context.packageManager
        return (if (extend.isTv) pm.getLeanbackLaunchIntentForPackage(pkg) ?: pm.getLaunchIntentForPackage(pkg)
        else pm.getLaunchIntentForPackage(pkg) ?: pm.getLeanbackLaunchIntentForPackage(pkg))
    }

    private fun launchableApps(): List<AppEntry> {
        val pm = context.packageManager
        val intents = listOf(Intent.CATEGORY_LAUNCHER, Intent.CATEGORY_LEANBACK_LAUNCHER).flatMap { cat ->
            pm.queryIntentActivities(Intent(Intent.ACTION_MAIN).addCategory(cat), 0)
        }
        return intents.map { it.activityInfo.packageName }.distinct().map { pkg -> appEntry(pkg, launchable = true) }
    }

    private fun appEntry(pkg: String, launchable: Boolean? = null): AppEntry {
        val pm = context.packageManager
        val ai = runCatching { pm.getApplicationInfo(pkg, 0) }.getOrNull()
        val label = ai?.let { pm.getApplicationLabel(it).toString() } ?: pkg
        val system = ai != null && (ai.flags and ApplicationInfo.FLAG_SYSTEM) != 0
        val version = runCatching { pm.getPackageInfo(pkg, 0).versionName }.getOrNull()
        return AppEntry(pkg, label, launchable ?: (launchIntent(pkg) != null), system, version)
    }

    private fun isInstalled(pkg: String): Boolean =
        runCatching { context.packageManager.getApplicationInfo(pkg, 0); true }.getOrDefault(false)

    /** A package from a package name or an app's label ("Settings", "YouTube"). */
    private fun resolveApp(name: String): String {
        if (isInstalled(name)) return name
        val apps = launchableApps()
        val q = Selectors.normalizeText(name)
        val exact = apps.filter { Selectors.normalizeText(it.label) == q }
        val pick = exact.ifEmpty { apps.filter { Selectors.normalizeText(it.label).startsWith(q) } }
            .ifEmpty { apps.filter { Selectors.normalizeText(it.label).contains(q) } }
        return when {
            pick.size == 1 -> pick[0].pkg
            pick.isEmpty() -> throw CommandFailure(
                CommandFailure.APP_NOT_FOUND,
                "No installed app is called or has the package \"$name\". Run apps to list them.",
            )
            else -> pick.firstOrNull { it.pkg.endsWith("." + q.replace(" ", "")) }?.pkg ?: throw CommandFailure(
                CommandFailure.AMBIGUOUS,
                "\"$name\" matches several apps: ${pick.take(8).joinToString(", ") { "${it.label} (${it.pkg})" }}. Use the package name.",
            )
        }
    }

    private fun looksLikeUrl(s: String): Boolean =
        s.contains("://") || s.startsWith("www.") || Regex("^(mailto|tel|sms|geo|market|intent):", RegexOption.IGNORE_CASE).containsMatchIn(s)

    private suspend fun open(cmd: Cmd.Open): Outcome {
        val a = a11y()
        val (pkg, url) = when {
            cmd.url != null -> resolveApp(cmd.target) to cmd.url
            looksLikeUrl(cmd.target) -> null to cmd.target
            else -> resolveApp(cmd.target) to null
        }
        val intent = if (url != null) {
            val uri = Uri.parse(if (url.startsWith("www.")) "https://$url" else url)
            Intent(Intent.ACTION_VIEW, uri).apply { pkg?.let { setPackage(it) } }
        } else {
            launchIntent(pkg!!) ?: throw CommandFailure(CommandFailure.APP_NOT_FOUND, "$pkg is installed but has no screen to open (no launcher activity).")
        }
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        val before = a.currentForeground()
        try {
            // Started from the accessibility service, which Android allows to start activities from the background.
            withContext(Dispatchers.Main) { a.startActivity(intent) }
        } catch (e: android.content.ActivityNotFoundException) {
            throw CommandFailure(CommandFailure.APP_NOT_FOUND, "No app on this device can open $url.")
        }
        // Wait until something new is in front.
        val expected = pkg
        var fg: String? = null
        val until = SystemClock.elapsedRealtime() + 5_000
        while (SystemClock.elapsedRealtime() < until) {
            delay(250)
            fg = a.currentForeground()
            if (fg == expected || (expected == null && fg != before)) break
        }
        val out = buildJsonObject {
            pkg?.let { put("package", it) }
            url?.let { put("url", it) }
            put("foreground", fg?.let { JsonPrimitive(it) } ?: JsonNull)
            a.foregroundActivity?.let { put("activity", it) }
        }
        val what = url ?: pkg
        val note = when {
            expected == null || fg == expected -> ""
            fg != before -> " (Android brought back its task, which is showing $fg)"
            else -> " (asked Android to open it, but $fg is still in front after 5 s)"
        }
        return Outcome(out, "Opened $what$note")
    }

    private suspend fun close(cmd: Cmd.Close): Outcome {
        val a = a11y()
        val fg = a.currentForeground()
        val pkg = cmd.app?.let { resolveApp(it) } ?: fg ?: throw CommandFailure(CommandFailure.NOT_FOUND, "No app is in front to close.")
        var wentHome = false
        if (fg == pkg || cmd.app == null) {
            a.global(AccessibilityService.GLOBAL_ACTION_HOME)
            wentHome = true
            delay(400)
        }
        var killed = false
        if (pkg != context.packageName) {
            context.getSystemService(ActivityManager::class.java).killBackgroundProcesses(pkg)
            killed = true
        }
        val out = buildJsonObject { put("package", pkg); put("wentHome", wentHome); put("killedBackground", killed) }
        return Outcome(
            out,
            "Closed $pkg (${if (wentHome) "went home; " else ""}ended its background processes. Android may keep it in recent apps; " +
                "force-stopping needs Android debugging).",
        )
    }

    private fun apps(cmd: Cmd.Apps): Outcome {
        val pm = context.packageManager
        val entries = if (cmd.all) {
            pm.getInstalledApplications(PackageManager.MATCH_ALL).map { appEntry(it.packageName) }
        } else {
            launchableApps().filter { !it.system || runCatching { (pm.getApplicationInfo(it.pkg, 0).flags and ApplicationInfo.FLAG_UPDATED_SYSTEM_APP) != 0 }.getOrDefault(false) }
        }.sortedBy { it.label.lowercase() }
        val out = buildJsonObject {
            put("apps", buildJsonArray {
                entries.forEach { e ->
                    add(buildJsonObject {
                        put("package", e.pkg); put("label", e.label); put("launchable", e.launchable); put("system", e.system)
                        put("version", e.version?.let { JsonPrimitive(it) } ?: JsonNull)
                    })
                }
            })
            put("count", entries.size)
            put("all", cmd.all)
        }
        val text = entries.joinToString("\n") { "${it.pkg}  ${it.label}" }.ifEmpty { "(no apps)" }
        return Outcome(out, text)
    }

    private fun appState(): Outcome {
        val a = a11y()
        val pkg = a.currentForeground() ?: capture().foregroundPackage
        // The activity comes from window events; only report it when it belongs to that package.
        val activity = a.foregroundActivity?.takeIf { a.foregroundPackage == pkg }
        val out = buildJsonObject {
            put("package", pkg?.let { JsonPrimitive(it) } ?: JsonNull)
            put("activity", activity?.let { JsonPrimitive(it) } ?: JsonNull)
            put("state", "foreground")
        }
        return Outcome(out, "${pkg ?: "unknown"}${activity?.let { " / $it" } ?: ""}")
    }

    // ───────────── pop-ups ─────────────

    private fun alertJson(a: AlertInfo?) = buildJsonObject {
        put("present", a != null)
        if (a != null) {
            put("kind", a.kind)
            put("title", a.title?.let { JsonPrimitive(it) } ?: JsonNull)
            put("message", a.message?.let { JsonPrimitive(it) } ?: JsonNull)
            put("buttons", JsonArray(a.buttons.map { JsonPrimitive(Alerts.labelDeep(it)) }))
            put("accept", a.accept?.let { JsonPrimitive(Alerts.labelDeep(it)) } ?: JsonNull)
            put("dismiss", a.dismiss?.let { JsonPrimitive(Alerts.labelDeep(it)) } ?: JsonNull)
        }
    }

    private fun alertText(a: AlertInfo?) = if (a == null) "No alert on screen" else
        "Alert: ${listOfNotNull(a.title, a.message).joinToString(" — ").ifEmpty { "(untitled)" }} [${a.buttons.joinToString(", ") { Alerts.labelDeep(it) }}]"

    private suspend fun alert(cmd: Cmd.Alert, run: Run): Outcome {
        var current = Alerts.detect(capture())
        when (cmd.action) {
            "get" -> return Outcome(alertJson(current), alertText(current))
            "wait" -> {
                val until = SystemClock.elapsedRealtime() + min(cmd.waitMs ?: 5_000, run.remainingMs() - 250)
                while (current == null && SystemClock.elapsedRealtime() < until) {
                    delay(300)
                    current = Alerts.detect(capture())
                }
                if (current == null) throw CommandFailure(CommandFailure.WAIT_TIMEOUT, "No alert appeared within ${cmd.waitMs} ms.", buildJsonObject { put("reason", "wait_target_absent") })
                return Outcome(alertJson(current), alertText(current))
            }
        }
        val alert = current ?: throw CommandFailure(CommandFailure.NOT_FOUND, "No alert on screen. If a sheet is visible, it's app UI: use snapshot -i and press its button.")
        val button = (if (cmd.action == "accept") alert.accept else alert.dismiss) ?: throw CommandFailure(
            CommandFailure.NOT_FOUND,
            "The alert has no button that clearly ${if (cmd.action == "accept") "accepts" else "dismisses"} it (buttons: ${alert.buttons.joinToString(", ") { Alerts.labelDeep(it) }}). Press one by ref.",
        )
        val target = button.nearestClickable() ?: button
        if (info(target)?.performAction(AccessibilityNodeInfo.ACTION_CLICK) != true) {
            val (x, y) = pointOf(target, a11y().screenBounds())
            if (!a11y().tap(x.toFloat(), y.toFloat())) throw CommandFailure(CommandFailure.ACTION_FAILED, "Couldn't press \"${Alerts.labelDeep(button)}\".")
        }
        return Outcome(
            JsonObject(alertJson(alert) + ("pressed" to JsonPrimitive(Alerts.labelDeep(button)))),
            "Pressed \"${Alerts.labelDeep(button)}\" (${cmd.action}). Run alert get to confirm it closed.",
        )
    }

    private fun notifications(): Outcome {
        val listener = ExtendNotificationListener.instance ?: throw CommandFailure(
            CommandFailure.NOT_READY,
            "Notification access isn't on: Settings › Notifications › Device & app notifications › Silicon Extend.",
        )
        val items = listener.items()
        val out = buildJsonObject {
            put("items", buildJsonArray {
                items.forEach { n ->
                    add(buildJsonObject {
                        put("app", n.app)
                        put("package", n.packageName)
                        put("title", n.title?.let { JsonPrimitive(it) } ?: JsonNull)
                        put("text", n.text?.let { JsonPrimitive(it) } ?: JsonNull)
                        put("posted_at", Instant.ofEpochMilli(n.postedAtMs).toString())
                        put("ongoing", n.ongoing)
                        put("category", n.category?.let { JsonPrimitive(it) } ?: JsonNull)
                        put("key", n.key)
                    })
                }
            })
        }
        val text = if (items.isEmpty()) "No notifications" else items.joinToString("\n") { n ->
            "${n.app}: ${n.title ?: ""}${n.text?.let { " — $it" } ?: ""}"
        }
        return Outcome(out, text)
    }

    // ───────────── TV display ─────────────

    private fun attachmentFor(value: String, run: Run, prefix: String): Attachment? {
        val atts = run.frame.attachments
        return atts.firstOrNull { it.name == value || it.name == value.substringAfterLast('/') }
            ?: atts.firstOrNull { it.contentType.startsWith(prefix) }.takeIf { !value.startsWith("http") }
    }

    private suspend fun display(cmd: Cmd.Display, run: Run): Outcome {
        val a = a11y()
        if (cmd is Cmd.Display.Clear) {
            val was = DisplayActivity.clear()
            return Outcome(buildJsonObject { put("cleared", was) }, if (was) "Display cleared" else "Nothing was on the display")
        }
        val show = cmd as Cmd.Display.Show
        val intent = Intent(context, DisplayActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_NO_ANIMATION)
        intent.putExtra(DisplayActivity.EXTRA_KIND, show.kind)
        when (show.kind) {
            "url", "text" -> intent.putExtra(DisplayActivity.EXTRA_VALUE, show.value)
            "image", "video" -> {
                val local = File(show.value).takeIf { show.value.startsWith("/") && it.isFile }
                val att = if (local == null) attachmentFor(show.value, run, if (show.kind == "image") "image/" else "video/") else null
                if (local != null) {
                    val dir = File(context.cacheDir, "display").apply { mkdirs() }
                    dir.listFiles()?.forEach { it.delete() }
                    val file = File(dir, local.name)
                    local.copyTo(file, overwrite = true)
                    intent.putExtra(DisplayActivity.EXTRA_FILE, file.absolutePath)
                } else if (att != null) {
                    val dir = File(context.cacheDir, "display").apply { mkdirs() }
                    dir.listFiles()?.forEach { it.delete() }
                    val file = File(dir, att.name.substringAfterLast('/').ifEmpty { "media" })
                    file.writeBytes(Base64.decode(att.contentBase64, Base64.DEFAULT))
                    intent.putExtra(DisplayActivity.EXTRA_FILE, file.absolutePath)
                } else if (show.value.startsWith("http://") || show.value.startsWith("https://")) {
                    intent.putExtra(DisplayActivity.EXTRA_VALUE, show.value)
                } else {
                    throw CommandFailure.invalid(
                        "display show --${show.kind} needs an http(s) URL, or the file sent as an attachment with the command; \"${show.value}\" is neither.",
                    )
                }
            }
        }
        val since = SystemClock.elapsedRealtime()
        withContext(Dispatchers.Main) { a.startActivity(intent) }
        val shown = DisplayActivity.awaitShown(4_000, since)
        val out = buildJsonObject { put("kind", show.kind); put("shown", shown) }
        if (!shown) throw CommandFailure(CommandFailure.ACTION_FAILED, "Asked Android to show the display, but it didn't come to the front within 4 s.")
        delay(500) // let the first frame draw before the next command looks at the screen
        return Outcome(out, "Showing ${show.kind} full screen; it stays until display clear or Back on the remote.")
    }

    // ───────────── screenshots ─────────────

    private suspend fun screenshot(cmd: Cmd.Screenshot, run: Run): Outcome {
        val a = a11y()
        var bmp = a.screenshot()
        if (cmd.cropOn != null) {
            val cap = capture()
            val n = Selectors.resolveAll(cmd.cropOn, cap.allNodes, cap.screen).firstOrNull()
                ?: throw CommandFailure(CommandFailure.NOT_FOUND, "--crop-on ${cmd.cropOn.raw} matches nothing on screen.")
            val b = n.bounds.intersect(Bounds(0, 0, bmp.width, bmp.height))
            if (b.isEmpty) throw CommandFailure(CommandFailure.NOT_FOUND, "--crop-on element is off screen.")
            bmp = Bitmap.createBitmap(bmp, b.left, b.top, b.width, b.height)
        }
        if (cmd.overlayRefs) {
            val snap = run.session.last ?: SnapshotEngine.build(capture(), SnapshotOptions(interactive = true)).also { run.session.last = it }
            bmp = bmp.copy(Bitmap.Config.ARGB_8888, true)
            val canvas = Canvas(bmp)
            val stroke = Paint().apply { style = Paint.Style.STROKE; strokeWidth = 4f; color = Color.rgb(255, 64, 129) }
            val fill = Paint().apply { color = Color.rgb(255, 64, 129) }
            val text = Paint().apply { color = Color.WHITE; textSize = 30f; isAntiAlias = true }
            for (n in snap.nodes) {
                val r = n.node.bounds
                if (r.isEmpty) continue
                canvas.drawRect(r.left.toFloat(), r.top.toFloat(), r.right.toFloat(), r.bottom.toFloat(), stroke)
                val label = "@" + n.ref
                val w = text.measureText(label) + 12
                canvas.drawRect(r.left.toFloat(), r.top.toFloat(), r.left + w, r.top + 36f, fill)
                canvas.drawText(label, r.left + 6f, r.top + 28f, text)
            }
        }
        if (cmd.scale != null && cmd.scale < 1f) {
            bmp = Bitmap.createScaledBitmap(bmp, max(1, (bmp.width * cmd.scale).roundToInt()), max(1, (bmp.height * cmd.scale).roundToInt()), true)
        }
        val bytes = ByteArrayOutputStream().use { out ->
            bmp.compress(Bitmap.CompressFormat.PNG, 100, out)
            out.toByteArray()
        }
        val name = (cmd.name?.substringAfterLast('/')?.takeIf { it.isNotBlank() } ?: "screenshot").let {
            if (it.lowercase().endsWith(".png")) it else "$it.png"
        }
        val file = upload(run, bytes, name, "image/png", "screenshot")
        val out = buildJsonObject {
            put("files", JsonArray(listOf(JsonPrimitive(file.uploadId))))
            put("name", name)
            put("width", bmp.width)
            put("height", bmp.height)
            put("size_bytes", bytes.size)
        }
        return Outcome(out, "Screenshot $name (${bmp.width}x${bmp.height}, ${bytes.size / 1024} KB)")
    }

    private suspend fun upload(run: Run, bytes: ByteArray, name: String, contentType: String, kind: String): ProducedFile {
        val id = run.frame.uploadIds.getOrNull(run.uploadCursor) ?: throw CommandFailure(
            CommandFailure.UPLOAD_FAILED,
            "The command came with ${run.frame.uploadIds.size} upload id(s), all used, so $name can't be uploaded.",
        )
        run.uploadCursor++
        val cred = extend.secrets.readCredential() ?: throw CommandFailure(CommandFailure.NOT_READY, "This device isn't paired.")
        try {
            extend.api.upload(cred, id, bytes, contentType, name)
        } catch (e: ApiException) {
            throw CommandFailure(CommandFailure.UPLOAD_FAILED, "Uploading $name failed: HTTP ${e.status} ${e.message}")
        } catch (e: java.io.IOException) {
            throw CommandFailure(CommandFailure.UPLOAD_FAILED, "Uploading $name failed: ${e.message}")
        }
        return ProducedFile(id, name, contentType, kind, bytes.size.toLong()).also { run.files += it }
    }

    // ───────────── several steps ─────────────

    private suspend fun steps(kind: String, steps: List<Step>, run: Run): Outcome {
        if (run.depth > 0) throw CommandFailure.invalid("$kind can't run inside another batch or replay")
        run.depth++
        val results = ArrayList<JsonObject>()
        val texts = ArrayList<String>()
        try {
            for ((i, step) in steps.withIndex()) {
                if (step.command in setOf("batch", "replay", "test")) throw CommandFailure.invalid("Step ${i + 1}: $kind can't contain ${step.command}")
                val label = "${i + 1}/${steps.size} ${step.command}${step.line?.let { " (line $it)" } ?: ""}"
                try {
                    val o = runStep(step.command, step.args, run)
                    results += buildJsonObject { put("command", step.command); put("ok", true); put("output", o.output); put("text", o.text) }
                    texts += "ok   $label: ${o.text.lineSequence().firstOrNull().orEmpty()}"
                } catch (f: CommandFailure) {
                    results += buildJsonObject {
                        put("command", step.command); put("ok", false)
                        put("error", buildJsonObject { put("code", f.code); put("message", f.message ?: "") })
                    }
                    texts += "FAIL $label: ${f.message}"
                    throw CommandFailure(
                        f.code,
                        "$kind stopped at step $label: ${f.message}\n" + texts.joinToString("\n"),
                        buildJsonObject { put("failedStep", i + 1); put("steps", JsonArray(results)) },
                    )
                }
            }
        } finally {
            run.depth--
        }
        return Outcome(buildJsonObject { put("steps", JsonArray(results)); put("count", steps.size) }, texts.joinToString("\n"))
    }

    private fun scriptAttachment(name: String?, run: Run): Attachment {
        val atts = run.frame.attachments
        if (atts.isEmpty()) throw CommandFailure.invalid("replay needs the script sent as an attachment with the command (the extend CLI reads it and attaches it).")
        return name?.let { n -> atts.firstOrNull { it.name == n || it.name == n.substringAfterLast('/') } } ?: atts.first()
    }

    private suspend fun replay(cmd: Cmd.Replay, run: Run): Outcome {
        val local = cmd.script?.let { File(it) }?.takeIf { cmd.script.startsWith("/") && it.isFile }
        val (name, script) = if (local != null) {
            local.name to local.readText()
        } else {
            val att = scriptAttachment(cmd.script, run)
            att.name to String(Base64.decode(att.contentBase64, Base64.DEFAULT), Charsets.UTF_8)
        }
        var steps = Scripts.parseAd(script)
        if (cmd.keepSession && steps.lastOrNull()?.command == "close") steps = steps.dropLast(1)
        if (steps.isEmpty()) throw CommandFailure.invalid("$name has no steps")
        return steps("replay $name", steps, run)
    }

    private suspend fun testSuite(cmd: Cmd.TestSuite, run: Run): Outcome {
        val fromArgs = cmd.scripts.map { File(it) }.filter { it.isAbsolute && it.isFile }
            .map { Attachment(it.name, "text/plain", Base64.encodeToString(it.readBytes(), Base64.NO_WRAP)) }
        val scripts = fromArgs.ifEmpty { run.frame.attachments.filter { it.name.endsWith(".ad") }.ifEmpty { run.frame.attachments } }
        if (scripts.isEmpty()) throw CommandFailure.invalid("test needs the .ad scripts sent as attachments with the command.")
        val results = ArrayList<JsonObject>()
        val lines = ArrayList<String>()
        var failed = 0
        for ((i, att) in scripts.withIndex()) {
            val script = String(Base64.decode(att.contentBase64, Base64.DEFAULT), Charsets.UTF_8)
            try {
                steps("test ${att.name}", Scripts.parseAd(script), run)
                results += buildJsonObject { put("script", att.name); put("ok", true) }
                lines += "pass ${i + 1}/${scripts.size} ${att.name}"
            } catch (f: CommandFailure) {
                failed++
                results += buildJsonObject { put("script", att.name); put("ok", false); put("error", f.message ?: f.code) }
                lines += "fail ${i + 1}/${scripts.size} ${att.name}: ${f.message?.lineSequence()?.firstOrNull()}"
            }
        }
        val out = buildJsonObject { put("results", JsonArray(results)); put("failed", failed); put("passed", scripts.size - failed) }
        if (failed > 0) throw CommandFailure(CommandFailure.ASSERTION_FAILED, lines.joinToString("\n"), out)
        return Outcome(out, lines.joinToString("\n"))
    }

    internal companion object {
        /** Commands that run through Android debugging. */
        private val ADB_COMMANDS = setOf("adb", "install", "reinstall", "record", "logs")

        /** Why a session's captures were discarded when the device gave up on it while offline. */
        val OFFLINE_TOO_LONG = "this device lost contact with Extend for more than ${SessionRetention.GRACE_MS / 60_000} minutes " +
            "during the session, longer than Extend keeps a session for an offline device, so the device ended the capture and deleted it."
    }
}

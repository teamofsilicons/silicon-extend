package com.teamofsilicons.extend.driver

import kotlinx.serialization.json.JsonElement

/** A command that failed; becomes `result` with `ok:false` and this `error`. */
class CommandFailure(val code: String, message: String, val details: JsonElement? = null) : Exception(message) {
    companion object {
        const val INVALID_ARGS = "invalid_args"
        const val UNSUPPORTED = "unsupported_on_device"
        const val NOT_FOUND = "element_not_found"
        const val AMBIGUOUS = "ambiguous_match"
        const val STALE_REF = "stale_ref"
        const val ACTION_FAILED = "action_failed"
        const val ASSERTION_FAILED = "assertion_failed"
        const val WAIT_TIMEOUT = "wait_timeout"
        const val TIMEOUT = "command_timeout"
        const val NOT_READY = "device_not_ready"
        const val UPLOAD_FAILED = "upload_failed"
        const val APP_NOT_FOUND = "app_not_found"

        fun invalid(message: String) = CommandFailure(INVALID_ARGS, message)
        fun unsupported(message: String) = CommandFailure(UNSUPPORTED, message)
    }
}

/** What a command acts on. */
sealed interface Target {
    data class Ref(val ref: String) : Target
    data class Sel(val chain: SelectorChain) : Target
    data class Point(val x: Int, val y: Int) : Target
}

enum class Direction { UP, DOWN, LEFT, RIGHT }

/** A parsed command, ready to run. */
sealed interface Cmd {
    data class Debug(val command: com.teamofsilicons.extend.adb.AdbCommand) : Cmd
    data class Snapshot(val options: SnapshotOptions, val diff: Boolean) : Cmd
    data class Get(val what: String, val target: Target) : Cmd
    data class Find(val locator: String, val query: String, val action: String, val value: String?, val waitMs: Long?, val pick: String?) : Cmd
    data class Is(val predicate: String, val chain: SelectorChain, val expected: String?) : Cmd
    sealed interface Wait : Cmd {
        data class Duration(val ms: Long) : Wait
        data class ForText(val text: String, val timeoutMs: Long?) : Wait
        data class ForTarget(val target: Target, val timeoutMs: Long?) : Wait
        data class Absent(val chain: SelectorChain, val timeoutMs: Long?) : Wait
    }
    data class Screenshot(val name: String?, val scale: Float?, val overlayRefs: Boolean, val cropOn: SelectorChain?) : Cmd
    data class Click(val target: Target, val count: Int = 1, val holdMs: Long? = null, val intervalMs: Long = 100) : Cmd
    data class LongPress(val target: Target, val ms: Long) : Cmd
    data class Fill(val target: Target, val text: String, val delayMs: Long?) : Cmd
    data class Type(val text: String, val delayMs: Long?) : Cmd
    data class Focus(val target: Target) : Cmd
    data class Scroll(val direction: String, val fraction: Float?, val pixels: Int?) : Cmd
    data class Swipe(val x1: Int, val y1: Int, val x2: Int, val y2: Int, val durationMs: Long, val count: Int, val pauseMs: Long, val pingPong: Boolean) : Cmd
    sealed interface Gesture : Cmd {
        data class Pan(val x: Int, val y: Int, val dx: Int, val dy: Int, val durationMs: Long, val pointers: Int) : Gesture
        data class Fling(val direction: Direction, val x: Int, val y: Int, val distance: Int?) : Gesture
        data class Pinch(val scale: Float, val x: Int?, val y: Int?) : Gesture
        data class Rotate(val degrees: Float, val x: Int?, val y: Int?) : Gesture
        data class Drag(val from: Target, val to: Target, val holdMs: Long, val moveMs: Long, val dropHoldMs: Long) : Gesture
    }
    data object Back : Cmd
    data object Home : Cmd
    data object AppSwitcher : Cmd
    data class TvRemote(val longPress: Boolean, val button: String, val durationMs: Long?) : Cmd
    data class Keyboard(val action: String) : Cmd
    data class Clipboard(val write: String?) : Cmd
    data class Open(val target: String, val url: String?) : Cmd
    data class Close(val app: String?) : Cmd
    data class Apps(val all: Boolean) : Cmd
    data object AppState : Cmd
    data class Alert(val action: String, val waitMs: Long?) : Cmd
    data object Notifications : Cmd
    sealed interface Display : Cmd {
        data class Show(val kind: String, val value: String) : Display
        data object Clear : Display
    }
    data class Batch(val stepsJson: String) : Cmd
    data class Replay(val script: String?, val keepSession: Boolean) : Cmd
    data class TestSuite(val scripts: List<String>) : Cmd
}

/**
 * Parses agent-device CLI tokens (`command` + `args`, as `extend` forwards them) into a [Cmd].
 * Grammar follows `understanding/cli.yaml` `device_commands` and agent-device's docs. Flags that
 * only change how the CLI prints (`--json`) or that Extend handles elsewhere (`--ttl`,
 * `--permanent`) are accepted and ignored.
 */
object CommandParser {
    /** Commands this app never runs, each with the precise reason. */
    val UNSUPPORTED: Map<String, String> = mapOf(
        "hover" to "hover needs a mouse pointer; Android touch screens and TVs have none. Use longpress for the touch equivalent.",
        "terminal" to "terminal is for computers (Mac, Windows, Linux); use adb shell for Android debugging.",
    )

    private val GLOBAL_BOOLEAN_FLAGS = setOf("--json", "--settle", "--verbose", "--permanent", "--force-full", "--no-settle")
    private val GLOBAL_VALUE_FLAGS = setOf("--timeout", "--ttl")

    fun parse(command: String, args: List<String>): Cmd {
        if (command in setOf("adb", "install", "reinstall", "record", "logs")) {
            return Cmd.Debug(com.teamofsilicons.extend.adb.AdbCommands.parse(command, args))
        }
        val a = Args.of(args)
        return when (command) {
            "snapshot" -> parseSnapshot(a, diffAlias = false)
            "diff" -> {
                val what = a.positional(0) ?: throw CommandFailure.invalid("Usage: diff snapshot [-i] [-d <depth>] [-s <scope>] | diff screenshot --baseline <file>")
                when (what) {
                    "snapshot" -> parseSnapshot(a.dropPositional(1), diffAlias = true)
                    "screenshot" -> throw CommandFailure.unsupported(
                        "diff screenshot needs the baseline image on the device, and the Android app can't fetch Briefcase files yet. " +
                            "Take screenshots and compare them on your side.",
                    )
                    else -> throw CommandFailure.invalid("diff takes snapshot or screenshot, not \"$what\"")
                }
            }
            "get" -> {
                a.noUnknownFlags()
                val what = a.positional(0) ?: throw CommandFailure.invalid("Usage: get text|attrs <@ref|selector>")
                if (what != "text" && what != "attrs") throw CommandFailure.invalid("get takes text or attrs, not \"$what\"")
                Cmd.Get(what, target(a.positionals.drop(1), "get $what"))
            }
            "find" -> parseFind(a)
            "is" -> parseIs(a)
            "wait" -> parseWait(a)
            "screenshot" -> {
                val scale = a.float("--scale")
                if (scale != null && (scale < 0.01f || scale > 1f)) throw CommandFailure.invalid("--scale must be between 0.01 and 1, got $scale")
                val cropOn = a.value("--crop-on")?.let { parseSelectorArg(it) }
                val overlay = a.flag("--overlay-refs")
                a.flag("--fullscreen")
                a.noUnknownFlags()
                if (a.positionals.size > 1) throw CommandFailure.invalid("screenshot takes at most one name, got ${a.positionals}")
                Cmd.Screenshot(a.positional(0), scale, overlay, cropOn)
            }
            "click" -> {
                val button = a.value("--button")
                if (button != null && button != "primary") {
                    throw CommandFailure.unsupported("click --button $button needs a mouse; Android has only touch. Use longpress for a context menu.")
                }
                a.noUnknownFlags()
                Cmd.Click(target(a.positionals, "click"))
            }
            "press" -> {
                val count = a.int("--count") ?: 1
                val hold = a.long("--hold-ms")
                val interval = a.long("--interval-ms") ?: 100
                a.value("--jitter-px")
                a.flag("--double-tap").let { if (it) return Cmd.Click(target(a.positionals, "press"), 2, hold, 80) }
                a.noUnknownFlags()
                if (count < 1 || count > 200) throw CommandFailure.invalid("--count must be 1–200, got $count")
                Cmd.Click(target(a.positionals, "press"), count, hold, interval)
            }
            "longpress" -> {
                a.noUnknownFlags()
                val p = a.positionals
                if (p.size >= 2 && p[0].isInt() && p[1].isInt()) {
                    val ms = p.getOrNull(2)?.let { it.toLongOrNull() ?: throw CommandFailure.invalid("longpress duration must be milliseconds, got \"$it\"") }
                    if (p.size > 3) throw CommandFailure.invalid("Usage: longpress <x> <y> [ms]")
                    Cmd.LongPress(Target.Point(p[0].toInt(), p[1].toInt()), ms ?: 800)
                } else {
                    val last = p.lastOrNull()
                    val (tokens, ms) = if (p.size > 1 && last != null && last.toLongOrNull() != null) p.dropLast(1) to last.toLong() else p to 800L
                    Cmd.LongPress(target(tokens, "longpress"), ms)
                }
            }
            "fill" -> {
                val delay = a.long("--delay-ms")
                a.noUnknownFlags()
                val p = a.positionals
                if (p.size < 2) throw CommandFailure.invalid("Usage: fill <@ref|selector> <text>")
                val (t, rest) = targetWithRest(p, "fill")
                if (rest.isEmpty()) throw CommandFailure.invalid("fill needs the text to type after the target")
                Cmd.Fill(t, rest.joinToString(" "), delay)
            }
            "type" -> {
                val delay = a.long("--delay-ms")
                a.noUnknownFlags()
                if (a.positionals.isEmpty()) throw CommandFailure.invalid("Usage: type <text>")
                if (a.positionals.size == 1 && a.positionals[0].matches(REF)) {
                    throw CommandFailure.invalid("type takes text only. Use fill ${a.positionals[0]} \"text\" to target a field, or press ${a.positionals[0]} then type.")
                }
                Cmd.Type(a.positionals.joinToString(" "), delay)
            }
            "focus" -> {
                a.noUnknownFlags()
                Cmd.Focus(target(a.positionals, "focus"))
            }
            "scroll" -> {
                val pixels = a.int("--pixels")
                a.noUnknownFlags()
                val dir = a.positional(0)?.lowercase() ?: throw CommandFailure.invalid("Usage: scroll up|down|left|right [fraction] [--pixels <n>]")
                if (dir !in setOf("up", "down", "left", "right", "top", "bottom")) {
                    throw CommandFailure.invalid("scroll direction must be up, down, left, right, top or bottom, got \"$dir\"")
                }
                val fraction = a.positional(1)?.let { it.toFloatOrNull() ?: throw CommandFailure.invalid("scroll amount must be a fraction like 0.5, got \"$it\"") }
                if (fraction != null && (fraction <= 0f || fraction > 1f)) throw CommandFailure.invalid("scroll fraction must be in (0, 1], got $fraction")
                if (pixels != null && pixels <= 0) throw CommandFailure.invalid("--pixels must be positive")
                Cmd.Scroll(dir, fraction, pixels)
            }
            "swipe" -> {
                val count = a.int("--count") ?: 1
                val pause = a.long("--pause-ms") ?: 0
                val pattern = a.value("--pattern") ?: "one-way"
                a.noUnknownFlags()
                val p = a.positionals
                if (p.size !in 4..5 || p.take(4).any { !it.isInt() }) throw CommandFailure.invalid("Usage: swipe <x1> <y1> <x2> <y2>")
                if (count !in 1..200) throw CommandFailure.invalid("--count must be 1–200")
                if (pause !in 0..10_000) throw CommandFailure.invalid("--pause-ms must be 0–10000")
                if (pattern != "one-way" && pattern != "ping-pong") throw CommandFailure.invalid("--pattern must be one-way or ping-pong")
                val duration = p.getOrNull(4)?.toLongOrNull() ?: 250
                Cmd.Swipe(p[0].toInt(), p[1].toInt(), p[2].toInt(), p[3].toInt(), duration, count, pause, pattern == "ping-pong")
            }
            "gesture" -> parseGesture(a)
            "hover", "terminal" -> throw CommandFailure.unsupported(UNSUPPORTED.getValue(command))
            "back" -> {
                a.flag("--in-app"); a.flag("--system"); a.noUnknownFlags(); noPositionals(a, "back")
                Cmd.Back
            }
            "home" -> { a.noUnknownFlags(); noPositionals(a, "home"); Cmd.Home }
            "app-switcher" -> { a.noUnknownFlags(); noPositionals(a, "app-switcher"); Cmd.AppSwitcher }
            "tv-remote" -> {
                val duration = a.long("--duration-ms")
                a.noUnknownFlags()
                val action = a.positional(0) ?: throw CommandFailure.invalid("Usage: tv-remote press|longpress <button>")
                if (action != "press" && action != "longpress") throw CommandFailure.invalid("tv-remote takes press or longpress, not \"$action\"")
                val button = a.positional(1)?.lowercase() ?: throw CommandFailure.invalid("tv-remote $action needs a button: ${TV_BUTTONS.joinToString("|")}")
                if (button !in TV_BUTTONS) throw CommandFailure.invalid("Unknown remote button \"$button\"; buttons are ${TV_BUTTONS.joinToString(", ")}")
                Cmd.TvRemote(action == "longpress" || (duration != null && duration > 0), button, duration)
            }
            "keyboard" -> {
                a.noUnknownFlags()
                val action = (a.positional(0) ?: "status").let { if (it == "get") "status" else it }
                if (action != "status" && action != "dismiss") throw CommandFailure.invalid("keyboard takes status or dismiss, not \"$action\"")
                Cmd.Keyboard(action)
            }
            "clipboard" -> {
                a.noUnknownFlags()
                when (val action = a.positional(0)) {
                    "read" -> Cmd.Clipboard(null)
                    "write" -> {
                        if (a.positionals.size < 2) throw CommandFailure.invalid("Usage: clipboard write <text> (use \"\" to clear)")
                        Cmd.Clipboard(a.positionals.drop(1).joinToString(" "))
                    }
                    else -> throw CommandFailure.invalid("clipboard takes read or write <text>, not \"${action ?: ""}\"")
                }
            }
            "open" -> {
                val surface = a.value("--surface")
                if (surface != null && surface != "app") throw CommandFailure.unsupported("--surface $surface is for computers; Android opens apps and links only.")
                a.flag("--relaunch")
                a.noUnknownFlags()
                val t = a.positional(0) ?: throw CommandFailure.invalid("Usage: open <app|url> [url]")
                if (a.positionals.size > 2) throw CommandFailure.invalid("open takes an app or link, and optionally a link to open in it")
                Cmd.Open(t, a.positional(1))
            }
            "close" -> {
                if (a.has("--save-script")) {
                    throw CommandFailure.unsupported("close --save-script isn't recorded on Android yet; keep the steps on your side and replay them with replay/batch.")
                }
                a.flag("--shutdown")
                a.noUnknownFlags()
                Cmd.Close(a.positional(0))
            }
            "apps" -> {
                val all = a.flag("--all")
                a.flag("--user")
                a.noUnknownFlags()
                Cmd.Apps(all)
            }
            "appstate" -> { a.noUnknownFlags(); Cmd.AppState }
            "alert" -> {
                a.noUnknownFlags()
                val action = a.positional(0) ?: "get"
                when (action) {
                    "get", "accept", "dismiss" -> Cmd.Alert(action, null)
                    "wait" -> Cmd.Alert("wait", a.positional(1)?.let { it.toLongOrNull() ?: throw CommandFailure.invalid("alert wait takes milliseconds, got \"$it\"") } ?: 5_000)
                    else -> throw CommandFailure.invalid("alert takes get, wait <ms>, accept or dismiss, not \"$action\"")
                }
            }
            "notifications" -> {
                a.noUnknownFlags()
                if (a.positionals.isNotEmpty()) throw CommandFailure.invalid("notifications takes no arguments, got ${a.positionals}")
                Cmd.Notifications
            }
            "display" -> parseDisplay(a)
            "batch" -> {
                val steps = a.value("--steps")
                if (a.has("--steps-file")) throw CommandFailure.invalid("--steps-file is read by the extend CLI; the device needs the steps inline with --steps '<json>'")
                a.value("--on-error"); a.value("--max-steps")
                a.noUnknownFlags()
                Cmd.Batch(steps ?: throw CommandFailure.invalid("Usage: batch --steps '<json array of {\"command\":…,\"args\":[…]}>'"))
            }
            "replay" -> {
                if (a.has("--from") || a.has("--plan-digest")) throw CommandFailure.unsupported("replay --from/--plan-digest (resuming a divergence) isn't supported on Android yet; replay the whole script.")
                if (a.has("--maestro")) throw CommandFailure.unsupported("Maestro flows aren't supported on the Android app; use .ad scripts.")
                val keep = a.flag("--keep-session")
                a.flag("-u"); a.flag("--update")
                a.noUnknownFlags()
                Cmd.Replay(a.positional(0), keep)
            }
            "test" -> {
                a.value("--retries"); a.value("--artifacts-dir"); a.value("--platform")
                a.noUnknownFlags()
                Cmd.TestSuite(a.positionals)
            }
            else -> throw CommandFailure.unsupported(
                "\"$command\" isn't a command the Android app knows. Commands: ${KNOWN.sorted().joinToString(", ")}.",
            )
        }
    }

    val TV_BUTTONS = listOf("up", "down", "left", "right", "select", "back", "home", "menu", "play-pause", "volume-up", "volume-down", "mute", "power")

    val KNOWN = setOf(
        "snapshot", "diff", "get", "find", "is", "wait", "screenshot", "record", "click", "press", "longpress", "fill",
        "type", "focus", "scroll", "swipe", "gesture", "back", "home", "app-switcher", "tv-remote", "keyboard",
        "clipboard", "open", "close", "apps", "appstate", "alert", "notifications", "display", "batch", "replay", "test",
    )

    private val REF = Regex("^@e\\d+$")

    private fun String.isInt() = toIntOrNull() != null

    private fun noPositionals(a: Args, command: String) {
        if (a.positionals.isNotEmpty()) throw CommandFailure.invalid("$command takes no arguments, got ${a.positionals}")
    }

    private fun parseSnapshot(a: Args, diffAlias: Boolean): Cmd {
        val interactive = a.flag("-i") or a.flag("--interactive")
        val depth = (a.value("-d") ?: a.value("--depth"))?.let {
            it.toIntOrNull()?.takeIf { d -> d >= 0 } ?: throw CommandFailure.invalid("-d takes a depth (0 or more), got \"$it\"")
        }
        val scope = a.value("-s") ?: a.value("--scope")
        val raw = a.flag("--raw")
        val diff = a.flag("--diff") || diffAlias
        if (a.flag("--actions")) {
            throw CommandFailure.unsupported("--actions names iOS custom accessibility actions; agent-device rejects it on Android and so does this app.")
        }
        a.flag("-c"); a.flag("--compact")
        a.noUnknownFlags()
        if (a.positionals.isNotEmpty()) throw CommandFailure.invalid("snapshot takes only flags, got ${a.positionals}")
        return Cmd.Snapshot(SnapshotOptions(interactive, depth, scope, raw), diff)
    }

    private val LOCATORS = setOf("text", "label", "value", "role", "id")

    private fun parseFind(a: Args): Cmd {
        val pick = when {
            a.flag("--first") -> "first"
            a.flag("--last") -> "last"
            else -> null
        }
        a.noUnknownFlags()
        var p = a.positionals
        if (p.isEmpty()) throw CommandFailure.invalid("Usage: find <text> [click|fill <text>|list] | find label|role|text|id|value <value> <action>")
        var locator = "any"
        if (p.size >= 2 && p[0] in LOCATORS) {
            locator = p[0]
            p = p.drop(1)
        }
        val query = p[0]
        val rest = p.drop(1)
        val actionToken = rest.firstOrNull()?.lowercase()?.let { if (it == "press" || it == "tap") "click" else it } ?: "click"
        return when (actionToken) {
            "click", "list", "focus", "exists" -> {
                if (rest.size > 1) throw CommandFailure.invalid("find … $actionToken takes nothing after it, got ${rest.drop(1)}")
                Cmd.Find(locator, query, actionToken, null, null, pick)
            }
            "fill", "type" -> {
                if (rest.size < 2) throw CommandFailure.invalid("find … $actionToken needs the text to type")
                Cmd.Find(locator, query, actionToken, rest.drop(1).joinToString(" "), null, pick)
            }
            "wait" -> Cmd.Find(locator, query, "wait", null, rest.getOrNull(1)?.let { it.toLongOrNull() ?: throw CommandFailure.invalid("find … wait takes milliseconds") } ?: 10_000, pick)
            "get" -> {
                val what = rest.getOrNull(1)
                if (what != "text" && what != "attrs") throw CommandFailure.invalid("find … get takes text or attrs")
                Cmd.Find(locator, query, "get_$what", null, null, pick)
            }
            "longpress", "swipe" -> throw CommandFailure.invalid("find has no $actionToken form; use find … list, then longpress/swipe on the element's rect")
            else -> throw CommandFailure.invalid("Unknown find action \"$actionToken\"; actions are click, list, focus, fill, type, exists, wait, get text, get attrs")
        }
    }

    private val PREDICATES = setOf("visible", "hidden", "exists", "absent", "editable", "selected", "focused", "text")

    private fun parseIs(a: Args): Cmd {
        a.noUnknownFlags()
        val pred = a.positional(0) ?: throw CommandFailure.invalid("Usage: is visible|hidden|exists|absent|editable|selected|focused|text <selector> [value]")
        if (pred !in PREDICATES) throw CommandFailure.invalid("Unknown predicate \"$pred\"; predicates are ${PREDICATES.joinToString(", ")}")
        val rest = a.positionals.drop(1)
        if (rest.isEmpty()) throw CommandFailure.invalid("is $pred needs a selector")
        if (rest[0].startsWith("@")) throw CommandFailure.invalid("is takes a selector expression, not a ref like ${rest[0]} (as in agent-device); e.g. is $pred 'label=\"Continue\"'")
        val split = Selectors.splitFromArgs(rest, preferTrailingValue = pred == "text")
            ?: throw CommandFailure.invalid("\"${rest.joinToString(" ")}\" isn't a selector; e.g. 'role=button label=\"Continue\"' or 'id=com.example:id/login'")
        val (chain, remaining) = split
        if (pred == "text") {
            if (remaining.isEmpty()) throw CommandFailure.invalid("is text needs the expected text after the selector")
            return Cmd.Is(pred, chain, remaining.joinToString(" "))
        }
        if (remaining.isNotEmpty()) throw CommandFailure.invalid("Unexpected ${remaining} after the selector")
        return Cmd.Is(pred, chain, null)
    }

    private fun parseWait(a: Args): Cmd {
        a.flag("--stable")
        a.noUnknownFlags()
        val p = a.positionals
        if (p.isEmpty()) throw CommandFailure.invalid("Usage: wait <ms> | wait text <text> [ms] | wait <@ref|selector> [ms] | wait absent <selector> [ms]")
        if (p.size == 1 && p[0].toLongOrNull() != null) {
            val ms = p[0].toLong()
            if (ms < 0) throw CommandFailure.invalid("wait takes a positive number of milliseconds")
            return Cmd.Wait.Duration(ms)
        }
        fun trailingMs(tokens: List<String>): Pair<List<String>, Long?> {
            val last = tokens.lastOrNull()
            return if (tokens.size > 1 && last?.toLongOrNull() != null) tokens.dropLast(1) to last.toLong() else tokens to null
        }
        return when (p[0]) {
            "text" -> {
                val (t, ms) = trailingMs(p.drop(1))
                if (t.isEmpty()) throw CommandFailure.invalid("wait text needs the text to wait for")
                Cmd.Wait.ForText(t.joinToString(" "), ms)
            }
            "absent" -> {
                val (t, ms) = trailingMs(p.drop(1))
                val chain = Selectors.splitFromArgs(t)?.takeIf { it.second.isEmpty() }?.first
                    ?: throw CommandFailure.invalid("wait absent takes a selector, e.g. wait absent 'label=\"Loading...\"' 5000")
                Cmd.Wait.Absent(chain, ms)
            }
            else -> {
                val (t, ms) = trailingMs(p)
                Cmd.Wait.ForTarget(target(t, "wait"), ms)
            }
        }
    }

    private fun parseGesture(a: Args): Cmd {
        val pointers = a.int("--pointer-count") ?: 1
        a.noUnknownFlags()
        val p = a.positionals
        fun ints(from: Int, n: Int, usage: String): List<Int> {
            if (p.size < from + n) throw CommandFailure.invalid("Usage: $usage")
            return (from until from + n).map { p[it].toIntOrNull() ?: throw CommandFailure.invalid("Usage: $usage (\"${p[it]}\" isn't a number)") }
        }
        return when (p.firstOrNull()) {
            "pan" -> {
                val usage = "gesture pan <x> <y> <dx> <dy> [durationMs] [--pointer-count 1|2]"
                val v = ints(1, 4, usage)
                if (pointers !in 1..2) throw CommandFailure.invalid("--pointer-count must be 1 or 2")
                Cmd.Gesture.Pan(v[0], v[1], v[2], v[3], p.getOrNull(5)?.toLongOrNull() ?: 500, pointers)
            }
            "fling" -> {
                val usage = "gesture fling up|down|left|right <x> <y> [distance]"
                val dir = p.getOrNull(1)?.uppercase()?.let { d -> Direction.entries.firstOrNull { it.name == d } }
                    ?: throw CommandFailure.invalid("Usage: $usage")
                val v = ints(2, 2, usage)
                Cmd.Gesture.Fling(dir, v[0], v[1], p.getOrNull(4)?.toIntOrNull())
            }
            "pinch" -> {
                val scale = p.getOrNull(1)?.toFloatOrNull()?.takeIf { it > 0f } ?: throw CommandFailure.invalid("Usage: gesture pinch <scale> [x y]")
                Cmd.Gesture.Pinch(scale, p.getOrNull(2)?.toIntOrNull(), p.getOrNull(3)?.toIntOrNull())
            }
            "rotate" -> {
                val deg = p.getOrNull(1)?.toFloatOrNull() ?: throw CommandFailure.invalid("Usage: gesture rotate <degrees> [x y]")
                Cmd.Gesture.Rotate(deg, p.getOrNull(2)?.toIntOrNull(), p.getOrNull(3)?.toIntOrNull())
            }
            "drag" -> {
                val rest = p.drop(1)
                val (from, to, afterTo) = twoTargets(rest, "gesture drag")
                val nums = afterTo.map { it.toLongOrNull() ?: throw CommandFailure.invalid("gesture drag timings must be milliseconds, got \"$it\"") }
                val hold = nums.getOrNull(0) ?: 800
                val move = nums.getOrNull(1) ?: 500
                val drop = nums.getOrNull(2) ?: 0
                if (hold + move + drop > 10_000) throw CommandFailure.invalid("gesture drag is capped at 10000 ms in total")
                Cmd.Gesture.Drag(from, to, hold, move, drop)
            }
            "transform" -> throw CommandFailure.unsupported("gesture transform isn't implemented on the Android app yet; use gesture pan, pinch and rotate separately.")
            else -> throw CommandFailure.invalid("Usage: gesture pan|fling|drag|pinch|rotate …")
        }
    }

    private fun parseDisplay(a: Args): Cmd {
        val action = a.positional(0) ?: throw CommandFailure.invalid("Usage: display show --url|--image|--video|--text <value> | display clear")
        return when (action) {
            "clear" -> { a.noUnknownFlags(); Cmd.Display.Clear }
            "show" -> {
                val options = listOf("--url", "--image", "--video", "--text").mapNotNull { f -> a.value(f)?.let { f.removePrefix("--") to it } }
                a.noUnknownFlags()
                if (options.size != 1) throw CommandFailure.invalid("display show takes exactly one of --url, --image, --video, --text")
                val extra = a.positionals.drop(1)
                val (kind, value) = options[0]
                // `--text` may arrive split over several tokens.
                Cmd.Display.Show(kind, if (kind == "text" && extra.isNotEmpty()) (listOf(value) + extra).joinToString(" ") else value)
            }
            else -> throw CommandFailure.invalid("display takes show or clear, not \"$action\"")
        }
    }

    private fun parseSelectorArg(expr: String): SelectorChain = try {
        Selectors.parse(expr)
    } catch (e: SelectorException) {
        throw CommandFailure.invalid(e.message ?: "Invalid selector")
    }

    /** One target from all of [tokens]. */
    fun target(tokens: List<String>, command: String): Target {
        val (t, rest) = targetWithRest(tokens, command)
        if (rest.isNotEmpty()) throw CommandFailure.invalid("Unexpected ${rest} after the target of $command")
        return t
    }

    /**
     * Two targets in a row (`gesture drag <from> <to>`): the first is the shortest prefix that
     * leaves a valid second target, so two adjacent selectors split correctly.
     */
    fun twoTargets(tokens: List<String>, command: String): Triple<Target, Target, List<String>> {
        var lastError: CommandFailure? = null
        for (i in 1..tokens.size) {
            val first = try {
                targetWithRest(tokens.subList(0, i), command).takeIf { it.second.isEmpty() }?.first
            } catch (e: CommandFailure) {
                lastError = e
                null
            } ?: continue
            try {
                val (second, rest) = targetWithRest(tokens.subList(i, tokens.size), command)
                return Triple(first, second, rest)
            } catch (e: CommandFailure) {
                lastError = e
            }
        }
        throw lastError ?: CommandFailure.invalid("$command needs two targets")
    }

    /** A target from the front of [tokens], and what follows it. */
    fun targetWithRest(tokens: List<String>, command: String): Pair<Target, List<String>> {
        if (tokens.isEmpty()) throw CommandFailure.invalid("$command needs a target: @ref, a selector like 'label=\"Continue\"', or x y")
        val first = tokens[0]
        if (first.startsWith("@")) {
            if (!first.matches(REF)) throw CommandFailure.invalid("\"$first\" isn't a ref; refs look like @e12")
            return Target.Ref(first) to tokens.drop(1)
        }
        if (tokens.size >= 2 && first.isInt() && tokens[1].isInt()) {
            return Target.Point(first.toInt(), tokens[1].toInt()) to tokens.drop(2)
        }
        val split = try {
            Selectors.splitFromArgs(tokens, preferTrailingValue = command == "fill")
        } catch (e: SelectorException) {
            throw CommandFailure.invalid(e.message ?: "Invalid selector")
        }
        if (split != null) return Target.Sel(split.first) to split.second
        // A whole expression passed as one token, e.g. 'role="button" label="Go"'.
        Selectors.tryParse(first)?.let { return Target.Sel(it) to tokens.drop(1) }
        throw CommandFailure.invalid(
            "\"$first\" isn't a ref, a selector or coordinates. Use @eN from snapshot, a selector like 'label=\"$first\"', " +
                "or find \"$first\" click.",
        )
    }

    /**
     * Flags and positionals. Flags that take a value are declared as the command reads them;
     * anything left over that looks like a flag is an error. A token like `-80` is a number, not
     * a flag.
     */
    class Args private constructor(private val tokens: List<String>) {
        private val consumed = BooleanArray(tokens.size)

        init {
            // Global flags are consumed up front.
            var i = 0
            while (i < tokens.size) {
                val t = tokens[i]
                if (t in GLOBAL_BOOLEAN_FLAGS) consumed[i] = true
                if (t in GLOBAL_VALUE_FLAGS) {
                    consumed[i] = true
                    if (i + 1 < tokens.size) consumed[i + 1] = true
                    i++
                }
                if (GLOBAL_VALUE_FLAGS.any { t.startsWith("$it=") }) consumed[i] = true
                i++
            }
        }

        private fun isFlagToken(t: String) = t.startsWith("-") && t.length > 1 && t.toDoubleOrNull() == null

        val positionals: List<String>
            get() = tokens.indices.filter { !consumed[it] && !isFlagToken(tokens[it]) }.map { tokens[it] }

        fun positional(i: Int): String? = positionals.getOrNull(i)

        fun has(name: String): Boolean = tokens.indices.any { !consumed[it] && (tokens[it] == name || tokens[it].startsWith("$name=")) }

        fun flag(name: String): Boolean {
            var found = false
            for (i in tokens.indices) if (!consumed[i] && tokens[i] == name) {
                consumed[i] = true
                found = true
            }
            return found
        }

        fun value(name: String): String? {
            for (i in tokens.indices) {
                if (consumed[i]) continue
                val t = tokens[i]
                if (t.startsWith("$name=")) {
                    consumed[i] = true
                    return t.substring(name.length + 1)
                }
                if (t == name) {
                    consumed[i] = true
                    if (i + 1 >= tokens.size) throw CommandFailure.invalid("$name needs a value")
                    consumed[i + 1] = true
                    return tokens[i + 1]
                }
            }
            return null
        }

        fun int(name: String): Int? = value(name)?.let { it.toIntOrNull() ?: throw CommandFailure.invalid("$name takes a whole number, got \"$it\"") }
        fun long(name: String): Long? = value(name)?.let { it.toLongOrNull() ?: throw CommandFailure.invalid("$name takes milliseconds, got \"$it\"") }
        fun float(name: String): Float? = value(name)?.let { it.toFloatOrNull() ?: throw CommandFailure.invalid("$name takes a number, got \"$it\"") }

        fun noUnknownFlags() {
            val unknown = tokens.indices.filter { !consumed[it] && isFlagToken(tokens[it]) }.map { tokens[it] }
            if (unknown.isNotEmpty()) throw CommandFailure.invalid("Unknown flag${if (unknown.size > 1) "s" else ""} ${unknown.joinToString(", ")}")
        }

        fun dropPositional(n: Int): Args {
            val keep = ArrayList<String>()
            var dropped = 0
            for (i in tokens.indices) {
                if (!consumed[i] && !isFlagToken(tokens[i]) && dropped < n) {
                    dropped++
                    continue
                }
                keep += tokens[i]
            }
            return of(keep)
        }

        companion object {
            fun of(tokens: List<String>) = Args(tokens)
        }
    }
}

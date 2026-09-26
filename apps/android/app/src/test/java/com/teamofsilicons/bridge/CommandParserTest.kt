package com.teamofsilicons.bridge

import com.teamofsilicons.bridge.driver.Cmd
import com.teamofsilicons.bridge.driver.CommandFailure
import com.teamofsilicons.bridge.driver.CommandParser
import com.teamofsilicons.bridge.driver.Direction
import com.teamofsilicons.bridge.driver.SnapshotOptions
import com.teamofsilicons.bridge.driver.Target
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/** Argument parsing for every command the Android app accepts, and refusals for the rest. */
class CommandParserTest {
    private fun p(command: String, vararg args: String): Cmd = CommandParser.parse(command, args.toList())

    private fun fails(code: String, command: String, vararg args: String): String {
        try {
            CommandParser.parse(command, args.toList())
        } catch (f: CommandFailure) {
            assertEquals("${f.message}", code, f.code)
            return f.message ?: ""
        }
        fail("$command ${args.toList()} should fail with $code")
        return ""
    }

    private fun sel(cmd: Cmd, get: (Cmd) -> Target): String = (get(cmd) as Target.Sel).chain.raw

    @Test
    fun snapshot() {
        assertEquals(Cmd.Snapshot(SnapshotOptions(), false), p("snapshot"))
        assertEquals(Cmd.Snapshot(SnapshotOptions(interactive = true), false), p("snapshot", "-i"))
        assertEquals(Cmd.Snapshot(SnapshotOptions(true, 3, "Contacts", false), false), p("snapshot", "-i", "-d", "3", "-s", "Contacts"))
        assertEquals(Cmd.Snapshot(SnapshotOptions(depth = 2, scope = "@e4"), false), p("snapshot", "--depth", "2", "--scope", "@e4"))
        assertEquals(Cmd.Snapshot(SnapshotOptions(raw = true), true), p("snapshot", "--raw", "--diff"))
        assertEquals(Cmd.Snapshot(SnapshotOptions(interactive = true), false), p("snapshot", "-i", "--json", "--force-full"))
        assertEquals(Cmd.Snapshot(SnapshotOptions(interactive = true), false), p("snapshot", "-i", "--timeout", "5000"))
        fails("invalid_args", "snapshot", "-d", "x")
        fails("invalid_args", "snapshot", "-d")
        fails("invalid_args", "snapshot", "--bogus")
        fails("invalid_args", "snapshot", "extra")
        fails("unsupported_on_device", "snapshot", "--actions")
    }

    @Test
    fun diff() {
        assertEquals(Cmd.Snapshot(SnapshotOptions(interactive = true), true), p("diff", "snapshot", "-i"))
        fails("unsupported_on_device", "diff", "screenshot", "--baseline", "x")
        fails("invalid_args", "diff")
        fails("invalid_args", "diff", "video")
    }

    @Test
    fun get() {
        assertEquals(Cmd.Get("text", Target.Ref("@e1")), p("get", "text", "@e1"))
        val attrs = p("get", "attrs", "label=\"Email\"") as Cmd.Get
        assertEquals("label=\"Email\"", (attrs.target as Target.Sel).chain.raw)
        fails("invalid_args", "get", "value", "@e1")
        fails("invalid_args", "get", "text")
        fails("invalid_args", "get")
    }

    @Test
    fun find() {
        assertEquals(Cmd.Find("any", "Settings", "click", null, null, null), p("find", "Settings", "click"))
        assertEquals(Cmd.Find("any", "Settings", "click", null, null, null), p("find", "Settings"))
        assertEquals(Cmd.Find("any", "Sign In", "click", null, null, null), p("find", "Sign In", "tap"))
        assertEquals(Cmd.Find("text", "Sign In", "click", null, null, null), p("find", "text", "Sign In", "click"))
        assertEquals(Cmd.Find("label", "Email", "fill", "user@example.com", null, null), p("find", "label", "Email", "fill", "user@example.com"))
        assertEquals(Cmd.Find("role", "button", "click", null, null, null), p("find", "role", "button", "click"))
        assertEquals(Cmd.Find("id", "com.example:id/login", "click", null, null, "first"), p("find", "id", "com.example:id/login", "click", "--first"))
        assertEquals(Cmd.Find("any", "Follow", "list", null, null, null), p("find", "Follow", "list"))
        assertEquals(Cmd.Find("any", "Done", "wait", null, 3000, null), p("find", "Done", "wait", "3000"))
        assertEquals(Cmd.Find("any", "Title", "get_text", null, null, "last"), p("find", "Title", "get", "text", "--last"))
        assertEquals(Cmd.Find("any", "x", "type", "hello world", null, null), p("find", "x", "type", "hello", "world"))
        // A lone locator word is a query, not a locator.
        assertEquals(Cmd.Find("any", "text", "click", null, null, null), p("find", "text"))
        fails("invalid_args", "find")
        fails("invalid_args", "find", "x", "fill")
        fails("invalid_args", "find", "x", "longpress")
        fails("invalid_args", "find", "x", "explode")
    }

    @Test
    fun isPredicates() {
        val v = p("is", "visible", "role=\"button\"", "label=\"Continue\"") as Cmd.Is
        assertEquals("visible", v.predicate)
        assertEquals("role=\"button\" label=\"Continue\"", v.chain.raw)
        val t = p("is", "text", "id=\"greeting\"", "Welcome back") as Cmd.Is
        assertEquals("id=\"greeting\"", t.chain.raw)
        assertEquals("Welcome back", t.expected)
        for (pred in listOf("hidden", "exists", "absent", "editable", "selected", "focused")) {
            assertEquals(pred, (p("is", pred, "label=Wi-Fi") as Cmd.Is).predicate)
        }
        fails("invalid_args", "is", "visible", "@e3")
        fails("invalid_args", "is", "shiny", "label=x")
        fails("invalid_args", "is", "text", "label=x")
        fails("invalid_args", "is", "visible", "Continue")
        fails("invalid_args", "is", "visible")
    }

    @Test
    fun waitForms() {
        assertEquals(Cmd.Wait.Duration(1500), p("wait", "1500"))
        assertEquals(Cmd.Wait.ForText("Welcome back", null), p("wait", "text", "Welcome back"))
        assertEquals(Cmd.Wait.ForText("Welcome back", 4000), p("wait", "text", "Welcome", "back", "4000"))
        assertEquals(Cmd.Wait.ForTarget(Target.Ref("@e12"), null), p("wait", "@e12"))
        val w = p("wait", "role=\"button\" label=\"Continue\"", "5000") as Cmd.Wait.ForTarget
        assertEquals(5000L, w.timeoutMs)
        assertEquals("role=\"button\" label=\"Continue\"", (w.target as Target.Sel).chain.raw)
        val a = p("wait", "absent", "label=\"Loading...\"", "5000") as Cmd.Wait.Absent
        assertEquals("label=\"Loading...\"", a.chain.raw)
        assertEquals(5000L, a.timeoutMs)
        fails("invalid_args", "wait")
        fails("invalid_args", "wait", "-5")
        fails("invalid_args", "wait", "text")
        fails("invalid_args", "wait", "absent", "Loading")
    }

    @Test
    fun screenshot() {
        assertEquals(Cmd.Screenshot(null, null, false, null), p("screenshot"))
        assertEquals(Cmd.Screenshot("home.png", 0.5f, true, null), p("screenshot", "home.png", "--scale", "0.5", "--overlay-refs"))
        val c = p("screenshot", "--crop-on", "label=\"Card\"", "--fullscreen") as Cmd.Screenshot
        assertEquals("label=\"Card\"", c.cropOn!!.raw)
        fails("invalid_args", "screenshot", "--scale", "2")
        fails("invalid_args", "screenshot", "--scale", "0")
        fails("invalid_args", "screenshot", "a", "b")
        fails("invalid_args", "screenshot", "--crop-on", "nonsense")
    }

    @Test
    fun recordIsMissing() {
        val msg = fails("unsupported_on_device", "record", "start")
        assertTrue(msg.contains("MediaProjection"))
        fails("unsupported_on_device", "record", "stop")
    }

    @Test
    fun clickAndPress() {
        assertEquals(Cmd.Click(Target.Ref("@e2")), p("click", "@e2"))
        assertEquals(Cmd.Click(Target.Ref("@e2")), p("click", "@e2", "--button", "primary"))
        assertEquals("label=\"Continue\"", sel(p("click", "label=\"Continue\"")) { (it as Cmd.Click).target })
        assertEquals("text=Continue", sel(p("click", "text=Continue")) { (it as Cmd.Click).target })
        assertEquals("id=com.android.settings:id/search", sel(p("click", "id=com.android.settings:id/search")) { (it as Cmd.Click).target })
        assertEquals("role=button label=\"Go\"", sel(p("click", "role=button", "label=\"Go\"")) { (it as Cmd.Click).target })
        assertEquals("role=\"button\" label=\"Go\"", sel(p("click", "role=\"button\" label=\"Go\"")) { (it as Cmd.Click).target })
        assertEquals(Cmd.Click(Target.Point(300, 500)), p("click", "300", "500"))
        assertEquals(Cmd.Click(Target.Point(300, 500)), p("press", "300", "500"))
        assertEquals(Cmd.Click(Target.Point(300, 500), 12, null, 45), p("press", "300", "500", "--count", "12", "--interval-ms", "45"))
        assertEquals(Cmd.Click(Target.Point(300, 500), 6, 120, 30), p("press", "300", "500", "--count", "6", "--hold-ms", "120", "--interval-ms", "30", "--jitter-px", "2"))
        assertEquals(Cmd.Click(Target.Ref("@e4")), p("press", "@e4"))
        fails("unsupported_on_device", "click", "@e1", "--button", "secondary")
        fails("invalid_args", "click")
        fails("invalid_args", "click", "@x1")
        fails("invalid_args", "click", "Continue")
        fails("invalid_args", "click", "@e1", "@e2")
        fails("invalid_args", "press", "1", "2", "--count", "0")
        fails("invalid_args", "press", "1", "2", "--count", "abc")
    }

    @Test
    fun longpress() {
        assertEquals(Cmd.LongPress(Target.Point(300, 500), 800), p("longpress", "300", "500"))
        assertEquals(Cmd.LongPress(Target.Point(300, 500), 1200), p("longpress", "300", "500", "1200"))
        assertEquals(Cmd.LongPress(Target.Ref("@e3"), 800), p("longpress", "@e3"))
        assertEquals(Cmd.LongPress(Target.Ref("@e3"), 1500), p("longpress", "@e3", "1500"))
        fails("invalid_args", "longpress", "300", "500", "slow")
        fails("invalid_args", "longpress")
    }

    @Test
    fun fillTypeFocus() {
        assertEquals(Cmd.Fill(Target.Ref("@e2"), "text", null), p("fill", "@e2", "text"))
        assertEquals(Cmd.Fill(Target.Ref("@e2"), "search", 80), p("fill", "@e2", "search", "--delay-ms", "80"))
        assertEquals(Cmd.Fill(Target.Ref("@e7"), "On my way", null), p("fill", "@e7", "On my way"))
        assertEquals(Cmd.Fill(Target.Ref("@e7"), "On my way", null), p("fill", "@e7", "On", "my", "way"))
        val f = p("fill", "label=\"Email\"", "user@example.com") as Cmd.Fill
        assertEquals("label=\"Email\"", (f.target as Target.Sel).chain.raw)
        assertEquals("user@example.com", f.text)
        assertEquals(Cmd.Fill(Target.Point(100, 200), "hi", null), p("fill", "100", "200", "hi"))
        assertEquals(Cmd.Type("text", null), p("type", "text"))
        assertEquals(Cmd.Type("query here", 80), p("type", "query", "here", "--delay-ms", "80"))
        assertEquals(Cmd.Focus(Target.Ref("@e2")), p("focus", "@e2"))
        fails("invalid_args", "fill", "@e2")
        fails("invalid_args", "fill")
        fails("invalid_args", "type")
        fails("invalid_args", "type", "@e3")
        fails("invalid_args", "focus")
    }

    @Test
    fun scrollAndSwipe() {
        assertEquals(Cmd.Scroll("down", null, null), p("scroll", "down"))
        assertEquals(Cmd.Scroll("down", 0.5f, null), p("scroll", "down", "0.5"))
        assertEquals(Cmd.Scroll("up", null, 320), p("scroll", "up", "--pixels", "320"))
        assertEquals(Cmd.Scroll("left", null, null), p("scroll", "LEFT"))
        assertEquals(Cmd.Scroll("top", null, null), p("scroll", "top"))
        fails("invalid_args", "scroll")
        fails("invalid_args", "scroll", "sideways")
        fails("invalid_args", "scroll", "down", "2")
        fails("invalid_args", "scroll", "down", "--pixels", "-5")
        assertEquals(Cmd.Swipe(540, 1500, 540, 500, 250, 1, 0, false), p("swipe", "540", "1500", "540", "500"))
        assertEquals(Cmd.Swipe(540, 1500, 540, 500, 250, 8, 30, true), p("swipe", "540", "1500", "540", "500", "--count", "8", "--pause-ms", "30", "--pattern", "ping-pong"))
        fails("invalid_args", "swipe", "1", "2", "3")
        fails("invalid_args", "swipe", "1", "2", "3", "x")
        fails("invalid_args", "swipe", "1", "2", "3", "4", "--count", "500")
        fails("invalid_args", "swipe", "1", "2", "3", "4", "--pattern", "zigzag")
    }

    @Test
    fun gestures() {
        assertEquals(Cmd.Gesture.Pan(200, 420, 0, -80, 500, 1), p("gesture", "pan", "200", "420", "0", "-80", "500"))
        assertEquals(Cmd.Gesture.Pan(200, 420, 80, -40, 700, 2), p("gesture", "pan", "200", "420", "80", "-40", "700", "--pointer-count", "2"))
        assertEquals(Cmd.Gesture.Fling(Direction.RIGHT, 200, 420, 180), p("gesture", "fling", "right", "200", "420", "180"))
        assertEquals(Cmd.Gesture.Pinch(2.0f, null, null), p("gesture", "pinch", "2.0"))
        assertEquals(Cmd.Gesture.Pinch(0.5f, 200, 400), p("gesture", "pinch", "0.5", "200", "400"))
        assertEquals(Cmd.Gesture.Rotate(35f, 200, 420), p("gesture", "rotate", "35", "200", "420"))
        val d = p("gesture", "drag", "id=\"drag-source\"", "id=\"drop-target\"") as Cmd.Gesture.Drag
        assertEquals("id=\"drag-source\"", (d.from as Target.Sel).chain.raw)
        assertEquals("id=\"drop-target\"", (d.to as Target.Sel).chain.raw)
        assertEquals(800L, d.holdMs)
        val d2 = p("gesture", "drag", "@e4", "label=\"Archive\"", "700", "600", "200") as Cmd.Gesture.Drag
        assertEquals(Target.Ref("@e4"), d2.from)
        assertEquals(Triple(700L, 600L, 200L), Triple(d2.holdMs, d2.moveMs, d2.dropHoldMs))
        fails("invalid_args", "gesture", "pan", "1", "2")
        fails("invalid_args", "gesture", "fling", "sideways", "1", "2")
        fails("invalid_args", "gesture", "pinch")
        fails("invalid_args", "gesture", "drag", "@e1", "@e2", "9000", "9000")
        fails("unsupported_on_device", "gesture", "transform", "1", "2", "3", "4", "2", "35")
        fails("invalid_args", "gesture")
    }

    @Test
    fun navigation() {
        assertEquals(Cmd.Back, p("back"))
        assertEquals(Cmd.Back, p("back", "--system"))
        assertEquals(Cmd.Back, p("back", "--in-app"))
        assertEquals(Cmd.Home, p("home"))
        assertEquals(Cmd.AppSwitcher, p("app-switcher"))
        fails("invalid_args", "home", "now")
        fails("invalid_args", "back", "--wat")
    }

    @Test
    fun tvRemote() {
        assertEquals(Cmd.TvRemote(false, "down", null), p("tv-remote", "press", "down"))
        assertEquals(Cmd.TvRemote(true, "select", null), p("tv-remote", "longpress", "select"))
        assertEquals(Cmd.TvRemote(true, "select", 900), p("tv-remote", "press", "select", "--duration-ms", "900"))
        for (b in CommandParser.TV_BUTTONS) assertEquals(b, (p("tv-remote", "press", b) as Cmd.TvRemote).button)
        fails("invalid_args", "tv-remote", "press", "turbo")
        fails("invalid_args", "tv-remote", "tap", "up")
        fails("invalid_args", "tv-remote", "press")
        fails("invalid_args", "tv-remote")
    }

    @Test
    fun keyboardAndClipboard() {
        assertEquals(Cmd.Keyboard("status"), p("keyboard", "status"))
        assertEquals(Cmd.Keyboard("status"), p("keyboard", "get"))
        assertEquals(Cmd.Keyboard("status"), p("keyboard"))
        assertEquals(Cmd.Keyboard("dismiss"), p("keyboard", "dismiss"))
        fails("invalid_args", "keyboard", "show")
        assertEquals(Cmd.Clipboard(null), p("clipboard", "read"))
        assertEquals(Cmd.Clipboard("https://example.com"), p("clipboard", "write", "https://example.com"))
        assertEquals(Cmd.Clipboard(""), p("clipboard", "write", ""))
        assertEquals(Cmd.Clipboard("two words"), p("clipboard", "write", "two", "words"))
        fails("invalid_args", "clipboard", "write")
        fails("invalid_args", "clipboard")
        fails("invalid_args", "clipboard", "paste")
    }

    @Test
    fun apps() {
        assertEquals(Cmd.Open("com.whatsapp", null), p("open", "com.whatsapp"))
        assertEquals(Cmd.Open("Settings", null), p("open", "Settings"))
        assertEquals(Cmd.Open("https://example.com", null), p("open", "https://example.com"))
        assertEquals(Cmd.Open("com.example.myapp", "myapp://screen/to"), p("open", "com.example.myapp", "myapp://screen/to", "--relaunch"))
        assertEquals(Cmd.Open("YouTube", null), p("open", "YouTube", "--surface", "app"))
        fails("unsupported_on_device", "open", "Finder", "--surface", "desktop")
        fails("invalid_args", "open")
        fails("invalid_args", "open", "a", "b", "c")
        assertEquals(Cmd.Close(null), p("close"))
        assertEquals(Cmd.Close("com.whatsapp"), p("close", "com.whatsapp"))
        fails("unsupported_on_device", "close", "--save-script")
        assertEquals(Cmd.Apps(false), p("apps"))
        assertEquals(Cmd.Apps(true), p("apps", "--all"))
        assertEquals(Cmd.AppState, p("appstate"))
        assertTrue(fails("unsupported_on_device", "install", "com.x", "app.apk").contains("wireless-debugging bridge"))
        fails("unsupported_on_device", "reinstall", "com.x", "app.apk")
    }

    @Test
    fun alertsNotificationsDisplay() {
        assertEquals(Cmd.Alert("get", null), p("alert"))
        assertEquals(Cmd.Alert("get", null), p("alert", "get"))
        assertEquals(Cmd.Alert("accept", null), p("alert", "accept"))
        assertEquals(Cmd.Alert("dismiss", null), p("alert", "dismiss"))
        assertEquals(Cmd.Alert("wait", 3000), p("alert", "wait", "3000"))
        assertEquals(Cmd.Alert("wait", 5000), p("alert", "wait"))
        fails("invalid_args", "alert", "wait", "soon")
        fails("invalid_args", "alert", "close")
        assertEquals(Cmd.Notifications, p("notifications"))
        assertEquals(Cmd.Notifications, p("notifications", "--json"))
        assertEquals(Cmd.Display.Show("url", "https://example.com"), p("display", "show", "--url", "https://example.com"))
        assertEquals(Cmd.Display.Show("image", "/data/cat.png"), p("display", "show", "--image", "/data/cat.png"))
        assertEquals(Cmd.Display.Show("video", "https://x/v.mp4"), p("display", "show", "--video", "https://x/v.mp4"))
        assertEquals(Cmd.Display.Show("text", "Dinner is ready"), p("display", "show", "--text", "Dinner is ready"))
        assertEquals(Cmd.Display.Show("text", "Dinner is ready"), p("display", "show", "--text", "Dinner", "is", "ready"))
        assertEquals(Cmd.Display.Clear, p("display", "clear"))
        fails("invalid_args", "display", "show")
        fails("invalid_args", "display", "show", "--url", "a", "--text", "b")
        fails("invalid_args", "display", "blink")
        fails("invalid_args", "display")
    }

    @Test
    fun replayBatchTest() {
        assertEquals(Cmd.Batch("""[{"command":"home"}]"""), p("batch", "--steps", """[{"command":"home"}]"""))
        fails("invalid_args", "batch", "--steps-file", "/tmp/x.json")
        fails("invalid_args", "batch")
        assertEquals(Cmd.Replay("./session.ad", false), p("replay", "./session.ad"))
        assertEquals(Cmd.Replay("/cache/session.ad", true), p("replay", "/cache/session.ad", "--keep-session"))
        fails("unsupported_on_device", "replay", "x.ad", "--from", "4", "--plan-digest", "abc")
        fails("unsupported_on_device", "replay", "flow.yaml", "--maestro")
        assertEquals(Cmd.TestSuite(listOf("a.ad", "b.ad")), p("test", "a.ad", "b.ad", "--retries", "1"))
    }

    @Test
    fun bridgeAdditionsAndUnknown() {
        assertTrue(fails("unsupported_on_device", "adb", "shell", "dumpsys").contains("adb"))
        fails("unsupported_on_device", "logs", "start")
        assertTrue(fails("unsupported_on_device", "terminal", "run", "ls").contains("computers"))
        assertTrue(fails("unsupported_on_device", "hover", "@e1").contains("pointer"))
        assertTrue(fails("unsupported_on_device", "teleport").contains("Commands:"))
    }

    @Test
    fun ttlAndPermanentAreIgnoredEverywhere() {
        assertEquals(Cmd.Screenshot("a.png", null, false, null), p("screenshot", "a.png", "--ttl", "7d", "--permanent"))
        assertEquals(Cmd.Home, p("home", "--json"))
        assertEquals(Cmd.Screenshot(null, null, false, null), p("screenshot", "--ttl=1h"))
    }
}

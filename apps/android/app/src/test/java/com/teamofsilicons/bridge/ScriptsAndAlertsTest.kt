package com.teamofsilicons.bridge

import com.teamofsilicons.bridge.driver.Alerts
import com.teamofsilicons.bridge.driver.Bounds
import com.teamofsilicons.bridge.driver.Capture
import com.teamofsilicons.bridge.driver.CommandFailure
import com.teamofsilicons.bridge.driver.Scripts
import com.teamofsilicons.bridge.driver.Step
import com.teamofsilicons.bridge.driver.UiNode
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.fail
import org.junit.Test

class ScriptsAndAlertsTest {
    @Test
    fun shellWords() {
        assertEquals(listOf("click", "label=\"Continue\""), Scripts.shellWords("click 'label=\"Continue\"'"))
        assertEquals(listOf("fill", "@e2", "hello world"), Scripts.shellWords("fill @e2 \"hello world\""))
        assertEquals(listOf("type", "say \"hi\""), Scripts.shellWords("type \"say \\\"hi\\\"\""))
        assertEquals(listOf("a", "b c"), Scripts.shellWords("a b\\ c"))
        assertEquals(listOf("x", ""), Scripts.shellWords("x \"\""))
        try {
            Scripts.shellWords("fill 'open")
            fail()
        } catch (_: CommandFailure) {
        }
    }

    @Test
    fun adScripts() {
        val script = """
            context platform=android timeout=60000
            # Open settings and search
            open Settings
            click 'id="com.android.settings:id/search_action_bar"'

            fill 'id="android:id/search_src_text"' "wifi"
            wait text Wi-Fi 5000
            close
        """.trimIndent()
        val steps = Scripts.parseAd(script)
        assertEquals(listOf("open", "click", "fill", "wait", "close"), steps.map { it.command })
        assertEquals(Step("fill", listOf("id=\"android:id/search_src_text\"", "wifi"), 6), steps[2])
        assertEquals(listOf("text", "Wi-Fi", "5000"), steps[3].args)
    }

    @Test
    fun batchSteps() {
        val steps = Scripts.parseBatch("""[{"command":"open","args":["Settings"]},{"command":"snapshot","args":["-i"]},{"command":"home"}]""")
        assertEquals(listOf(Step("open", listOf("Settings")), Step("snapshot", listOf("-i")), Step("home", emptyList())), steps)
        val legacy = Scripts.parseBatch("""[{"command":"scroll","positionals":["down"],"flags":{"pixels":"300","json":true}}]""")
        assertEquals(Step("scroll", listOf("down", "--pixels", "300", "--json")), legacy.single())
        for (bad in listOf("{}", "[]", "not json", """[{"args":[]}]""", """[{"command":"open","input":{"app":"settings"}}]""", """[{"command":"x","args":[1]}]""")) {
            try {
                Scripts.parseBatch(bad)
                fail("should reject $bad")
            } catch (f: CommandFailure) {
                assertEquals("invalid_args", f.code)
            }
        }
    }

    private val screen = Bounds(0, 0, 1080, 2400)

    @Test
    fun permissionPrompt() {
        val dialog = UiNode(
            className = "android.widget.FrameLayout", packageName = "com.google.android.permissioncontroller", bounds = Bounds(60, 800, 1020, 1700), windowType = "application",
            children = listOf(
                UiNode(className = "android.widget.TextView", text = "Allow Maps to access this device's location?", resourceId = "com.android.permissioncontroller:id/permission_message"),
                UiNode(className = "android.widget.Button", text = "While using the app", resourceId = "com.android.permissioncontroller:id/permission_allow_foreground_only_button", clickable = true, bounds = Bounds(100, 1300, 980, 1400)),
                UiNode(className = "android.widget.Button", text = "Only this time", resourceId = "com.android.permissioncontroller:id/permission_allow_one_time_button", clickable = true, bounds = Bounds(100, 1400, 980, 1500)),
                UiNode(className = "android.widget.Button", text = "Don't allow", resourceId = "com.android.permissioncontroller:id/permission_deny_button", clickable = true, bounds = Bounds(100, 1500, 980, 1600)),
            ),
        )
        val a = Alerts.detect(Capture(listOf(UiNode(className = "android.widget.FrameLayout", bounds = screen, windowType = "application"), dialog), screen, null))
        assertNotNull(a)
        assertEquals("permission", a!!.kind)
        assertEquals("While using the app", a.accept!!.label)
        assertEquals("Don't allow", a.dismiss!!.label)
        assertEquals("Allow Maps to access this device's location?", a.title)
    }

    @Test
    fun alertDialog() {
        val root = UiNode(
            className = "android.widget.FrameLayout", bounds = screen, windowType = "application",
            children = listOf(
                UiNode(className = "android.widget.TextView", text = "Delete photo?", resourceId = "android:id/alertTitle"),
                UiNode(className = "android.widget.TextView", text = "This can't be undone.", resourceId = "android:id/message"),
                UiNode(className = "android.widget.Button", text = "Delete", resourceId = "android:id/button1", clickable = true),
                UiNode(className = "android.widget.Button", text = "Cancel", resourceId = "android:id/button2", clickable = true),
            ),
        )
        val a = Alerts.detect(Capture(listOf(root), screen, null))!!
        assertEquals("Delete photo?", a.title)
        assertEquals("This can't be undone.", a.message)
        assertEquals("Delete", a.accept!!.label)
        assertEquals("Cancel", a.dismiss!!.label)
    }

    @Test
    fun smallDialogWindowAndNoAlert() {
        val app = UiNode(className = "android.widget.FrameLayout", bounds = screen, windowType = "application")
        val small = UiNode(
            className = "android.widget.FrameLayout", bounds = Bounds(100, 900, 980, 1500), windowType = "application",
            children = listOf(
                UiNode(className = "android.widget.TextView", text = "Rate this app"),
                UiNode(className = "android.widget.Button", text = "Not now", clickable = true, bounds = Bounds(100, 1300, 500, 1400)),
                UiNode(className = "android.widget.Button", text = "OK", clickable = true, bounds = Bounds(500, 1300, 980, 1400)),
            ),
        )
        val a = Alerts.detect(Capture(listOf(app, small), screen, null))!!
        assertEquals("Rate this app", a.title)
        assertEquals("OK", a.accept!!.label)
        assertEquals("Not now", a.dismiss!!.label)
        assertNull(Alerts.detect(Capture(listOf(app), screen, null)))
    }
}

package com.teamofsilicons.extend

import com.teamofsilicons.extend.driver.Bounds
import com.teamofsilicons.extend.driver.Capture
import com.teamofsilicons.extend.driver.Roles
import com.teamofsilicons.extend.driver.SnapshotEngine
import com.teamofsilicons.extend.driver.SnapshotOptions
import com.teamofsilicons.extend.driver.UiNode
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SnapshotTest {
    private val screen = Bounds(0, 0, 1080, 2400)

    private fun row(title: String, summary: String, top: Int) = UiNode(
        className = "android.widget.LinearLayout", bounds = Bounds(0, top, 1080, top + 200), clickable = true, focusable = true,
        children = listOf(
            UiNode(className = "android.widget.ImageView", resourceId = "android:id/icon", bounds = Bounds(40, top + 60, 120, top + 140)),
            UiNode(
                className = "android.widget.RelativeLayout", bounds = Bounds(160, top, 1000, top + 200),
                children = listOf(
                    UiNode(className = "android.widget.TextView", text = title, resourceId = "android:id/title", bounds = Bounds(160, top + 40, 900, top + 100)),
                    UiNode(className = "android.widget.TextView", text = summary, resourceId = "android:id/summary", bounds = Bounds(160, top + 100, 900, top + 160)),
                ),
            ),
        ),
    )

    /** Roughly the Settings home screen: a toolbar, a search bar, a list of rows, one hidden row. */
    private fun settings(): Capture {
        val list = UiNode(
            className = "androidx.recyclerview.widget.RecyclerView", resourceId = "com.android.settings:id/recycler_view",
            bounds = Bounds(0, 500, 1080, 2400), scrollable = true, focusable = true,
            children = listOf(
                row("Network & internet", "Mobile, Wi‑Fi, hotspot", 500),
                row("Connected devices", "Bluetooth, pairing", 700),
                row("Apps", "Recent apps, default apps", 900),
                UiNode(className = "android.widget.TextView", text = "Invisible", bounds = Bounds(0, 2600, 1080, 2700), visibleToUser = false),
            ),
        )
        val root = UiNode(
            className = "android.widget.FrameLayout", packageName = "com.android.settings", bounds = screen, windowType = "application",
            children = listOf(
                UiNode(
                    className = "android.view.ViewGroup", bounds = Bounds(0, 0, 1080, 300),
                    children = listOf(UiNode(className = "android.widget.TextView", text = "Settings", bounds = Bounds(40, 100, 600, 250), heading = true)),
                ),
                UiNode(
                    className = "android.widget.LinearLayout", resourceId = "com.android.settings:id/search_action_bar", contentDescription = "Search settings",
                    bounds = Bounds(40, 320, 1040, 460), clickable = true,
                    children = listOf(UiNode(className = "android.widget.TextView", text = "Search Settings", bounds = Bounds(120, 340, 800, 440))),
                ),
                list,
            ),
        )
        return Capture(listOf(root), screen, "com.android.settings")
    }

    @Test
    fun refsAreAssignedInDocumentOrderAndResolveBack() {
        val snap = SnapshotEngine.build(settings(), SnapshotOptions())
        assertEquals((1..snap.nodes.size).map { "e$it" }, snap.nodes.map { it.ref })
        assertEquals("Settings", snap.ref("@e1")!!.label)
        assertEquals(snap.nodes[3], snap.ref("@e4"))
        assertEquals(snap.nodes[3], snap.ref("e4"))
        assertNull(snap.ref("@e999"))
        // A second snapshot starts over at e1.
        val again = SnapshotEngine.build(settings(), SnapshotOptions(interactive = true))
        assertEquals("e1", again.nodes.first().ref)
    }

    @Test
    fun defaultViewFoldsUnlabelledGroupsAndHidesInvisible() {
        val snap = SnapshotEngine.build(settings(), SnapshotOptions())
        val text = snap.text()
        assertTrue(text, text.startsWith("Snapshot: ${snap.nodes.size} visible nodes"))
        assertFalse(text.contains("Invisible"))
        assertTrue(text, text.contains("@e1 [text] \"Settings\""))
        assertTrue(text, text.contains("[group] \"Search settings\""))
        assertTrue(text, text.contains("[list]"))
        // Unlabelled layouts disappear; their labelled children are re-indented under the list.
        val network = snap.nodes.first { it.label == "Network & internet" }
        assertEquals("text", network.role)
        assertTrue(snap.nodes.none { it.role == "group" && it.label.isEmpty() })
        assertFalse(snap.truncated)
    }

    @Test
    fun interactiveKeepsTargetsAndTheirLabels() {
        val snap = SnapshotEngine.build(settings(), SnapshotOptions(interactive = true))
        val labels = snap.nodes.map { it.label }
        assertTrue(labels.toString(), labels.contains("Search settings"))
        assertTrue(labels.contains("Network & internet"))
        assertTrue(labels.contains("Mobile, Wi‑Fi, hotspot"))
        // The heading isn't interactive and has no interactive relative.
        assertFalse(labels.contains("Settings"))
        // Icons are visual-only.
        assertTrue(snap.nodes.none { it.role == "image" })
    }

    @Test
    fun depthLimitsTheTree() {
        val full = SnapshotEngine.build(settings(), SnapshotOptions())
        val shallow = SnapshotEngine.build(settings(), SnapshotOptions(depth = 0))
        assertTrue(shallow.nodes.all { it.depth == 0 })
        assertTrue(shallow.nodes.size < full.nodes.size)
    }

    @Test
    fun scopeReRootsAtTheFirstMatch() {
        val scoped = SnapshotEngine.build(settings(), SnapshotOptions(scope = "recycler_view"))
        assertEquals("list", scoped.nodes.first().role)
        assertEquals(0, scoped.nodes.first().depth)
        assertTrue(scoped.nodes.none { it.label == "Settings" })
        assertTrue(scoped.nodes.any { it.label == "Apps" })
        val none = SnapshotEngine.build(settings(), SnapshotOptions(scope = "Bluetooth settings page"))
        assertTrue(none.nodes.isEmpty())
        assertTrue(none.text().contains("nothing on screen matches scope"))
        // Scope by ref uses that ref's label from the previous snapshot.
        val prev = SnapshotEngine.build(settings(), SnapshotOptions())
        val appsRef = prev.nodes.first { it.label == "Apps" }.ref
        val byRef = SnapshotEngine.build(settings(), SnapshotOptions(scope = "@$appsRef"), prev)
        assertEquals("Apps", byRef.nodes.first().label)
    }

    @Test
    fun rawKeepsEverything() {
        val raw = SnapshotEngine.build(settings(), SnapshotOptions(raw = true))
        assertEquals(settings().allNodes.size, raw.nodes.size)
        assertTrue(raw.nodes.any { it.label == "Invisible" })
    }

    @Test
    fun jsonHasTheDocumentedFields() {
        val snap = SnapshotEngine.build(settings(), SnapshotOptions())
        val json = snap.json()
        assertEquals(false, json["truncated"]!!.jsonPrimitive.content.toBoolean())
        val nodes = json["nodes"]!!.jsonArray
        val first = nodes[0].jsonObject
        for (key in listOf("ref", "role", "label", "text", "value", "rect", "enabled", "focused", "selected", "editable", "children")) {
            assertTrue("missing $key in $first", key in first)
        }
        assertEquals("@e1", first["ref"]!!.jsonPrimitive.content)
        val rect = first["rect"]!!.jsonObject
        assertEquals(listOf("x", "y", "w", "h"), rect.keys.toList())
        // Children nest: the tree's total equals the flat list.
        fun count(arr: kotlinx.serialization.json.JsonArray): Int = arr.sumOf { 1 + count(it.jsonObject["children"]!!.jsonArray) }
        assertEquals(snap.nodes.size, count(nodes))
        assertEquals(snap.nodes.size, json["refs"]!!.jsonArray.size)
    }

    @Test
    fun linesShowStateMarkers() {
        val sw = UiNode(className = "android.widget.Switch", text = "Wi-Fi", bounds = Bounds(0, 0, 100, 100), clickable = true, checked = true)
        val off = UiNode(className = "android.widget.CheckBox", text = "Sync", bounds = Bounds(0, 100, 100, 200), clickable = true, checked = false, enabled = false)
        val field = UiNode(className = "android.widget.EditText", text = "", hint = "Email", bounds = Bounds(0, 200, 100, 300), editable = true, focused = true, focusable = true)
        val snap = SnapshotEngine.build(Capture(listOf(UiNode(className = "android.widget.FrameLayout", bounds = screen, children = listOf(sw, off, field))), screen, null), SnapshotOptions())
        val t = snap.text()
        assertTrue(t, t.contains("[switch] \"Wi-Fi\" [checked]"))
        assertTrue(t, t.contains("[checkbox] \"Sync\" [disabled] [unchecked]"))
        assertTrue(t, t.contains("[text-field] [focused]"))
    }

    @Test
    fun truncatesAtFiveThousandNodes() {
        val many = (0 until 6000).map { UiNode(className = "android.widget.TextView", text = "row $it", bounds = Bounds(0, it, 10, it + 1)) }
        val snap = SnapshotEngine.build(Capture(listOf(UiNode(className = "android.widget.FrameLayout", bounds = screen, children = many)), screen, null), SnapshotOptions())
        assertTrue(snap.truncated)
        assertTrue(snap.nodes.size < 5000)
    }

    @Test
    fun diffReportsAddedAndRemovedLines() {
        val before = SnapshotEngine.build(settings(), SnapshotOptions(interactive = true))
        val after = SnapshotEngine.build(
            Capture(listOf(settings().roots[0].let { r -> UiNode(className = r.className, bounds = r.bounds, children = r.children.take(2)) }), screen, null),
            SnapshotOptions(interactive = true),
        )
        val (text, json) = SnapshotEngine.diff(before, after)
        assertTrue(text, text.startsWith("Snapshot diff:"))
        assertTrue(json.jsonObject["removed"]!!.jsonPrimitive.content.toInt() > 0)
        val (same, _) = SnapshotEngine.diff(before, SnapshotEngine.build(settings(), SnapshotOptions(interactive = true)))
        assertTrue(same.startsWith("No changes"))
    }

    @Test
    fun rolesFollowAgentDevice() {
        assertEquals("button", Roles.formatRole("android.widget.Button"))
        assertEquals("button", Roles.formatRole("android.widget.ImageButton"))
        assertEquals("text", Roles.formatRole("android.widget.TextView"))
        assertEquals("text-field", Roles.formatRole("android.widget.EditText"))
        assertEquals("list", Roles.formatRole("androidx.recyclerview.widget.RecyclerView"))
        assertEquals("group", Roles.formatRole("android.view.View"))
        assertEquals("scroll-area", Roles.formatRole("android.widget.ScrollView"))
        assertEquals("switch", Roles.formatRole("android.widget.Switch"))
        assertEquals("image", Roles.formatRole("android.widget.ImageView"))
        assertEquals("webview", Roles.formatRole("android.webkit.WebView"))
        assertEquals("button", Roles.normalizeType("android.widget.Button"))
    }
}

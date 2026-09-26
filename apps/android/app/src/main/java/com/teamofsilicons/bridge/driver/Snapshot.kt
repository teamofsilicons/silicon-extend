package com.teamofsilicons.bridge.driver

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/** What the screen looked like at one moment: one root per window, bottom window first. */
data class Capture(val roots: List<UiNode>, val screen: Bounds, val foregroundPackage: String?) {
    /** Every node, document order. */
    val allNodes: List<UiNode> by lazy { roots.flatMap { it.walk().toList() } }
}

data class SnapshotOptions(
    val interactive: Boolean = false,
    val depth: Int? = null,
    val scope: String? = null,
    val raw: Boolean = false,
)

/** One line of a snapshot: an element with its `@eN` ref. */
class SnapNode(
    val ref: String,
    val node: UiNode,
    val depth: Int,
    val role: String,
    val label: String,
) {
    val children = mutableListOf<SnapNode>()
    var parent: SnapNode? = null
}

class Snapshot(
    val nodes: List<SnapNode>,
    val roots: List<SnapNode>,
    val totalNodes: Int,
    val truncated: Boolean,
    val options: SnapshotOptions,
    val screen: Bounds,
) {
    private val byRef = nodes.associateBy { it.ref }

    fun ref(ref: String): SnapNode? = byRef[ref.removePrefix("@")]

    fun text(): String {
        val sb = StringBuilder()
        sb.append("Snapshot: ${nodes.size} visible nodes ($totalNodes total)")
        if (truncated) sb.append(" [truncated at ${SnapshotEngine.MAX_NODES} nodes; scroll or scope to see the rest]")
        for (n in nodes) {
            sb.append('\n').append(SnapshotEngine.formatLine(n))
        }
        if (nodes.isEmpty()) {
            sb.append('\n').append(
                when {
                    options.scope != null -> "(nothing on screen matches scope \"${options.scope}\")"
                    options.interactive && options.depth != null -> "(no interactive elements within depth ${options.depth}; retry without -d)"
                    else -> "(no elements)"
                },
            )
        }
        return sb.toString()
    }

    fun json(): JsonObject = buildJsonObject {
        put("nodes", JsonArray(roots.map { nodeJson(it) }))
        put("refs", JsonArray(nodes.map { JsonPrimitive("@" + it.ref) }))
        put("truncated", truncated)
        put("total_nodes", totalNodes)
        put("visible_nodes", nodes.size)
        put("screen", rectJson(screen))
    }

    companion object {
        fun rectJson(b: Bounds): JsonObject = buildJsonObject {
            put("x", b.left); put("y", b.top); put("w", b.width); put("h", b.height)
        }

        fun nodeJson(n: SnapNode, withChildren: Boolean = true): JsonObject = buildJsonObject {
            val u = n.node
            put("ref", "@" + n.ref)
            put("role", n.role)
            put("label", u.label?.let { JsonPrimitive(it) } ?: kotlinx.serialization.json.JsonNull)
            put("text", n.label)
            put("value", u.value?.let { JsonPrimitive(it) } ?: kotlinx.serialization.json.JsonNull)
            put("id", u.identifier?.let { JsonPrimitive(it) } ?: kotlinx.serialization.json.JsonNull)
            put("type", u.className?.let { JsonPrimitive(it) } ?: kotlinx.serialization.json.JsonNull)
            put("rect", rectJson(u.bounds))
            put("enabled", u.enabled)
            put("focused", u.focused)
            put("selected", u.selected)
            put("editable", u.isEditable)
            put("hittable", u.hittable)
            u.checked?.let { put("checked", it) }
            if (u.password) put("password", true)
            if (u.scrollable) put("scrollable", true)
            if (u.hintShowing) put("hint_showing", true)
            u.hint?.let { put("placeholder", it) }
            u.packageName?.let { put("package", it) }
            if (withChildren) put("children", buildJsonArray { n.children.forEach { add(nodeJson(it)) } })
        }
    }
}

/**
 * Turns a [Capture] into agent-device's snapshot: the same inclusion rules as its Android
 * presentation (`ui-hierarchy-inclusion.ts`), the same line format (`snapshot-lines.ts`), and refs
 * `e1…eN` in document order, valid until the next snapshot in the session.
 */
object SnapshotEngine {
    const val MAX_NODES = 5000

    fun build(capture: Capture, options: SnapshotOptions, previous: Snapshot? = null): Snapshot {
        // 1. Flatten in document order, capped like agent-device's helper.
        val flat = ArrayList<Pair<UiNode, Int>>()
        val rootStarts = HashSet<Int>()
        var truncated = false
        outer@ for (root in capture.roots) {
            rootStarts += flat.size
            val stack = ArrayDeque<Pair<UiNode, Int>>()
            stack.addLast(root to 0)
            while (stack.isNotEmpty()) {
                if (flat.size >= MAX_NODES) {
                    truncated = true
                    break@outer
                }
                val (n, d) = stack.removeLast()
                flat += n to d
                for (i in n.children.indices.reversed()) stack.addLast(n.children[i] to d + 1)
            }
        }

        // 2. Which nodes are kept.
        val descendantHittable = HashMap<UiNode, Boolean>()
        fun hasHittableDescendant(n: UiNode): Boolean = descendantHittable.getOrPut(n) {
            n.children.any { (it.hittable && it.visibleToUser) || hasHittableDescendant(it) }
        }
        fun ancestorHittable(n: UiNode): Boolean {
            var p = n.parent
            while (p != null) {
                if (p.hittable) return true
                p = p.parent
            }
            return false
        }
        fun ancestorCollection(n: UiNode): Boolean {
            var p = n.parent
            while (p != null) {
                if (Roles.isCollection(p.className)) return true
                p = p.parent
            }
            return false
        }
        fun include(n: UiNode): Boolean {
            if (options.raw) return true
            if (!n.visibleToUser) return false
            val hasText = !n.label.isNullOrBlank()
            val hasId = !n.identifier.isNullOrBlank()
            val meaningfulText = hasText && !Roles.isGenericResourceId(n.label)
            val meaningfulId = hasId && !Roles.isGenericResourceId(n.identifier)
            val structural = Roles.isStructural(n.className)
            val visual = Roles.isVisual(n.className)
            if (options.interactive) {
                if (n.bounds.isEmpty) return false
                if (n.hittable) return true
                if (Roles.isScrollableType(n.className) && hasHittableDescendant(n)) return true
                if (!meaningfulText && !meaningfulId) return false
                if (visual) return false
                val anc = ancestorHittable(n)
                val coll = ancestorCollection(n)
                if (structural && !coll && !anc) return false
                return anc || hasHittableDescendant(n) || coll
            }
            if (structural || visual) {
                if (n.hittable) return true
                if (meaningfulText || meaningfulId) return true
                return hasHittableDescendant(n)
            }
            return true
        }

        // 3. Display lines: unlabeled groups are folded away (unless --raw); depth re-counted.
        data class Line(val node: UiNode, val rawDepth: Int, val depth: Int, val role: String, val label: String)
        val lines = ArrayList<Line>()
        val visibleDepths = ArrayList<Int>()
        for ((i, entry) in flat.withIndex()) {
            val (n, d) = entry
            // Each window starts again at depth 0.
            if (i in rootStarts) visibleDepths.clear()
            if (!include(n)) continue
            val role = n.role
            val label = displayLabel(n, role)
            if (!options.raw && role == "group" && label.isEmpty()) continue
            while (visibleDepths.isNotEmpty() && d <= visibleDepths.last()) visibleDepths.removeAt(visibleDepths.size - 1)
            val adjusted = visibleDepths.size
            visibleDepths += d
            lines += Line(n, d, adjusted, role, label)
        }

        // 4. Scope: the subtree of the first line whose label, value or id contains the scope text.
        var scoped: List<Line> = lines
        val scope = options.scope
        if (scope != null) {
            val needle: String? = if (scope.startsWith("@")) {
                previous?.ref(scope)?.let { it.label.ifEmpty { it.node.label ?: it.node.identifier } }
            } else {
                scope
            }
            scoped = emptyList()
            if (!needle.isNullOrBlank()) {
                val q = Selectors.normalizeText(needle)
                for ((idx, line) in lines.withIndex()) {
                    val n = line.node
                    val hit = listOf(n.label, n.value, n.identifier).any { Selectors.normalizeText(it).contains(q) }
                    if (!hit) continue
                    val sub = ArrayList<Line>()
                    sub += line
                    var j = idx + 1
                    while (j < lines.size && lines[j].depth > line.depth) {
                        sub += lines[j]
                        j++
                    }
                    // Under -i a scope match must still leave something actionable.
                    if (options.interactive && sub.none { it.node.hittable }) continue
                    val base = line.depth
                    scoped = sub.map { it.copy(depth = it.depth - base) }
                    break
                }
            }
        }

        // 5. Depth limit.
        val limited = options.depth?.let { max -> scoped.filter { it.depth <= max } } ?: scoped

        // 6. Refs and tree.
        val snapNodes = ArrayList<SnapNode>(limited.size)
        val roots = ArrayList<SnapNode>()
        val stack = ArrayList<SnapNode>()
        for ((i, line) in limited.withIndex()) {
            val sn = SnapNode("e${i + 1}", line.node, line.depth, line.role, line.label)
            while (stack.isNotEmpty() && stack.last().depth >= line.depth) stack.removeAt(stack.size - 1)
            val parent = stack.lastOrNull()
            if (parent == null) roots += sn else {
                parent.children += sn
                sn.parent = parent
            }
            stack += sn
            snapNodes += sn
        }
        val total = if (options.raw) flat.size else flat.count { it.first.visibleToUser }
        return Snapshot(snapNodes, roots, total, truncated, options, capture.screen)
    }

    /** `displayLabel` in snapshot-lines.ts. */
    fun displayLabel(n: UiNode, role: String): String {
        val label = n.label?.trim().orEmpty()
        val value = n.value?.trim().orEmpty()
        val editable = role == "text-field" || role == "text-view" || role == "search" || n.isEditable
        if (editable) {
            if (value.isNotEmpty()) return value
            if (label.isNotEmpty()) return label
        } else if (label.isNotEmpty()) {
            return label
        }
        if (value.isNotEmpty()) return value
        val id = n.identifier?.trim().orEmpty()
        if (id.isEmpty()) return ""
        if (Roles.isGenericResourceId(id) && role in setOf("group", "image", "list", "collection")) return ""
        return id
    }

    fun stateMarkers(n: UiNode): List<String> {
        val m = ArrayList<String>()
        if (!n.enabled) m += "disabled"
        if (n.selected) m += "selected"
        n.checked?.let { m += if (it) "checked" else "unchecked" }
        if (n.focused && n.isEditable) m += "focused"
        if (n.password) m += "password"
        return m
    }

    fun formatLine(n: SnapNode): String {
        val indent = "  ".repeat(n.depth)
        val label = escape(n.label)
        val textPart = if (label.isNotEmpty()) " \"$label\"" else ""
        val meta = stateMarkers(n.node).joinToString("") { " [$it]" }
        return "$indent@${n.ref} [${n.role}]$textPart$meta".trimEnd()
    }

    private fun escape(s: String): String =
        s.replace(Regex("\\s+"), " ").replace("\\", "\\\\").replace("\"", "\\\"").let {
            if (it.length > 200) it.take(197) + "..." else it
        }

    /** Line diff between two snapshots' texts (agent-device `diff snapshot`), refs ignored. */
    fun diff(previous: Snapshot, current: Snapshot): Pair<String, JsonElement> {
        fun strip(n: SnapNode) = formatLine(n).replace(Regex("@e\\d+ "), "")
        val a = previous.nodes.map(::strip)
        val b = current.nodes.map(::strip)
        // LCS table
        val dp = Array(a.size + 1) { IntArray(b.size + 1) }
        for (i in a.indices.reversed()) for (j in b.indices.reversed()) {
            dp[i][j] = if (a[i] == b[j]) dp[i + 1][j + 1] + 1 else maxOf(dp[i + 1][j], dp[i][j + 1])
        }
        val out = ArrayList<String>()
        var added = 0
        var removed = 0
        var i = 0
        var j = 0
        while (i < a.size || j < b.size) {
            when {
                i < a.size && j < b.size && a[i] == b[j] -> { out += "  " + formatLine(current.nodes[j]); i++; j++ }
                j < b.size && (i >= a.size || dp[i][j + 1] >= dp[i + 1][j]) -> { out += "+ " + formatLine(current.nodes[j]); added++; j++ }
                else -> { out += "- " + a[i]; removed++; i++ }
            }
        }
        val text = if (added == 0 && removed == 0) "No changes since the previous snapshot (${b.size} nodes)." else
            "Snapshot diff: +$added -$removed\n" + out.joinToString("\n")
        val json = buildJsonObject {
            put("added", added)
            put("removed", removed)
            put("lines", JsonArray(out.map { JsonPrimitive(it) }))
        }
        return text to json
    }
}

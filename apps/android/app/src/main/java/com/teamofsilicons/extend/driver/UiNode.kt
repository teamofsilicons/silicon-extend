package com.teamofsilicons.extend.driver

/** A rectangle in physical screen pixels, as the accessibility tree reports it. */
data class Bounds(val left: Int, val top: Int, val right: Int, val bottom: Int) {
    val width: Int get() = right - left
    val height: Int get() = bottom - top
    val centerX: Int get() = left + width / 2
    val centerY: Int get() = top + height / 2
    val isEmpty: Boolean get() = width <= 0 || height <= 0

    fun intersects(other: Bounds): Boolean =
        left < other.right && other.left < right && top < other.bottom && other.top < bottom

    fun intersect(other: Bounds): Bounds = Bounds(
        maxOf(left, other.left), maxOf(top, other.top), minOf(right, other.right), minOf(bottom, other.bottom),
    )

    companion object {
        val EMPTY = Bounds(0, 0, 0, 0)
    }
}

/**
 * One element of the screen, copied out of an `AccessibilityNodeInfo` so everything that reads
 * the screen (snapshots, selectors, alerts) is plain Kotlin and testable on the JVM. [handle] keeps
 * the live node so actions can reach it.
 *
 * Naming follows agent-device's Android mapping: `label` is the text or else the content
 * description, `value` is the text, `identifier` is the resource id.
 */
class UiNode(
    val className: String? = null,
    val text: String? = null,
    val contentDescription: String? = null,
    val resourceId: String? = null,
    val packageName: String? = null,
    val hint: String? = null,
    val bounds: Bounds = Bounds.EMPTY,
    val clickable: Boolean = false,
    val longClickable: Boolean = false,
    val focusable: Boolean = false,
    val focused: Boolean = false,
    val selected: Boolean = false,
    /** Null when the node isn't checkable. */
    val checked: Boolean? = null,
    val enabled: Boolean = true,
    val editable: Boolean = false,
    val password: Boolean = false,
    val scrollable: Boolean = false,
    val visibleToUser: Boolean = true,
    val hintShowing: Boolean = false,
    val heading: Boolean = false,
    val inputType: Int = 0,
    /** `application`, `input_method`, `system`, `accessibility_overlay`, `split_screen_divider`, `magnification_overlay` or null. */
    val windowType: String? = null,
    val windowTitle: String? = null,
    val handle: Any? = null,
    children: List<UiNode> = emptyList(),
) {
    val children: List<UiNode> = children
    var parent: UiNode? = null
        private set

    init {
        for (c in children) c.parent = this
    }

    val label: String? get() = text?.takeIf { it.isNotBlank() } ?: contentDescription?.takeIf { it.isNotBlank() }
    val value: String? get() = text
    val identifier: String? get() = resourceId

    /** A node a touch or D-pad/keyboard focus can act on (agent-device's `hittable`). */
    val hittable: Boolean get() = clickable || focusable || focused

    val isEditable: Boolean
        get() = editable || Roles.normalizeType(className).let { it.contains("edittext") || it.contains("autocompletetextview") }

    val role: String get() = Roles.formatRole(className ?: "Element")

    /** The nearest node, this one included, that a tap can act on. */
    fun nearestClickable(): UiNode? {
        var n: UiNode? = this
        while (n != null) {
            if (n.clickable) return n
            n = n.parent
        }
        return null
    }

    /** Depth-first, document order, this node first. */
    fun walk(): Sequence<UiNode> = sequence {
        val stack = ArrayDeque<UiNode>()
        stack.addLast(this@UiNode)
        while (stack.isNotEmpty()) {
            val n = stack.removeLast()
            yield(n)
            for (i in n.children.indices.reversed()) stack.addLast(n.children[i])
        }
    }

    override fun toString(): String = "UiNode($role ${label?.let { "\"$it\"" } ?: ""} $bounds)"
}

/** agent-device's role vocabulary (capture-kit `snapshot-lines.ts`). */
object Roles {
    private val ROLE_LABELS = mapOf(
        "application" to "application",
        "navigationbar" to "navigation-bar",
        "tabbar" to "tab-bar",
        "button" to "button",
        "imagebutton" to "button",
        "link" to "link",
        "cell" to "cell",
        "statictext" to "text",
        "checkedtextview" to "text",
        "textbox" to "text-field",
        "textfield" to "text-field",
        "edittext" to "text-field",
        "textarea" to "text-view",
        "switch" to "switch",
        "slider" to "slider",
        "image" to "image",
        "imageview" to "image",
        "webview" to "webview",
        "framelayout" to "group",
        "linearlayout" to "group",
        "relativelayout" to "group",
        "constraintlayout" to "group",
        "viewgroup" to "group",
        "view" to "group",
        "listview" to "list",
        "recyclerview" to "list",
        "collectionview" to "collection",
        "searchfield" to "search",
        "heading" to "heading",
        "activityindicator" to "activity-indicator",
        "progressindicator" to "progress-indicator",
        "segmentedcontrol" to "segmented-control",
        "group" to "group",
        "window" to "window",
        "checkbox" to "checkbox",
        "radio" to "radio",
        "menuitem" to "menu-item",
        "toolbar" to "toolbar",
        "scrollarea" to "scroll-area",
        "scrollview" to "scroll-area",
        "nestedscrollview" to "scroll-area",
        "table" to "table",
    )

    /** `android.widget.Button` → `button` (contracts `normalizeType`). */
    fun normalizeType(type: String?): String {
        var n = (type ?: "").trim().lowercase()
        val sep = maxOf(n.lastIndexOf('.'), n.lastIndexOf('/'))
        if (sep != -1) n = n.substring(sep + 1)
        return n
    }

    /** The role shown in snapshot text (`formatRole`). */
    fun formatRole(type: String): String {
        val raw = type
        var normalized = type.lowercase()
        val isAndroidClass = raw.contains('.') &&
            (raw.startsWith("android.") || raw.startsWith("androidx.") || raw.startsWith("com."))
        if (normalized.contains('.')) {
            normalized = normalized
                .removePrefix("android.widget.")
                .removePrefix("android.view.")
                .removePrefix("android.webkit.")
                .removePrefix("androidx.")
                .removePrefix("com.google.android.")
                .removePrefix("com.android.")
            if (isAndroidClass && normalized.contains('.')) {
                normalized = normalized.substring(normalized.lastIndexOf('.') + 1)
            }
        }
        if (normalized == "textview") return if (isAndroidClass) "text" else "text-view"
        return ROLE_LABELS[normalized] ?: normalized.ifEmpty { "element" }
    }

    fun isStructural(type: String?): Boolean {
        val short = normalizeType(type)
        return short.contains("layout") || short == "viewgroup" || short == "view"
    }

    fun isVisual(type: String?): Boolean {
        val short = normalizeType(type)
        return short == "imageview" || short == "imagebutton"
    }

    fun isScrollableType(type: String?): Boolean {
        val short = normalizeType(type)
        return short.contains("scrollview") || short.contains("recyclerview") || short.contains("listview") ||
            short.contains("gridview") || short.contains("viewpager")
    }

    fun isCollection(type: String?): Boolean {
        val short = normalizeType(type)
        return short.contains("recyclerview") || short.contains("listview") || short.contains("gridview")
    }

    private val GENERIC_ID = Regex("^[\\w.]+:id/[\\w.-]+$", RegexOption.IGNORE_CASE)
    fun isGenericResourceId(value: String?): Boolean = value != null && GENERIC_ID.matches(value.trim())
}

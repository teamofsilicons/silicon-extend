package com.teamofsilicons.extend.driver

/**
 * System and app pop-ups, derived from the screen tree the way the device engine does on Android:
 * runtime permission prompts, `AlertDialog`s, and small dialog windows.
 */
data class AlertInfo(
    val kind: String,
    val title: String?,
    val message: String?,
    val buttons: List<UiNode>,
    val accept: UiNode?,
    val dismiss: UiNode?,
)

object Alerts {
    private val PERMISSION_ACCEPT_IDS = listOf(
        "permission_allow_foreground_only_button",
        "permission_allow_button",
        "permission_allow_one_time_button",
        "permission_allow_all_button",
        "permission_allow_selected_button",
    )
    private val PERMISSION_DISMISS_IDS = listOf("permission_deny_button", "permission_deny_and_dont_ask_again_button")

    private val POSITIVE = listOf(
        "ok", "okay", "allow", "accept", "yes", "continue", "confirm", "agree", "i agree", "got it", "done", "open",
        "while using the app", "only this time", "allow all", "turn on", "enable",
    )
    private val NEGATIVE = listOf(
        "cancel", "deny", "don't allow", "dont allow", "do not allow", "dismiss", "no", "not now", "close", "decline",
        "never", "reject", "no thanks", "later", "skip",
    )

    private fun idName(n: UiNode): String = n.identifier?.substringAfter(":id/", "").orEmpty()

    fun detect(capture: Capture): AlertInfo? {
        val nodes = capture.allNodes.filter { it.visibleToUser }

        // Runtime permission prompt (PermissionController).
        val perm = nodes.filter { it.identifier?.contains("permissioncontroller:id/") == true }
        if (perm.isNotEmpty()) {
            val buttons = perm.filter { it.clickable && !it.label.isNullOrBlank() }
            val accept = PERMISSION_ACCEPT_IDS.firstNotNullOfOrNull { id -> perm.firstOrNull { idName(it) == id } }
            val dismiss = PERMISSION_DISMISS_IDS.firstNotNullOfOrNull { id -> perm.firstOrNull { idName(it) == id } }
            val message = perm.firstOrNull { idName(it) == "permission_message" }?.label
            if (accept != null || dismiss != null) {
                return AlertInfo("permission", message, null, buttons, accept, dismiss)
            }
        }

        // AlertDialog (framework or AppCompat both use android:id/button1…3).
        val b1 = nodes.firstOrNull { it.identifier == "android:id/button1" }
        val b2 = nodes.firstOrNull { it.identifier == "android:id/button2" }
        val b3 = nodes.firstOrNull { it.identifier == "android:id/button3" }
        if (b1 != null || b2 != null) {
            val title = nodes.firstOrNull { idName(it) == "alertTitle" }?.label
            val message = nodes.firstOrNull { it.identifier == "android:id/message" }?.label
            return AlertInfo("dialog", title, message, listOfNotNull(b1, b3, b2), b1, b2 ?: b3)
        }

        // A small application window above the full-screen one.
        val screenArea = capture.screen.width.toLong() * capture.screen.height
        val dialogRoot = capture.roots.drop(1).lastOrNull { root ->
            root.windowType == "application" && !root.bounds.isEmpty &&
                root.bounds.width.toLong() * root.bounds.height < screenArea * 0.85
        }
        if (dialogRoot != null) {
            val inside = dialogRoot.walk().filter { it.visibleToUser }.toList()
            val buttons = inside.filter { it.clickable && !SnapshotEngine.displayLabel(it, it.role).isBlank() }
            val texts = inside.filter { !it.clickable && !it.label.isNullOrBlank() }.mapNotNull { it.label }
            if (buttons.isNotEmpty() && buttons.size <= 4) {
                fun labelOf(n: UiNode) = Selectors.normalizeText(labelDeep(n))
                val accept = buttons.firstOrNull { labelOf(it) in POSITIVE }
                val dismiss = buttons.firstOrNull { labelOf(it) in NEGATIVE }
                return AlertInfo("dialog", texts.firstOrNull(), texts.drop(1).firstOrNull(), buttons, accept, dismiss)
            }
        }
        return null
    }

    /** A clickable container's own label, or else its first labelled descendant's. */
    fun labelDeep(n: UiNode): String =
        n.label ?: n.walk().drop(1).firstNotNullOfOrNull { it.label } ?: n.identifier.orEmpty()
}

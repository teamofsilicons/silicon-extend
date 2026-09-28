package com.teamofsilicons.extend.driver

/**
 * What a click does when the accessibility click didn't work: Android refused ACTION_CLICK on the
 * element and on its nearest clickable ancestor, or it said yes and nothing on screen changed. TVs
 * do this often: many TV apps act on the remote's select key and ignore the accessibility click.
 *
 * The element is focused first on a TV, then, in order:
 * - with Android debugging connected: a real select key press (`input keyevent`, on a TV where the
 *   element took focus) or a real tap at its centre (`input tap`);
 * - on a TV from Android 13, where the element took focus: accessibility's own select key;
 * - a touch gesture at its centre.
 */
object ClickFallback {
    enum class Method(val wire: String) {
        ADB_SELECT("adb_select"),
        ADB_TAP("adb_tap"),
        ACCESSIBILITY_SELECT("accessibility_select"),
        GESTURE_TAP("tap"),
    }

    /** How long a click waits for the screen to change before it counts as having done nothing. */
    const val SETTLE_MS = 1_000L

    fun order(tv: Boolean, adbConnected: Boolean, focused: Boolean, dpadSupported: Boolean): List<Method> = buildList {
        if (adbConnected) add(if (tv && focused) Method.ADB_SELECT else Method.ADB_TAP)
        if (tv && focused && dpadSupported) add(Method.ACCESSIBILITY_SELECT)
        add(Method.GESTURE_TAP)
    }

    /** The shell command for an Android debugging method. */
    fun adbCommand(method: Method, x: Int, y: Int, sdk: Int): String = when (method) {
        Method.ADB_SELECT -> TvRemoteKeys.command("select", longPress = false, durationMs = null, sdk = sdk)
        Method.ADB_TAP -> "input tap ${x.coerceAtLeast(0)} ${y.coerceAtLeast(0)}"
        else -> throw IllegalArgumentException("$method doesn't go through Android debugging")
    }
}

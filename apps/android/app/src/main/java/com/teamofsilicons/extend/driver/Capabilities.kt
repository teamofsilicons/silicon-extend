package com.teamofsilicons.extend.driver

/**
 * Capability names (`crates/extend-protocol/src/capability.rs`) and which of them each command
 * needs (`COMMANDS[].any_of`). A command runs only when the device currently reports at least one
 * of its capabilities, so what `hello` says and what the device does can't disagree.
 */
object Capabilities {
    const val SCREEN_READ = "screen.read"
    const val SCREEN_CAPTURE = "screen.capture"
    const val SCREEN_RECORD = "screen.record"
    const val INPUT_TOUCH = "input.touch"
    const val INPUT_POINTER = "input.pointer"
    const val INPUT_TEXT = "input.text"
    const val INPUT_KEYBOARD = "input.keyboard"
    const val INPUT_REMOTE = "input.remote"
    const val NAV_SYSTEM = "nav.system"
    const val APPS_LAUNCH = "apps.launch"
    const val APPS_LIST = "apps.list"
    const val APPS_INSTALL = "apps.install"
    const val ALERTS = "alerts"
    const val CLIPBOARD = "clipboard"
    const val LOGS = "logs"
    const val REPLAY = "replay"
    const val TAKEOVER = "takeover"
    const val NOTIFICATIONS = "notifications"
    const val ADB = "adb"
    const val TERMINAL = "terminal"
    const val DISPLAY = "display"
    const val LINKS = "links"

    /** capability.rs `DeviceOs::Android.full_capabilities()`. */
    val ANDROID_FULL = listOf(
        SCREEN_READ, SCREEN_CAPTURE, SCREEN_RECORD, INPUT_TOUCH, INPUT_TEXT, INPUT_KEYBOARD, NAV_SYSTEM, APPS_LAUNCH,
        APPS_LIST, APPS_INSTALL, ALERTS, CLIPBOARD, LOGS, REPLAY, TAKEOVER, NOTIFICATIONS, ADB, LINKS,
    )

    /** capability.rs `DeviceOs::AndroidTv.full_capabilities()`. */
    val ANDROID_TV_FULL = listOf(
        SCREEN_READ, SCREEN_CAPTURE, INPUT_TEXT, INPUT_REMOTE, NAV_SYSTEM, APPS_LAUNCH, APPS_LIST, APPS_INSTALL, ALERTS,
        LOGS, REPLAY, TAKEOVER, ADB, DISPLAY, LINKS,
    )

    val COMMAND_CAPABILITIES: Map<String, List<String>> = mapOf(
        "snapshot" to listOf(SCREEN_READ),
        "diff" to listOf(SCREEN_READ, SCREEN_CAPTURE),
        "get" to listOf(SCREEN_READ),
        "find" to listOf(SCREEN_READ),
        "is" to listOf(SCREEN_READ),
        "wait" to listOf(SCREEN_READ),
        "screenshot" to listOf(SCREEN_CAPTURE),
        "record" to listOf(SCREEN_RECORD),
        "click" to listOf(INPUT_POINTER, INPUT_TOUCH),
        "press" to listOf(INPUT_TOUCH, INPUT_POINTER),
        "longpress" to listOf(INPUT_TOUCH),
        "fill" to listOf(INPUT_TEXT),
        "type" to listOf(INPUT_TEXT),
        "focus" to listOf(INPUT_TEXT),
        "scroll" to listOf(INPUT_TOUCH, INPUT_POINTER),
        "swipe" to listOf(INPUT_TOUCH),
        "gesture" to listOf(INPUT_TOUCH),
        "hover" to listOf(INPUT_POINTER),
        "back" to listOf(NAV_SYSTEM),
        "home" to listOf(NAV_SYSTEM),
        "app-switcher" to listOf(NAV_SYSTEM),
        "tv-remote" to listOf(INPUT_REMOTE),
        "keyboard" to listOf(INPUT_KEYBOARD),
        "clipboard" to listOf(CLIPBOARD),
        "open" to listOf(APPS_LAUNCH, LINKS),
        "close" to listOf(APPS_LAUNCH),
        "apps" to listOf(APPS_LIST),
        "appstate" to listOf(APPS_LAUNCH),
        "install" to listOf(APPS_INSTALL),
        "reinstall" to listOf(APPS_INSTALL),
        "alert" to listOf(ALERTS),
        "logs" to listOf(LOGS),
        "replay" to listOf(REPLAY),
        "test" to listOf(REPLAY),
        "batch" to listOf(REPLAY),
        "terminal" to listOf(TERMINAL),
        "adb" to listOf(ADB),
        "notifications" to listOf(NOTIFICATIONS),
        "display" to listOf(DISPLAY),
    )

    // Why a debugging capability is missing depends on the Android version: adb.DebuggingPath.missingReason.

    /** Debugging was connected, then the device restarted and Android turned Wireless debugging off. */
    fun afterRestartReason(what: String, tv: Boolean) =
        "$what needs Android debugging, and Android turned Wireless debugging off when this ${if (tv) "TV" else "phone"} restarted. " +
            "The Carbon turns it back on in Settings › System › Developer options › Wireless debugging; Extend then reconnects by itself."
}

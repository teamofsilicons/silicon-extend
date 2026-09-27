package com.teamofsilicons.extend.adb

/**
 * How Android debugging reaches this device, by Android version. Pure: every decision takes the
 * SDK level (and whether this is a TV), so JVM tests cover each version.
 *
 * - Android 11+ ([Mode.WIRELESS]): Wireless debugging. The Carbon pairs with Android's pairing
 *   code, Extend finds the connection port by mDNS, and every connection must start TLS. A TV with
 *   network debugging can still use its legacy port (usually 5555).
 * - Android 8–10 ([Mode.NETWORK]): there is no Wireless debugging (no pairing code, no mDNS, no
 *   TLS). TVs, TV boxes and Fire TV turn on "Network debugging" / "ADB debugging"; a phone needs
 *   `adb tcpip 5555` run once from a computer. Extend connects to 127.0.0.1:5555 with its RSA key
 *   and Android asks "Allow USB debugging?" on the screen the first time.
 */
object DebuggingPath {
    enum class Mode { WIRELESS, NETWORK }

    /** Wireless debugging (pairing codes, TLS) exists from Android 11. */
    const val WIRELESS_SDK = 30
    /** The port network debugging and `adb tcpip` listen on. */
    const val LEGACY_PORT = 5555
    /** How long a connect started by the Carbon waits for them to answer Android's "Allow USB debugging?" prompt. */
    const val APPROVAL_WAIT_SECONDS = 60L
    /** How long any other connect waits (the reconnect loop, a key Android already trusts). */
    const val CONNECT_WAIT_SECONDS = 8L

    fun mode(sdk: Int): Mode = if (sdk >= WIRELESS_SDK) Mode.WIRELESS else Mode.NETWORK

    /** Pairing with a code is possible (Android 11+ only). */
    fun canPair(sdk: Int): Boolean = mode(sdk) == Mode.WIRELESS

    /**
     * The port to connect to when the Carbon asked for [requested] (0: none given). Android 11+
     * discovers it (0 stays 0); Android 8–10 can't discover anything, so it is the legacy port.
     */
    fun connectPort(requested: Int, sdk: Int): Int =
        if (requested == 0 && mode(sdk) == Mode.NETWORK) LEGACY_PORT else requested

    /**
     * Seconds to wait for Android to accept a connection. A plain (RSA) connection the Carbon
     * started may need them to answer the prompt with the remote, which takes longer than 8 s.
     */
    fun connectWaitSeconds(requireTls: Boolean, startedByCarbon: Boolean): Long =
        if (!requireTls && startedByCarbon) APPROVAL_WAIT_SECONDS else CONNECT_WAIT_SECONDS

    /** The setup step's key: phones on Android 11+ use Wireless debugging, everything else network debugging. */
    fun stepKey(tv: Boolean, sdk: Int): String = if (!tv && mode(sdk) == Mode.WIRELESS) "wireless_debugging" else "network_debugging"

    private fun noun(tv: Boolean) = if (tv) "TV" else "phone"

    /** Where network debugging is switched on, for a TV, a Fire TV or a phone on Android 8–10. */
    fun legacySwitch(tv: Boolean, fire: Boolean): String = when {
        fire -> "Settings › My Fire TV › Developer options › ADB debugging › On"
        tv -> "Settings › Device Preferences › Developer options › Network debugging (on some TVs ADB debugging or USB debugging) › On"
        else -> "Settings › System › Developer options › USB debugging › On"
    }

    /** What the Carbon does once on an Android 8–10 phone: there is nothing to switch on in Settings for the network. */
    const val TCPIP_STEP = "connect this phone to a computer by USB once and run `adb tcpip 5555` there (it lasts until the phone restarts)"

    /** The prompt Android shows for a new debugging key, and the answer that makes it stick. */
    const val APPROVE = "select Allow when Android asks \"Allow USB debugging?\" (tick \"Always allow from this computer\" so it doesn't ask again)"

    /** The network debugging step's help on Android 8–10. */
    fun legacyStepHelp(tv: Boolean, fire: Boolean, release: String): String = if (tv) {
        "${legacySwitch(tv, fire)}. This TV runs Android $release, which has no Wireless debugging. " +
            "Then tap Connect Android debugging below and $APPROVE."
    } else {
        "${legacySwitch(tv, fire)}. Android $release has no Wireless debugging, so $TCPIP_STEP. " +
            "Then tap Connect Android debugging below and $APPROVE."
    }

    /**
     * Why a debugging capability is missing. [what] is the feature ("Installing apps"). Android 11+
     * names Wireless or network debugging; Android 8–10 names what that version needs.
     */
    fun missingReason(what: String, tv: Boolean, fire: Boolean, sdk: Int): String = when {
        mode(sdk) == Mode.WIRELESS -> "$what needs Android debugging. Turn on Wireless or Network debugging, then connect it in the Extend app's setup."
        tv -> "$what needs Android debugging. The Carbon turns on network debugging (${legacySwitch(tv, fire)}), then connects it in the Extend app's setup."
        else -> "$what needs Android debugging. On Android 10 and older the Carbon turns on USB debugging, runs `adb tcpip 5555` once from a computer, " +
            "then connects it in the Extend app's setup."
    }

    /** The Android debugging card's words on Android 8–10 (Android 11+ uses the Wireless debugging words). */
    fun legacyCardText(connected: Boolean, enabled: Boolean, tv: Boolean, fire: Boolean, release: String, port: Int): String = when {
        connected && tv -> "Connected · app installation, device logs, screenshots and remote buttons are available."
        connected -> "Connected · app installation, device logs, recording and screenshots are available."
        enabled && tv -> "Connected before, but not connected right now; Extend keeps reconnecting to port $port. " +
            "Check that network debugging is still on (${legacySwitch(tv, fire)}), then tap Connect. To stop using it, tap Disconnect Android debugging."
        enabled -> "Connected before, but not connected right now; Extend keeps reconnecting to port $port. " +
            "`adb tcpip 5555` lasts until the phone restarts: run it again from a computer, then tap Connect. To stop using it, tap Disconnect Android debugging."
        tv -> "This TV runs Android $release, which has no Wireless debugging. Once network debugging is on (the step above), " +
            "tap Connect Android debugging and $APPROVE."
        else -> "Android $release has no Wireless debugging. Once `adb tcpip 5555` has run (the step above), tap Connect Android debugging and $APPROVE."
    }

    /** Nothing listens on [port]: say what turns it on for this device. */
    fun refused(port: Int, tv: Boolean, fire: Boolean, sdk: Int): String = when {
        mode(sdk) == Mode.WIRELESS -> "Nothing answered on debugging port $port. Check Wireless debugging and the port it shows, then connect again."
        tv -> "Nothing answered on debugging port $port. Turn on network debugging (${legacySwitch(tv, fire)}), then tap Connect again."
        else -> "Nothing answered on debugging port $port. Android 10 and older have no Wireless debugging: $TCPIP_STEP, then tap Connect again."
    }

    /** The peer accepted the socket but never finished connecting within [seconds]. */
    fun notAnswered(port: Int, seconds: Long, requireTls: Boolean): String = if (requireTls) {
        "Android debugging did not connect on port $port within $seconds seconds. Check Wireless debugging and its connection port, " +
            "and approve Android's debugging prompt if it shows one."
    } else {
        "Android debugging did not connect on port $port within $seconds seconds. When Android asks \"Allow USB debugging?\", select Allow " +
            "(tick \"Always allow from this computer\"), then tap Connect again."
    }

    /** What the card says after the Carbon disconnected debugging. */
    fun disconnected(sdk: Int): String = if (mode(sdk) == Mode.WIRELESS) {
        "Disconnected. You can also forget Silicon Extend in Android's Wireless debugging settings."
    } else {
        "Disconnected. You can also remove the key in Developer options › Revoke USB debugging authorisations."
    }

    /**
     * The shell command that turns on the accessibility service [component] (flattened) and keeps
     * every other enabled service. For devices whose maker hides Android's Accessibility page:
     * the Carbon runs it from the app once Android debugging is connected (the shell may write
     * secure settings). Never run for a remote command.
     */
    fun enableAccessibilityCommand(component: String): String {
        require(component.matches(Regex("[A-Za-z0-9._]+/[A-Za-z0-9._]+"))) { "Not a component name: $component" }
        val c = AdbWire.quote(component)
        return "cur=\$(settings get secure enabled_accessibility_services); " +
            "case \":\$cur:\" in *:$component:*) ;; " +
            "*) if [ -z \"\$cur\" ] || [ \"\$cur\" = null ]; then new=$c; else new=\"\$cur\":$c; fi; " +
            "settings put secure enabled_accessibility_services \"\$new\" ;; esac; " +
            "settings put secure accessibility_enabled 1"
    }

    /** The screen can't be captured: Android 8–10 have no accessibility screenshots, and debugging isn't connected. */
    fun screenshotReason(tv: Boolean, fire: Boolean, sdk: Int, release: String): String =
        "Screenshots through accessibility need Android 11; this ${noun(tv)} runs Android $release. " +
            missingReason("Taking screenshots here", tv, fire, sdk)
}

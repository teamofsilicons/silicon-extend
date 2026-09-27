package com.teamofsilicons.extend.driver

import android.view.KeyEvent

/**
 * `tv-remote` through Android debugging: each button is a real key event (`input keyevent`), so
 * Menu works and so do TVs older than Android 13, where accessibility has no D-pad actions.
 */
object TvRemoteKeys {
    /** Android key codes for the buttons `tv-remote` names. Power is left out on purpose ([POWER_REFUSAL]). */
    val KEYCODES: Map<String, Int> = linkedMapOf(
        "up" to KeyEvent.KEYCODE_DPAD_UP,
        "down" to KeyEvent.KEYCODE_DPAD_DOWN,
        "left" to KeyEvent.KEYCODE_DPAD_LEFT,
        "right" to KeyEvent.KEYCODE_DPAD_RIGHT,
        "select" to KeyEvent.KEYCODE_DPAD_CENTER,
        "back" to KeyEvent.KEYCODE_BACK,
        "home" to KeyEvent.KEYCODE_HOME,
        "menu" to KeyEvent.KEYCODE_MENU,
        "play-pause" to KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE,
        "volume-up" to KeyEvent.KEYCODE_VOLUME_UP,
        "volume-down" to KeyEvent.KEYCODE_VOLUME_DOWN,
        "mute" to KeyEvent.KEYCODE_VOLUME_MUTE,
    )

    const val POWER_REFUSAL =
        "The Extend app won't press power: it could turn the TV off, and nothing on the TV could turn it back on without Android debugging."

    /**
     * The shell command for one press. A long press holds the key for [durationMs] where Android's
     * `input` supports `--duration` (Android 13+), otherwise for Android's long-press time.
     */
    fun command(button: String, longPress: Boolean, durationMs: Long?, sdk: Int): String {
        val code = KEYCODES[button] ?: throw IllegalArgumentException(
            if (button == "power") POWER_REFUSAL else "Unknown remote button \"$button\"",
        )
        val hold = when {
            !longPress -> ""
            durationMs != null && durationMs > 0 && sdk >= 33 -> "--duration $durationMs "
            else -> "--longpress "
        }
        return "input keyevent $hold$code"
    }

    /**
     * Why `input.remote` is missing right now, or null when remote buttons work: through Android
     * debugging on any TV, or through accessibility on Android 13+.
     */
    fun missingReason(
        dpadSupported: Boolean,
        a11yConnected: Boolean,
        adbConnected: Boolean,
        a11yReason: String,
        androidRelease: String,
    ): String? = when {
        adbConnected -> null
        dpadSupported && a11yConnected -> null
        !dpadSupported ->
            "Remote buttons on this TV (Android $androidRelease) need Android debugging: turn on network debugging in Developer options, " +
                "then connect Android debugging in the Extend app's setup."
        else -> "$a11yReason Or connect Android debugging in the Extend app's setup."
    }
}

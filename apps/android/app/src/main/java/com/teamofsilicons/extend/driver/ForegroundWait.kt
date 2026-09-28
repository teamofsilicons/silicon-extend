package com.teamofsilicons.extend.driver

/**
 * How long to wait for an activity the accessibility service started from the background (the
 * display, the clipboard reader, an app `open` starts) to come to the front.
 *
 * Up to Android 11, pressing Home holds every activity start from the background for 5 seconds
 * (Android's app-switch protection); the start then happens when that window ends. So `display
 * show` right after `home` on an Android 8–11 TV comes to the front about 5 s later, and a 4 s
 * wait would call it a failure while the display then appears anyway (and a Silicon that retries
 * stacks displays). Android 12 no longer holds these starts.
 */
object ForegroundWait {
    /** Android's app-switch delay after Home. */
    const val APP_SWITCH_DELAY_MS = 5_000L
    /** The last Android version (API level) that holds background starts after Home. */
    const val LAST_HOLDING_SDK = 30
    /** Past the app-switch window, time for the activity to start and draw. */
    private const val MARGIN_MS = 1_500L

    /** The wait on [sdk]: [usual] from Android 12, at least the app-switch window plus a margin before that. */
    fun ms(sdk: Int, usual: Long): Long = if (sdk <= LAST_HOLDING_SDK) maxOf(usual, APP_SWITCH_DELAY_MS + MARGIN_MS) else usual

    /** A wait in seconds for messages ("6.5 s", "4 s"). */
    fun seconds(ms: Long): String = if (ms % 1000 == 0L) "${ms / 1000} s" else "${ms / 1000}.${(ms % 1000) / 100} s"
}

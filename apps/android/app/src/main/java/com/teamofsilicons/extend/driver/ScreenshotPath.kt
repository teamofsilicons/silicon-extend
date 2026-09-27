package com.teamofsilicons.extend.driver

/**
 * How `screenshot` captures the screen on this Android version. Accessibility screenshots
 * (`AccessibilityService.takeScreenshot`) exist from Android 11; before that, and whenever the
 * accessibility service isn't connected, Android debugging's `screencap` does it. Neither: the
 * device doesn't report `screen.capture`, with the reason.
 */
enum class ScreenshotPath {
    ACCESSIBILITY, ADB, NONE;

    companion object {
        /** Accessibility screenshots need API 30. */
        const val ACCESSIBILITY_SDK = 30

        fun choose(sdk: Int, a11yConnected: Boolean, adbConnected: Boolean): ScreenshotPath = when {
            sdk >= ACCESSIBILITY_SDK && a11yConnected -> ACCESSIBILITY
            adbConnected -> ADB
            else -> NONE
        }
    }
}

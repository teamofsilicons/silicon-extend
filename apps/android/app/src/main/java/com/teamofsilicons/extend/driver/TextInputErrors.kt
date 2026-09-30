package com.teamofsilicons.extend.driver

import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

object TextInputErrors {
    fun missingFocusedField(keyboardVisible: Boolean, adbConnected: Boolean): CommandFailure {
        if (!keyboardVisible) return CommandFailure(
            "text_input_not_focused",
            "No text field has input focus. Focus one first (press @ref or focus @ref), or use fill @ref \"text\".",
        )
        val guidance = if (adbConnected)
            "Android debugging is connected; it can send input through adb shell."
        else "To send input through Android debugging, connect it in the Extend app's setup on this device."
        return CommandFailure(
            "text_input_unavailable",
            "The keyboard is visible, but Android accessibility does not expose its focused text field. " +
                "No text was sent. Inspect the screen. $guidance",
            buildJsonObject {
                put("keyboardVisible", true)
                put("focusedFieldObservable", false)
                put("inputSent", false)
                put("adbConnected", adbConnected)
            },
        )
    }
}

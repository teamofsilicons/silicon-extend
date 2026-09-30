package com.teamofsilicons.extend.driver

import com.teamofsilicons.extend.adb.AdbWire
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import java.io.IOException

/** Text input for a visible keyboard whose focused field accessibility cannot expose. */
object AdbTextInput {
    suspend fun execute(
        text: String,
        delayMs: Long?,
        keyboardVisible: Boolean,
        adbConnected: Boolean,
        shell: suspend (String) -> Int,
    ): Outcome {
        if (!keyboardVisible || !adbConnected) throw TextInputErrors.missingFocusedField(keyboardVisible, adbConnected)
        val command = command(text, delayMs)
        val exitCode = try {
            shell(command)
        } catch (_: IOException) {
            currentCoroutineContext().ensureActive()
            throw unconfirmed(null)
        }
        if (exitCode != 0) throw unconfirmed(exitCode)
        val warning = "Accessibility cannot read this field, so the text was not verified. Inspect the screen before retrying; text may already have been entered."
        return Outcome(buildJsonObject {
            put("method", "adb_text")
            put("chars", text.length)
            put("inputDispatched", true)
            put("verified", false)
            put("verification", "unavailable")
            put("verificationReason", "focused_field_unavailable")
            put("warning", warning)
        }, "Dispatched [redacted ${text.length} chars] through Android debugging. $warning")
    }

    private fun command(text: String, delayMs: Long?): String {
        if (delayMs != null && delayMs > 0) throw unsupported(
            "delay_not_supported",
            "This field needs Android debugging input, which does not support --delay-ms. Omit the delay or use a field that accessibility can read.",
        )
        if (text.any { it.code !in 32..126 }) throw unsupported(
            "unsupported_characters",
            "Android debugging input here supports only printable ASCII, including spaces. Use a field that accessibility can read for other characters.",
        )
        // Android's input text interprets every %s as a space, including inside a quoted argument.
        if (text.contains("%s")) throw unsupported(
            "literal_percent_s",
            "Android debugging input would change the literal %s sequence into a space. Use a field that accessibility can read for this text.",
        )
        return AdbWire.argv(listOf("input", "text", text.replace(" ", "%s")))
    }

    private fun unsupported(reason: String, message: String) = CommandFailure(
        "text_input_unsupported", "$message No text was sent.",
        buildJsonObject { put("method", "adb_text"); put("reason", reason); put("inputSent", false) },
    )

    private fun unconfirmed(exitCode: Int?) = CommandFailure(
        CommandFailure.ACTION_FAILED,
        "Android debugging did not confirm the text input${exitCode?.let { " (exit $it)" }.orEmpty()}. " +
            "Text may already have been entered; no second input attempt was made. Inspect the screen before retrying.",
        buildJsonObject {
            put("method", "adb_text")
            put("delivery", "unconfirmed")
            put("replayed", false)
            exitCode?.let { put("exitCode", it) }
        },
    )
}

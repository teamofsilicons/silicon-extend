package com.teamofsilicons.extend.driver

import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import java.io.IOException

/** A simple coordinate tap chooses its transport before sending input, and never replays it. */
object CoordinateTap {
    /** Null leaves refs, repeated presses and held presses with their existing executor. */
    suspend fun execute(
        command: Cmd.Click,
        adbConnected: Boolean,
        adbTap: suspend (Int, Int) -> Int,
        gestureTap: suspend (Int, Int) -> Boolean,
    ): ClickFallback.Method? {
        val point = command.target as? Target.Point ?: return null
        if (command.count != 1 || command.holdMs != null) return null
        if (adbConnected) {
            val exitCode = try {
                adbTap(point.x, point.y)
            } catch (_: IOException) {
                currentCoroutineContext().ensureActive()
                throw unconfirmed(point, null)
            }
            if (exitCode != 0) throw unconfirmed(point, exitCode)
            return ClickFallback.Method.ADB_TAP
        }
        if (!gestureTap(point.x, point.y)) {
            throw CommandFailure(CommandFailure.ACTION_FAILED, "Android cancelled the tap at (${point.x}, ${point.y}), usually because the screen changed or another gesture started.")
        }
        return ClickFallback.Method.GESTURE_TAP
    }

    private fun unconfirmed(point: Target.Point, exitCode: Int?) = CommandFailure(
        CommandFailure.ACTION_FAILED,
        "Android debugging did not confirm the tap at (${point.x}, ${point.y})${exitCode?.let { " (exit $it)" }.orEmpty()}. " +
            "The tap may already have reached the screen; no second tap was sent. Inspect the screen before retrying.",
        buildJsonObject {
            put("method", ClickFallback.Method.ADB_TAP.wire)
            put("delivery", "unconfirmed")
            put("replayed", false)
            exitCode?.let { put("exitCode", it) }
        },
    )
}

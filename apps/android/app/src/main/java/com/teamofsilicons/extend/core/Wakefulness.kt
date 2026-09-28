package com.teamofsilicons.extend.core

import android.app.KeyguardManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import androidx.core.content.ContextCompat
import com.teamofsilicons.extend.Extend

/**
 * Whether this device is awake, for `extend device ls` and the wake flow (UNDERSTANDING.md, Waking
 * a device). Awake is information, never a gate: commands, Android debugging and the terminal keep
 * working whatever it says, and Extend never wakes the device itself.
 *
 * - Phones and tablets: awake only once the Carbon is past the lock screen, that is after
 *   `USER_PRESENT` following the last `SCREEN_OFF`, or when the app starts or reconnects while the
 *   screen is on and unlocked. A screen lit by a notification on the lock screen is `locked`.
 *   `USER_PRESENT` is an unlock by a person, so it carries `input_seen: true`.
 * - TVs: awake while interactive (a screensaver, a dream, counts as awake: the TV is on), else in
 *   `standby`. A TV can't tell whether a person turned it on, so `input_seen` is left out.
 *
 * [next] and [snapshot] are the rules, pure for JVM tests; [Watcher] feeds them the broadcasts.
 */
object Wakefulness {
    /** What the app reports: `awake`, why not (`screen_off`, `locked`, `standby`), and whether a person did it. */
    data class Reading(val awake: Boolean, val sleepState: String? = null, val inputSeen: Boolean? = null) {
        val ui: AwakeUi get() = AwakeUi(awake, sleepState)
    }

    enum class Event { SCREEN_OFF, SCREEN_ON, USER_PRESENT, DREAMING_STARTED, DREAMING_STOPPED }

    const val SCREEN_OFF = "screen_off"
    const val LOCKED = "locked"
    const val STANDBY = "standby"

    /** The state read directly (the app started, or a connection opened). */
    fun snapshot(tv: Boolean, interactive: Boolean, keyguardLocked: Boolean): Reading = when {
        tv -> if (interactive) Reading(true) else Reading(false, STANDBY)
        !interactive -> Reading(false, SCREEN_OFF)
        keyguardLocked -> Reading(false, LOCKED)
        else -> Reading(true)
    }

    /**
     * The state after [event], from [current] (null: not known yet) and what the device says now.
     * On a phone, `SCREEN_ON` alone never makes it awake: the lock screen is still up.
     */
    fun next(tv: Boolean, current: Reading?, event: Event, interactive: Boolean, keyguardLocked: Boolean): Reading = when {
        tv -> when (event) {
            Event.SCREEN_OFF -> Reading(false, STANDBY)
            Event.USER_PRESENT -> Reading(true, inputSeen = true)
            // A dream (screensaver) runs on a TV that is on.
            Event.SCREEN_ON, Event.DREAMING_STARTED, Event.DREAMING_STOPPED -> if (interactive) Reading(true) else Reading(false, STANDBY)
        }
        else -> when (event) {
            Event.SCREEN_OFF -> Reading(false, SCREEN_OFF)
            Event.SCREEN_ON -> Reading(false, LOCKED)
            Event.USER_PRESENT -> Reading(true, inputSeen = true)
            // A phone dreaming (a charging screensaver) keeps whatever it was.
            Event.DREAMING_STARTED, Event.DREAMING_STOPPED -> current ?: snapshot(tv = false, interactive, keyguardLocked)
        }
    }

    /** Whether [b] should be sent after [a]: the value or the reason changed, or a person just unlocked it. */
    fun changed(a: Reading?, b: Reading): Boolean = a == null || a.awake != b.awake || a.sleepState != b.sleepState || b.inputSeen == true

    /**
     * Listens for the screen and lock broadcasts while the foreground service runs (registered at
     * runtime: Android delivers none of them to manifest receivers). [onChange] gets every reading
     * worth sending; [onScreenOn] runs at every `SCREEN_ON`, so a connection that went quiet while
     * the screen was off is re-made at once.
     */
    class Watcher(
        private val context: Context,
        private val tv: () -> Boolean,
        private val onChange: (Reading) -> Unit,
        private val onScreenOn: () -> Unit,
    ) {
        private val power = context.getSystemService(PowerManager::class.java)
        private val keyguard = context.getSystemService(KeyguardManager::class.java)
        private val main = Handler(Looper.getMainLooper())
        @Volatile var current: Reading? = null
            private set
        private var registered = false

        private val receiver = object : BroadcastReceiver() {
            override fun onReceive(c: Context, intent: Intent) {
                val event = when (intent.action) {
                    Intent.ACTION_SCREEN_OFF -> Event.SCREEN_OFF
                    Intent.ACTION_SCREEN_ON -> Event.SCREEN_ON
                    Intent.ACTION_USER_PRESENT -> Event.USER_PRESENT
                    Intent.ACTION_DREAMING_STARTED -> Event.DREAMING_STARTED
                    Intent.ACTION_DREAMING_STOPPED -> Event.DREAMING_STOPPED
                    else -> return
                }
                if (event == Event.SCREEN_OFF) {
                    // Keeps the CPU up long enough for the frame to leave; never turns the screen on.
                    runCatching {
                        power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "SiliconExtend:awake").acquire(SCREEN_OFF_HOLD_MS)
                    }
                }
                apply(Wakefulness.next(tv(), current, event, interactive(), keyguardLocked()))
                if (event == Event.SCREEN_ON) {
                    onScreenOn()
                    // A phone without a lock screen sends USER_PRESENT as it wakes; one whose lock
                    // screen Smart Lock or the like skipped may not. Look again once it has settled.
                    if (!tv()) main.postDelayed(settle, SETTLE_MS)
                }
            }
        }

        private val settle = Runnable {
            val now = current
            if (now != null && !now.awake && interactive() && !keyguardLocked()) apply(Reading(true))
        }

        fun start() {
            if (registered) return
            registered = true
            val filter = IntentFilter().apply {
                addAction(Intent.ACTION_SCREEN_OFF)
                addAction(Intent.ACTION_SCREEN_ON)
                addAction(Intent.ACTION_USER_PRESENT)
                addAction(Intent.ACTION_DREAMING_STARTED)
                addAction(Intent.ACTION_DREAMING_STOPPED)
            }
            // Exported: USER_PRESENT comes from SystemUI, not the system process, and a not-exported
            // receiver never gets it (before Android 13 ContextCompat guards one with a permission
            // SystemUI doesn't hold). These are all protected broadcasts: no app can send them.
            ContextCompat.registerReceiver(context, receiver, filter, ContextCompat.RECEIVER_EXPORTED)
            apply(read())
        }

        fun stop() {
            if (!registered) return
            registered = false
            main.removeCallbacks(settle)
            runCatching { context.unregisterReceiver(receiver) }
        }

        /** Reads the state again (a connection opened): a phone unlocked meanwhile is awake now. */
        fun read(): Reading = snapshot(tv(), interactive(), keyguardLocked())

        /**
         * The reading to send on a connection that just opened, read afresh (the device may have
         * been unlocked while it was away). A change also goes to the other connections.
         */
        fun forConnect(): Reading {
            apply(read())
            return current ?: read()
        }

        private fun apply(reading: Reading) {
            val before = current
            current = reading
            if (changed(before, reading)) {
                Extend.log("awake: ${reading.awake}${reading.sleepState?.let { " ($it)" } ?: ""}${if (reading.inputSeen == true) ", unlocked" else ""}")
                onChange(reading)
            }
        }

        private fun interactive(): Boolean = runCatching { power.isInteractive }.getOrDefault(true)
        private fun keyguardLocked(): Boolean = runCatching { keyguard.isKeyguardLocked }.getOrDefault(false)
    }

    /** How long the CPU is kept up after the screen goes off, so the `awake` frame is sent. */
    const val SCREEN_OFF_HOLD_MS = 5_000L

    /** How long after SCREEN_ON a phone without USER_PRESENT is looked at again. */
    const val SETTLE_MS = 1_500L
}

package com.teamofsilicons.extend.service

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import com.teamofsilicons.extend.Extend
import com.teamofsilicons.extend.R
import com.teamofsilicons.extend.core.WakeNotice
import com.teamofsilicons.extend.core.WakeRequests
import com.teamofsilicons.extend.ui.MainActivity
import kotlinx.coroutines.delay
import java.time.Instant

/**
 * The one notification a phone or tablet shows for every open wake request (UNDERSTANDING.md,
 * Waking a device): the Silicon's name and reason, on the lock screen only its name
 * ([Notification.VISIBILITY_PRIVATE] with a name-only public version). It never turns the screen
 * on and has no full-screen intent: Extend never wakes a device; the Carbon does, by unlocking it.
 *
 * A TV shows none: it answers that it can't, and its Carbon hears through Ting.
 */
object WakeNotifications {
    const val CHANNEL_WAKE = "wake"
    const val ID_WAKE = 4

    /** A quiet post joins this group, which never alerts (Notification.GROUP_ALERT_SUMMARY with no summary). */
    private const val QUIET_GROUP = "com.teamofsilicons.extend.wake.quiet"

    fun createChannel(context: Context) {
        context.getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL_WAKE, "Silicons asking to use this device", NotificationManager.IMPORTANCE_HIGH).apply {
                description = "Shows when a Silicon asks you to wake or unlock this device, with its name and reason."
                setShowBadge(true)
                lockscreenVisibility = Notification.VISIBILITY_PRIVATE
            },
        )
    }

    /** What `wake_request_shown` answers: whether the device could show the request, and why not. */
    data class Showing(val shown: Boolean, val note: String?)

    fun showing(context: Context, tv: Boolean, noun: String): Showing {
        if (tv) return Showing(false, "This TV can't show notifications; its Carbon was told through Ting.")
        val nm = context.getSystemService(NotificationManager::class.java)
        val off = Showing(false, "Notifications are off for Silicon Extend on this $noun.")
        // Android 13+: a runtime permission; before that, the app-wide switch in Settings.
        if (Build.VERSION.SDK_INT >= 33 && context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) return off
        if (!runCatching { nm.areNotificationsEnabled() }.getOrDefault(true)) return off
        val channel = runCatching { nm.getNotificationChannel(CHANNEL_WAKE) }.getOrNull()
        if (channel != null && channel.importance == NotificationManager.IMPORTANCE_NONE) {
            return Showing(false, "Wake requests are turned off for Silicon Extend on this $noun (its “Silicons asking to use this device” notifications).")
        }
        return Showing(true, null)
    }

    /**
     * Posts (or removes) the notification for [lines]. [alert]: a new request may sound or vibrate
     * (the service allows it at most every 15 minutes); every other post is quiet. [wait]: return
     * only once Android shows the new version, so what a Silicon reads right after (the
     * `notifications` command, `dumpsys notification`) is never the old one.
     */
    suspend fun render(context: Context, lines: List<WakeRequests.Line>, noun: String, alert: Boolean, wait: Boolean) {
        val nm = context.getSystemService(NotificationManager::class.java)
        val notice = WakeNotice.of(lines, noun)
        if (notice == null) {
            nm.cancel(ID_WAKE)
            if (wait) settle(nm) { it == null }
            return
        }
        val expires = lines.mapNotNull { runCatching { Instant.parse(it.expiresAt).toEpochMilli() }.getOrNull() }.maxOrNull()
        val n = build(context, notice, alert, expires?.let { it - System.currentTimeMillis() })
        // Android drops an update when the app posts more than 5 a second, so a version that must
        // replace what is showing (a session just started) is posted as a new notification, which
        // it never drops: the old one is removed first.
        if (wait) runCatching { nm.cancel(ID_WAKE) }
        runCatching { nm.notify(ID_WAKE, n) }.onFailure { Extend.log("couldn't post the wake notification", it) }
        if (wait) {
            val shown = settle(nm) { it?.extras?.getCharSequence(Notification.EXTRA_TITLE)?.toString() == notice.title && it.extras?.getCharSequence(Notification.EXTRA_TEXT)?.toString() == notice.text }
            if (!shown) {
                // Never leave the old words up: better no notification than another side's request.
                Extend.log("the wake notification didn't update in time; removed it")
                nm.cancel(ID_WAKE)
                settle(nm) { it == null }
            }
        }
    }

    /**
     * Everything the notification is, as plain values (JVM tests check it): private on the lock
     * screen with a name-only public version, a reminder, alerting only for a new request the
     * service marked `alert`, and gone by itself when the last request expires.
     */
    data class Spec(
        val title: String,
        val text: String,
        val bigText: String,
        val publicTitle: String,
        val publicText: String,
        val visibility: Int,
        val category: String,
        val onlyAlertOnce: Boolean,
        /** Posted in a group that never alerts. */
        val quiet: Boolean,
        val timeoutMs: Long?,
        val fullScreen: Boolean = false,
    ) {
        companion object {
            fun of(notice: WakeNotice, alert: Boolean, timeoutMs: Long?) = Spec(
                title = notice.title,
                text = notice.text,
                bigText = notice.bigText,
                publicTitle = notice.publicTitle,
                publicText = notice.publicText,
                visibility = Notification.VISIBILITY_PRIVATE,
                category = Notification.CATEGORY_REMINDER,
                onlyAlertOnce = !alert,
                quiet = !alert,
                timeoutMs = timeoutMs?.takeIf { it > 0 },
            )
        }
    }

    fun build(context: Context, notice: WakeNotice, alert: Boolean, timeoutMs: Long?): Notification = build(context, Spec.of(notice, alert, timeoutMs))

    fun build(context: Context, spec: Spec): Notification {
        val open = PendingIntent.getActivity(
            context, 4, Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        // The lock screen's version when it hides content: who asked, never why.
        val public = Notification.Builder(context, CHANNEL_WAKE)
            .setSmallIcon(R.drawable.ic_extend_mark)
            .setColor(context.getColor(R.color.extend_cobalt))
            .setContentTitle(spec.publicTitle)
            .setContentText(spec.publicText)
            .build()
        val b = Notification.Builder(context, CHANNEL_WAKE)
            .setSmallIcon(R.drawable.ic_extend_mark)
            .setColor(context.getColor(R.color.extend_cobalt))
            .setContentTitle(spec.title)
            .setContentText(spec.text)
            .setStyle(Notification.BigTextStyle().bigText(spec.bigText))
            .setCategory(spec.category)
            .setVisibility(spec.visibility)
            .setPublicVersion(public)
            .setOnlyAlertOnce(spec.onlyAlertOnce)
            .setShowWhen(true)
            .setContentIntent(open)
        if (spec.quiet) {
            // Quiet on every Android version (Notification.Builder.setSilent is Android 12+).
            b.setGroup(QUIET_GROUP).setGroupAlertBehavior(Notification.GROUP_ALERT_SUMMARY)
        }
        // Gone by itself when the last request expires, even if the app misses the end.
        spec.timeoutMs?.let { b.setTimeoutAfter(it) }
        return b.build()
    }

    fun cancel(context: Context) {
        runCatching { context.getSystemService(NotificationManager::class.java).cancel(ID_WAKE) }
    }

    /** Waits (up to a second) until Android's active notifications show what [done] expects; whether they did. */
    private suspend fun settle(nm: NotificationManager, done: (Notification?) -> Boolean): Boolean {
        repeat(20) {
            val current = runCatching { nm.activeNotifications.firstOrNull { it.id == ID_WAKE }?.notification }.getOrNull()
            if (done(current)) return true
            delay(50)
        }
        return false
    }
}

package com.teamofsilicons.extend.notif

import android.app.Notification
import android.content.ComponentName
import android.content.Context
import android.service.notification.NotificationListenerService
import com.teamofsilicons.extend.Extend

/** Lets a Silicon read the phone's current notifications (`extend notifications`). */
class ExtendNotificationListener : NotificationListenerService() {
    override fun onListenerConnected() {
        instance = this
        Extend.get(this).onCapabilitiesMayHaveChanged()
    }

    override fun onListenerDisconnected() {
        if (instance === this) instance = null
        Extend.get(this).onCapabilitiesMayHaveChanged()
    }

    data class Item(
        val key: String,
        val packageName: String,
        val app: String,
        val title: String?,
        val text: String?,
        val postedAtMs: Long,
        val ongoing: Boolean,
        val category: String?,
    )

    fun items(): List<Item> {
        val pm = packageManager
        return activeNotifications.orEmpty()
            .sortedByDescending { it.postTime }
            .map { sbn ->
                val extras = sbn.notification.extras
                val title = extras.getCharSequence(Notification.EXTRA_TITLE)?.toString()
                val text = (extras.getCharSequence(Notification.EXTRA_BIG_TEXT) ?: extras.getCharSequence(Notification.EXTRA_TEXT))?.toString()
                val label = runCatching { pm.getApplicationLabel(pm.getApplicationInfo(sbn.packageName, 0)).toString() }
                    .getOrDefault(sbn.packageName)
                Item(
                    key = sbn.key,
                    packageName = sbn.packageName,
                    app = label,
                    title = title,
                    text = text,
                    postedAtMs = sbn.postTime,
                    ongoing = sbn.isOngoing,
                    category = sbn.notification.category,
                )
            }
    }

    companion object {
        @Volatile var instance: ExtendNotificationListener? = null
            private set

        fun component(context: Context) = ComponentName(context, ExtendNotificationListener::class.java)
    }
}

package com.teamofsilicons.extend.service

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import com.teamofsilicons.extend.Extend
import com.teamofsilicons.extend.R
import com.teamofsilicons.extend.core.Link
import com.teamofsilicons.extend.core.Phase
import com.teamofsilicons.extend.core.UiState
import com.teamofsilicons.extend.ui.MainActivity
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

/**
 * Keeps the device connected to Extend while the app isn't on screen, and carries the in-use
 * notification with its Stop button (phones and tablets).
 */
class ExtendForegroundService : Service() {
    private var scope: CoroutineScope? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        createChannels(this)
        val extend = Extend.get(this)
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(ID_CONNECTION, connectionNotification(extend.state.value), ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        } else {
            startForeground(ID_CONNECTION, connectionNotification(extend.state.value))
        }
        extend.connection.start()
        val s = CoroutineScope(SupervisorJob() + Dispatchers.Main)
        scope = s
        s.launch {
            extend.state.map { NotificationModel.of(it) }.distinctUntilChanged().collect { render(extend.state.value) }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        Extend.get(this).connection.start()
        return START_STICKY
    }

    override fun onDestroy() {
        scope?.cancel()
        super.onDestroy()
    }

    /** The parts of the state the notifications show. */
    private data class NotificationModel(
        val phase: Phase, val link: Link, val code: String?, val silicon: String?, val stopping: Boolean,
        val takeover: String?, val tv: Boolean, val name: String?, val env: String?,
    ) {
        companion object {
            fun of(s: UiState) = NotificationModel(
                s.phase, s.link, s.pairing.code, s.session?.siliconId, s.session?.stopping == true,
                s.takeover?.reason, s.isTv, s.device?.name, s.environment?.name,
            )
        }
    }

    private fun render(state: UiState) {
        val nm = getSystemService(NotificationManager::class.java)
        nm.notify(ID_CONNECTION, connectionNotification(state))
        val session = state.session
        if (session != null && !state.isTv) {
            nm.notify(ID_IN_USE, inUseNotification(state))
        } else {
            nm.cancel(ID_IN_USE)
        }
    }

    private fun openApp(): PendingIntent = PendingIntent.getActivity(
        this, 0, Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    private fun action(action: String, requestCode: Int): PendingIntent = PendingIntent.getBroadcast(
        this, requestCode, Intent(this, ActionReceiver::class.java).setAction(action),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    private fun connectionNotification(state: UiState): Notification {
        val env = state.environment?.let { " · Test environment: ${it.name}" } ?: ""
        val (title, text) = when (state.phase) {
            Phase.STARTING -> "Silicon Extend" to "Starting…"
            Phase.UNPAIRED -> "Waiting to be paired" to (state.pairing.code?.let { "Pairing code $it — enter it on extend.teamofsilicons.com" } ?: "Getting a pairing code…")
            Phase.PAIRED -> when (state.link) {
                Link.CONNECTED -> "Connected to Silicon Extend$env" to (state.device?.let { "${it.name} · paired to ${it.owner.id}" } ?: "Paired")
                Link.CONNECTING -> "Connecting to Silicon Extend…$env" to "Paired"
                Link.OFFLINE -> "Silicon Extend is offline$env" to (state.linkDetail ?: "Reconnecting")
                Link.SUPERSEDED -> "Silicon Extend: connected elsewhere" to (state.linkDetail ?: "")
                Link.UPGRADE_REQUIRED -> "Update Silicon Extend" to (state.linkDetail ?: "")
            }
        }
        val builder = Notification.Builder(this, CHANNEL_CONNECTION)
            .setSmallIcon(R.drawable.ic_extend_mark)
            .setContentTitle(title)
            .setContentText(text)
            .setOngoing(true)
            .setContentIntent(openApp())
        if (Build.VERSION.SDK_INT >= 31) builder.setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE)
        return builder.build()
    }

    private fun inUseNotification(state: UiState): Notification {
        val s = state.session!!
        val builder = Notification.Builder(this, CHANNEL_IN_USE)
            .setSmallIcon(R.drawable.ic_extend_mark)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setCategory(Notification.CATEGORY_STATUS)
            .setContentIntent(openApp())
        val takeover = state.takeover
        if (takeover != null) {
            builder.setContentTitle("${s.siliconId} needs you on this device")
                .setContentText(takeover.reason)
                .setStyle(Notification.BigTextStyle().bigText(takeover.reason + "\nTap Done when you've finished."))
                .addAction(Notification.Action.Builder(null, "Done", action(ActionReceiver.ACTION_TAKEOVER_DONE, 2)).build())
                .addAction(Notification.Action.Builder(null, "Stop", action(ActionReceiver.ACTION_STOP, 1)).build())
        } else {
            builder.setContentTitle(if (s.stopping) "Stopping ${s.siliconId}…" else "${s.siliconId} is using this device")
                .setContentText("Tap Stop to end the session now.")
                .addAction(Notification.Action.Builder(null, "Stop", action(ActionReceiver.ACTION_STOP, 1)).build())
        }
        return builder.build()
    }

    companion object {
        const val CHANNEL_CONNECTION = "connection"
        const val CHANNEL_IN_USE = "in_use"
        const val ID_CONNECTION = 1
        const val ID_IN_USE = 2

        fun createChannels(context: Context) {
            val nm = context.getSystemService(NotificationManager::class.java)
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL_CONNECTION, "Connection", NotificationManager.IMPORTANCE_LOW).apply {
                    description = "Shows that this device is connected to Silicon Extend."
                },
            )
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL_IN_USE, "Silicon using this device", NotificationManager.IMPORTANCE_HIGH).apply {
                    description = "Shows which Silicon is using this device, with a Stop button."
                    setShowBadge(true)
                },
            )
        }

        fun start(context: Context) {
            runCatching {
                context.startForegroundService(Intent(context, ExtendForegroundService::class.java))
            }.onFailure { Extend.log("couldn't start the foreground service", it) }
        }
    }
}

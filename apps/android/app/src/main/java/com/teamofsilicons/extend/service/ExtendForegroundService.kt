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
import com.teamofsilicons.extend.adb.DebuggingAfterRestart
import com.teamofsilicons.extend.config.DeviceInfo
import com.teamofsilicons.extend.core.InUseIndicator
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
 * Keeps the device connected to Extend while the app isn't on screen, carries the in-use
 * notification with its Stop button (phones and tablets), and listens for the screen turning on
 * and off, so every connection says whether the device is awake ([com.teamofsilicons.extend.core.Wakefulness]).
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
        // Screen and lock broadcasts reach only receivers registered at runtime, so they are
        // registered here, for as long as the service keeps the device connected.
        extend.connection.wakefulness.start()
        extend.connection.start()
        val s = CoroutineScope(SupervisorJob() + Dispatchers.Main)
        scope = s
        // Each notification is posted again only when what it shows changed: Android drops updates
        // beyond 5 a second per app, which could hold back the wake notification's.
        s.launch {
            extend.state.map { NotificationModel.of(it).copy(silicon = null, stopping = false, takeover = null, through = null) }
                .distinctUntilChanged().collect { renderConnection(extend.state.value) }
        }
        s.launch {
            extend.state.map { st -> st.session?.let { InUseModel(it.siliconId, it.stopping, st.takeover?.reason, it.carbon, st.pairs.size > 1, st.isTv, InUseIndicator.show(st)) } }
                .distinctUntilChanged().collect { renderInUse(extend.state.value) }
        }
        s.launch {
            // After a restart turned Wireless debugging off, ask the Carbon to turn it back on.
            extend.state.map { (it.report?.debuggingAfterRestart == DebuggingAfterRestart.Status.OFF) to it.isTv }
                .distinctUntilChanged()
                .collect { (off, tv) ->
                    val nm = getSystemService(NotificationManager::class.java)
                    if (off) nm.notify(ID_DEBUGGING, debuggingOffNotification(tv)) else nm.cancel(ID_DEBUGGING)
                }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        Extend.get(this).connection.start()
        return START_STICKY
    }

    override fun onDestroy() {
        Extend.get(this).connection.wakefulness.stop()
        scope?.cancel()
        super.onDestroy()
    }

    /** The parts of the state the notifications show. */
    private data class NotificationModel(
        val phase: Phase, val link: Link, val detail: String?, val code: String?, val silicon: String?, val stopping: Boolean,
        val takeover: String?, val tv: Boolean, val pairs: List<Pair<String?, String>>, val env: String?, val through: String?,
    ) {
        companion object {
            fun of(s: UiState) = NotificationModel(
                s.phase, s.link, s.linkDetail, s.pairing.code, s.session?.siliconId, s.session?.stopping == true,
                s.takeover?.reason, s.isTv, s.pairs.map { it.name to it.carbon }, s.environment?.name, s.session?.carbon,
            )
        }
    }

    /** What the in-use notification shows. */
    private data class InUseModel(val silicon: String, val stopping: Boolean, val takeover: String?, val carbon: String?, val several: Boolean, val tv: Boolean, val show: InUseIndicator.Show)

    private fun renderConnection(state: UiState) {
        getSystemService(NotificationManager::class.java).notify(ID_CONNECTION, connectionNotification(state))
    }

    private fun renderInUse(state: UiState) {
        val nm = getSystemService(NotificationManager::class.java)
        if (state.session != null && !state.isTv && InUseIndicator.show(state) != InUseIndicator.Show.NONE) {
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
        val app = DeviceInfo.appName(state.isTv)
        val (title, text) = when (state.phase) {
            Phase.STARTING -> app to "Starting…"
            Phase.UNPAIRED -> "Waiting to be paired" to (state.pairing.code?.let { "Pairing code $it — enter it on extend.teamofsilicons.com" } ?: "Getting a pairing code…")
            Phase.PAIRED -> when (state.link) {
                Link.CONNECTED -> "Connected to $app$env" to pairedLine(state)
                Link.CONNECTING -> "Connecting to $app…$env" to "Paired"
                Link.OFFLINE -> "$app is offline$env" to (state.linkDetail ?: "Reconnecting")
                Link.SUPERSEDED -> "$app: connected elsewhere" to (state.linkDetail ?: "")
                Link.UPGRADE_REQUIRED -> "Update $app" to (state.linkDetail ?: "")
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

    /** "Living room TV · paired to c:alice", or "Paired to c:alice and c:bob" for a device several Carbons paired. */
    private fun pairedLine(state: UiState): String {
        val pairs = state.pairs
        return when {
            pairs.isEmpty() -> "Paired"
            pairs.size == 1 -> pairs[0].let { p -> (p.name?.let { "$it · " } ?: "") + "paired to ${p.carbon}" }
            else -> "Paired to " + com.teamofsilicons.extend.core.SetupReport.list(pairs.map { it.carbon })
        }
    }

    private fun inUseNotification(state: UiState): Notification {
        val s = state.session!!
        // Which Carbon gave it access, once the device has more than one.
        val through = s.carbon?.takeIf { state.pairs.size > 1 }?.let { "Through $it. " } ?: ""
        val builder = Notification.Builder(this, CHANNEL_IN_USE)
            .setSmallIcon(R.drawable.ic_extend_mark)
            // Interface's cobalt for the icon, app name and actions.
            .setColor(getColor(R.color.extend_cobalt))
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
                .setContentText("${through}Tap Stop to end the session now.")
                .addAction(Notification.Action.Builder(null, "Stop", action(ActionReceiver.ACTION_STOP, 1)).build())
        }
        return builder.build()
    }

    /** Wireless debugging went off with a restart: one tap opens its settings page. */
    private fun debuggingOffNotification(tv: Boolean): Notification {
        val m = DebuggingOffMessage.of(tv)
        val open = PendingIntent.getActivity(
            this, 3, DebuggingAfterRestart.wirelessDebuggingIntent(this).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return Notification.Builder(this, CHANNEL_SETUP)
            .setSmallIcon(R.drawable.ic_extend_mark)
            .setColor(getColor(R.color.extend_cobalt))
            .setContentTitle(m.title)
            .setContentText(m.text)
            .setStyle(Notification.BigTextStyle().bigText(m.bigText))
            .setCategory(Notification.CATEGORY_REMINDER)
            .setOnlyAlertOnce(true)
            .setAutoCancel(true)
            .setContentIntent(open)
            .addAction(Notification.Action.Builder(null, m.action, open).build())
            .build()
    }

    companion object {
        const val CHANNEL_CONNECTION = "connection"
        const val CHANNEL_IN_USE = "in_use"
        const val CHANNEL_SETUP = "setup"
        const val ID_CONNECTION = 1
        const val ID_IN_USE = 2
        const val ID_DEBUGGING = 3
        /** The wake-request notification ([WakeNotifications]). */
        const val ID_WAKE = WakeNotifications.ID_WAKE

        fun createChannels(context: Context) {
            val nm = context.getSystemService(NotificationManager::class.java)
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL_CONNECTION, "Connection", NotificationManager.IMPORTANCE_LOW).apply {
                    description = "Shows that this device is connected to Extend."
                },
            )
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL_IN_USE, "Silicon using this device", NotificationManager.IMPORTANCE_HIGH).apply {
                    description = "Shows which Silicon is using this device, with a Stop button."
                    setShowBadge(true)
                },
            )
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL_SETUP, "Setup needs you", NotificationManager.IMPORTANCE_DEFAULT).apply {
                    description = "Asks you to turn something back on, such as wireless debugging after this device restarts."
                },
            )
            WakeNotifications.createChannel(context)
        }

        fun start(context: Context) {
            runCatching {
                context.startForegroundService(Intent(context, ExtendForegroundService::class.java))
            }.onFailure { Extend.log("couldn't start the foreground service", it) }
        }
    }
}

/** The words of the "turn wireless debugging back on" notification. */
data class DebuggingOffMessage(val title: String, val text: String, val bigText: String, val action: String) {
    companion object {
        fun of(tv: Boolean): DebuggingOffMessage {
            val noun = if (tv) "TV" else "phone"
            return DebuggingOffMessage(
                title = "Turn wireless debugging back on",
                text = "This $noun restarted, which turned wireless debugging off.",
                bigText = "This $noun restarted, and Android turns wireless debugging off when it restarts. Silicons can still read and control the screen, " +
                    "but until it is back on they can't install apps, read device logs, record the screen or run adb commands here. Tap to open Wireless debugging and turn it on; " +
                    "Extend reconnects by itself.",
                action = "Open Wireless debugging",
            )
        }
    }
}

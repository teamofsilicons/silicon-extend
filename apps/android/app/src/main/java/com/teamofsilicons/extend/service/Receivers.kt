package com.teamofsilicons.extend.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import com.teamofsilicons.extend.Extend

/** Starts the connection when the device starts (or the app is updated), if it's paired. */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_MY_PACKAGE_REPLACED -> {
                val extend = Extend.get(context)
                if (extend.secrets.readCredential() != null) ExtendForegroundService.start(context)
            }
        }
    }
}

/** Stop and Done from the in-use notification. */
class ActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val extend = Extend.get(context)
        when (intent.action) {
            ACTION_STOP -> extend.connection.stopSession()
            ACTION_TAKEOVER_DONE -> extend.connection.takeoverDone()
        }
    }

    companion object {
        const val ACTION_STOP = "com.teamofsilicons.extend.STOP"
        const val ACTION_TAKEOVER_DONE = "com.teamofsilicons.extend.TAKEOVER_DONE"
    }
}

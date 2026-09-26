package com.teamofsilicons.bridge.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import com.teamofsilicons.bridge.Bridge

/** Starts the connection when the device starts (or the app is updated), if it's paired. */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_MY_PACKAGE_REPLACED -> {
                val bridge = Bridge.get(context)
                if (bridge.secrets.readCredential() != null) BridgeForegroundService.start(context)
            }
        }
    }
}

/** Stop and Done from the in-use notification. */
class ActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val bridge = Bridge.get(context)
        when (intent.action) {
            ACTION_STOP -> bridge.connection.stopSession()
            ACTION_TAKEOVER_DONE -> bridge.connection.takeoverDone()
        }
    }

    companion object {
        const val ACTION_STOP = "com.teamofsilicons.bridge.STOP"
        const val ACTION_TAKEOVER_DONE = "com.teamofsilicons.bridge.TAKEOVER_DONE"
    }
}

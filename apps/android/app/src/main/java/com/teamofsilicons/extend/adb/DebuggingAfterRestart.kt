package com.teamofsilicons.extend.adb

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.provider.Settings

/**
 * Android turns Wireless debugging off whenever the device restarts. When this device had Android
 * debugging connected before its last restart and it is off now, the app asks the Carbon to turn
 * it back on: a notification that opens Wireless debugging, and a setup step that needs the Carbon
 * (sent to Extend in `hello`/`setup_progress`, so the website shows it too).
 */
object DebuggingAfterRestart {
    enum class Status {
        /** Nothing to ask: debugging is connected, was never connected, was disconnected by the Carbon, or no restart since. */
        NONE,
        /** Restarted since debugging last connected, and Wireless debugging is off: the Carbon has to turn it on. */
        OFF,
        /** Wireless debugging is on again and Extend is reconnecting to it. */
        RECONNECTING,
    }

    /**
     * [enabled]: the Carbon connected debugging and hasn't disconnected it in the app. [pairedWithCode]:
     * it is Wireless debugging (a TV's legacy network-debugging port survives restarts). [lastConnectedBoot]
     * and [bootCount] are `Settings.Global.BOOT_COUNT` values.
     */
    fun status(
        enabled: Boolean,
        pairedWithCode: Boolean,
        connected: Boolean,
        lastConnectedBoot: Int?,
        bootCount: Int,
        wirelessDebuggingOn: Boolean,
    ): Status = when {
        !enabled || !pairedWithCode || connected -> Status.NONE
        lastConnectedBoot == null || bootCount < 0 || lastConnectedBoot >= bootCount -> Status.NONE
        !wirelessDebuggingOn -> Status.OFF
        else -> Status.RECONNECTING
    }

    fun status(context: Context, adb: LocalAdb): Status = status(
        enabled = adb.enabled,
        pairedWithCode = adb.pairedWithCode,
        connected = adb.connected,
        lastConnectedBoot = adb.lastConnectedBoot,
        bootCount = LocalAdb.bootCount(context),
        wirelessDebuggingOn = !adb.wirelessDebuggingOff,
    )

    /** Settings' own Wireless debugging tile; long-pressing it opens the Wireless debugging page. */
    private val WIRELESS_DEBUGGING_TILE = ComponentName("com.android.settings", "com.android.settings.development.qstile.DevelopmentTiles\$WirelessDebugging")

    /**
     * Opens the Wireless debugging page itself where Settings supports it (Android 11+: the same
     * intent as long-pressing its quick-settings tile), otherwise Developer options.
     */
    fun wirelessDebuggingIntent(context: Context): Intent {
        val developer = Intent(Settings.ACTION_APPLICATION_DEVELOPMENT_SETTINGS)
        val settingsPackage = runCatching { developer.resolveActivity(context.packageManager)?.packageName }.getOrNull()
        // The page opens only while Developer options are on; before that, Developer options explains.
        val devOptions = runCatching { Settings.Global.getInt(context.contentResolver, Settings.Global.DEVELOPMENT_SETTINGS_ENABLED, 0) == 1 }.getOrDefault(false)
        if (devOptions && settingsPackage == WIRELESS_DEBUGGING_TILE.packageName) {
            val tile = Intent(ACTION_QS_TILE_PREFERENCES)
                .setPackage(settingsPackage)
                .putExtra(Intent.EXTRA_COMPONENT_NAME, WIRELESS_DEBUGGING_TILE)
            if (runCatching { tile.resolveActivity(context.packageManager) }.getOrNull() != null) return tile
        }
        return developer
    }

    /** `TileService.ACTION_QS_TILE_PREFERENCES`. */
    const val ACTION_QS_TILE_PREFERENCES = "android.service.quicksettings.action.QS_TILE_PREFERENCES"
}

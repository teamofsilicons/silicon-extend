package com.teamofsilicons.extend.config

import android.app.UiModeManager
import android.content.Context
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.os.Build
import com.teamofsilicons.extend.BuildConfig
import com.teamofsilicons.extend.protocol.ExtendJson
import com.teamofsilicons.extend.protocol.TestingEnvironment

/**
 * Non-secret settings and what the app remembers about its pair. The device credential itself
 * lives in [com.teamofsilicons.extend.security.SecretStore].
 */
class Config(context: Context) {
    private val prefs = context.getSharedPreferences("extend_config", Context.MODE_PRIVATE)

    /** The Extend service base URL, without a trailing slash. */
    var serviceUrl: String
        get() = (prefs.getString(KEY_SERVICE_URL, null) ?: BuildConfig.DEFAULT_SERVICE_URL).trimEnd('/')
        set(value) = prefs.edit().putString(KEY_SERVICE_URL, value.trim().trimEnd('/')).apply()

    val serviceUrlOverridden: Boolean get() = prefs.contains(KEY_SERVICE_URL)

    fun resetServiceUrl() = prefs.edit().remove(KEY_SERVICE_URL).apply()

    /** Developer setting: behave as a TV (os `android_tv`, badge, remote) on a phone or emulator. */
    var forceTv: Boolean
        get() = prefs.getBoolean(KEY_FORCE_TV, false)
        set(value) = prefs.edit().putBoolean(KEY_FORCE_TV, value).apply()

    var deviceId: String?
        get() = prefs.getString(KEY_DEVICE_ID, null)
        set(value) = prefs.edit().putString(KEY_DEVICE_ID, value).apply()

    var environment: TestingEnvironment?
        get() = prefs.getString(KEY_ENVIRONMENT, null)?.let {
            runCatching { ExtendJson.decodeFromString(TestingEnvironment.serializer(), it) }.getOrNull()
        }
        set(value) = prefs.edit().putString(
            KEY_ENVIRONMENT,
            value?.let { ExtendJson.encodeToString(TestingEnvironment.serializer(), it) },
        ).apply()

    fun clearPair() {
        prefs.edit().remove(KEY_DEVICE_ID).remove(KEY_ENVIRONMENT).apply()
    }

    fun webSocketUrl(path: String): String {
        val base = serviceUrl
        val ws = when {
            base.startsWith("https://") -> "wss://" + base.removePrefix("https://")
            base.startsWith("http://") -> "ws://" + base.removePrefix("http://")
            else -> base
        }
        return ws + path
    }

    companion object {
        private const val KEY_SERVICE_URL = "service_url"
        private const val KEY_FORCE_TV = "force_tv"
        private const val KEY_DEVICE_ID = "device_id"
        private const val KEY_ENVIRONMENT = "environment"
    }
}

/** What kind of device this is, as the protocol names it. */
object DeviceInfo {
    const val APP_VERSION: String = BuildConfig.VERSION_NAME

    /** The app's name on phones and tablets. */
    const val APP_NAME = "Silicon Extend"

    /** The app's name on TVs (UNDERSTANDING.md: "Silicon Extend TV"); `res/values-television` gives the launcher the same. */
    const val TV_APP_NAME = "Silicon Extend TV"

    /** The name the app's own screens and notifications use. */
    fun appName(tv: Boolean): String = if (tv) TV_APP_NAME else APP_NAME

    /**
     * The name Android's Settings list the app under (its label as the system resolves it: the TV
     * name on a television), for help text that walks the Carbon through Settings.
     */
    fun systemLabel(context: Context): String =
        runCatching { context.getString(com.teamofsilicons.extend.R.string.app_name) }.getOrNull()?.takeIf { it.isNotBlank() } ?: APP_NAME

    fun isTv(context: Context, config: Config): Boolean {
        if (config.forceTv) return true
        val ui = context.getSystemService(UiModeManager::class.java)
        if (ui?.currentModeType == Configuration.UI_MODE_TYPE_TELEVISION) return true
        val pm = context.packageManager
        return pm.hasSystemFeature(PackageManager.FEATURE_LEANBACK) ||
            pm.hasSystemFeature(PackageManager.FEATURE_TELEVISION) ||
            isFireTv(context)
    }

    fun isFireTv(context: Context): Boolean =
        context.packageManager.hasSystemFeature("amazon.hardware.fire_tv")

    /** `android` or `android_tv`. */
    fun os(context: Context, config: Config): String = if (isTv(context, config)) "android_tv" else "android"

    val osVersion: String get() = Build.VERSION.RELEASE ?: Build.VERSION.SDK_INT.toString()

    val model: String get() = Build.MODEL ?: "Android device"
}

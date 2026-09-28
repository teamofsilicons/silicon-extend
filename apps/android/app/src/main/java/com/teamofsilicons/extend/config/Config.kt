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
 * Non-secret settings and what the app remembers about its pairs. The credentials themselves live
 * in [com.teamofsilicons.extend.security.SecretStore].
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

    /**
     * The device id of each Carbon's pair of this device, in the order they were made. 1.0 kept
     * one ([KEY_DEVICE_ID]); it is read as the first until the list is written.
     */
    var pairIds: List<String>
        get() = prefs.getString(KEY_PAIR_IDS, null)?.split(',')?.filter { it.isNotBlank() }
            ?: listOfNotNull(prefs.getString(KEY_DEVICE_ID, null))
        set(value) = prefs.edit().putString(KEY_PAIR_IDS, value.joinToString(",")).remove(KEY_DEVICE_ID).apply()

    /** The first pair's device id (the app's first enrollment), or null when unpaired. */
    val deviceId: String? get() = pairIds.firstOrNull()

    val paired: Boolean get() = pairIds.isNotEmpty()

    /** The test environment this device is in. Every pair of one device is in the same one. */
    var environment: TestingEnvironment?
        get() = prefs.getString(KEY_ENVIRONMENT, null)?.let {
            runCatching { ExtendJson.decodeFromString(TestingEnvironment.serializer(), it) }.getOrNull()
        }
        set(value) = prefs.edit().putString(
            KEY_ENVIRONMENT,
            value?.let { ExtendJson.encodeToString(TestingEnvironment.serializer(), it) },
        ).apply()

    /**
     * The device's `in_use_indicator` as last read from Extend or set here (shown until either
     * says otherwise), so the badge and notification follow it from the moment the app starts.
     */
    var inUseIndicatorShown: Boolean
        get() = prefs.getBoolean(KEY_INDICATOR, true)
        set(value) = prefs.edit().putBoolean(KEY_INDICATOR, value).apply()

    /** The Carbon changed [inUseIndicatorShown] here and Extend hasn't heard yet (offline): sent on the next connection. */
    var inUseIndicatorPending: Boolean
        get() = prefs.getBoolean(KEY_INDICATOR_PENDING, false)
        set(value) = prefs.edit().putBoolean(KEY_INDICATOR_PENDING, value).apply()

    /** The session the badge or notification last announced ([com.teamofsilicons.extend.core.InUseIndicator.key]). */
    var announcedSession: String?
        get() = prefs.getString(KEY_ANNOUNCED, null)
        set(value) = prefs.edit().putString(KEY_ANNOUNCED, value).apply()

    /** One Carbon's pair ended; the environment goes with the last one. */
    fun clearPair(deviceId: String) {
        val rest = pairIds.filter { it != deviceId }
        if (rest.isEmpty()) clearPairs() else pairIds = rest
    }

    /** Every pair ended: a new pairing starts from the defaults (the indicator shown). */
    fun clearPairs() {
        prefs.edit().remove(KEY_PAIR_IDS).remove(KEY_DEVICE_ID).remove(KEY_ENVIRONMENT)
            .remove(KEY_INDICATOR).remove(KEY_INDICATOR_PENDING).remove(KEY_ANNOUNCED).apply()
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
        /** 1.0: the one pair's device id. */
        private const val KEY_DEVICE_ID = "device_id"
        private const val KEY_PAIR_IDS = "pair_ids"
        private const val KEY_ENVIRONMENT = "environment"
        private const val KEY_INDICATOR = "in_use_indicator_shown"
        private const val KEY_INDICATOR_PENDING = "in_use_indicator_pending"
        private const val KEY_ANNOUNCED = "announced_session"
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
        val pm = context.packageManager
        fun has(feature: String) = runCatching { pm.hasSystemFeature(feature) }.getOrDefault(false)
        return looksLikeTv(
            uiModeTv = ui?.currentModeType == Configuration.UI_MODE_TYPE_TELEVISION,
            features = NOT_A_TV_FEATURES.plus(TV_FEATURES).plus(PackageManager.FEATURE_TOUCHSCREEN).filter(::has).toSet(),
        )
    }

    /** Features only TVs declare (Fire TV's own included). */
    private val TV_FEATURES = listOf(PackageManager.FEATURE_LEANBACK, PackageManager.FEATURE_TELEVISION, FIRE_TV)

    /**
     * Devices without a touchscreen that aren't TVs: Chromebooks and other computers running
     * Android apps, cars, watches and embedded boards. Spelled out: `FEATURE_PC` is Android 8.1+.
     */
    private val NOT_A_TV_FEATURES = listOf(
        "android.hardware.type.pc", "org.chromium.arc", "org.chromium.arc.device_management",
        "android.hardware.type.automotive", "android.hardware.type.watch", "android.hardware.type.embedded",
    )

    private const val FIRE_TV = "amazon.hardware.fire_tv"

    /**
     * Whether a device with these [features] (the ones among [TV_FEATURES], [NOT_A_TV_FEATURES] and
     * the touchscreen it declares) is a TV. Besides Android TV, Google TV and Fire TV, many TV boxes
     * and projectors run the phone build of Android behind their maker's launcher, with no TV
     * feature and a normal UI mode; they have no touchscreen, which no phone or tablet lacks.
     */
    fun looksLikeTv(uiModeTv: Boolean, features: Set<String>): Boolean = when {
        uiModeTv || TV_FEATURES.any { it in features } -> true
        PackageManager.FEATURE_TOUCHSCREEN in features -> false
        else -> NOT_A_TV_FEATURES.none { it in features }
    }

    fun isFireTv(context: Context): Boolean =
        context.packageManager.hasSystemFeature(FIRE_TV)

    /** What the app calls this device in sentences: "TV", "tablet" or "phone". */
    fun noun(context: Context, tv: Boolean): String = when {
        tv -> "TV"
        runCatching { context.resources.configuration.smallestScreenWidthDp }.getOrDefault(0) >= 600 -> "tablet"
        else -> "phone"
    }

    /** `android` or `android_tv`. */
    fun os(context: Context, config: Config): String = if (isTv(context, config)) "android_tv" else "android"

    val osVersion: String get() = Build.VERSION.RELEASE ?: Build.VERSION.SDK_INT.toString()

    val model: String get() = Build.MODEL ?: "Android device"

    /**
     * Keep the app lean: Android calls this a low-RAM device, or gives apps 128 MB of heap or less.
     * TVs and TV boxes with 512 MB–1 GB (a MediaTek MT9255 TV with 440 MB free) kill even a visible
     * app when it takes tens of MB at once, and then its Android debugging connection is gone too.
     */
    fun lean(context: Context): Boolean = runCatching {
        val am = context.getSystemService(android.app.ActivityManager::class.java)
        lean(am.isLowRamDevice, am.memoryClass)
    }.getOrDefault(false)

    fun lean(lowRam: Boolean, memoryClassMb: Int): Boolean = lowRam || memoryClassMb in 1..128
}

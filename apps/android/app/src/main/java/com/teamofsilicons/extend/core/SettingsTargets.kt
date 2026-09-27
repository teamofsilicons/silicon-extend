package com.teamofsilicons.extend.core

import android.content.ActivityNotFoundException
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import com.teamofsilicons.extend.Extend

/** A settings page a setup step sends the Carbon to. */
enum class SettingsPage { ACCESSIBILITY, ABOUT, DEVELOPER_OPTIONS, WIRELESS_DEBUGGING, NOTIFICATION_ACCESS, BATTERY, APP_DETAILS }

/**
 * One way to open a settings page: an action and/or an explicit component, with an optional
 * `package:` data URI and extras. Plain data, so the order of routes is testable on the JVM.
 */
data class SettingsRoute(
    val action: String? = null,
    val component: Pair<String, String>? = null,
    /** The package set on an implicit intent (Settings' own quick-settings tile page). */
    val pkg: String? = null,
    val data: String? = null,
    val extras: Map<String, String> = emptyMap(),
    /** A ComponentName extra: key to (package, class). */
    val componentExtra: Pair<String, Pair<String, String>>? = null,
) {
    fun intent(): Intent {
        val i = if (action != null) Intent(action) else Intent()
        component?.let { (p, c) -> i.component = ComponentName(p, c) }
        pkg?.let { i.setPackage(it) }
        data?.let { i.data = Uri.parse(it) }
        for ((k, v) in extras) i.putExtra(k, v)
        componentExtra?.let { (k, pc) -> i.putExtra(k, ComponentName(pc.first, pc.second)) }
        return i
    }
}

/**
 * A page and every way to reach it, in order. [name] and [where] go into the message shown when
 * none of the routes opens anything on this device.
 */
data class SettingsTarget(val page: SettingsPage, val name: String, val where: String, val routes: List<SettingsRoute>) {
    /** Shown when nothing opened: which setting, and where it usually is. */
    fun unavailableMessage(tv: Boolean): String {
        val noun = if (tv) "TV" else "device"
        val verb = if (tv) "select" else "tap"
        val instead = if (page == SettingsPage.ACCESSIBILITY) {
            " Or connect Android debugging below and $verb \"Turn on accessibility through debugging\"."
        } else ""
        return "This $noun didn't let Extend open $name. It is usually at $where. " +
            "If this $noun's own settings menu doesn't show it, ask its maker how to reach Android's $name.$instead"
    }
}

/**
 * Every setup button's routes. Some TVs, TV boxes and projectors replace Android's settings with
 * their maker's own menu and hide Android's pages, so a button is often the Carbon's only way in:
 *
 * - On an Android TV (not Fire TV) Android TV Settings' own page is tried first, by its explicit
 *   component. A maker's menu can claim the standard action and open a page without the setting
 *   the step needs (for example an "About" with no Build entry to select).
 * - Then the standard `Settings` action.
 * - Then the phone Settings app's page, by component (TV boxes and projectors often run the phone
 *   build of Android behind a custom launcher).
 * - Then the main settings screen, as a last way in.
 * A route is used only when Android resolves it, and a route that fails to start is skipped, so
 * a component that doesn't exist on this version costs nothing. When nothing opens, the app
 * shows [SettingsTarget.unavailableMessage].
 */
object SettingsRoutes {
    const val TV_SETTINGS = "com.android.tv.settings"
    const val SETTINGS = "com.android.settings"

    // android.provider.Settings actions, spelled out so the routes stay plain JVM data.
    const val ACTION_SETTINGS = "android.settings.SETTINGS"
    const val ACTION_ACCESSIBILITY = "android.settings.ACCESSIBILITY_SETTINGS"
    const val ACTION_DEVICE_INFO = "android.settings.DEVICE_INFO_SETTINGS"
    const val ACTION_DEVELOPMENT = "android.settings.APPLICATION_DEVELOPMENT_SETTINGS"
    /** The same page's older action, which Android TV Settings also declares. */
    const val ACTION_DEVELOPMENT_LEGACY = "com.android.settings.APPLICATION_DEVELOPMENT_SETTINGS"
    const val ACTION_NOTIFICATION_LISTENER = "android.settings.ACTION_NOTIFICATION_LISTENER_SETTINGS"
    /** Android 11+. */
    const val ACTION_NOTIFICATION_LISTENER_DETAIL = "android.settings.NOTIFICATION_LISTENER_DETAIL_SETTINGS"
    const val EXTRA_NOTIFICATION_LISTENER_COMPONENT = "android.provider.extra.NOTIFICATION_LISTENER_COMPONENT_NAME"
    const val ACTION_REQUEST_IGNORE_BATTERY = "android.settings.REQUEST_IGNORE_BATTERY_OPTIMIZATIONS"
    const val ACTION_IGNORE_BATTERY = "android.settings.IGNORE_BATTERY_OPTIMIZATION_SETTINGS"
    const val ACTION_APP_DETAILS = "android.settings.APPLICATION_DETAILS_SETTINGS"
    /** `TileService.ACTION_QS_TILE_PREFERENCES`: what long-pressing a quick-settings tile opens. */
    const val ACTION_QS_TILE_PREFERENCES = "android.service.quicksettings.action.QS_TILE_PREFERENCES"
    const val EXTRA_COMPONENT_NAME = "android.intent.extra.COMPONENT_NAME"
    /** Settings' own Wireless debugging tile (Android 11+); long-pressing it opens the Wireless debugging page. */
    val WIRELESS_DEBUGGING_TILE = SETTINGS to "com.android.settings.development.qstile.DevelopmentTiles\$WirelessDebugging"

    /**
     * Android TV Settings' pages. On Android TV 14 the About, Developer options, app details and
     * main pages are the classes below and Accessibility is `oemlink.AccessibilitySettingsActivity`
     * (checked on the Android TV 14 emulator); older TV Settings name Accessibility
     * `system.AccessibilityActivity`. A class a version doesn't have is skipped.
     */
    private val TV_PAGES: Map<SettingsPage, List<String>> = mapOf(
        SettingsPage.ACCESSIBILITY to listOf("$TV_SETTINGS.system.AccessibilityActivity", "$TV_SETTINGS.oemlink.AccessibilitySettingsActivity"),
        SettingsPage.ABOUT to listOf("$TV_SETTINGS.about.AboutActivity", "$TV_SETTINGS.device.DeviceInfoSettingsActivity"),
        SettingsPage.DEVELOPER_OPTIONS to listOf("$TV_SETTINGS.system.development.DevelopmentActivity"),
        SettingsPage.APP_DETAILS to listOf("$TV_SETTINGS.device.apps.AppManagementActivity"),
    )

    /** The phone Settings app's pages (Android 8 onwards; older names after newer). */
    private val PHONE_PAGES: Map<SettingsPage, List<String>> = mapOf(
        SettingsPage.ACCESSIBILITY to listOf("$SETTINGS.Settings\$AccessibilitySettingsActivity"),
        SettingsPage.ABOUT to listOf("$SETTINGS.Settings\$MyDeviceInfoActivity", "$SETTINGS.Settings\$DeviceInfoSettingsActivity"),
        SettingsPage.DEVELOPER_OPTIONS to listOf(
            "$SETTINGS.Settings\$DevelopmentSettingsDashboardActivity",
            "$SETTINGS.Settings\$DevelopmentSettingsActivity",
            "$SETTINGS.DevelopmentSettings",
        ),
        SettingsPage.NOTIFICATION_ACCESS to listOf("$SETTINGS.Settings\$NotificationAccessSettingsActivity"),
    )

    /** Android's main settings screens by class: Android TV Settings' and the phone Settings app's. */
    val MAIN_CLASSES: List<Pair<String, String>> = listOf(TV_SETTINGS to "$TV_SETTINGS.MainSettings", SETTINGS to "$SETTINGS.Settings")

    private val MAIN = listOf(SettingsRoute(ACTION_SETTINGS)) + MAIN_CLASSES.map { SettingsRoute(component = it) }

    /** The main settings screen: a way in, but not the page itself (the step then offers other screens). */
    fun isMain(route: SettingsRoute): Boolean = route in MAIN

    /**
     * Whether a button that took [route] and opened [opened] reached only a main settings screen:
     * the route is one ([isMain]), or what opened is one, by Android's class or because it also
     * answers the main settings action ([mainHandlers]: those handlers). A maker's menu that claims
     * the Accessibility action opens its main page, and a route to a page can land there.
     */
    fun reachedMain(route: SettingsRoute, opened: OpenedScreen, mainHandlers: Collection<Pair<String, String>>): Boolean =
        isMain(route) || (MAIN_CLASSES + mainHandlers).any { (p, c) -> opened.isOne(p, c) }

    /** Android's own classes for [page]: Android TV Settings' first, then the phone Settings app's. */
    fun known(page: SettingsPage): List<Pair<String, String>> =
        TV_PAGES[page].orEmpty().map { TV_SETTINGS to it } + PHONE_PAGES[page].orEmpty().map { SETTINGS to it }

    /**
     * The routes to [page], in order, for this device. [pkg]: this app's package (for per-app
     * pages); [listener]: the notification listener's flattened component; [devOptions]: whether
     * Developer options are on (the Wireless debugging page opens only then).
     */
    fun routes(
        page: SettingsPage,
        sdk: Int,
        tv: Boolean,
        fire: Boolean,
        pkg: String,
        listener: String? = null,
        devOptions: Boolean = true,
    ): List<SettingsRoute> {
        val packageData = "package:$pkg"
        fun explicit(pages: Map<SettingsPage, List<String>>, owner: String, data: String? = null) =
            pages[page].orEmpty().map { SettingsRoute(component = owner to it, data = data) }
        val standard: List<SettingsRoute> = when (page) {
            SettingsPage.ACCESSIBILITY -> listOf(SettingsRoute(ACTION_ACCESSIBILITY))
            SettingsPage.ABOUT -> listOf(SettingsRoute(ACTION_DEVICE_INFO))
            SettingsPage.DEVELOPER_OPTIONS -> listOf(SettingsRoute(ACTION_DEVELOPMENT), SettingsRoute(ACTION_DEVELOPMENT_LEGACY))
            SettingsPage.WIRELESS_DEBUGGING -> {
                // The page itself (Android 11+, only while Developer options are on), else Developer options.
                val tile = if (sdk >= 30 && devOptions) {
                    listOf(SettingsRoute(ACTION_QS_TILE_PREFERENCES, pkg = SETTINGS, componentExtra = EXTRA_COMPONENT_NAME to WIRELESS_DEBUGGING_TILE))
                } else emptyList()
                return tile + routes(SettingsPage.DEVELOPER_OPTIONS, sdk, tv, fire, pkg, listener, devOptions)
            }
            SettingsPage.NOTIFICATION_ACCESS -> buildList {
                if (sdk >= 30 && listener != null) {
                    add(SettingsRoute(ACTION_NOTIFICATION_LISTENER_DETAIL, extras = mapOf(EXTRA_NOTIFICATION_LISTENER_COMPONENT to listener)))
                }
                add(SettingsRoute(ACTION_NOTIFICATION_LISTENER))
            }
            SettingsPage.BATTERY -> {
                val own = listOf(SettingsRoute(ACTION_REQUEST_IGNORE_BATTERY, data = packageData), SettingsRoute(ACTION_IGNORE_BATTERY))
                return own + routes(SettingsPage.APP_DETAILS, sdk, tv, fire, pkg, listener, devOptions)
            }
            SettingsPage.APP_DETAILS -> listOf(SettingsRoute(ACTION_APP_DETAILS, data = packageData))
        }
        val perAppData = if (page == SettingsPage.APP_DETAILS) packageData else null
        val tvFirst = if (tv && !fire) explicit(TV_PAGES, TV_SETTINGS, perAppData) else emptyList()
        // Fire TV's settings answer the standard actions; the phone Settings pages would be the wrong UI there.
        val phone = if (fire) emptyList() else explicit(PHONE_PAGES, SETTINGS, perAppData)
        return (tvFirst + standard + phone + MAIN).distinct()
    }

    /** Where each page usually is, for the message shown when nothing opens. */
    fun where(page: SettingsPage, sdk: Int, tv: Boolean, fire: Boolean, label: String): String = when (page) {
        SettingsPage.ACCESSIBILITY -> when {
            fire -> "Settings › Accessibility › $label"
            tv -> "Settings › Device Preferences › Accessibility › $label"
            else -> "Settings › Accessibility › $label"
        }
        SettingsPage.ABOUT -> when {
            fire -> "Settings › My Fire TV › About"
            tv -> "Settings › Device Preferences › About"
            sdk < 28 -> "Settings › System › About phone"
            else -> "Settings › About phone"
        }
        SettingsPage.DEVELOPER_OPTIONS -> when {
            fire -> "Settings › My Fire TV › Developer options"
            tv -> "Settings › Device Preferences › Developer options"
            else -> "Settings › System › Developer options"
        }
        SettingsPage.WIRELESS_DEBUGGING -> if (tv) "Settings › Device Preferences › Developer options › Wireless debugging"
            else "Settings › System › Developer options › Wireless debugging"
        SettingsPage.NOTIFICATION_ACCESS -> if (sdk < 31) "Settings › Apps & notifications › Special app access › Notification access › $label"
            else "Settings › Notifications › Device & app notifications › $label"
        SettingsPage.BATTERY -> if (sdk < 31) "Settings › Apps & notifications › Special app access › Battery optimisation › $label"
            else "Settings › Apps › $label › App battery usage"
        SettingsPage.APP_DETAILS -> "Settings › Apps › $label"
    }

    private val NAMES = mapOf(
        SettingsPage.ACCESSIBILITY to "Accessibility settings",
        SettingsPage.ABOUT to "the About screen",
        SettingsPage.DEVELOPER_OPTIONS to "Developer options",
        SettingsPage.WIRELESS_DEBUGGING to "Wireless debugging",
        SettingsPage.NOTIFICATION_ACCESS to "Notification access",
        SettingsPage.BATTERY to "battery optimisation settings",
        SettingsPage.APP_DETAILS to "this app's settings",
    )

    fun target(
        page: SettingsPage, sdk: Int, tv: Boolean, fire: Boolean, pkg: String, label: String,
        listener: String? = null, devOptions: Boolean = true,
    ) = SettingsTarget(page, NAMES.getValue(page), where(page, sdk, tv, fire, label), routes(page, sdk, tv, fire, pkg, listener, devOptions))
}

/**
 * What a setup button did: opened the page itself ([Outcome.PAGE]), only reached the main
 * settings screen ([Outcome.MAIN_SCREEN], where a maker's menu may not have the entry), or
 * opened nothing ([Outcome.NOTHING], with [message]). [opened]: the activity it opened.
 */
data class OpenResult(val outcome: Outcome, val message: String? = null, val opened: OpenedScreen? = null) {
    enum class Outcome { PAGE, MAIN_SCREEN, NOTHING }

    /** The step should offer the other screens on this device right away. */
    val offerOthers: Boolean get() = outcome != Outcome.PAGE
}

/** Opens a [SettingsTarget] on this device. */
object SettingsLauncher {
    /**
     * The activity [intent] would start, or null when none exists or it can't be started by this
     * app. `Intent.resolveActivity` returns an explicit component without checking that it exists,
     * so PackageManager is asked instead (it checks explicit components too).
     */
    private fun resolve(context: Context, intent: Intent): OpenedScreen? {
        val info = runCatching { context.packageManager.resolveActivity(intent, PackageManager.MATCH_DEFAULT_ONLY) }.getOrNull()?.activityInfo
            ?: return null
        if (!info.exported && info.packageName != context.packageName) return null
        return OpenedScreen(info.packageName, info.name, info.targetActivity)
    }

    /** Every activity that answers the main settings action here (a maker's menu included). */
    private fun mainHandlers(context: Context): List<Pair<String, String>> {
        @Suppress("DEPRECATION")
        val found = runCatching { context.packageManager.queryIntentActivities(Intent(SettingsRoutes.ACTION_SETTINGS), PackageManager.MATCH_ALL) }
            .getOrDefault(emptyList())
        return found.mapNotNull { it.activityInfo }.map { it.packageName to it.name }
    }

    /** The first route Android resolves, as an intent (for a notification's PendingIntent), or null. */
    fun firstResolvable(context: Context, target: SettingsTarget): Intent? = target.routes.asSequence()
        .map { it.intent() }
        .firstOrNull { resolve(context, it) != null }

    /**
     * Tries each route in order: only one Android resolves, started as a new task; one that fails
     * to start (not found, or not exported to other apps) is skipped. Says whether the page itself
     * opened, only a main settings screen (judged by what opened, [SettingsRoutes.reachedMain]), or
     * nothing (with the message naming the setting and where it usually is), and what opened.
     */
    fun open(context: Context, target: SettingsTarget, tv: Boolean): OpenResult {
        for (route in target.routes) {
            val intent = route.intent().addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            val resolved = resolve(context, intent) ?: continue
            try {
                context.startActivity(intent)
                val main = SettingsRoutes.reachedMain(route, resolved, mainHandlers(context))
                Extend.log("setup: opened ${target.page} with ${resolved.component}${if (main) " (a main settings screen)" else ""}")
                return OpenResult(if (main) OpenResult.Outcome.MAIN_SCREEN else OpenResult.Outcome.PAGE, opened = resolved)
            } catch (e: ActivityNotFoundException) {
                Extend.log("setup: ${resolved.component} didn't open ${target.page}", e)
            } catch (e: SecurityException) {
                Extend.log("setup: ${resolved.component} refused to open ${target.page}", e)
            }
        }
        Extend.log("setup: nothing opened ${target.page}")
        return OpenResult(OpenResult.Outcome.NOTHING, target.unavailableMessage(tv))
    }

    /**
     * Opens a screen the finder listed, by its explicit component (with the action it answers, if
     * any), as a new task. Null once it opened, or what to tell the Carbon when it didn't.
     */
    fun openCandidate(context: Context, candidate: SettingsCandidate, tv: Boolean): String? {
        val intent = (candidate.action?.let { Intent(it) } ?: Intent())
            .setComponent(ComponentName(candidate.pkg, candidate.cls))
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        val noun = if (tv) "TV" else "device"
        val failed = "This $noun didn't let Extend open ${candidate.title}. Try the next one."
        return try {
            context.startActivity(intent)
            Extend.log("setup: opened ${candidate.screen} candidate ${candidate.component} (${candidate.matches.joinToString { "${it.kind}:${it.text}" }})")
            null
        } catch (e: ActivityNotFoundException) {
            Extend.log("setup: ${candidate.component} isn't there any more", e)
            failed
        } catch (e: SecurityException) {
            Extend.log("setup: ${candidate.component} refused to open", e)
            failed
        } catch (e: RuntimeException) {
            // A maker's activity can fail in odd ways (a bad intent, a missing extra); the list stays usable.
            Extend.log("setup: ${candidate.component} failed to open", e)
            failed
        }
    }
}

package com.teamofsilicons.extend.core

import android.content.Context
import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.os.SystemClock
import android.provider.Settings
import com.teamofsilicons.extend.Extend
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

/**
 * Reads this device's activities for [SettingsFinder]: (a) every activity that answers a
 * screen's standard action, disabled ones included (they are logged, never offered), not only
 * the one Android would pick; (b) every exported activity of every installed package (the app
 * holds QUERY_ALL_PACKAGES). Everything else is [SettingsFinder]'s pure logic.
 */
object SettingsFinderScan {
    /**
     * How long a scan is reused: packages rarely change while the Carbon is in setup. A scan is
     * also made again when Developer options were turned on or off since (Android 9's Settings
     * enables its Developer options page only then), and when the list asks ([results]' refresh).
     */
    private const val FRESH_MS = 60_000L

    private class Scan(val at: Long, val devOptions: Boolean, val results: FinderResults)

    private val lock = Mutex()
    @Volatile private var cached: Scan? = null

    /**
     * Every screen's candidates, scanned off the main thread; [refresh] scans again. Null when the
     * scan failed or found no activity at all: then the list says Extend couldn't read this
     * device's screens, rather than that the device hides them. A failure isn't cached.
     */
    suspend fun results(context: Context, refresh: Boolean = false): FinderResults? = lock.withLock {
        withContext(Dispatchers.IO) {
            val devOptions = runCatching {
                Settings.Global.getInt(context.contentResolver, Settings.Global.DEVELOPMENT_SETTINGS_ENABLED, 0) == 1
            }.getOrDefault(false)
            cached?.let { c ->
                if (!refresh && c.devOptions == devOptions && SystemClock.elapsedRealtime() - c.at < FRESH_MS) return@withContext c.results
            }
            val started = SystemClock.elapsedRealtime()
            val activities = try {
                scan(context)
            } catch (e: Exception) {
                Extend.log("setup: the settings scan failed", e)
                null
            }
            if (activities.isNullOrEmpty()) {
                if (activities != null) Extend.log("setup: the settings scan found no activities")
                cached = null
                return@withContext null
            }
            val r = SettingsFinder.findAll(activities, context.packageName)
            Extend.log(
                "setup: scanned ${activities.size} activities in ${SystemClock.elapsedRealtime() - started} ms (developer options ${if (devOptions) "on" else "off"}); " +
                    r.entries.joinToString("; ") { (screen, list) -> "$screen: ${list.joinToString { "${it.component} [${it.kind}]" }.ifEmpty { "none" }}" },
            )
            val disabled = activities.filter { !it.enabled && it.actions.isNotEmpty() }
            if (disabled.isNotEmpty()) Extend.log("setup: disabled handlers (not offered): ${disabled.joinToString { "${it.pkg}/${it.name} ${it.actions}" }}")
            cached = Scan(SystemClock.elapsedRealtime(), devOptions, r)
            r
        }
    }

    /** The device's activities as plain data. */
    fun scan(context: Context): List<FoundActivity> {
        val pm = context.packageManager
        val own = context.packageName
        val out = ArrayList<FoundActivity>()
        val appLabels = HashMap<String, String?>()
        fun appLabel(app: ApplicationInfo): String? = appLabels.getOrPut(app.packageName) {
            runCatching { app.loadLabel(pm).toString() }.getOrNull()
        }
        fun found(info: ActivityInfo, enabled: Boolean, actions: Set<String>): FoundActivity {
            val app = info.applicationInfo
            // Its own label only: without one Android falls back to the app's, which is appLabel.
            val ownLabel = if (info.labelRes != 0 || info.nonLocalizedLabel != null) runCatching { info.loadLabel(pm).toString() }.getOrNull() else null
            return FoundActivity(
                pkg = info.packageName,
                name = info.name,
                label = ownLabel,
                appLabel = appLabel(app),
                targetActivity = info.targetActivity,
                exported = info.exported,
                // Not info.enabled: that is the manifest's value. Settings ships Developer options
                // disabled in its manifest and enables the component once they are on; Android's
                // queries below already apply that runtime state. The app's own flag is runtime.
                enabled = enabled && app.enabled,
                permission = info.permission?.takeIf { context.checkSelfPermission(it) != PackageManager.PERMISSION_GRANTED },
                system = (app.flags and ApplicationInfo.FLAG_SYSTEM) != 0,
                actions = actions,
            )
        }

        // (a) Every handler of each standard action. MATCH_ALL: no filtering to the default
        // choice; MATCH_DISABLED_COMPONENTS: disabled handlers too, told apart by a second query.
        for (action in SettingsFinder.ACTIONS) {
            val intent = Intent(action)
            val all = query(pm, intent, PackageManager.MATCH_ALL or PackageManager.MATCH_DISABLED_COMPONENTS)
            val enabled = query(pm, intent, PackageManager.MATCH_ALL).mapTo(HashSet()) { it.packageName to it.name }
            for (info in all) out += found(info, (info.packageName to info.name) in enabled, setOf(action))
        }

        // (b) Every installed package's exported activities. Disabled components are left out by
        // Android here (no MATCH_DISABLED_COMPONENTS). One package at a time, so a huge one can
        // fail alone without losing the rest.
        @Suppress("DEPRECATION")
        val packages = runCatching { pm.getInstalledPackages(0) }.getOrDefault(emptyList())
        for (p in packages) {
            if (p.packageName == own) continue
            @Suppress("DEPRECATION")
            val info = runCatching { pm.getPackageInfo(p.packageName, PackageManager.GET_ACTIVITIES) }.getOrNull() ?: continue
            for (a in info.activities.orEmpty()) if (a.exported) out += found(a, enabled = true, actions = emptySet())
        }
        return out
    }

    private fun query(pm: PackageManager, intent: Intent, flags: Int): List<ActivityInfo> {
        @Suppress("DEPRECATION")
        return runCatching { pm.queryIntentActivities(intent, flags) }.getOrDefault(emptyList()).mapNotNull { it.activityInfo }
    }
}

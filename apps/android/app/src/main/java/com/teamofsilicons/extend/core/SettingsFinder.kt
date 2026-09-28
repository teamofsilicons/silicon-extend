package com.teamofsilicons.extend.core

/**
 * "Can't find it?": the other screens on this device that may be the system screen a setup step
 * needs. Makers that replace Android's settings with their own menu (Network, Time, Common,
 * Accounts, System info…) often hide Accessibility, About and Developer options, while Android's
 * own pages, or the maker's, are still installed and can be opened directly.
 *
 * Pure: [SettingsFinder.find] takes the device's activities as plain data ([FoundActivity], read
 * by `SettingsFinderScan`), so matching, ranking and de-duplication are JVM-testable.
 */
enum class FindScreen(
    /** What the Carbon looks for, in words ("look for Accessibility there"). */
    val title: String,
    /** Standard actions whose handlers are this screen. */
    val actions: List<String>,
    /** Android's own classes for this screen: Android TV Settings, then the phone Settings app. */
    val known: List<Pair<String, String>>,
    /** Words that point at this screen wherever they appear: in any app's class name or label. */
    val strong: List<String>,
    /** Words that point at it only inside a settings or maker app ("about" is in every app). */
    val weak: List<String> = emptyList(),
) {
    ACCESSIBILITY(
        "Accessibility",
        listOf(SettingsRoutes.ACTION_ACCESSIBILITY),
        SettingsRoutes.known(SettingsPage.ACCESSIBILITY),
        strong = listOf("accessib", "a11y"),
    ),
    ABOUT(
        "About (or System info)",
        listOf(SettingsRoutes.ACTION_DEVICE_INFO),
        SettingsRoutes.known(SettingsPage.ABOUT),
        strong = listOf("deviceinfo", "buildnumber", "systeminfo", "sysinfo"),
        weak = listOf("about", "status", "version"),
    ),
    DEVELOPER_OPTIONS(
        "Developer options",
        listOf(SettingsRoutes.ACTION_DEVELOPMENT, SettingsRoutes.ACTION_DEVELOPMENT_LEGACY),
        SettingsRoutes.known(SettingsPage.DEVELOPER_OPTIONS),
        strong = listOf("develop", "devopt"),
    ),
    /**
     * Network, USB, Wireless or ADB debugging: usually a switch inside Developer options, sometimes
     * its own screen. Its words for the Carbon depend on the device ([SettingsFinder.screenName]).
     */
    DEBUGGING(
        "Android debugging",
        emptyList(),
        emptyList(),
        strong = emptyList(),
        // Only inside settings and maker apps ("adb" is inside ordinary words: "LoadBalancer"), and
        // only "adb" or debugging named after its switch. Not "debug" alone: Android 16's Settings
        // has DebuggingDataActivity and AppDebuggingDataActivity, one crashes Settings and the other
        // closes at once, and a maker's "Debug" menu is usually its factory menu.
        weak = listOf("adb", "usbdebug", "networkdebug", "wirelessdebug", "wifidebug", "remotedebug"),
    ),
    ;

    /** Its name without the alternative: "About (or System info)" → "About". */
    val short: String get() = title.substringBefore(" (")
}

/**
 * The activity a setup button opened, as Android resolved it: its package, its class, and the
 * class an activity-alias stands for.
 */
data class OpenedScreen(val pkg: String, val cls: String, val target: String? = null) {
    private val names: Set<String> get() = setOfNotNull(cls, target)

    /** It is [p]/[c] (by its own class or the one it stands for). */
    fun isOne(p: String, c: String): Boolean = p == pkg && c in names

    /** It is [c] (an alias and its target are one screen). */
    fun isOne(c: SettingsCandidate): Boolean = c.pkg == pkg && (c.cls in names || c.target?.let { it in names } == true)

    /** Short (`com.android.settings/.Settings`), for the log. */
    val component: String get() = if (cls.startsWith("$pkg.")) "$pkg/${cls.removePrefix(pkg)}" else "$pkg/$cls"
}

/**
 * One activity on the device, as discovery saw it. [actions]: the standard actions it was found
 * handling. [label]: its own label (null when it only has its app's). [permission]: a permission
 * it needs that this app doesn't hold (null: anyone may start it).
 */
data class FoundActivity(
    val pkg: String,
    val name: String,
    val label: String? = null,
    val appLabel: String? = null,
    val targetActivity: String? = null,
    val exported: Boolean = true,
    val enabled: Boolean = true,
    val permission: String? = null,
    val system: Boolean = true,
    val actions: Set<String> = emptySet(),
)

/** Why a candidate is listed, strongest first. */
enum class MatchKind {
    /** It answers the screen's standard action. */
    ACTION,
    /** It is Android's own class for the screen. */
    KNOWN,
    /** Its class name or its label has one of the screen's words. */
    NAME,
    /** Its class sits in a part of a settings app named after the screen. */
    PATH,
    /** A main settings screen, where the Carbon can look for the entry. */
    MAIN,
}

/** What matched: the kind, and the text that matched (an action, a class name, a label). */
data class FinderMatch(val kind: MatchKind, val text: String)

/**
 * A screen the Carbon can open instead. [action] goes on the explicit intent (null: none).
 * [appLabel]: its app's name, null when the app has none (Android would show its package name).
 * [target]: the class an activity-alias stands for. [system]: preinstalled. [openedByButton]: the
 * step's own button opens it, so the Carbon has already looked there.
 */
data class SettingsCandidate(
    val screen: FindScreen,
    val pkg: String,
    val cls: String,
    val action: String?,
    val appLabel: String?,
    val screenName: String,
    val matches: List<FinderMatch>,
    val target: String? = null,
    val system: Boolean = true,
    val openedByButton: Boolean = false,
) {
    val kind: MatchKind get() = matches.minOf { it.kind }
    /** Found as the screen itself (not only as a main settings screen to look in). */
    val specific: Boolean get() = kind != MatchKind.MAIN

    /**
     * Where it goes in a list, lowest first: 0 Android's settings app answering the screen's action;
     * 1 Android's own class for the screen (which the setup button may never reach when a maker's
     * page answers the action first); 2 another preinstalled app answering the action; 3 a name that
     * fits; 4 a section of a settings app named after the screen; 5 anything from a user-installed
     * app (it can't change Android's settings itself); 6 a main settings screen to look in.
     */
    val tier: Int
        get() = when {
            kind == MatchKind.MAIN -> 6
            !system -> 5
            kind == MatchKind.ACTION && SettingsFinder.packageRank(pkg) == 0 -> 0
            matches.any { it.kind == MatchKind.KNOWN } -> 1
            kind == MatchKind.ACTION -> 2
            kind == MatchKind.NAME -> 3
            else -> 4
        }

    /** "Settings · Accessibility": the app, then the screen (just the screen when the app has no name). */
    val title: String
        get() = when {
            appLabel == null -> screenName
            screenName.equals(appLabel, ignoreCase = true) -> appLabel
            else -> "$appLabel · $screenName"
        }

    /** Why it is listed, for the Carbon, in this device's words ([SettingsFinder.screenName]). */
    fun reason(tv: Boolean, sdk: Int): String {
        val m = matches.minBy { it.kind }
        val name = SettingsFinder.screenName(screen, tv, sdk)
        val short = name.substringBefore(" (")
        if (openedByButton) {
            return if (m.kind == MatchKind.MAIN) "The button above opens this: look for $name there" else "The button above opens this screen"
        }
        return when (m.kind) {
            MatchKind.ACTION -> "Listed with Android as the $short screen"
            MatchKind.KNOWN -> "Android's own $short screen"
            MatchKind.NAME -> "Named like ${article(short)} $short screen"
            // The screen's name, not the section's: a maker's section can be called "a11y" or "adv".
            MatchKind.PATH -> "In the $short part of ${appLabel ?: "its app"}"
            MatchKind.MAIN -> "Main settings screen: look for $name there"
        }
    }

    /** "an Accessibility", "a Developer options", "a USB debugging": an acronym by its first letter's sound. */
    private fun article(word: String): String {
        val acronym = word.length > 1 && word[0].isUpperCase() && word[1].isUpperCase()
        val first = word.first().uppercaseChar()
        return if (if (acronym) first in "AEFHILMNORSX" else first in "AEIOU") "an" else "a"
    }

    /** Its package and class, short (`com.android.settings/.Settings$…`), for the log and support. */
    val component: String get() = if (cls.startsWith("$pkg.")) "$pkg/${cls.removePrefix(pkg)}" else "$pkg/$cls"
}

/**
 * A setup step's "Can't find it?" list: the screens to look for, in order, and what to do on
 * the one that opens.
 */
data class FindHelp(val screens: List<FindScreen>, val lookFor: String) {
    companion object {
        private const val NEXT = " If it isn't there, press Back and try the next one."

        /** The step's second button, which shows the list (the step help points at it by name). */
        fun buttonLabel(tv: Boolean): String = "Can't find it? Other screens on this ${if (tv) "TV" else "device"}"

        fun accessibility(label: String): FindHelp = FindHelp(
            listOf(FindScreen.ACCESSIBILITY),
            "Open one and look for $label (on some screens under Services, Downloaded apps or Installed services), then turn it on.$NEXT",
        )

        fun developerOptions(tv: Boolean): FindHelp {
            val noun = if (tv) "TV" else "device"
            val verb = if (tv) "select" else "tap"
            return FindHelp(
                listOf(FindScreen.ABOUT, FindScreen.DEVELOPER_OPTIONS),
                "Open one. On an About or System info screen, $verb Build (or Build number, or the version) 7 times, until the $noun says " +
                    "you are a developer. On a Developer options screen, turn Developer options on.$NEXT",
            )
        }

        fun debugging(tv: Boolean, sdk: Int): FindHelp = FindHelp(
            listOf(FindScreen.DEVELOPER_OPTIONS, FindScreen.DEBUGGING),
            "Open one and turn on ${SettingsFinder.debuggingSwitch(tv, sdk)}.$NEXT",
        )
    }
}

/** The candidates found for every [FindScreen], best first. */
typealias FinderResults = Map<FindScreen, List<SettingsCandidate>>

object SettingsFinder {
    /** How many screens a step lists. */
    const val LIMIT = 8

    /** Every action discovery asks Android about: each screen's, and the main settings screen's. */
    val ACTIONS: List<String> = (FindScreen.entries.flatMap { it.actions } + SettingsRoutes.ACTION_SETTINGS).distinct()

    /**
     * Maker and chipset packages whose apps may hold settings pages (MediaTek, MStar, Realtek,
     * Amlogic, HiSilicon, Rockchip, Allwinner boards, and TV brands).
     */
    private val MAKERS = listOf(
        "com.mediatek", "com.mtk", "com.mstar", "com.realtek", "com.amlogic", "com.droidlogic", "com.hisilicon", "com.rockchip",
        "com.softwinner", "com.allwinner", "com.tcl", "com.hisense", "com.skyworth", "com.konka", "com.changhong", "com.haier",
        "com.cvte", "com.xiaomi", "com.mitv", "com.sony", "com.tpv", "com.philips", "com.sharp", "com.panasonic", "com.toshiba",
        "com.vestel", "com.nvidia", "com.amazon",
    )

    /** Never a settings page: Android's own dialogs and pickers. */
    private val SKIP_PACKAGES = setOf("android")

    /** Words a class name ends in that say nothing about which screen it is. */
    private val SUFFIXES = listOf("activity", "settings", "setting", "preferences", "preference", "dashboard", "fragment", "screen", "page")

    /**
     * 0: Android's Settings apps; 1: another app named settings; 2: a maker's app; 3: anything
     * else. Words like "about" count only below 3.
     */
    fun packageRank(pkg: String): Int {
        val p = pkg.lowercase()
        return when {
            p == SettingsRoutes.TV_SETTINGS || p == SettingsRoutes.SETTINGS -> 0
            "setting" in p -> 1
            MAKERS.any { p == it || p.startsWith("$it.") } -> 2
            else -> 3
        }
    }

    /** Lowercase letters and digits only: "Developer options" → "developeroptions". */
    internal fun squash(text: String): String = text.lowercase().filter { it in 'a'..'z' || it in '0'..'9' }

    /** A class's own name, inner class included: `a.b.Settings$AccessibilityActivity` → "AccessibilityActivity". */
    internal fun simpleName(cls: String): String = cls.substringAfterLast('.').substringAfterLast('$')

    /** A name without the endings every activity has, squashed: "DevelopmentSettingsDashboardActivity" → "development". */
    internal fun core(name: String): String {
        var s = squash(name)
        var changed = true
        while (changed) {
            changed = false
            for (suffix in SUFFIXES) if (s.length > suffix.length && s.endsWith(suffix)) {
                s = s.removeSuffix(suffix)
                changed = true
            }
        }
        return s
    }

    /** Short forms in class names, spelled out for the Carbon ("DevOptActivity" → "Developer options"). */
    private val SPELLED = mapOf("dev" to "developer", "opt" to "options", "opts" to "options", "a11y" to "accessibility", "sysinfo" to "system info")

    /** Words shown in capitals. */
    private val CAPITALS = setOf("adb", "usb", "tv")

    /**
     * A class name in words: "AccessibilitySettingsActivity" → "Accessibility settings",
     * "DevOptActivity" → "Developer options", "AccessibilityAlias" → "Accessibility".
     */
    fun readable(cls: String): String {
        val simple = simpleName(cls).removeSuffix("Alias").removeSuffix("Activity").ifEmpty { simpleName(cls) }
        val words = simple.replace('_', ' ')
            .replace(Regex("(?<=[a-z0-9])(?=[A-Z])|(?<=[A-Z])(?=[A-Z][a-z])"), " ")
            .trim().split(Regex("\\s+")).filter { it.isNotEmpty() }
        if (words.isEmpty()) return cls
        return words.mapIndexed { i, w ->
            val lower = w.lowercase()
            val word = when {
                lower in SPELLED -> SPELLED.getValue(lower)
                lower in CAPITALS -> w.uppercase()
                w.length > 1 && w.all { it.isUpperCase() } -> w // TV, USB, ADB
                else -> lower
            }
            if (i == 0) word.replaceFirstChar { it.uppercase() } else word
        }.joinToString(" ")
    }

    /** A package or class name ("com.android.systemui.accessibility.accessibilitymenu"), not a name for people. */
    private val CODE_NAME = Regex("[A-Za-z_][A-Za-z0-9_]*(\\.[A-Za-z_][A-Za-z0-9_\$]*)+")

    /**
     * [text] if it is fit to show, else null: not blank, not the package's name (Android falls back
     * to it when an app has no label), not a package or class name.
     */
    internal fun shown(text: String?, pkg: String): String? {
        val t = text?.trim()
        if (t.isNullOrEmpty() || t == pkg || CODE_NAME.matches(t)) return null
        return t
    }

    /** "AccessibilityInversionSettings", "System info" → its words, lowercase: [accessibility, inversion, settings]. */
    internal fun words(text: String): List<String> =
        text.replace(Regex("(?<=[a-z0-9])(?=[A-Z])|(?<=[A-Z])(?=[A-Z][a-z])"), " ").lowercase().split(Regex("[^a-z0-9]+")).filter { it.isNotEmpty() }

    /** Which of [keys] [text] has, starting where one of its words starts, or null. */
    private fun atWordStart(text: String, keys: List<String>): String? {
        val w = words(text)
        val joined = w.joinToString("")
        val starts = w.runningFold(0) { at, word -> at + word.length }.dropLast(1)
        return keys.firstOrNull { k -> starts.any { joined.startsWith(k, it) } }
    }

    /**
     * The screen's word that [text] has, starting where one of its words starts (so "SystemInfo"
     * has "systeminfo", but "AccessibilityInversion" has no "version" and "LoadBalancer" no "adb"),
     * or null. [weakToo]: the weak words count here.
     */
    private fun keyword(screen: FindScreen, text: String, weakToo: Boolean): String? =
        atWordStart(text, screen.strong) ?: if (weakToo) atWordStart(text, screen.weak) else null

    /**
     * Words that mark a screen that can erase, reset, reboot or reflash the device, or put it in
     * a mode the Carbon can't easily leave: factory reset and "System recovery", storage format,
     * software update (Android 16's Developer options section holds `development.DSULoader`,
     * "Select DSU Package", which installs another system image), a maker's factory, engineering,
     * hotel or shop (retail demo) menu, the setup wizard. Matched where a word starts ("Preset"
     * isn't "reset").
     */
    private val HARMFUL = listOf(
        "factory", "reset", "recovery", "restore", "wipe", "erase", "format", "masterclear", "clear", "delete", "remove", "uninstall",
        "reboot", "restart", "shutdown", "poweroff", "update", "upgrade", "ota", "flash", "dsu", "dynsystem", "gsi",
        "engineer", "engmode", "hotel", "shop", "retail", "demo", "setupwizard", "suw", "oobe", "provision",
    )

    /**
     * Never offered, whatever else it matches: a screen whose package, class (any of its sections:
     * `com.maker.settings.factory.DevelopmentActivity`), label or app's name says it resets, wipes,
     * reboots or updates the device, or is a factory or engineering menu ([HARMFUL]).
     */
    internal fun harmful(a: FoundActivity): String? =
        listOfNotNull(a.pkg, a.name, a.targetActivity, a.label, a.appLabel).firstNotNullOfOrNull { atWordStart(it, HARMFUL) }

    /** What makes [a] a candidate for [screen], strongest first (empty: it isn't one). */
    internal fun matches(screen: FindScreen, a: FoundActivity): List<FinderMatch> {
        val out = ArrayList<FinderMatch>()
        // Names count only in preinstalled apps: a user-installed app can't change Android's
        // settings, so only an action counts there (even in a package named like a maker's,
        // `com.amazon.avod…`, or with "setting" in it). In a preinstalled settings or maker app
        // labels, sections and the weak words count too; in another preinstalled app only its
        // class name does (a Play services help page labelled "System info" is no About screen).
        val byName = a.system
        val weakToo = a.system && packageRank(a.pkg) <= 2
        for (action in screen.actions) if (action in a.actions) out += FinderMatch(MatchKind.ACTION, action)
        val target = a.targetActivity
        if (screen.known.any { (p, c) -> p == a.pkg && (c == a.name || c == target) }) out += FinderMatch(MatchKind.KNOWN, a.name)
        val simple = simpleName(a.name)
        if (byName && keyword(screen, simple, weakToo) != null) out += FinderMatch(MatchKind.NAME, simple)
        if (weakToo) a.label?.takeIf { keyword(screen, it, weakToo) != null }?.let { out += FinderMatch(MatchKind.NAME, it) }
        // A settings or maker app named after the screen (a maker's own "System info" app). Not any
        // app: Google's "Android Accessibility Suite" is TalkBack, not Android's Accessibility page.
        if (weakToo) a.appLabel?.takeIf { keyword(screen, it, weakToo) != null }?.let { out += FinderMatch(MatchKind.NAME, it) }
        if (weakToo && out.none { it.kind == MatchKind.NAME }) {
            // `com.maker.setting.accessibility.MainActivity`: a section of a settings app named after the screen.
            val rest = a.name.removePrefix("${a.pkg}.").substringBeforeLast('.', "")
            rest.split('.').firstOrNull { keyword(screen, it, weakToo = true) != null }?.let { out += FinderMatch(MatchKind.PATH, it) }
        }
        if (SettingsRoutes.ACTION_SETTINGS in a.actions || SettingsRoutes.MAIN_CLASSES.any { (p, c) -> p == a.pkg && (c == a.name || c == target) }) {
            out += FinderMatch(MatchKind.MAIN, a.name)
            // A main settings screen that also claims this screen's action (a maker's menu answering
            // everything) is where the step's own button already went: it counts as a main screen.
            if (out.all { it.kind == MatchKind.ACTION || it.kind == MatchKind.MAIN }) out.removeAll { it.kind == MatchKind.ACTION }
        }
        return out
    }

    /** How closely a name fits: 0 when it is just the screen's word ("AccessibilityActivity"). */
    private fun closeness(screen: FindScreen, a: FoundActivity): Int {
        val names = listOfNotNull(simpleName(a.name), a.label).map { core(it) }
        val words = screen.strong + screen.weak
        return names.minOf { n -> words.filter { it in n }.minOfOrNull { n.length - it.length } ?: (n.length + 100) }
    }

    /**
     * Words of screens that are never one a step needs, matched where a word of the class name or
     * label starts: licences and legal notices (Android TV's `about.LicenseActivity`, "Third Party
     * Source"), a managed device's info (`EnterprisePrivacySettingsActivity`, "Managed device info").
     */
    private val NOT_SETTINGS = listOf("license", "licence", "legal", "enterprise", "managed", "thirdparty", "opensource")

    /**
     * Pages for one item that open nothing without an extra naming it: Android TV 14's page for a
     * single accessibility service closes at once when started without one.
     */
    private val ITEM_PAGES = setOf("${SettingsRoutes.TV_SETTINGS}.oemlink.AccessibilityServiceActivity")

    /**
     * Not a screen to send the Carbon to: a stand-in for a switched-off page (Android 9's Settings
     * answers the Developer options action with `DevelopmentSettingsDisabledActivity` while they
     * are off, which only says "enable developer options first" and closes), a stub that only
     * shows "no app can do this" (Android TV's `frameworkpackagestubs.Stubs$SettingsStub`, which
     * also answers the Accessibility action), a test screen (`WifiStatusTest`), a licence, legal or
     * managed-device page ([NOT_SETTINGS]), or a page for one item ([ITEM_PAGES]).
     */
    internal fun notAScreen(a: FoundActivity): Boolean {
        val w = words(simpleName(a.name))
        val joined = w.joinToString("")
        return "disabled" in joined || "unavailable" in joined || "notavailable" in joined ||
            "test" in w || "tests" in w || "stub" in w || "stubs" in w ||
            a.name in ITEM_PAGES || a.targetActivity?.let { it in ITEM_PAGES } == true ||
            listOfNotNull(a.name, a.targetActivity, a.label).any { atWordStart(it, NOT_SETTINGS) != null }
    }

    /**
     * Starts it can take: exported, enabled, no permission this app lacks, not this app, not
     * Android's dialogs, not a stand-in or test screen, nothing that can reset, wipe, reboot or
     * update the device ([harmful]).
     */
    fun startable(a: FoundActivity, ownPkg: String): Boolean =
        a.exported && a.enabled && a.permission == null && a.pkg != ownPkg && a.pkg !in SKIP_PACKAGES && !notAScreen(a) &&
            harmful(a) == null

    /**
     * The screens on this device that may be [screen], best first ([SettingsCandidate.tier]):
     * Android's settings app answering its standard action, Android's own classes, other
     * preinstalled handlers of the action, names that fit (settings apps before others, the
     * closest name first), sections named after it, user-installed apps' handlers, then main
     * settings screens to look in; at most [limit]. One entry per screen: an activity-alias and
     * its target are one.
     */
    fun find(screen: FindScreen, activities: List<FoundActivity>, ownPkg: String, limit: Int = LIMIT): List<SettingsCandidate> {
        data class Scored(val a: FoundActivity, val c: SettingsCandidate)
        val scored = merge(activities).filter { startable(it, ownPkg) }.mapNotNull { a ->
            matches(screen, a).takeIf { it.isNotEmpty() }?.let { Scored(a, candidate(screen, a, it)) }
        }
        val ranked = scored.sortedWith(
            compareBy<Scored>(
                { it.c.tier },
                { packageRank(it.a.pkg) },
                { if (it.c.matches.any { m -> m.kind == MatchKind.NAME }) closeness(screen, it.a) else 0 },
                { it.a.pkg },
                { it.a.name },
            ),
        )
        val seen = HashSet<Pair<String, String>>()
        return ranked.filter { seen.add(it.a.pkg to (it.a.targetActivity ?: it.a.name)) }.take(limit).map { it.c }
    }

    private fun candidate(screen: FindScreen, a: FoundActivity, matches: List<FinderMatch>): SettingsCandidate {
        // The action it was found answering goes on the intent: some screens read it to pick their page.
        val action = screen.actions.firstOrNull { it in a.actions } ?: SettingsRoutes.ACTION_SETTINGS.takeIf { it in a.actions }
        val app = shown(a.appLabel, a.pkg)
        val own = shown(a.label, a.pkg)?.takeIf { !it.equals(app, ignoreCase = true) }
        val kind = matches.minOf { it.kind }
        // Without a label of its own: the screen's name when Android lists it as that screen or it
        // is Android's class for it ("Developer options", not "Development"), else its class name in
        // words. An alias's own name, not its target's (Android 16's `DebuggingDataActivity` stands
        // for `spa.SpaBridgeActivity`).
        val name = own ?: if (kind == MatchKind.ACTION || kind == MatchKind.KNOWN) screen.short else readable(a.name)
        return SettingsCandidate(screen, a.pkg, a.name, action, app, name, matches, a.targetActivity, a.system)
    }

    /** One entry per (package, class): the actions found for it joined, enabled if any source saw it enabled. */
    fun merge(activities: List<FoundActivity>): List<FoundActivity> {
        val out = LinkedHashMap<Pair<String, String>, FoundActivity>()
        for (a in activities) {
            val key = a.pkg to a.name
            val prev = out[key]
            out[key] = if (prev == null) a else prev.copy(
                label = prev.label ?: a.label,
                appLabel = prev.appLabel ?: a.appLabel,
                targetActivity = prev.targetActivity ?: a.targetActivity,
                exported = prev.exported || a.exported,
                enabled = prev.enabled || a.enabled,
                permission = prev.permission ?: a.permission,
                actions = prev.actions + a.actions,
            )
        }
        return out.values.toList()
    }

    /** Every screen's candidates. */
    fun findAll(activities: List<FoundActivity>, ownPkg: String, limit: Int = LIMIT): FinderResults =
        FindScreen.entries.associateWith { find(it, activities, ownPkg, limit) }

    /**
     * A step's list, at most [limit]: every one of [help]'s screens' candidates, a screen listed for
     * an earlier one left out, ordered by [SettingsCandidate.tier] across the screens (so Android's
     * Developer options page comes before names that only fit About) and by [help]'s order within a
     * tier; main settings screens last. [opened]: what the step's button opened, moved to the end
     * and marked, since the Carbon has already looked there.
     */
    fun forStep(help: FindHelp, results: FinderResults, limit: Int = LIMIT, opened: OpenedScreen? = null): List<SettingsCandidate> {
        val seen = HashSet<Pair<String, String>>()
        val all = help.screens.flatMap { results[it].orEmpty() }.filter { seen.add(it.pkg to (it.target ?: it.cls)) }.sortedBy { it.tier }
        val (byButton, rest) = all.partition { opened?.isOne(it) == true }
        val last = byButton.take(1).map { it.copy(openedByButton = true) }
        return rest.take(limit - last.size) + last
    }

    /** The debugging switch's name here. */
    fun debuggingName(tv: Boolean, sdk: Int): String = when {
        tv -> "Network debugging"
        sdk >= 30 -> "Wireless debugging"
        // Android 8–10 phones: USB debugging, then `adb tcpip 5555` once from a computer (the step says so).
        else -> "USB debugging"
    }

    /** What turns Android debugging on here, in words. */
    fun debuggingSwitch(tv: Boolean, sdk: Int): String =
        if (tv) "Network debugging (or ADB debugging or USB debugging)" else debuggingName(tv, sdk)

    /** What the Carbon looks for, in this device's words. */
    fun screenName(screen: FindScreen, tv: Boolean, sdk: Int): String =
        if (screen == FindScreen.DEBUGGING) debuggingName(tv, sdk) else screen.title

    /**
     * Plain words for a step when this device hides what it needs: no screen for Accessibility
     * (then Android debugging is the way in), and no screen for Developer options (then the
     * maker's System info menu is where the build entry usually is).
     */
    fun notes(help: FindHelp, results: FinderResults, tv: Boolean, sdk: Int, devOptions: Boolean): List<String> {
        val noun = if (tv) "TV" else "device"
        val verb = if (tv) "select" else "tap"
        fun hidden(screen: FindScreen) = results[screen].orEmpty().none { it.specific }
        val debugging = if (!tv && sdk >= 30) "wireless debugging" else "network debugging"
        val makerMenu = "Some ${noun}s keep it under System info (or About): $verb the build or version entry 7 times, " +
            "then look for Developer options or ${debuggingSwitch(tv, sdk)} in the settings menu."
        val out = ArrayList<String>()
        val screens = help.screens
        when {
            FindScreen.ACCESSIBILITY in screens -> if (hidden(FindScreen.ACCESSIBILITY)) {
                out += "This $noun hides Android's Accessibility setting: Extend found no screen for it. Android debugging lets Extend " +
                    "turn on its own accessibility instead: turn on $debugging (the optional steps below), connect it in the Android debugging " +
                    "card, then $verb “Turn on accessibility through debugging”."
                if (!devOptions && hidden(FindScreen.DEVELOPER_OPTIONS)) {
                    out += if (hidden(FindScreen.ABOUT)) "Developer options is hidden too. $makerMenu"
                    // Android shows Developer options only once they are on (Android 9's Settings
                    // disables its page until then): the About screen was found, so the step below turns them on.
                    else "Developer options is off, so this $noun doesn't show $debugging yet: turn Developer options on first with the Developer options step below."
                }
            }
            FindScreen.ABOUT in screens -> if (!devOptions && hidden(FindScreen.ABOUT) && hidden(FindScreen.DEVELOPER_OPTIONS)) {
                out += "This $noun hides Android's About screen and Developer options. $makerMenu"
            }
            FindScreen.DEVELOPER_OPTIONS in screens -> if (hidden(FindScreen.DEVELOPER_OPTIONS) && hidden(FindScreen.DEBUGGING)) {
                out += when {
                    devOptions -> "Extend found no Developer options screen on this $noun. Look for ${debuggingSwitch(tv, sdk)} in the $noun's own settings menu."
                    // Android shows Developer options only once they are on: the step above turns them on.
                    !hidden(FindScreen.ABOUT) -> "Developer options is off, so this $noun doesn't show it yet. Turn it on first (the Developer options step), then come back."
                    else -> "This $noun hides Developer options. $makerMenu"
                }
            }
        }
        return out
    }
}

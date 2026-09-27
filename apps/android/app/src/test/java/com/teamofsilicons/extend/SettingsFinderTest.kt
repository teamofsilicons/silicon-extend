package com.teamofsilicons.extend

import com.teamofsilicons.extend.adb.DebuggingAfterRestart
import com.teamofsilicons.extend.core.FindHelp
import com.teamofsilicons.extend.core.FindScreen
import com.teamofsilicons.extend.core.FoundActivity
import com.teamofsilicons.extend.core.MatchKind
import com.teamofsilicons.extend.core.OpenResult
import com.teamofsilicons.extend.core.OpenedScreen
import com.teamofsilicons.extend.core.SettingsCandidate
import com.teamofsilicons.extend.core.SettingsFinder
import com.teamofsilicons.extend.core.SettingsRoute
import com.teamofsilicons.extend.core.SettingsRoutes
import com.teamofsilicons.extend.core.SetupReport
import com.teamofsilicons.extend.core.SetupSignals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * "Can't find it?": which screens on a device may be the system screen a setup step needs, in
 * what order, and what the step says when the device hides it. Each device is a fake activity
 * list shaped like what `SettingsFinderScan` reads from PackageManager.
 */
class SettingsFinderTest {
    private val own = "com.teamofsilicons.extend"
    private val a11y = SettingsRoutes.ACTION_ACCESSIBILITY
    private val dev = SettingsRoutes.ACTION_DEVELOPMENT
    private val devLegacy = SettingsRoutes.ACTION_DEVELOPMENT_LEGACY
    private val info = SettingsRoutes.ACTION_DEVICE_INFO
    private val main = SettingsRoutes.ACTION_SETTINGS

    private fun settings(name: String, label: String? = null, vararg actions: String, enabled: Boolean = true) =
        FoundActivity("com.android.settings", "com.android.settings.$name", label, "Settings", enabled = enabled, actions = actions.toSet())

    private fun tvSettings(name: String, label: String? = null, vararg actions: String, target: String? = null) =
        FoundActivity("com.android.tv.settings", "com.android.tv.settings.$name", label, "Settings", targetActivity = target?.let { "com.android.tv.settings.$it" }, actions = actions.toSet())

    /** Android 9 phone Settings with Developer options off, as read on the Android 9 emulator, plus TalkBack. */
    private val aospPhone = listOf(
        settings("Settings", null, main),
        settings("Settings\$AccessibilitySettingsActivity", "Accessibility", a11y),
        settings("Settings\$AccessibilityDaltonizerSettingsActivity", "Color correction"),
        settings("Settings\$AccessibilityInversionSettingsActivity", "Color inversion"),
        settings("accessibility.AccessibilitySettingsForSetupWizardActivity", "Vision settings"),
        settings("Settings\$MyDeviceInfoActivity", "About phone", info),
        // Off: Settings disables the real page and answers the action with a toast-only stand-in.
        settings("Settings\$DevelopmentSettingsDashboardActivity", "Developer options", dev, enabled = false),
        settings("development.DevelopmentSettingsDisabledActivity", null, dev, devLegacy),
        FoundActivity("com.google.android.marvin.talkback", "com.google.android.accessibility.talkback.TalkBackPreferencesActivity",
            "TalkBack settings", "Android Accessibility Suite", system = true),
        FoundActivity("com.google.android.youtube", "com.google.android.apps.youtube.app.settings.AboutPrefsActivity", "About", "YouTube"),
        // Seen on the Android 9 emulator: a test screen, and a Play services page labelled like About.
        settings("wifi.WifiStatusTest", "Wi-Fi status test"),
        FoundActivity("com.google.android.gms", "com.google.android.gms.googlehelp.helpactivities.DeviceSignalsExportActivity", "System info", "Google Play services"),
        FoundActivity("com.android.phone", "com.android.phone.settings.AccessibilitySettingsActivity", "Accessibility", "Phone Services"),
        FoundActivity(own, "$own.ui.MainActivity", null, "Silicon Extend", system = false, actions = emptySet()),
        FoundActivity(own, "$own.a11y.AccessibilitySettingsActivity", null, "Silicon Extend", system = false),
    )

    /** Android TV Settings (Android 9), with an alias to its Accessibility page. */
    private val androidTv = listOf(
        tvSettings("MainSettings", null, main),
        tvSettings("system.AccessibilityActivity", null, a11y),
        tvSettings("accessibility.AccessibilityAlias", null, target = "system.AccessibilityActivity"),
        tvSettings("system.AccessibilityShortcutActivity", "Accessibility shortcut"),
        tvSettings("accessories.AddAccessoryActivity", "Pair accessory"),
        tvSettings("about.AboutActivity", null, info),
        tvSettings("system.development.DevelopmentActivity", null, dev, devLegacy),
        FoundActivity("com.google.android.youtube.tv", "com.google.android.apps.youtube.tv.activity.AboutActivity", null, "YouTube"),
    )

    /**
     * A maker TV like the Carbon's MediaTek board: its own settings app (Network, Time, Common,
     * Accounts, System info) answers the main action; Android TV Settings is still installed but
     * its Accessibility page no longer answers the action; a sideloaded developer-options shortcut.
     */
    private val makerTv = listOf(
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.MainActivity", null, "Settings", actions = setOf(main)),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.common.CommonActivity", "Common", "Settings"),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.sysinfo.SystemInfoActivity", "System info", "Settings"),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.common.a11y.MainActivity", null, "Settings"),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.AccessibilityInternalActivity", null, "Settings", exported = false),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.factory.DevelopmentActivity", null, "Settings", permission = "com.maker.permission.FACTORY"),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.adv.DevOptActivity", null, "Settings"),
        FoundActivity("com.mediatek.wwtv.tvcenter", "com.mediatek.wwtv.tvcenter.nav.AdbSwitchActivity", "ADB switch", "TV"),
        tvSettings("MainSettings", null),
        tvSettings("system.AccessibilityActivity", null),
        FoundActivity("com.example.devtools", "com.example.devtools.MainActivity", "Developer options", "Dev Tools", system = false),
        FoundActivity("com.example.devtools", "com.example.devtools.LoadBalancerActivity", null, "Dev Tools", system = false),
        // Named after accessibility, but an app's own name: TalkBack's is "Android Accessibility Suite".
        FoundActivity("com.example.reader", "com.example.reader.MainActivity", null, "Accessibility Reader", system = false),
        FoundActivity("com.streaming.app", "com.streaming.app.AboutActivity", "About", "Streaming", system = false),
        FoundActivity("android", "com.android.internal.app.AccessibilityButtonChooserActivity", null, "Android System"),
    )

    /** A maker TV where nothing but the maker's own main menu is there. */
    private val bareTv = listOf(
        FoundActivity("com.maker.launcher", "com.maker.launcher.settings.MenuActivity", null, "Launcher", actions = setOf(main)),
        FoundActivity("com.streaming.app", "com.streaming.app.AboutActivity", "About", "Streaming", system = false),
    )

    private fun find(screen: FindScreen, device: List<FoundActivity>) = SettingsFinder.find(screen, device, own)
    private fun List<SettingsCandidate>.classes() = map { it.cls.substringAfterLast('.') }

    // ───────────── An AOSP phone ─────────────

    @Test fun onAPhoneTheActionHandlerComesFirstThenOtherAccessibilityScreensThenTheMainScreen() {
        val list = find(FindScreen.ACCESSIBILITY, aospPhone)
        assertEquals(
            listOf(
                "Settings\$AccessibilitySettingsActivity",
                // The closer name first: "AccessibilityInversion" is shorter than "AccessibilityDaltonizer".
                "Settings\$AccessibilityInversionSettingsActivity",
                "Settings\$AccessibilityDaltonizerSettingsActivity",
                // Not the setup wizard's "Vision settings" (it never lists downloaded services).
                // Another preinstalled app's page named after accessibility (by class name only), after Settings'.
                "AccessibilitySettingsActivity",
                "Settings",
            ),
            list.classes(),
        )
        val first = list.first()
        assertEquals(MatchKind.ACTION, first.kind)
        assertEquals("the action goes on the explicit intent", a11y, first.action)
        assertEquals("Settings · Accessibility", first.title)
        assertEquals("Listed with Android as the Accessibility screen", first.reason(tv = false, sdk = 28))
        assertEquals("com.android.settings/.Settings\$AccessibilitySettingsActivity", first.component)
        assertEquals(MatchKind.MAIN, list.last().kind)
        assertEquals(main, list.last().action)
        assertEquals("Main settings screen: look for Accessibility there", list.last().reason(tv = false, sdk = 28))
        assertTrue("TalkBack's app label isn't Android's Accessibility page", list.none { it.pkg == "com.google.android.marvin.talkback" })
        assertTrue("never Extend itself", list.none { it.pkg == own })
    }

    @Test fun aPhoneWithDeveloperOptionsOffOffersAboutButNotTheDisabledPageOrItsToastOnlyStandIn() {
        val devList = find(FindScreen.DEVELOPER_OPTIONS, aospPhone)
        assertTrue(devList.classes().toString(), devList.none { it.specific })
        val about = find(FindScreen.ABOUT, aospPhone)
        assertEquals("com.android.settings.Settings\$MyDeviceInfoActivity", about.first().cls)
        assertEquals("Settings · About phone", about.first().title)
        assertEquals(listOf("com.android.settings.Settings\$MyDeviceInfoActivity", "com.android.settings.Settings"), about.map { it.cls })
        assertTrue("another app's About screen isn't Android's", about.none { it.pkg == "com.google.android.youtube" })

        val r = SettingsFinder.findAll(aospPhone, own)
        val step = SettingsFinder.forStep(FindHelp.developerOptions(tv = false), r)
        assertEquals(listOf("Settings\$MyDeviceInfoActivity", "Settings"), step.classes())
        val notes = SettingsFinder.notes(FindHelp.debugging(tv = false, sdk = 28), r, tv = false, sdk = 28, devOptions = false)
        assertEquals(listOf("Developer options is off, so this device doesn't show it yet. Turn it on first (the Developer options step), then come back."), notes)
        assertEquals("Accessibility is there: nothing to add", emptyList<String>(), SettingsFinder.notes(FindHelp.accessibility("Silicon Extend"), r, false, 28, false))
    }

    // ───────────── Android TV ─────────────

    @Test fun onAndroidTvTheAliasAndItsTargetAreOneScreenAndTheActionHandlerLeads() {
        val list = find(FindScreen.ACCESSIBILITY, androidTv)
        assertEquals(listOf("AccessibilityActivity", "AccessibilityShortcutActivity", "MainSettings"), list.classes())
        assertEquals(listOf(MatchKind.ACTION, MatchKind.KNOWN, MatchKind.NAME), list.first().matches.map { it.kind })
        assertEquals("Settings · Accessibility", list.first().title)
        assertEquals("Settings · Accessibility shortcut", list[1].title)
        assertEquals(MatchKind.NAME, list[1].kind)
        assertTrue("Add accessory isn't accessibility", list.none { it.cls.endsWith("AddAccessoryActivity") })

        val devList = find(FindScreen.DEVELOPER_OPTIONS, androidTv)
        assertEquals("com.android.tv.settings.system.development.DevelopmentActivity", devList.first().cls)
        assertEquals("the new action first", dev, devList.first().action)
        assertEquals("Android lists it as Developer options: called that, not \"Development\"", "Settings · Developer options", devList.first().title)
        val about = find(FindScreen.ABOUT, androidTv)
        assertEquals(listOf("AboutActivity", "MainSettings"), about.classes())
        assertTrue(about.none { it.pkg.startsWith("com.google.android.youtube") })
    }

    // ───────────── A maker TV with odd names ─────────────

    @Test fun onAMakerTvKnownClassesWithoutTheirActionAndAMakerSectionAreFound() {
        val list = find(FindScreen.ACCESSIBILITY, makerTv)
        assertEquals(listOf("AccessibilityActivity", "MainActivity", "MainSettings", "MainActivity"), list.classes())
        assertEquals("Android TV Settings' page, though it no longer answers the action", MatchKind.KNOWN, list[0].kind)
        assertNull("found by class only: no action on the intent", list[0].action)
        assertEquals("com.maker.tvsettings.common.a11y.MainActivity", list[1].cls)
        assertEquals(MatchKind.PATH, list[1].kind)
        assertEquals("the screen's name, not the section's code", "In the Accessibility part of Settings", list[1].reason(tv = true, sdk = 28))
        // Android TV Settings' main screen before the maker's menu, where the step's button already went.
        assertEquals("com.android.tv.settings.MainSettings", list[2].cls)
        assertEquals(MatchKind.MAIN, list[2].kind)
        assertEquals("com.maker.tvsettings.MainActivity", list[3].cls)
        assertEquals(MatchKind.MAIN, list[3].kind)
        assertTrue("an app's own name doesn't make it Android's page", list.none { it.pkg == "com.example.reader" })
        assertTrue("not exported, needs a permission, or Android's own dialog: never offered",
            list.none { it.cls.endsWith("AccessibilityInternalActivity") || it.pkg == "android" })

        val about = find(FindScreen.ABOUT, makerTv)
        assertEquals("com.maker.tvsettings.sysinfo.SystemInfoActivity", about.first().cls)
        assertEquals("Settings · System info", about.first().title)
        assertEquals("Named like an About screen", about.first().reason(tv = true, sdk = 28))
        assertTrue("a streaming app's About screen isn't the TV's", about.none { it.pkg == "com.streaming.app" })

        val devList = find(FindScreen.DEVELOPER_OPTIONS, makerTv)
        assertEquals(listOf("com.maker.tvsettings.adv.DevOptActivity"), devList.filter { it.specific }.map { it.cls })
        assertEquals("Settings · Developer options", devList.first().title)
        assertEquals("Named like a Developer options screen", devList.first().reason(tv = true, sdk = 28))
        assertTrue("a user-installed shortcut app can't open more than Android's own screens", devList.none { it.pkg == "com.example.devtools" })
        assertTrue("a permission-protected factory page is left out", devList.none { it.cls.contains("factory") })
        val debugging = find(FindScreen.DEBUGGING, makerTv)
        assertEquals("a maker app's ADB switch", "com.mediatek.wwtv.tvcenter.nav.AdbSwitchActivity", debugging.first().cls)
        assertEquals("Named like a Network debugging screen", debugging.first().reason(tv = true, sdk = 28))
        assertTrue("\"adb\" inside an ordinary word counts only in settings and maker apps", debugging.none { it.cls.endsWith("LoadBalancerActivity") })

        val r = SettingsFinder.findAll(makerTv, own)
        assertEquals(emptyList<String>(), SettingsFinder.notes(FindHelp.accessibility("Silicon Extend TV"), r, tv = true, sdk = 28, devOptions = false))
        val step = SettingsFinder.forStep(FindHelp.debugging(tv = true, sdk = 28), r)
        assertEquals("Developer options first, then debugging, main screens last",
            listOf("com.maker.tvsettings.adv.DevOptActivity", "com.mediatek.wwtv.tvcenter.nav.AdbSwitchActivity"),
            step.filter { it.specific }.map { it.cls })
        assertTrue(step.takeLast(2).all { it.kind == MatchKind.MAIN })
    }

    // ───────────── Nothing found ─────────────

    @Test fun whenATvHidesAccessibilityAndDeveloperOptionsItSaysSoAndPointsAtDebuggingAndSystemInfo() {
        val r = SettingsFinder.findAll(bareTv, own)
        assertTrue(FindScreen.entries.all { s -> r.getValue(s).none { it.specific } })
        val a11yStep = SettingsFinder.forStep(FindHelp.accessibility("Silicon Extend TV"), r)
        assertEquals("only the maker's main menu, to look in", listOf("com.maker.launcher.settings.MenuActivity"), a11yStep.map { it.cls })
        assertEquals("Launcher · Menu", a11yStep.single().title)

        val notes = SettingsFinder.notes(FindHelp.accessibility("Silicon Extend TV"), r, tv = true, sdk = 28, devOptions = false)
        assertEquals(2, notes.size)
        assertTrue(notes[0], notes[0].startsWith("This TV hides Android's Accessibility setting"))
        assertTrue(notes[0], notes[0].contains("network debugging") && notes[0].contains("“Turn on accessibility through debugging”"))
        assertEquals(
            "Developer options is hidden too. Some TVs keep it under System info (or About): select the build or version entry 7 times, " +
                "then look for Developer options or Network debugging (or ADB debugging or USB debugging) in the settings menu.",
            notes[1],
        )
        val onlyA11y = SettingsFinder.notes(FindHelp.accessibility("Silicon Extend TV"), r, tv = true, sdk = 28, devOptions = true)
        assertEquals("Developer options already on: only the debugging way in", 1, onlyA11y.size)
        val devStep = SettingsFinder.notes(FindHelp.developerOptions(tv = true), r, tv = true, sdk = 28, devOptions = false)
        assertTrue(devStep.single(), devStep.single().startsWith("This TV hides Android's About screen and Developer options. Some TVs keep it under System info"))
        val debugging = SettingsFinder.notes(FindHelp.debugging(tv = true, sdk = 28), r, tv = true, sdk = 28, devOptions = false)
        assertTrue(debugging.single(), debugging.single().startsWith("This TV hides Developer options."))

        val empty = SettingsFinder.findAll(emptyList(), own)
        assertTrue(SettingsFinder.forStep(FindHelp.accessibility("x"), empty).isEmpty())
        assertEquals(2, SettingsFinder.notes(FindHelp.accessibility("x"), empty, tv = true, sdk = 28, devOptions = false).size)
    }

    // ───────────── Never a screen that resets, wipes, reboots or updates ─────────────

    /**
     * A maker TV whose settings keep factory reset, "System recovery", reboot and software update
     * next to the pages the steps need, some in sections named like those pages, none protected by
     * a permission; plus Android TV's own reset pages and its toast-only settings stub.
     */
    private val riskyTv = listOf(
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.MainActivity", null, "Settings", actions = setOf(main)),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.sysinfo.SystemInfoActivity", "System info", "Settings"),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.sysinfo.FactoryResetActivity", null, "Settings"),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.sysinfo.SoftwareUpgradeActivity", "Software version", "Settings"),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.about.RebootConfirmActivity", null, "Settings"),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.common.SystemRecoveryActivity", "System info", "Settings"),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.common.a11y.ClearAllActivity", null, "Settings"),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.factory.DevelopmentActivity", null, "Settings"),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.adv.DevOptActivity", null, "Settings"),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.hotel.AdbActivity", null, "Settings"),
        FoundActivity("com.mediatek.wwtv.tvcenter", "com.mediatek.wwtv.tvcenter.nav.DebugMenuActivity", null, "Factory menu"),
        FoundActivity("com.mediatek.engineermode", "com.mediatek.engineermode.DeviceInfoActivity", null, "EngineerMode"),
        FoundActivity("com.mediatek.ota", "com.mediatek.ota.VersionActivity", null, "System update"),
        FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.store.ShopDemoActivity", "About store mode", "Settings"),
        tvSettings("oemlink.FactoryResetActivity"),
        tvSettings("device.StorageResetActivity"),
        FoundActivity("com.android.tv.frameworkpackagestubs", "com.android.tv.frameworkpackagestubs.Stubs\$SettingsStub", "None", "Activity Stub", actions = setOf(a11y)),
    )

    @Test fun aScreenThatCanResetWipeRebootOrUpdateTheTvIsNeverOffered() {
        val r = SettingsFinder.findAll(riskyTv, own)
        val offered = FindScreen.entries.flatMap { r.getValue(it) }.map { it.cls }.toSet()
        assertEquals(
            "only the main menu, System info, the Developer options page, nothing else",
            setOf("com.maker.tvsettings.MainActivity", "com.maker.tvsettings.sysinfo.SystemInfoActivity", "com.maker.tvsettings.adv.DevOptActivity"),
            offered,
        )
        assertEquals("com.maker.tvsettings.sysinfo.SystemInfoActivity", r.getValue(FindScreen.ABOUT).first().cls)
        assertTrue("the toast-only stub isn't Android's Accessibility page", r.getValue(FindScreen.ACCESSIBILITY).none { it.specific })
        for (help in listOf(FindHelp.accessibility("Silicon Extend TV"), FindHelp.developerOptions(tv = true), FindHelp.debugging(tv = true, sdk = 28))) {
            assertTrue(SettingsFinder.forStep(help, r).none { it.cls in setOf("com.maker.tvsettings.sysinfo.FactoryResetActivity", "com.android.tv.settings.oemlink.FactoryResetActivity") })
        }

        fun harmful(cls: String, label: String? = null, app: String? = "Settings") =
            SettingsFinder.harmful(FoundActivity(cls.substringBeforeLast('.'), cls, label, app))
        assertEquals("reset", harmful("com.maker.settings.MasterResetActivity"))
        assertEquals("recovery", harmful("com.maker.settings.MainActivity", label = "System recovery"))
        assertEquals("factory", harmful("com.mstar.tv.menu.DebugActivity", app = "Factory Menu"))
        assertEquals("format", harmful("com.android.settings.Settings\$FormatStorageActivity"))
        assertEquals("setupwizard", harmful("com.android.settings.accessibility.AccessibilitySettingsForSetupWizardActivity"))
        // Seen on the Android 16 emulator, in Developer options' section: installs another system image.
        assertEquals("dsu", harmful("com.android.settings.development.DSULoader", label = "Select DSU Package"))
        val android16 = listOf(
            settings("Settings\$DevelopmentSettingsActivity", "Developer options", dev, devLegacy),
            settings("development.DSULoader", "Select DSU Package"),
        )
        assertEquals(listOf("Settings\$DevelopmentSettingsActivity"), find(FindScreen.DEVELOPER_OPTIONS, android16).classes())
        assertNull("\"Preset\" isn't \"reset\"", harmful("com.maker.settings.picture.PresetActivity"))
        assertNull("\"Information\" isn't \"format\"", harmful("com.maker.settings.SystemInformationActivity", label = "System information"))
        assertNull(harmful("com.android.tv.settings.about.AboutActivity"))
        assertNull(harmful("com.android.settings.Settings\$DevelopmentSettingsDashboardActivity", label = "Developer options"))
        assertNull(harmful("com.android.tv.settings.MainSettings"))
    }

    // ───────────── Ranking, de-duplication, the cap ─────────────

    @Test fun androidsOwnPagesComeBeforeAMakersActionHandlerThenNamesAndSettingsAppsBeforeOthers() {
        val device = listOf(
            FoundActivity("com.vendor.tools", "com.vendor.tools.AccessibilityActivity", null, "Tools", system = true),
            FoundActivity("com.vendor.settings", "com.vendor.settings.AccessibilityPanelActivity", null, "Vendor settings"),
            FoundActivity("com.vendor.settings", "com.vendor.settings.AccessibilityActivity", null, "Vendor settings"),
            tvSettings("oemlink.AccessibilitySettingsActivity"),
            FoundActivity("com.vendor.settings", "com.vendor.settings.Handler", null, "Vendor settings", actions = setOf(a11y)),
            settings("Settings\$AccessibilitySettingsActivity", "Accessibility"),
        )
        val list = find(FindScreen.ACCESSIBILITY, device)
        assertEquals(
            listOf(
                // Android's own classes first: when a maker's page answers the action, the setup
                // button stops there and never reaches Android's page.
                "com.android.settings.Settings\$AccessibilitySettingsActivity",
                "com.android.tv.settings.oemlink.AccessibilitySettingsActivity",
                "com.vendor.settings.Handler",
                "com.vendor.settings.AccessibilityActivity",
                "com.vendor.settings.AccessibilityPanelActivity",
                "com.vendor.tools.AccessibilityActivity",
            ),
            list.map { it.cls },
        )
        assertEquals(listOf(1, 1, 2, 3, 3, 3), list.map { it.tier })
        // Android's settings app answering the action leads, before its known classes.
        val stock = find(FindScreen.ACCESSIBILITY, device + settings("Settings\$AccessibilityDashboardActivity", null, a11y))
        assertEquals("com.android.settings.Settings\$AccessibilityDashboardActivity", stock.first().cls)
        assertEquals(0, stock.first().tier)
    }

    @Test fun anActivityFoundTwiceIsOneCandidateWithBothFindingsAndTheListIsCapped() {
        // Found answering the action (a disabled-components query saw it disabled) and by the package scan (enabled).
        val twice = listOf(
            settings("Settings\$AccessibilitySettingsActivity", null, a11y, enabled = false),
            settings("Settings\$AccessibilitySettingsActivity", "Accessibility"),
        )
        val merged = SettingsFinder.merge(twice).single()
        assertTrue(merged.enabled)
        assertEquals(setOf(a11y), merged.actions)
        assertEquals("Accessibility", merged.label)
        assertEquals(1, find(FindScreen.ACCESSIBILITY, twice).size)

        val many = (1..20).map { settings("Settings\$Accessibility${it}Activity") }
        assertEquals(SettingsFinder.LIMIT, find(FindScreen.ACCESSIBILITY, many).size)
        assertEquals(8, SettingsFinder.LIMIT)
        val onlyDisabled = listOf(settings("Settings\$AccessibilitySettingsActivity", null, a11y, enabled = false))
        assertTrue("a disabled handler is never offered", find(FindScreen.ACCESSIBILITY, onlyDisabled).isEmpty())
    }

    @Test fun aMakerMenuThatAlsoClaimsTheAccessibilityActionCountsAsAMainScreen() {
        // It is where the step's own button already went, without the entry.
        val device = listOf(FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.MainActivity", null, "Settings", actions = setOf(main, a11y)))
        val c = find(FindScreen.ACCESSIBILITY, device).single()
        assertEquals(MatchKind.MAIN, c.kind)
        assertFalse(c.specific)
        assertEquals("opened with the action it answers, in case it picks its page from it", a11y, c.action)
        val r = SettingsFinder.findAll(device, own)
        assertTrue(SettingsFinder.notes(FindHelp.accessibility("x"), r, tv = true, sdk = 28, devOptions = true).single().startsWith("This TV hides"))
    }

    @Test fun screenNamesAreReadable() {
        assertEquals("Accessibility settings", SettingsFinder.readable("com.android.settings.Settings\$AccessibilitySettingsActivity"))
        assertEquals("Main settings", SettingsFinder.readable("com.android.tv.settings.MainSettings"))
        assertEquals("Development settings dashboard", SettingsFinder.readable("x.Settings\$DevelopmentSettingsDashboardActivity"))
        assertEquals("USB debugging", SettingsFinder.readable("x.USBDebuggingActivity"))
        assertEquals("ADB switch", SettingsFinder.readable("x.AdbSwitchActivity"))
        assertEquals("Usb debugging, spelled in capitals", "USB debugging", SettingsFinder.readable("x.UsbDebuggingActivity"))
        assertEquals("Developer options", SettingsFinder.readable("x.DevOptActivity"))
        assertEquals("Accessibility menu settings", SettingsFinder.readable("x.A11yMenuSettingsActivity"))
        assertEquals("an alias is named after itself, without the word", "Accessibility", SettingsFinder.readable("x.AccessibilityAlias"))
        assertEquals("USB debugging", SettingsFinder.readable("x.UsbDebuggingActivityAlias"))
        assertEquals("System info", SettingsFinder.readable("x.system_info"))
        assertEquals("Activity", SettingsFinder.readable("x.Activity"))
        assertEquals("development", SettingsFinder.core("DevelopmentSettingsDashboardActivity"))
        assertEquals("accessibility", SettingsFinder.core("Settings\$AccessibilitySettingsActivity".substringAfter('$')))
        assertEquals(listOf("accessibility", "inversion", "settings", "activity"), SettingsFinder.words("AccessibilityInversionSettingsActivity"))
        assertEquals(listOf("usb", "debugging"), SettingsFinder.words("USBDebugging"))
        assertEquals(listOf("system", "info"), SettingsFinder.words("System info"))
        assertEquals(0, SettingsFinder.packageRank("com.android.tv.settings"))
        assertEquals(1, SettingsFinder.packageRank("com.mediatek.wwtv.setting"))
        assertEquals(2, SettingsFinder.packageRank("com.mediatek.wwtv.tvcenter"))
        assertEquals(3, SettingsFinder.packageRank("com.mediatekfan.app"))
    }

    // ───────────── Debugging screens ─────────────

    @Test fun debuggingScreensAreNamedAfterTheSwitchNotAndroid16sDebuggingDataPages() {
        val device = listOf(
            settings("Settings\$DevelopmentSettingsActivity", "Developer options", dev, devLegacy),
            // Android 16 (Pixel Settings), no label, aliases for Settings' SPA bridge: opening the
            // first crashes Settings ("Key DebuggingData is missing in the map"), the second closes at once.
            FoundActivity("com.android.settings", "com.google.android.settings.DebuggingDataActivity", null, "Settings", targetActivity = "com.android.settings.spa.SpaBridgeActivity"),
            FoundActivity("com.android.settings", "com.google.android.settings.AppDebuggingDataActivity", null, "Settings", targetActivity = "com.android.settings.spa.SpaAppBridgeActivity"),
            // A maker's debug menu (its factory menu, often) and a debug log page.
            FoundActivity("com.mediatek.wwtv.tvcenter", "com.mediatek.wwtv.tvcenter.nav.DebugMenuActivity", null, "TV"),
            FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.DebugLogActivity", "Debug log", "Settings"),
            // A maker's own debugging switches.
            FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.adv.AdbDebugActivity", "Network debugging", "Settings"),
            FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.adv.UsbDebuggingActivity", null, "Settings"),
        )
        val list = find(FindScreen.DEBUGGING, device)
        assertEquals(listOf("com.maker.tvsettings.adv.AdbDebugActivity", "com.maker.tvsettings.adv.UsbDebuggingActivity"), list.map { it.cls })
        assertEquals("Settings · USB debugging", list[1].title)
        val step = SettingsFinder.forStep(FindHelp.debugging(tv = false, sdk = 36), SettingsFinder.findAll(device, own))
        assertEquals("Settings\$DevelopmentSettingsActivity", step.first().cls.substringAfterLast('.'))
        assertTrue(step.none { "DebuggingData" in it.cls })
        // In this device's words: the switch a TV, a phone on Android 11+ and an older phone has.
        val c = list.first()
        assertEquals("Named like a Network debugging screen", c.reason(tv = true, sdk = 28))
        assertEquals("Named like a Wireless debugging screen", c.reason(tv = false, sdk = 34))
        assertEquals("Named like a USB debugging screen", c.reason(tv = false, sdk = 28))
        assertEquals("Wireless debugging", SettingsFinder.screenName(FindScreen.DEBUGGING, tv = false, sdk = 30))
        assertEquals("Accessibility", SettingsFinder.screenName(FindScreen.ACCESSIBILITY, tv = false, sdk = 30))
    }

    // ───────────── What the step's button opened ─────────────

    @Test fun aButtonThatLandsInAMakersMenuReachedOnlyItsMainScreen() {
        val makerMenu = "com.mediatek.tvsettings" to "com.mediatek.tvsettings.MainActivity"
        val handlers = listOf(makerMenu, "com.android.tv.settings" to "com.android.tv.settings.MainSettings")
        val a11yRoute = SettingsRoute(a11y)
        // The maker's menu answers the Accessibility action too: the button opened its main page.
        assertTrue(SettingsRoutes.reachedMain(a11yRoute, OpenedScreen(makerMenu.first, makerMenu.second), handlers))
        // Android's own page: the page itself.
        assertFalse(SettingsRoutes.reachedMain(a11yRoute, OpenedScreen("com.android.settings", "com.android.settings.Settings\$AccessibilitySettingsActivity"), handlers))
        // Android's main class, even when nothing listed it (by an alias's target too).
        assertTrue(SettingsRoutes.reachedMain(a11yRoute, OpenedScreen("com.android.settings", "com.android.settings.Settings"), emptyList()))
        assertTrue(SettingsRoutes.reachedMain(a11yRoute, OpenedScreen("com.android.settings", "com.android.settings.HomeAlias", "com.android.settings.Settings"), emptyList()))
        // The main route itself, whatever opened.
        assertTrue(SettingsRoutes.reachedMain(SettingsRoute(main), OpenedScreen("x", "x.Y"), emptyList()))
        assertEquals(listOf("com.android.tv.settings.MainSettings", "com.android.settings.Settings"), SettingsRoutes.MAIN_CLASSES.map { it.second })
        val r = OpenResult(OpenResult.Outcome.MAIN_SCREEN, opened = OpenedScreen(makerMenu.first, makerMenu.second))
        assertTrue(r.offerOthers)
        assertEquals("com.mediatek.tvsettings/.MainActivity", r.opened!!.component)
    }

    @Test fun theScreenTheButtonOpenedGoesLastAndSaysSo() {
        // A TV box on the phone build whose maker page answers the Accessibility action: the button
        // (TV pages, then the action, then the phone pages) stops at the maker's page.
        val device = listOf(
            FoundActivity("com.maker.settings", "com.maker.settings.a11y.PanelActivity", "Accessibility", "Settings", actions = setOf(a11y)),
            settings("Settings\$AccessibilitySettingsActivity", "Accessibility"),
            settings("Settings", null, main),
        )
        val r = SettingsFinder.findAll(device, own)
        val help = FindHelp.accessibility("Silicon Extend TV")
        assertEquals(
            "Android's hidden page first, the maker's action handler after it",
            listOf("com.android.settings.Settings\$AccessibilitySettingsActivity", "com.maker.settings.a11y.PanelActivity", "com.android.settings.Settings"),
            SettingsFinder.forStep(help, r).map { it.cls },
        )
        val afterButton = SettingsFinder.forStep(help, r, opened = OpenedScreen("com.maker.settings", "com.maker.settings.a11y.PanelActivity"))
        assertEquals(
            listOf("com.android.settings.Settings\$AccessibilitySettingsActivity", "com.android.settings.Settings", "com.maker.settings.a11y.PanelActivity"),
            afterButton.map { it.cls },
        )
        assertTrue(afterButton.last().openedByButton)
        assertEquals("The button above opens this screen", afterButton.last().reason(tv = true, sdk = 28))
        assertFalse(afterButton.first().openedByButton)
        val afterMain = SettingsFinder.forStep(help, r, opened = OpenedScreen("com.android.settings", "com.android.settings.Settings"))
        assertEquals("The button above opens this: look for Accessibility there", afterMain.last().reason(tv = true, sdk = 28))
        // Still within the cap, the opened one kept.
        val many = (1..20).map { settings("Settings\$Accessibility${it}Activity") } + device
        val capped = SettingsFinder.forStep(help, SettingsFinder.findAll(many, own), opened = OpenedScreen("com.maker.settings", "com.maker.settings.a11y.PanelActivity"))
        assertEquals(SettingsFinder.LIMIT, capped.size)
        assertEquals("com.maker.settings.a11y.PanelActivity", capped.last().cls)
    }

    // ───────────── User-installed apps, nameless apps ─────────────

    @Test fun aUserInstalledAppCountsOnlyAsAnActionHandlerAfterEveryPreinstalledScreen() {
        val device = listOf(
            // User-installed, in a maker's package space or named like settings: never by its names.
            FoundActivity("com.amazon.avod.thirdpartyclient", "com.amazon.avod.thirdpartyclient.AboutActivity", "About", "Prime Video", system = false),
            FoundActivity("com.example.settingsshortcut", "com.example.settingsshortcut.DevOptActivity", "Developer options", "Settings Shortcut", system = false),
            FoundActivity("com.xiaomi.mitv.tools", "com.xiaomi.mitv.tools.a11y.MainActivity", null, "Tools", system = false),
            // A user-installed shortcut answering the Accessibility action: listed, after the preinstalled screens.
            FoundActivity("com.example.shortcut", "com.example.shortcut.OpenAccessibility", null, "Shortcut", system = false, actions = setOf(a11y)),
            FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.sysinfo.SystemInfoActivity", "System info", "Settings"),
            FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.common.AccessibilityActivity", null, "Settings"),
            FoundActivity("com.maker.tvsettings", "com.maker.tvsettings.MainActivity", null, "Settings", actions = setOf(main)),
        )
        assertEquals(listOf("com.maker.tvsettings.sysinfo.SystemInfoActivity", "com.maker.tvsettings.MainActivity"), find(FindScreen.ABOUT, device).map { it.cls })
        assertTrue(find(FindScreen.DEVELOPER_OPTIONS, device).none { it.specific })
        val a11yList = find(FindScreen.ACCESSIBILITY, device)
        assertEquals(
            listOf("com.maker.tvsettings.common.AccessibilityActivity", "com.example.shortcut.OpenAccessibility", "com.maker.tvsettings.MainActivity"),
            a11yList.map { it.cls },
        )
        assertEquals(5, a11yList[1].tier)
        assertEquals("Shortcut · Accessibility", a11yList[1].title)
    }

    @Test fun anAppWithoutANameShowsJustTheScreen() {
        // Android 16's Accessibility Menu: no app label, so Android's fallback is its package name.
        val pkg = "com.android.systemui.accessibility.accessibilitymenu"
        val c = find(FindScreen.ACCESSIBILITY, listOf(FoundActivity(pkg, "$pkg.activity.A11yMenuSettingsActivity", "Accessibility Menu Settings", pkg))).single()
        assertNull(c.appLabel)
        assertEquals("Accessibility Menu Settings", c.title)
        val noLabels = find(FindScreen.ACCESSIBILITY, listOf(FoundActivity("com.maker.settings", "com.maker.settings.a11y.MainActivity", null, "com.maker.settings"))).single()
        assertEquals("Main", noLabels.title)
        assertEquals("In the Accessibility part of its app", noLabels.reason(tv = true, sdk = 28))
        assertNull(SettingsFinder.shown("com.maker.settings", "com.other"))
        assertNull(SettingsFinder.shown("com.android.settings.Settings\$Foo", "x"))
        assertNull(SettingsFinder.shown("  ", "x"))
        assertEquals("Settings", SettingsFinder.shown("Settings", "com.android.settings"))
        assertEquals("v1.2 info", SettingsFinder.shown("v1.2 info", "x"))
    }

    // ───────────── A step's list across its screens ─────────────

    /** Android TV 14's Settings as read on its emulator (Developer options on). */
    private val tv14 = listOf(
        tvSettings("MainSettings", null, main),
        tvSettings("about.AboutActivity", null, info),
        tvSettings("about.StatusActivity", "Status"),
        tvSettings("about.LicenseActivity", "Third Party Source"),
        tvSettings("enterprise.EnterprisePrivacySettingsActivity", "Managed device info"),
        tvSettings("system.development.DevelopmentActivity", null, dev, devLegacy),
        tvSettings("oemlink.AccessibilitySettingsActivity", null, a11y),
        // Closes at once without an extra naming the service.
        tvSettings("oemlink.AccessibilityServiceActivity"),
    )

    @Test fun acrossAStepsScreensAndroidsPagesComeFirstAndLicenceManagedAndOneServicePagesAreLeftOut() {
        val r = SettingsFinder.findAll(tv14, own)
        val step = SettingsFinder.forStep(FindHelp.developerOptions(tv = true), r)
        assertEquals(
            listOf("about.AboutActivity", "system.development.DevelopmentActivity", "about.StatusActivity", "MainSettings"),
            step.map { it.cls.removePrefix("com.android.tv.settings.") },
        )
        assertEquals(listOf("Settings · About", "Settings · Developer options", "Settings · Status", "Settings · Main settings"), step.map { it.title })
        assertEquals(listOf("oemlink.AccessibilitySettingsActivity", "MainSettings"), r.getValue(FindScreen.ACCESSIBILITY).map { it.cls.removePrefix("com.android.tv.settings.") })
        assertTrue(SettingsFinder.notAScreen(FoundActivity("com.android.settings", "com.android.settings.SettingsLicenseActivity", "Third-party licenses", "Settings")))
        assertFalse(SettingsFinder.notAScreen(FoundActivity("com.android.settings", "com.android.settings.Settings\$MyDeviceInfoActivity", "About phone", "Settings")))
    }

    @Test fun onTheAccessibilityStepDeveloperOptionsThatAreOnlyOffAreNotCalledHidden() {
        // Android 9 Settings with its Accessibility pages switched off (a maker TV) and Developer
        // options off: Settings disables that page until they are on, but About is there.
        val device = listOf(
            settings("Settings", null, main),
            settings("Settings\$MyDeviceInfoActivity", "About phone", info),
            settings("Settings\$DevelopmentSettingsDashboardActivity", "Developer options", dev, enabled = false),
        )
        val r = SettingsFinder.findAll(device, own)
        val notes = SettingsFinder.notes(FindHelp.accessibility("Silicon Extend TV"), r, tv = true, sdk = 28, devOptions = false)
        assertEquals(2, notes.size)
        assertTrue(notes[0], notes[0].startsWith("This TV hides Android's Accessibility setting"))
        assertEquals(
            "Developer options is off, so this TV doesn't show network debugging yet: turn Developer options on first with the Developer options step below.",
            notes[1],
        )
        val phone = SettingsFinder.notes(FindHelp.accessibility("Silicon Extend"), r, tv = false, sdk = 34, devOptions = false)
        assertEquals("Developer options is off, so this device doesn't show wireless debugging yet: turn Developer options on first with the Developer options step below.", phone[1])
    }

    // ───────────── The steps ─────────────

    private fun signals(sdk: Int, tv: Boolean, devOptions: Boolean = false) = SetupSignals(
        sdk = sdk, release = if (sdk == 28) "9" else "$sdk", tv = tv, fire = false, pkg = own,
        label = if (tv) "Silicon Extend TV" else "Silicon Extend", app = if (tv) "Silicon Extend TV" else "Silicon Extend",
        listener = "$own/.notif.ExtendNotificationListener", a11yConnected = false, a11yEnabled = false, listenerConnected = true,
        listenerGranted = true, postGranted = true, batteryOk = true, batteryRequestResolvable = true, devOptions = devOptions, adbWifi = false,
        adbOn = false, adbConnected = false, adbLastError = null, afterRestart = DebuggingAfterRestart.Status.NONE, dpadSupported = false,
    )

    @Test fun theStepsThatNeedASystemScreenOfferTheOtherScreensAndSayWhatToDoThere() {
        val tv = SetupReport.build(signals(28, tv = true))
        val byKey = tv.items.associateBy { it.step.key }
        assertEquals(listOf(FindScreen.ACCESSIBILITY), byKey.getValue("accessibility").find?.screens)
        assertTrue(byKey.getValue("accessibility").find!!.lookFor.startsWith("Open one and look for Silicon Extend TV"))
        assertEquals(listOf(FindScreen.ABOUT, FindScreen.DEVELOPER_OPTIONS), byKey.getValue("developer_options").find?.screens)
        assertTrue(byKey.getValue("developer_options").find!!.lookFor.contains("select Build (or Build number, or the version) 7 times"))
        val debugging = byKey.getValue("network_debugging").find!!
        assertEquals(listOf(FindScreen.DEVELOPER_OPTIONS, FindScreen.DEBUGGING), debugging.screens)
        assertEquals("Open one and turn on Network debugging (or ADB debugging or USB debugging). If it isn't there, press Back and try the next one.", debugging.lookFor)

        val phone = SetupReport.build(signals(34, tv = false)).items.associateBy { it.step.key }
        assertTrue(phone.getValue("developer_options").find!!.lookFor.contains("tap Build"))
        assertEquals("Open one and turn on Wireless debugging. If it isn't there, press Back and try the next one.", phone.getValue("wireless_debugging").find!!.lookFor)
        val phone9 = SetupReport.build(signals(28, tv = false)).items.associateBy { it.step.key }
        assertEquals("Open one and turn on USB debugging. If it isn't there, press Back and try the next one.", phone9.getValue("network_debugging").find!!.lookFor)
        assertNull("the battery step has its own request", phone.getValue("background").find)
        assertNull(phone.getValue("notification_access").find)
        val restart = SetupReport.restartStep(DebuggingAfterRestart.Status.OFF, tv = false, lastError = null, open = null, find = FindHelp.debugging(false, 34))!!
        assertEquals(listOf(FindScreen.DEVELOPER_OPTIONS, FindScreen.DEBUGGING), restart.find?.screens)
    }

    @Test fun reachingOnlyTheMainSettingsScreenOffersTheOtherScreens() {
        assertTrue(SettingsRoutes.isMain(SettingsRoute(main)))
        assertTrue(SettingsRoutes.isMain(SettingsRoute(component = "com.android.tv.settings" to "com.android.tv.settings.MainSettings")))
        assertFalse(SettingsRoutes.isMain(SettingsRoute(a11y)))
        assertFalse(OpenResult(OpenResult.Outcome.PAGE).offerOthers)
        assertTrue(OpenResult(OpenResult.Outcome.MAIN_SCREEN).offerOthers)
        assertTrue(OpenResult(OpenResult.Outcome.NOTHING, "x").offerOthers)
        // Every screen's known classes are the ones the setup buttons try.
        assertEquals(
            listOf("com.android.tv.settings.system.AccessibilityActivity", "com.android.tv.settings.oemlink.AccessibilitySettingsActivity",
                "com.android.settings.Settings\$AccessibilitySettingsActivity"),
            FindScreen.ACCESSIBILITY.known.map { it.second },
        )
        assertTrue(SettingsFinder.ACTIONS.containsAll(listOf(a11y, dev, devLegacy, info, main)))
    }
}

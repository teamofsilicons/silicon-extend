package com.teamofsilicons.extend

import com.teamofsilicons.extend.config.DeviceInfo
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/** UNDERSTANDING.md names the TV app "Silicon Extend TV": the launcher, Settings and the app's own screens say so on a TV. */
class TvNameTest {
    private fun strings(dir: String): Map<String, String> =
        Regex("""<string name="([^"]+)">([^<]*)</string>""").findAll(File("src/main/res/$dir/strings.xml").readText())
            .associate { it.groupValues[1] to it.groupValues[2] }

    @Test fun onATelevisionTheSystemLabelIsTheTvName() {
        val tv = strings("values-television")
        assertEquals("Silicon Extend TV", tv["app_name"])
        assertEquals("Silicon Extend TV", tv["accessibility_label"])
        assertEquals("Silicon Extend TV", tv["notification_listener_label"])
        assertTrue(tv.getValue("accessibility_description").startsWith("Silicon Extend TV reads what is on the screen"))
        assertEquals("Silicon Extend", strings("values")["app_name"])
    }

    @Test fun theTvLauncherEntryIsTheTvName() {
        val manifest = File("src/main/AndroidManifest.xml").readText()
        val leanback = Regex("""<intent-filter([^>]*)>\s*<action android:name="android.intent.action.MAIN" />\s*<category android:name="android.intent.category.LEANBACK_LAUNCHER" />""")
            .find(manifest) ?: throw AssertionError("no LEANBACK_LAUNCHER intent filter")
        assertTrue(leanback.groupValues[1], leanback.groupValues[1].contains("""android:label="@string/app_name_tv""""))
        assertEquals("Silicon Extend TV", strings("values")["app_name_tv"])
        assertTrue(manifest.contains("""android:banner="@drawable/tv_banner""""))
    }

    @Test fun theBannerCarriesTheNameAsOutlines() {
        val banner = File("src/main/res/drawable/tv_banner.xml").readText()
        assertTrue(banner.contains("\"Silicon Extend TV\""))
        // Mark, name and "TV" are drawn: paper, the mark, the name in ink, TV in cobalt.
        assertEquals(4, Regex("android:pathData=").findAll(banner).count())
        assertTrue(banner.contains("android:viewportWidth=\"320\" android:viewportHeight=\"180\""))
    }

    @Test fun theAppsOwnScreensUseTheTvName() {
        assertEquals("Silicon Extend TV", DeviceInfo.appName(tv = true))
        assertEquals("Silicon Extend", DeviceInfo.appName(tv = false))
    }
}

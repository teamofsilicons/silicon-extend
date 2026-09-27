package com.teamofsilicons.extend

import android.app.Notification
import com.teamofsilicons.extend.core.SessionUi
import com.teamofsilicons.extend.core.WakeNotice
import com.teamofsilicons.extend.core.WakeRequests
import com.teamofsilicons.extend.core.WakeUi
import com.teamofsilicons.extend.notif.ExtendNotificationListener
import com.teamofsilicons.extend.service.WakeNotifications
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant

/** Wake requests: what the device may show of each, in the app and in its notification. */
class WakeRequestsTest {
    private fun wake(id: String, silicon: String? = "si:chef", reason: String? = "Check the order screen", side: String? = "side-a", pair: String = "p1", at: Int = 0) =
        WakeUi(id, pair, silicon, reason, side, Instant.parse("2026-09-27T10:00:00Z").plusSeconds(at.toLong()).toString(), "2026-09-27T10:30:00Z")

    private fun session(side: String?) = SessionUi("si:bob", "a3f", "2026-09-27T10:05:00Z", pairId = "p2", carbon = "c:bob", side = side)

    @Test fun anotherSidesSessionHidesWhoAskedAndWhy() {
        val w = wake("w1")
        assertFalse(WakeRequests.redacted(w, null))
        assertFalse("the same side sees its own request", WakeRequests.redacted(w, session("side-a")))
        assertTrue(WakeRequests.redacted(w, session("side-b")))
        assertTrue("a session whose side isn't known hides everything", WakeRequests.redacted(w, session(null)))
        assertTrue("a request without a side is hidden while a session runs", WakeRequests.redacted(wake("w2", side = null), session("side-a")))
        assertTrue("the service already left the Silicon out", WakeRequests.redacted(wake("w3", silicon = null, reason = null), null))
        val lines = WakeRequests.lines(listOf(w), session("side-b"))
        assertNull(lines.single().silicon)
        assertNull(lines.single().reason)
    }

    @Test fun aFrameWithoutSiliconAndReasonReplacesWhatWasHeld() {
        val held = WakeRequests.upsert(emptyList(), wake("w1"))
        val replaced = WakeRequests.upsert(held, wake("w1", silicon = null, reason = null))
        assertEquals(1, replaced.size)
        assertNull(replaced.single().siliconId)
        assertNull(replaced.single().reason)
    }

    @Test fun requestsEndOneByOneByPairAndAtExpiry() {
        val list = listOf(wake("w1"), wake("w2", pair = "p2"))
        assertEquals(listOf("w2"), WakeRequests.remove(list, "w1").map { it.wakeId })
        assertEquals(listOf("w1"), WakeRequests.removePair(list, "p2").map { it.wakeId })
        val expires = Instant.parse("2026-09-27T10:30:00Z").toEpochMilli()
        assertEquals(2, WakeRequests.open(list, expires - 1).size)
        assertEquals(0, WakeRequests.open(list, expires).size)
        assertEquals(expires, WakeRequests.nextExpiry(list))
    }

    @Test fun oneNotificationListsEveryRequest() {
        val one = WakeNotice.of(WakeRequests.lines(listOf(wake("w1")), null), "phone")!!
        assertEquals("si:chef asks to use this phone", one.title)
        assertEquals("Check the order screen", one.text)
        assertEquals("the lock screen shows the name, never the reason", "si:chef asks to use this phone", one.publicTitle)
        assertFalse(one.publicText.contains("order"))
        val two = WakeNotice.of(WakeRequests.lines(listOf(wake("w1"), wake("w2", silicon = "si:sous", reason = "Plate check", at = 60)), null), "phone")!!
        assertEquals("newest first", "si:sous and 1 more ask to use this phone", two.title)
        assertTrue(two.bigText.contains("si:chef: Check the order screen"))
        assertNull(WakeNotice.of(emptyList(), "phone"))
    }

    @Test fun aHiddenRequestNamesNoSiliconAnywhere() {
        val lines = WakeRequests.lines(listOf(wake("w1"), wake("w2", silicon = "si:sous", reason = "Plate check", side = "side-a", at = 60)), session("side-b"))
        val n = WakeNotice.of(lines, "tablet")!!
        assertEquals("A Silicon asked to use this tablet", n.title)
        for (text in listOf(n.title, n.text, n.bigText, n.publicTitle, n.publicText)) {
            assertFalse(text, text.contains("si:") || text.contains("order") || text.contains("Plate"))
        }
        assertTrue(n.text.contains("Ting"))
    }

    @Test fun theNotificationIsPrivateWithANameOnlyPublicVersionAndAlertsOnlyWhenAsked() {
        val notice = WakeNotice.of(WakeRequests.lines(listOf(wake("w1")), null), "phone")!!
        val quiet = WakeNotifications.Spec.of(notice, alert = false, timeoutMs = 60_000)
        assertEquals(Notification.VISIBILITY_PRIVATE, quiet.visibility)
        assertEquals(Notification.CATEGORY_REMINDER, quiet.category)
        assertEquals("si:chef asks to use this phone", quiet.publicTitle)
        assertTrue(quiet.onlyAlertOnce)
        assertTrue(quiet.quiet)
        assertEquals(60_000L, quiet.timeoutMs)
        assertFalse("never a full-screen intent", quiet.fullScreen)
        val loud = WakeNotifications.Spec.of(notice, alert = true, timeoutMs = -5)
        assertFalse(loud.onlyAlertOnce)
        assertFalse(loud.quiet)
        assertNull("an expired request sets no timeout", loud.timeoutMs)
    }

    @Test fun silliconsNeverReadExtendsOwnNotifications() {
        assertFalse(ExtendNotificationListener.isVisibleToSilicons("com.teamofsilicons.extend", "com.teamofsilicons.extend"))
        assertTrue(ExtendNotificationListener.isVisibleToSilicons("com.whatsapp", "com.teamofsilicons.extend"))
        assertTrue(ExtendNotificationListener.isVisibleToSilicons(null, "com.teamofsilicons.extend"))
    }
}

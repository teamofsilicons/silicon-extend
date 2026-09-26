package com.teamofsilicons.extend

import androidx.compose.ui.graphics.toArgb
import com.teamofsilicons.extend.core.SessionUi
import com.teamofsilicons.extend.core.TakeoverUi
import com.teamofsilicons.extend.core.UiState
import com.teamofsilicons.extend.ui.DitherField
import com.teamofsilicons.extend.ui.Dissolve
import com.teamofsilicons.extend.ui.InUseBadge
import com.teamofsilicons.extend.ui.Inks
import com.teamofsilicons.extend.ui.Tokens
import com.teamofsilicons.extend.ui.licenceBlocks
import com.teamofsilicons.extend.ui.sentence
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/** The restyle's pure parts: the dithered field fallback, titles, the licence reflow and the fonts' notices. */
class UiLookTest {
    private val paper = Tokens.Paper.toArgb()

    @Test fun ditherFallbackPrintsOnlyInInterfaceInksAndDissolvesDown() {
        val w = 90
        val h = 37
        val px = DitherField.pixels(w, h, Dissolve.DOWN, 1.3)
        assertTrue("only print inks", px.all { it in DitherField.INKS })
        val row = { y: Int -> (0 until w).count { px[y * w + it] == paper } }
        assertEquals("the top row is solid ink", 0, row(0))
        assertTrue("the bottom row is paper (${row(h - 1)} of $w)", row(h - 1) >= w * 0.95)
        assertTrue("paper and ink are dithered together between them", (0 until h).any { row(it) in 1 until w })
    }

    @Test fun tvFieldDissolvesRightAndTheOrbOutward() {
        val w = 40
        val h = 60
        val right = DitherField.pixels(w, h, Dissolve.RIGHT, 2.1)
        val col = { x: Int -> (0 until h).count { right[it * w + x] == paper } }
        assertEquals(0, col(0))
        assertTrue(col(w - 1) >= h * 0.95)

        val n = 31
        val orb = DitherField.pixels(n, n, Dissolve.OUT, 4.2)
        assertTrue("the orb's centre is ink", orb[(n / 2) * n + n / 2] != paper)
        assertEquals("its corners are paper", listOf(paper, paper, paper, paper), listOf(orb[0], orb[n - 1], orb[(n - 1) * n], orb[n * n - 1]))
    }

    @Test fun theOrbDissolvesIntoPaperBeforeItsEdges() {
        val n = 41
        val orb = DitherField.pixels(n, n, Dissolve.OUT, 4.2)
        val mid = n / 2
        // The middle of each side, not only the corners: the orb must not be clipped by its tile.
        val edges = listOf(orb[mid], orb[(n - 1) * n + mid], orb[mid * n], orb[mid * n + n - 1])
        assertEquals("the middle of each edge is paper", listOf(paper, paper, paper, paper), edges)
        val border = (0 until n).flatMap { listOf(orb[it], orb[(n - 1) * n + it], orb[it * n], orb[it * n + n - 1]) }
        assertTrue("the whole outer ring is paper", border.all { it == paper })
    }

    @Test fun decorationNeverPrintsInStopRed() {
        val stops = setOf(Tokens.Stop.toArgb(), Tokens.StopDeep.toArgb(), 0xFFE0452B.toInt())
        assertTrue("no field ink is a Stop colour", DitherField.INKS.none { it in stops })
        assertEquals("the horizon is Interface's peach", Tokens.Horizon, Inks.horizon)
        for (d in Dissolve.values()) {
            assertTrue(DitherField.pixels(60, 60, d, 1.3).none { it in stops })
        }
    }

    @Test fun tvBadgeIsShortAndNamesTheSilicon() {
        val session = SessionUi("si:concierge", "s-1", "2026-09-27T10:00:00Z")
        assertNull("phones use the notification, not the badge", InUseBadge.from(UiState(session = session)))
        assertNull("nothing running, no badge", InUseBadge.from(UiState(isTv = true)))

        val using = InUseBadge.from(UiState(isTv = true, session = session))!!
        assertEquals("si:concierge is using this TV", using.spoken)
        assertFalse(using.waiting)

        val stopping = InUseBadge.from(UiState(isTv = true, session = session.copy(stopping = true)))!!
        assertEquals("si:concierge is stopping…", stopping.spoken)

        val waiting = InUseBadge.from(UiState(isTv = true, session = session, takeover = TakeoverUi("s-1", "Sign in to Netflix", null)))!!
        assertTrue(waiting.waiting)
        assertEquals("si:concierge", waiting.silicon)
        assertEquals("Sign in to Netflix", waiting.detail)
        assertEquals("si:concierge is waiting for you: Sign in to Netflix", waiting.spoken)
        for (b in listOf(using, stopping, waiting)) assertTrue("'${b.silicon} ${b.line}' stays short", "${b.silicon} ${b.line}".length <= 40)
    }

    @Test fun titlesAreSentencesEndingInOnePeriod() {
        assertEquals("Pair this device.", sentence("Pair this device"))
        assertEquals("Saket's Pixel.", sentence("Saket's Pixel."))
        assertEquals("Revoke pair?", sentence("Revoke pair?"))
        assertEquals("Starting…", sentence("Starting…"))
    }

    @Test fun licenceScreenTurnsRulesIntoHeadingsAndOnlyChangesWhitespace() {
        val raw = File("src/main/assets/open_source_licences.txt").readText()
        val blocks = licenceBlocks(raw)
        assertTrue(blocks.any { it.heading && it.text == "libadb-android 3.1.1 (vendored and modified)" })
        assertTrue(blocks.any { it.heading && it.text == "SIL Open Font License 1.1 (IBM Plex Sans, IBM Plex Mono, Source Serif 4)" })
        assertTrue(blocks.none { it.text.contains("==========") || it.text.contains("##########") })
        assertTrue(
            "wrapped prose is joined",
            blocks.any { it.text.startsWith("Silicon Extend for Android includes the open-source software below. Each part is used under the licence") },
        )
        assertTrue("artifact coordinates keep a line each", blocks.any { it.text.contains("\n  com.squareup.okhttp3:okhttp:4.12.0\n") || it.text.contains("\ncom.squareup.okhttp3:okhttp:4.12.0\n") })
        val rule = Regex("^[=#]{8,}$")
        fun words(t: String) = t.split(Regex("\\s+")).filter { it.isNotEmpty() && !rule.matches(it) }
        assertEquals("every word of the notices is shown, in order", words(raw), blocks.flatMap { words(it.text) })
    }

    @Test fun bundledFontsShipWithTheirOflNotice() {
        for (f in listOf("ibm_plex_sans.ttf", "ibm_plex_mono_regular.ttf", "ibm_plex_mono_medium.ttf", "source_serif_4_regular.ttf")) {
            val file = File("src/main/res/font/$f")
            assertTrue("$f is bundled", file.length() > 50_000)
            // TrueType files start with 0x00010000.
            assertEquals("$f is a TrueType font", listOf<Byte>(0, 1, 0, 0), file.readBytes().take(4))
        }
        val text = File("src/main/assets/open_source_licences.txt").readText()
        for (s in listOf(
            "IBM Plex Sans 3.201", "IBM Plex Mono 2.3", "Source Serif 4 4.005",
            "Copyright © 2017 IBM Corp. with Reserved Font Name \"Plex\"",
            "Copyright 2014 - 2023 Adobe (http://www.adobe.com/), with Reserved Font Name",
            "SIL OPEN FONT LICENSE Version 1.1 - 26 February 2007",
            "in Original or Modified Versions, may be sold by itself.",
        )) {
            assertTrue("notices mention: $s", text.contains(s))
        }
    }
}

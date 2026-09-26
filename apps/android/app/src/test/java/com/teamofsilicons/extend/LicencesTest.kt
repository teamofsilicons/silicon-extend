package com.teamofsilicons.extend

import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/** The notices asset ships the obligations of the components that need them. */
class LicencesTest {
    private val text = File("src/main/assets/open_source_licences.txt").readText()

    @Test fun libadbIsUsedUnderApacheWithItsBsdAndMitNotices() {
        assertTrue(text.contains("\"GPL-3.0-or-later OR Apache-2.0\"; Silicon Extend uses it under the Apache License 2.0"))
        assertTrue(text.contains("Copyright 2013 Cameron Gutman"))
        assertTrue(text.contains("Copyright 2013 Google Inc. (PRNGFixes)"))
        assertTrue(text.contains("Redistributions in binary form must reproduce the above copyright"))
        assertTrue(text.contains("Permission is hereby granted, free of charge"))
    }

    @Test fun spake2ShipsTheLgplTextsSourceAndReplacementInstructions() {
        assertTrue(text.contains("com.github.MuntashirAkon.spake2-java:spake2-android:2.2.1"))
        assertTrue(text.contains("https://github.com/MuntashirAkon/spake2-java"))
        assertTrue(text.contains("System.loadLibrary(\"spake2\")"))
        assertTrue(text.contains("GNU LESSER GENERAL PUBLIC LICENSE\n                       Version 3, 29 June 2007"))
        assertTrue(text.contains("GNU GENERAL PUBLIC LICENSE") && text.contains("Version 3, 29 June 2007"))
        assertTrue(text.contains("Version 2.1, February 1999"))
    }

    @Test fun everyOtherComponentIsListed() {
        for (artifact in listOf("com.squareup.okhttp3:okhttp:4.12.0", "com.squareup.okio:okio", "androidx.core:core:", "androidx.compose.ui:ui",
            "org.jetbrains.kotlinx:kotlinx-coroutines-android", "org.conscrypt:conscrypt-android:2.5.3", "org.bouncycastle:bcprov-jdk15to18:1.81")) {
            assertTrue("missing $artifact", text.contains(artifact))
        }
        assertTrue(text.contains("TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION"))
        assertTrue(text.contains("The Legion of the Bouncy Castle"))
        assertTrue(text.contains("THE SOFTWARE IS PROVIDED \"AS IS\" AND THE AUTHOR DISCLAIMS ALL WARRANTIES"))
        assertTrue(text.contains("Mozilla Public License"))
    }
}

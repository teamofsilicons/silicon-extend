package com.teamofsilicons.extend

import com.teamofsilicons.extend.display.DisplayImage
import com.teamofsilicons.extend.display.DisplayLoadState
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test
import java.io.IOException
import java.nio.file.Files

class DisplayImageTest {
    private fun download(response: MockResponse, limit: Long = 32, check: (java.io.File, () -> java.io.File) -> Unit) {
        val dir = Files.createTempDirectory("display-image").toFile()
        val server = MockWebServer()
        try {
            server.enqueue(response)
            val call = OkHttpClient().newCall(Request.Builder().url(server.url("/image")).build())
            check(dir) { DisplayImage.download(call, dir, limit) }
        } finally { server.close(); dir.deleteRecursively() }
    }

    @Test fun downloadsToDisk() = download(MockResponse().setBody("image")) { _, load ->
        assertEquals("image", load().readText())
    }

    @Test fun httpErrorsAreFailuresAndLeaveNoTemporaryFile() = download(MockResponse().setResponseCode(404).setBody("missing")) { dir, load ->
        val error = assertThrows(IOException::class.java) { load() }
        assertTrue(error.message!!.contains("HTTP 404"))
        assertEquals(0, dir.listFiles()!!.size)
    }

    @Test fun advertisedOversizeIsRefused() = download(MockResponse().setBody("x".repeat(33))) { dir, load ->
        assertThrows(IOException::class.java) { load() }
        assertEquals(0, dir.listFiles()!!.size)
    }

    @Test fun unknownLengthIsStillBounded() = download(MockResponse().setChunkedBody("x".repeat(33), 5)) { dir, load ->
        assertThrows(IOException::class.java) { load() }
        assertEquals(0, dir.listFiles()!!.size)
    }

    @Test fun samplingCapsDisplayDimensionsAndDecodedMemory() {
        assertEquals(1, DisplayImage.sampleSize(800, 600, 1920, 1080))
        for ((width, height) in listOf(3840 to 2160, 12000 to 9000, 1 to Int.MAX_VALUE, Int.MAX_VALUE to 1)) {
            val sample = DisplayImage.sampleSize(width, height, 3840, 2160)
            val w = (width.toLong() + sample - 1) / sample
            val h = (height.toLong() + sample - 1) / sample
            assertTrue(w <= 3840 && h <= 2160)
            assertTrue(w * h <= DisplayImage.MAX_PIXELS)
        }
        assertThrows(IOException::class.java) { DisplayImage.sampleSize(-1, -1, 1920, 1080) }
    }

    @Test fun oldLoadsCannotOverwriteANewerRequestOrResurrectAClearedDisplay() {
        val state = DisplayLoadState()
        state.begin("old")
        assertFalse(state.result!!.ready)
        state.begin("new")
        state.complete("old", "HTTP 404")
        assertEquals(DisplayLoadState.Result("new"), state.result)
        state.complete("new")
        assertTrue(state.result!!.ready)
        state.clear("old")
        assertTrue(state.result!!.ready)
        state.clear("new")
        state.complete("new")
        assertNull(state.result)
    }
}

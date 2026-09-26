package com.teamofsilicons.extend

import com.teamofsilicons.extend.adb.AdbDiscovery
import com.teamofsilicons.extend.net.ExtendApi
import kotlinx.coroutines.runBlocking
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test
import java.io.File
import java.nio.file.Files
import java.util.concurrent.TimeUnit

class ArtifactNameTest {
    @Test fun headerNamesArePrintableAsciiWithoutSlashes() {
        assertEquals("r_sum_.pdf", ExtendApi.headerSafeName("résumé.pdf"))
        assertEquals("__.png", ExtendApi.headerSafeName("写真.png"))
        assertEquals("a_b_c", ExtendApi.headerSafeName("a\\b/c"))
        assertEquals("file", ExtendApi.headerSafeName(""))
        assertEquals(255, ExtendApi.headerSafeName("x".repeat(400)).length)
    }

    @Test fun aPulledFileWithANonAsciiNameUploads() = runBlocking {
        val server = MockWebServer()
        val dir = Files.createTempDirectory("upload").toFile()
        try {
            val file = File(dir, "Café résumé.pdf").apply { writeText("pdf") }
            server.enqueue(MockResponse().setResponseCode(201))
            val api = ExtendApi({ server.url("/").toString().trimEnd('/') })
            api.uploadFile("device", "slot", file, "application/pdf")
            val request = server.takeRequest(2, TimeUnit.SECONDS)!!
            assertEquals("Caf_ r_sum_.pdf", request.getHeader("X-File-Name"))
            assertEquals("pdf", request.body.readUtf8())
        } finally { dir.deleteRecursively(); server.close() }
    }

    @Test fun discoveryAfterPairingOnlyAcceptsThePairedDevicesServiceName() {
        assertTrue(AdbDiscovery.matches("anything", null))
        assertTrue(AdbDiscovery.matches("adb-R5CT12345-AbCdEf", "R5CT12345-AbCdEf"))
        assertTrue(AdbDiscovery.matches("adb-R5CT12345-AbCdEf", "adb-R5CT12345-AbCdEf"))
        assertFalse(AdbDiscovery.matches("adb-EVIL-000000", "R5CT12345-AbCdEf"))
        assertFalse(AdbDiscovery.matches(null, "R5CT12345-AbCdEf"))
    }
}

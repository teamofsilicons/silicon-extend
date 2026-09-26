package com.teamofsilicons.bridge

import com.teamofsilicons.bridge.net.BridgeApi
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.SocketPolicy
import org.junit.Assert.*
import org.junit.Test
import java.io.File
import java.security.MessageDigest
import java.util.concurrent.TimeUnit

class ArtifactUploadTest {
    @Test fun streamingUploadPreservesBytesAndChecksum() = runBlocking {
        val server = MockWebServer()
        val file = File.createTempFile("bridge-upload", ".mp4")
        try {
            val bytes = ByteArray(200000) { (it % 256).toByte() }
            file.writeBytes(bytes)
            server.enqueue(MockResponse().setResponseCode(204))
            val api = BridgeApi({ server.url("/").toString().trimEnd('/') })
            api.uploadFile("test-device", "test-slot", file, "video/mp4")
            val request = server.takeRequest(2, TimeUnit.SECONDS)!!
            assertEquals("PUT", request.method)
            assertEquals("/api/v1/device/artifacts/test-slot", request.path)
            assertEquals("Bridge-Device test-device", request.getHeader("Authorization"))
            assertEquals(MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }, request.getHeader("X-Content-SHA256"))
            assertArrayEquals(bytes, request.body.readByteArray())
        } finally { file.delete(); server.close() }
    }
    @Test fun cancelledUploadDoesNotWaitForNetworkTimeout() = runBlocking {
        val server = MockWebServer()
        val file = File.createTempFile("bridge-upload", ".mp4").apply { writeText("video") }
        try {
            server.enqueue(MockResponse().setSocketPolicy(SocketPolicy.NO_RESPONSE))
            val api = BridgeApi({ server.url("/").toString().trimEnd('/') })
            val started = System.nanoTime()
            assertNull(withTimeoutOrNull(300) { api.uploadFile("test-device", "test-slot", file, "video/mp4") })
            assertTrue((System.nanoTime() - started) / 1_000_000 < 2000)
        } finally { file.delete(); server.close() }
    }
}

package com.teamofsilicons.extend

import com.teamofsilicons.extend.net.ApiException
import com.teamofsilicons.extend.net.ExtendApi
import com.teamofsilicons.extend.net.Socket
import com.teamofsilicons.extend.net.SocketEvent
import com.teamofsilicons.extend.protocol.EnrollmentCreate
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/** The service's error envelope, read through the real HTTP and WebSocket clients. */
class ServiceErrorTest {
    private val server = MockWebServer().apply { start() }

    @After fun stop() = server.shutdown()

    /** Exactly what `crates/extend-service` answers when the enrollment limit is reached (state.rs `rate_limit`). */
    private val limited = """{"type":"error","data":{"code":"rate_limited","message":"Too many new enrollments from this address; the limit is 60 per 60 minutes.","hint":"Retry in 754 seconds.","details":{"retry_after_s":754},"request_id":"req_1"}}"""

    @Test fun a429CarriesTheServiceCodeHintAndWait() = runBlocking {
        server.enqueue(MockResponse().setResponseCode(429).setHeader("Content-Type", "application/json").setBody(limited))
        val api = ExtendApi({ server.url("").toString().trimEnd('/') })
        try {
            api.createEnrollment(EnrollmentCreate(os = "android", appVersion = "1.0.0"))
            fail("expected a refusal")
        } catch (e: ApiException) {
            assertEquals(429, e.status)
            assertEquals("rate_limited", e.code)
            assertTrue(e.rateLimited)
            assertEquals("Too many new enrollments from this address; the limit is 60 per 60 minutes.", e.message)
            assertEquals("Retry in 754 seconds.", e.hint)
            assertEquals(754L, e.retryAfterS)
        }
    }

    @Test fun aRetryAfterHeaderIsUsedWhenTheBodyHasNoWait() {
        val e = ExtendApi.errorOf(429, "<html>Too Many Requests</html>", "30", "POST /api/v1/enrollments")
        assertTrue(e.rateLimited)
        assertEquals(30L, e.retryAfterS)
        assertEquals("HTTP 429 from POST /api/v1/enrollments", e.message)
        assertEquals(null, ExtendApi.errorOf(500, "", null, "GET /x").retryAfterS)
    }

    @Test fun aRefusedSocketUpgradeCarriesTheServiceError() = runBlocking {
        server.enqueue(MockResponse().setResponseCode(429).setHeader("Content-Type", "application/json").setBody(limited))
        val url = server.url("/api/v1/enrollments/e1/connect").toString().replaceFirst("http", "ws")
        val sock = Socket.open(ExtendApi.defaultClient(), url, "Extend-Enrollment ens_x")
        val ev = withTimeout(10_000) { sock.events.receive() }
        sock.close()
        ev as SocketEvent.Failed
        assertEquals(429, ev.httpStatus)
        assertEquals("rate_limited", ev.refusal?.code)
        assertEquals(754L, ev.refusal?.retryAfterS)
    }
}

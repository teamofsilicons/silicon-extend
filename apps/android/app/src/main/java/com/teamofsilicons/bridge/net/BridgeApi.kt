package com.teamofsilicons.bridge.net

import com.teamofsilicons.bridge.protocol.BridgeJson
import com.teamofsilicons.bridge.protocol.DeviceSelf
import com.teamofsilicons.bridge.protocol.EnrollmentCreate
import com.teamofsilicons.bridge.protocol.EnrollmentCreated
import com.teamofsilicons.bridge.protocol.EnrollmentState
import com.teamofsilicons.bridge.protocol.Frames
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import java.security.MessageDigest
import java.util.concurrent.TimeUnit

/** An HTTP answer that wasn't a success, with the service's error `code` when it sent one. */
class ApiException(val status: Int, val code: String?, message: String) : Exception(message)

/** The HTTP endpoints a Bridge app uses (`docs/device-protocol.md` sections 1–3). */
class BridgeApi(private val baseUrl: () -> String, val client: OkHttpClient = defaultClient()) {

    suspend fun createEnrollment(body: EnrollmentCreate): EnrollmentCreated = io {
        val payload = buildJsonObject {
            put("type", JsonPrimitive("enrollment"))
            put("data", BridgeJson.encodeToJsonElement(EnrollmentCreate.serializer(), body))
        }
        val request = Request.Builder()
            .url(baseUrl() + "/api/v1/enrollments")
            .post(payload.toString().toRequestBody(JSON))
            .build()
        client.newCall(request).execute().use { response ->
            val obj = expectObject(response)
            BridgeJson.decodeFromJsonElement(EnrollmentCreated.serializer(), obj)
        }
    }

    suspend fun getEnrollment(id: String, secret: String): EnrollmentState? = io {
        val request = Request.Builder()
            .url(baseUrl() + "/api/v1/enrollments/$id")
            .header("Authorization", "Bridge-Enrollment $secret")
            .get()
            .build()
        client.newCall(request).execute().use { response -> Frames.decodeEnrollmentState(expectObject(response)) }
    }

    suspend fun discardEnrollment(id: String, secret: String) = io {
        val request = Request.Builder()
            .url(baseUrl() + "/api/v1/enrollments/$id")
            .header("Authorization", "Bridge-Enrollment $secret")
            .delete()
            .build()
        client.newCall(request).execute().close()
    }

    suspend fun device(credential: String): DeviceSelf = io {
        val request = Request.Builder()
            .url(baseUrl() + "/api/v1/device")
            .header("Authorization", "Bridge-Device $credential")
            .get()
            .build()
        client.newCall(request).execute().use { response ->
            BridgeJson.decodeFromJsonElement(DeviceSelf.serializer(), expectObject(response))
        }
    }

    /** Revoke pair. */
    suspend fun revoke(credential: String) = io {
        val request = Request.Builder()
            .url(baseUrl() + "/api/v1/device")
            .header("Authorization", "Bridge-Device $credential")
            .delete()
            .build()
        client.newCall(request).execute().use { expectSuccess(it) }
    }

    /** Stop, for when the socket is down. */
    suspend fun stop(credential: String) = io {
        val request = Request.Builder()
            .url(baseUrl() + "/api/v1/device/stop")
            .header("Authorization", "Bridge-Device $credential")
            .post(ByteArray(0).toRequestBody(null))
            .build()
        client.newCall(request).execute().use { expectSuccess(it) }
    }

    /** Uploads one file a command produced, under one of the command's upload ids. */
    suspend fun upload(credential: String, uploadId: String, bytes: ByteArray, contentType: String, fileName: String) =
        io {
            val request = Request.Builder()
                .url(baseUrl() + "/api/v1/device/artifacts/$uploadId")
                .header("Authorization", "Bridge-Device $credential")
                .header("X-Content-SHA256", sha256Hex(bytes))
                .header("X-File-Name", fileName)
                .put(bytes.toRequestBody(contentType.toMediaType()))
                .build()
            client.newCall(request).execute().use { expectSuccess(it) }
        }

    private fun expectSuccess(response: Response) {
        if (!response.isSuccessful) throw toApiException(response)
    }

    private fun expectObject(response: Response): JsonObject {
        if (!response.isSuccessful) throw toApiException(response)
        val text = response.body?.string().orEmpty()
        return Frames.parseObject(text)
            ?: throw ApiException(response.code, null, "The service answered ${response.code} with a body that isn't a JSON object")
    }

    private fun toApiException(response: Response): ApiException {
        val body = runCatching { response.body?.string() }.getOrNull().orEmpty()
        val obj = Frames.parseObject(body)
        val code = (obj?.get("code") as? JsonPrimitive)?.contentOrNull
        val message = (obj?.get("message") as? JsonPrimitive)?.contentOrNull
        return ApiException(
            response.code,
            code,
            message ?: "HTTP ${response.code} from ${response.request.method} ${response.request.url.encodedPath}",
        )
    }

    private suspend fun <T> io(block: () -> T): T = withContext(Dispatchers.IO) { block() }

    companion object {
        private val JSON = "application/json".toMediaType()

        fun defaultClient(): OkHttpClient = OkHttpClient.Builder()
            .connectTimeout(15, TimeUnit.SECONDS)
            .readTimeout(60, TimeUnit.SECONDS)
            .writeTimeout(120, TimeUnit.SECONDS)
            // WebSocket-level pings so a dead connection is noticed even between app-level pings.
            .pingInterval(20, TimeUnit.SECONDS)
            .build()

        fun sha256Hex(bytes: ByteArray): String =
            MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
    }
}

package com.teamofsilicons.extend.net

import com.teamofsilicons.extend.protocol.ExtendJson
import com.teamofsilicons.extend.protocol.DeviceSelf
import com.teamofsilicons.extend.protocol.EnrollmentCreate
import com.teamofsilicons.extend.protocol.EnrollmentCreated
import com.teamofsilicons.extend.protocol.EnrollmentState
import com.teamofsilicons.extend.protocol.Frames
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
import okhttp3.RequestBody.Companion.asRequestBody
import java.io.File
import okhttp3.Response
import java.security.MessageDigest
import java.util.concurrent.TimeUnit

/**
 * An HTTP answer that wasn't a success, with the service's error `code`, its `hint` and, for
 * `rate_limited`, how long to wait (`details.retry_after_s`, or a `Retry-After` header).
 */
class ApiException(
    val status: Int,
    val code: String?,
    message: String,
    val hint: String? = null,
    val retryAfterS: Long? = null,
) : Exception(message) {
    /** Extend (or something in front of it) answered 429: it was reachable, and is limiting requests. */
    val rateLimited: Boolean get() = status == 429 || code == "rate_limited"
}

/** The HTTP endpoints an Extend app uses (`docs/device-protocol.md` sections 1–3). */
class ExtendApi(private val baseUrl: () -> String, val client: OkHttpClient = defaultClient()) {

    suspend fun createEnrollment(body: EnrollmentCreate): EnrollmentCreated = io {
        val payload = buildJsonObject {
            put("type", JsonPrimitive("enrollment"))
            put("data", ExtendJson.encodeToJsonElement(EnrollmentCreate.serializer(), body))
        }
        val request = Request.Builder()
            .url(baseUrl() + "/api/v1/enrollments")
            .post(payload.toString().toRequestBody(JSON))
            .build()
        client.newCall(request).execute().use { response ->
            val obj = expectObject(response)
            ExtendJson.decodeFromJsonElement(EnrollmentCreated.serializer(), obj)
        }
    }

    /**
     * "Pair with another Carbon" (1.1): a new pairing code for this same device, asked for with the
     * [credential] of any of its live pairs, so the code pairs into this device. It is then followed
     * like a first enrollment. A 1.0 service answers 404; a device at its pair limit, 409.
     */
    suspend fun createPairEnrollment(credential: String): EnrollmentCreated = io {
        // No body: the service takes everything it needs from the credential's pair
        // (contracts/v1/device/android.device.enrollments.create.json).
        val request = Request.Builder()
            .url(baseUrl() + "/api/v1/device/enrollments")
            .header("Authorization", "Extend-Device $credential")
            .post(ByteArray(0).toRequestBody(null))
            .build()
        client.newCall(request).execute().use { response ->
            ExtendJson.decodeFromJsonElement(EnrollmentCreated.serializer(), expectObject(response))
        }
    }

    suspend fun getEnrollment(id: String, secret: String): EnrollmentState? = io {
        val request = Request.Builder()
            .url(baseUrl() + "/api/v1/enrollments/$id")
            .header("Authorization", "Extend-Enrollment $secret")
            .get()
            .build()
        client.newCall(request).execute().use { response -> Frames.decodeEnrollmentState(expectObject(response)) }
    }

    suspend fun discardEnrollment(id: String, secret: String) = io {
        val request = Request.Builder()
            .url(baseUrl() + "/api/v1/enrollments/$id")
            .header("Authorization", "Extend-Enrollment $secret")
            .delete()
            .build()
        client.newCall(request).execute().close()
    }

    suspend fun device(credential: String): DeviceSelf = io {
        val request = Request.Builder()
            .url(baseUrl() + "/api/v1/device")
            .header("Authorization", "Extend-Device $credential")
            .get()
            .build()
        client.newCall(request).execute().use { response ->
            ExtendJson.decodeFromJsonElement(DeviceSelf.serializer(), expectObject(response))
        }
    }

    /** Revoke pair: ends the pair whose [credential] this is, and no other. */
    suspend fun revoke(credential: String) = io {
        val request = Request.Builder()
            .url(baseUrl() + "/api/v1/device")
            .header("Authorization", "Extend-Device $credential")
            .delete()
            .build()
        client.newCall(request).execute().use { expectSuccess(it) }
    }

    /** Stop, for when the socket is down. Any pair's credential stops the device's session. */
    suspend fun stop(credential: String) = io {
        val request = Request.Builder()
            .url(baseUrl() + "/api/v1/device/stop")
            .header("Authorization", "Extend-Device $credential")
            .post(ByteArray(0).toRequestBody(null))
            .build()
        client.newCall(request).execute().use { expectSuccess(it) }
    }

    /** Uploads one file a command produced, under one of the command's upload ids. */
    suspend fun upload(credential: String, uploadId: String, bytes: ByteArray, contentType: String, fileName: String) =
        io {
            val request = Request.Builder()
                .url(baseUrl() + "/api/v1/device/artifacts/$uploadId")
                .header("Authorization", "Extend-Device $credential")
                .header("X-Content-SHA256", sha256Hex(bytes))
                .header("X-File-Name", headerSafeName(fileName))
                .put(bytes.toRequestBody(contentType.toMediaType()))
                .build()
            client.newCall(request).execute().use { expectSuccess(it) }
        }

    /** Recordings are streamed from disk; never load an entire video into the app heap. */
    suspend fun uploadFile(credential: String, uploadId: String, file: File, contentType: String) = withContext(Dispatchers.IO) {
        val digest = MessageDigest.getInstance("SHA-256")
        file.inputStream().use { input ->
            val buffer = ByteArray(64 * 1024)
            while (true) {
                val n = input.read(buffer)
                if (n < 0) break
                digest.update(buffer, 0, n)
            }
        }
        val hash = digest.digest().joinToString("") { "%02x".format(it) }
        val request = Request.Builder()
            .url(baseUrl() + "/api/v1/device/artifacts/$uploadId")
            .header("Authorization", "Extend-Device $credential")
            .header("X-Content-SHA256", hash)
            .header("X-File-Name", headerSafeName(file.name))
            .put(file.asRequestBody(contentType.toMediaType()))
            .build()
        val call = client.newCall(request)
        kotlinx.coroutines.suspendCancellableCoroutine<Unit> { continuation ->
            continuation.invokeOnCancellation { call.cancel() }
            call.enqueue(object : okhttp3.Callback {
                override fun onFailure(call: okhttp3.Call, e: java.io.IOException) {
                    if (continuation.isActive) continuation.resumeWith(Result.failure(e))
                }
                override fun onResponse(call: okhttp3.Call, response: Response) {
                    response.use {
                        val result = runCatching { expectSuccess(it) }
                        if (continuation.isActive) continuation.resumeWith(result)
                    }
                }
            })
        }
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
        return errorOf(response.code, body, response.header("Retry-After"), "${response.request.method} ${response.request.url.encodedPath}")
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

        /**
         * `X-File-Name` for [name]: HTTP header values are printable ASCII, and the service refuses
         * slashes. Other characters become `_` (the name in the command's result keeps them).
         */
        fun headerSafeName(name: String): String {
            val safe = name.map { c -> if (c in ' '..'~' && c != '/' && c != '\\') c else '_' }.joinToString("").trim().take(255)
            return safe.ifEmpty { "file" }
        }

        /**
         * The service's error envelope (`{"type":"error","data":{code,message,hint,details}}`) as an
         * [ApiException]. [retryAfterHeader] is used when the body gives no `details.retry_after_s`.
         */
        fun errorOf(status: Int, body: String, retryAfterHeader: String?, what: String): ApiException {
            val obj = Frames.parseObject(body)
            fun text(o: JsonObject?, key: String) = (o?.get(key) as? JsonPrimitive)?.contentOrNull?.takeIf { it.isNotBlank() }
            val details = obj?.get("details") as? JsonObject
            val retry = (details?.get("retry_after_s") as? JsonPrimitive)?.contentOrNull?.toDoubleOrNull()?.let { kotlin.math.ceil(it).toLong() }
                ?: retryAfterHeader?.trim()?.toLongOrNull()
            return ApiException(
                status,
                text(obj, "code"),
                text(obj, "message") ?: "HTTP $status from $what",
                text(obj, "hint"),
                retry?.takeIf { it >= 0 },
            )
        }

        fun sha256Hex(bytes: ByteArray): String =
            MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
    }
}

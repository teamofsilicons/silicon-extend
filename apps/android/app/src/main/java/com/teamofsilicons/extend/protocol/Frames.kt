package com.teamofsilicons.extend.protocol

import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.KSerializer
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNamingStrategy
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject

/**
 * The device protocol (`docs/device-protocol.md`), mirrored from the Rust source of truth in
 * `crates/extend-protocol/src/frames.rs`: every frame is one JSON text message whose `type` names
 * the frame and whose other fields sit next to it (serde's internally tagged enums).
 *
 * Decoding is deliberately forgiving: unknown fields are ignored, unknown frame types decode to
 * [ServiceFrame.Unknown] instead of failing, and a frame whose fields arrive wrapped in a `data`
 * object (the `{"type","data"}` envelope `api.yaml` describes for the sockets) is unwrapped first.
 */
@OptIn(ExperimentalSerializationApi::class)
val ExtendJson: Json = Json {
    ignoreUnknownKeys = true
    explicitNulls = false
    encodeDefaults = true
    namingStrategy = JsonNamingStrategy.SnakeCase
    coerceInputValues = true
}

// ───────────── Shared shapes (model.rs) ─────────────

@Serializable
data class TestingEnvironment(
    val environmentId: String,
    val name: String,
    val state: String = "",
    val pairedDevices: Long = 0,
    val deviceLimit: Long = 5,
)

@Serializable
data class MissingCapability(val capability: String, val reason: String)

@Serializable
data class SetupStep(
    val key: String,
    val title: String,
    /** `todo`, `in_progress`, `needs_carbon`, `done` or `failed`. */
    val status: String,
    val help: String? = null,
    val error: String? = null,
)

@Serializable
data class Setup(
    /** `in_progress`, `needs_carbon` or `complete`. */
    val state: String,
    val steps: List<SetupStep>,
)

@Serializable
data class Attachment(val name: String, val contentType: String, val contentBase64: String)

@Serializable
data class CommandError(val code: String, val message: String, val details: JsonElement? = null)

@Serializable
data class ProducedFile(
    val uploadId: String,
    val name: String,
    val contentType: String,
    /** `screenshot`, `recording`, `log`, `replay_script`, `diff` or `other`. */
    val kind: String,
    val sizeBytes: Long,
)

@Serializable
data class Member(val type: String, val id: String, val displayName: String? = null)

@Serializable
data class InUse(val siliconId: String, val sessionId: String, val since: String, val paused: Boolean = false)

@Serializable
data class TakeoverInfo(
    val takeoverId: String? = null,
    val sessionId: String,
    val reason: String,
    val startedAt: String? = null,
    val expiresAt: String? = null,
)

@Serializable
data class DeviceSelf(
    val deviceId: String,
    val name: String,
    val owner: Member,
    val team: String = "",
    val os: String = "",
    val inUse: InUse? = null,
    val takeover: TakeoverInfo? = null,
    val setup: Setup? = null,
    val environment: TestingEnvironment? = null,
)

@Serializable
data class EnrollmentCreate(
    val os: String,
    val osVersion: String? = null,
    val model: String? = null,
    val appVersion: String,
    val agentDeviceVersion: String? = null,
)

@Serializable
data class EnrollmentCreated(
    val enrollmentId: String,
    val enrollmentSecret: String,
    val pairingCode: String,
    val codeExpiresAt: String,
    val rotatesEveryS: Long = 300,
)

// ───────────── Frames the service sends on the device socket ─────────────

sealed interface ServiceFrame {
    @Serializable
    data class Command(
        val id: String,
        val sessionId: String,
        val target: String? = null,
        val command: String,
        val args: List<String> = emptyList(),
        val attachments: List<Attachment> = emptyList(),
        val timeoutMs: Long = 30_000,
        val uploadIds: List<String> = emptyList(),
    ) : ServiceFrame

    @Serializable
    data class Cancel(val id: String) : ServiceFrame

    @Serializable
    data class SessionStarted(
        val target: String? = null,
        val sessionId: String,
        val siliconId: String,
        val since: String,
    ) : ServiceFrame

    @Serializable
    data class SessionEnded(val target: String? = null, val sessionId: String, val reason: String) : ServiceFrame

    @Serializable
    data class Takeover(
        val target: String? = null,
        val sessionId: String,
        val reason: String,
        val expiresAt: String,
    ) : ServiceFrame

    @Serializable
    data class TakeoverEnded(val target: String? = null, val sessionId: String) : ServiceFrame

    data object Refresh : ServiceFrame

    @Serializable
    data class Environment(val environment: TestingEnvironment? = null) : ServiceFrame

    @Serializable
    data class Unpaired(val reason: String) : ServiceFrame

    data object Superseded : ServiceFrame

    @Serializable
    data class Ping(val nonce: Long) : ServiceFrame

    /** A frame this app doesn't handle (`attach`, `setup_code` are for host computers) or can't parse. */
    data class Unknown(val type: String?, val raw: String, val problem: String? = null) : ServiceFrame
}

// ───────────── Frames on the enrollment socket ─────────────

sealed interface EnrollmentFrame {
    @Serializable
    data class Code(val pairingCode: String, val codeExpiresAt: String) : EnrollmentFrame

    @Serializable
    data class Paired(
        val deviceId: String,
        val deviceCredential: String,
        val environment: TestingEnvironment? = null,
    ) : EnrollmentFrame

    @Serializable
    data class Ping(val nonce: Long) : EnrollmentFrame

    data class Unknown(val type: String?, val raw: String, val problem: String? = null) : EnrollmentFrame
}

/** `GET /api/v1/enrollments/{id}` → `data`, tagged by `state`. */
sealed interface EnrollmentState {
    data class Waiting(val pairingCode: String, val codeExpiresAt: String) : EnrollmentState
    data class Paired(val deviceId: String, val deviceCredential: String, val environment: TestingEnvironment?) :
        EnrollmentState
}

// ───────────── Frames the device sends ─────────────

sealed interface DeviceFrame {
    @Serializable
    data class Hello(
        val appVersion: String,
        val os: String,
        val osVersion: String? = null,
        val model: String? = null,
        val agentDeviceVersion: String? = null,
        val capabilities: List<String>,
        val missing: List<MissingCapability> = emptyList(),
        val setup: Setup,
    ) : DeviceFrame

    @Serializable
    data class SetupProgress(val setup: Setup) : DeviceFrame

    @Serializable
    data class Result(
        val id: String,
        val ok: Boolean,
        val output: JsonElement = JsonNull,
        val text: String? = null,
        val error: CommandError? = null,
        val files: List<ProducedFile> = emptyList(),
    ) : DeviceFrame

    data object Stop : DeviceFrame
    data object TakeoverDone : DeviceFrame

    @Serializable
    data class Pong(val nonce: Long) : DeviceFrame
}

object Frames {
    /** WebSocket close codes (frames.rs `close`). */
    const val CLOSE_UNAUTHORIZED = 4401
    const val CLOSE_SUPERSEDED = 4409
    const val CLOSE_UPGRADE_REQUIRED = 4426

    /**
     * Largest `result` the app sends. The service refuses device messages over 16 MiB, and OkHttp
     * closes the socket instead of queueing more than 16 MiB, which would drop every other answer.
     */
    const val MAX_RESULT_BYTES = 15 * 1024 * 1024

    /** [result] itself, or a failure in its place when it is too large to send in one message. */
    fun fitResult(result: DeviceFrame.Result, limit: Int = MAX_RESULT_BYTES): DeviceFrame.Result {
        val size = encode(result).toByteArray(Charsets.UTF_8).size
        if (size <= limit) return result
        val message = "This command's result was ${(size + 1024 * 1024 - 1) / (1024 * 1024)} MiB, more than the " +
            "${limit / (1024 * 1024)} MiB one device message can carry, so it was not sent. Ask for less output, " +
            "for example by writing it to a file on the device and fetching that with adb pull."
        return DeviceFrame.Result(result.id, false, JsonNull, message, CommandError("action_failed", message), result.files)
    }

    fun encode(frame: DeviceFrame): String {
        val (type, body) = when (frame) {
            is DeviceFrame.Hello -> "hello" to fields(DeviceFrame.Hello.serializer(), frame)
            is DeviceFrame.SetupProgress -> "setup_progress" to fields(DeviceFrame.SetupProgress.serializer(), frame)
            is DeviceFrame.Result -> "result" to resultFields(frame)
            is DeviceFrame.Pong -> "pong" to fields(DeviceFrame.Pong.serializer(), frame)
            DeviceFrame.Stop -> "stop" to JsonObject(emptyMap())
            DeviceFrame.TakeoverDone -> "takeover_done" to JsonObject(emptyMap())
        }
        return buildJsonObject {
            put("type", JsonPrimitive(type))
            body.forEach { (k, v) -> put(k, v) }
        }.toString()
    }

    /** `result` always carries `output`, `text`, `error` and `files`, as the Rust side writes it. */
    private fun resultFields(frame: DeviceFrame.Result): JsonObject {
        val base = fields(DeviceFrame.Result.serializer(), frame)
        return JsonObject(
            base + mapOf(
                "text" to (frame.text?.let { JsonPrimitive(it) } ?: JsonNull),
                "error" to (frame.error?.let { ExtendJson.encodeToJsonElement(CommandError.serializer(), it) } ?: JsonNull),
            ),
        )
    }

    private fun <T> fields(serializer: KSerializer<T>, value: T): JsonObject =
        ExtendJson.encodeToJsonElement(serializer, value).jsonObject

    fun decodeService(text: String): ServiceFrame {
        val obj = parseObject(text) ?: return ServiceFrame.Unknown(null, text, "not a JSON object")
        val type = (obj["type"] as? JsonPrimitive)?.contentOrNull
        return try {
            when (type) {
                "command" -> ExtendJson.decodeFromJsonElement(ServiceFrame.Command.serializer(), obj)
                "cancel" -> ExtendJson.decodeFromJsonElement(ServiceFrame.Cancel.serializer(), obj)
                "session_started" -> ExtendJson.decodeFromJsonElement(ServiceFrame.SessionStarted.serializer(), obj)
                "session_ended" -> ExtendJson.decodeFromJsonElement(ServiceFrame.SessionEnded.serializer(), obj)
                "takeover" -> ExtendJson.decodeFromJsonElement(ServiceFrame.Takeover.serializer(), obj)
                "takeover_ended" -> ExtendJson.decodeFromJsonElement(ServiceFrame.TakeoverEnded.serializer(), obj)
                "refresh" -> ServiceFrame.Refresh
                "environment" -> ExtendJson.decodeFromJsonElement(ServiceFrame.Environment.serializer(), obj)
                "unpaired" -> ExtendJson.decodeFromJsonElement(ServiceFrame.Unpaired.serializer(), obj)
                "superseded" -> ServiceFrame.Superseded
                "ping" -> ExtendJson.decodeFromJsonElement(ServiceFrame.Ping.serializer(), obj)
                else -> ServiceFrame.Unknown(type, text)
            }
        } catch (e: Exception) {
            ServiceFrame.Unknown(type, text, e.message)
        }
    }

    fun decodeEnrollment(text: String): EnrollmentFrame {
        val obj = parseObject(text) ?: return EnrollmentFrame.Unknown(null, text, "not a JSON object")
        val type = (obj["type"] as? JsonPrimitive)?.contentOrNull
        return try {
            when (type) {
                "code" -> ExtendJson.decodeFromJsonElement(EnrollmentFrame.Code.serializer(), obj)
                "paired" -> ExtendJson.decodeFromJsonElement(EnrollmentFrame.Paired.serializer(), obj)
                "ping" -> ExtendJson.decodeFromJsonElement(EnrollmentFrame.Ping.serializer(), obj)
                else -> EnrollmentFrame.Unknown(type, text)
            }
        } catch (e: Exception) {
            EnrollmentFrame.Unknown(type, text, e.message)
        }
    }

    /** Decodes the `data` of `GET /api/v1/enrollments/{id}`. */
    fun decodeEnrollmentState(data: JsonObject): EnrollmentState? {
        val state = (data["state"] as? JsonPrimitive)?.contentOrNull
        return when (state) {
            "waiting" -> {
                val code = ExtendJson.decodeFromJsonElement(EnrollmentFrame.Code.serializer(), data)
                EnrollmentState.Waiting(code.pairingCode, code.codeExpiresAt)
            }
            "paired" -> {
                val p = ExtendJson.decodeFromJsonElement(EnrollmentFrame.Paired.serializer(), data)
                EnrollmentState.Paired(p.deviceId, p.deviceCredential, p.environment)
            }
            else -> null
        }
    }

    /**
     * Parses [text] as a JSON object. If the object is an envelope (`{"type": …, "data": {…}}`),
     * the fields of `data` are lifted next to `type` so both wire shapes decode the same way.
     */
    fun parseObject(text: String): JsonObject? {
        val element = try {
            ExtendJson.parseToJsonElement(text)
        } catch (_: Exception) {
            return null
        }
        val obj = element as? JsonObject ?: return null
        val data = obj["data"] as? JsonObject ?: return obj
        return JsonObject(data + obj.filterKeys { it != "data" })
    }
}

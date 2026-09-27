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

/**
 * `GET /api/v1/device`: this pair of the device, as its Carbon set it up. Each Carbon who paired the
 * device has their own pair, with its own id, name and credential.
 */
@Serializable
data class DeviceSelf(
    val deviceId: String,
    val name: String,
    val owner: Member,
    /** The Team the pair was made in. Devices belong to Carbons, so the app never shows it. */
    val team: String = "",
    val os: String = "",
    /** Set only while the device's session runs through this pair. */
    val inUse: InUse? = null,
    val takeover: TakeoverInfo? = null,
    val setup: Setup? = null,
    val environment: TestingEnvironment? = null,
    /** 1.1: the physical device this pair is of; every pair of this device shares it. */
    val instanceId: String? = null,
    /** 1.1: true for the pair made by the app's first enrollment, false for "Pair with another Carbon". */
    val firstPair: Boolean? = null,
)

@Serializable
data class EnrollmentCreate(
    val os: String,
    val osVersion: String? = null,
    val model: String? = null,
    val appVersion: String,
    /** The device engine's version; this app runs no engine, so it is always left out. */
    val engineVersion: String? = null,
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
        /**
         * 1.1: the session's side tag. While it runs, every wake request with another side (or none)
         * is shown without its Silicon and reason, before any command of the session runs.
         */
        val side: String? = null,
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

    /**
     * 1.1: a Silicon asks this device's Carbon to wake it. A frame without [siliconId] and [reason]
     * replaces what the app holds for [wakeId]: the device then names no Silicon.
     */
    @Serializable
    data class WakeRequest(
        val target: String? = null,
        val wakeId: String,
        val siliconId: String? = null,
        val reason: String? = null,
        val side: String? = null,
        /** Sound or vibrate for this one (the service allows it at most every 15 minutes). */
        val alert: Boolean = false,
        val createdAt: String,
        val expiresAt: String,
    ) : ServiceFrame

    /** 1.1: forget a wake request, whatever the reason (`woken`, `expired`, `withdrawn`, `declined`, or newer). */
    @Serializable
    data class WakeRequestEnded(val target: String? = null, val wakeId: String, val reason: String = "") : ServiceFrame

    /** 1.1: run the named failed setup step (every failed step when [step] is null) again now. */
    @Serializable
    data class SetupRetry(val target: String? = null, val step: String? = null) : ServiceFrame

    /**
     * A frame this app doesn't handle or can't parse: `attach` and `setup_code` are for host
     * computers, and `credential` (1.1) rotates only computers' credentials. An Android pair's
     * credential stays sealed in this app's Keystore, which Android debugging can't read, so the
     * service never rotates it.
     */
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
        /** The device engine's version; this app runs no engine, so it is always left out. */
        val engineVersion: String? = null,
        val capabilities: List<String>,
        val missing: List<MissingCapability> = emptyList(),
        val setup: Setup,
        /** 1.1: what this app does beyond 1.0 ([Frames.FEATURES]); left out when empty. */
        val features: List<String> = emptyList(),
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

    /**
     * 1.1: whether this device is awake. Sent after every hello and on every change, on each pair's
     * connection. [run] is random for each app process and [seq] grows across all its connections,
     * so the service can drop a stale frame from a sibling connection.
     */
    @Serializable
    data class Awake(
        val awake: Boolean,
        /** `screen_off`, `locked` or `standby`; left out when awake. */
        val sleepState: String? = null,
        /** true: an unlock or real input came with this change; left out: the app can't tell. */
        val inputSeen: Boolean? = null,
        val run: String? = null,
        val seq: Long? = null,
    ) : DeviceFrame

    /** 1.1: whether the device could show a wake request (once per [wakeId]); [note] says why not. */
    @Serializable
    data class WakeRequestShown(val wakeId: String, val shown: Boolean, val note: String? = null) : DeviceFrame
}

object Frames {
    /** `hello.features` (lib.rs `feature`): this app reruns failed setup steps on `setup_retry`. */
    const val FEATURE_SETUP_RETRY = "setup_retry"
    val FEATURES: List<String> = listOf(FEATURE_SETUP_RETRY)

    /** The longest `wake_request_shown.note` the service keeps (lib.rs `WAKE_NOTE_MAX_CHARS`). */
    const val WAKE_NOTE_MAX_CHARS = 300

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
            is DeviceFrame.Hello -> "hello" to fields(DeviceFrame.Hello.serializer(), frame).let { f ->
                // Like the Rust side (skip_serializing_if = "Vec::is_empty"): no features, no field.
                if (frame.features.isEmpty()) JsonObject(f - "features") else f
            }
            is DeviceFrame.SetupProgress -> "setup_progress" to fields(DeviceFrame.SetupProgress.serializer(), frame)
            is DeviceFrame.Result -> "result" to resultFields(frame)
            is DeviceFrame.Pong -> "pong" to fields(DeviceFrame.Pong.serializer(), frame)
            is DeviceFrame.Awake -> "awake" to fields(DeviceFrame.Awake.serializer(), frame)
            is DeviceFrame.WakeRequestShown -> "wake_request_shown" to fields(
                DeviceFrame.WakeRequestShown.serializer(),
                frame.copy(note = frame.note?.take(WAKE_NOTE_MAX_CHARS)),
            )
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
                "wake_request" -> ExtendJson.decodeFromJsonElement(ServiceFrame.WakeRequest.serializer(), obj)
                "wake_request_ended" -> ExtendJson.decodeFromJsonElement(ServiceFrame.WakeRequestEnded.serializer(), obj)
                "setup_retry" -> ExtendJson.decodeFromJsonElement(ServiceFrame.SetupRetry.serializer(), obj)
                // Never kept or logged whole: it would carry a credential (computers only).
                "credential" -> ServiceFrame.Unknown(type, "{\"type\":\"credential\"}")
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

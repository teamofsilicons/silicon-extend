package com.teamofsilicons.extend

import com.teamofsilicons.extend.protocol.ExtendJson
import com.teamofsilicons.extend.protocol.CommandError
import com.teamofsilicons.extend.protocol.DeviceFrame
import com.teamofsilicons.extend.protocol.EnrollmentCreated
import com.teamofsilicons.extend.protocol.EnrollmentFrame
import com.teamofsilicons.extend.protocol.EnrollmentState
import com.teamofsilicons.extend.protocol.Frames
import com.teamofsilicons.extend.protocol.MissingCapability
import com.teamofsilicons.extend.protocol.ProducedFile
import com.teamofsilicons.extend.protocol.ServiceFrame
import com.teamofsilicons.extend.protocol.Setup
import com.teamofsilicons.extend.protocol.SetupStep
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Every frame example in docs/device-protocol.md, decoded and encoded. */
class FramesTest {
    private fun json(s: String) = Json.parseToJsonElement(s).jsonObject

    @Test
    fun enrollmentResponse() {
        val body = """{"type":"enrollment","data":{
          "enrollment_id":"0192abcd","enrollment_secret":"ees_x","pairing_code":"4F9C2A",
          "code_expires_at":"2026-09-26T10:05:00.000Z","rotates_every_s":300}}"""
        val data = Frames.parseObject(body)!!
        val e = ExtendJson.decodeFromJsonElement(EnrollmentCreated.serializer(), data)
        assertEquals("0192abcd", e.enrollmentId)
        assertEquals("ees_x", e.enrollmentSecret)
        assertEquals("4F9C2A", e.pairingCode)
        assertEquals("2026-09-26T10:05:00.000Z", e.codeExpiresAt)
        assertEquals(300L, e.rotatesEveryS)
    }

    @Test
    fun enrollmentFrames() {
        val code = Frames.decodeEnrollment("""{"type":"code","pairing_code":"7B21E0","code_expires_at":"2026-09-26T10:10:00.000Z"}""")
        assertEquals(EnrollmentFrame.Code("7B21E0", "2026-09-26T10:10:00.000Z"), code)
        val paired = Frames.decodeEnrollment("""{"type":"paired","device_id":"7c1e09ab","device_credential":"edc_abc","environment":null}""")
        assertEquals(EnrollmentFrame.Paired("7c1e09ab", "edc_abc", null), paired)
        assertEquals(EnrollmentFrame.Ping(17), Frames.decodeEnrollment("""{"type":"ping","nonce":17}"""))
        assertEquals("""{"type":"pong","nonce":17}""", Frames.encode(DeviceFrame.Pong(17)))
    }

    @Test
    fun enrollmentFramesInDataEnvelope() {
        // api.yaml describes socket frames as {"type","data"}; both shapes decode the same.
        val code = Frames.decodeEnrollment("""{"type":"code","data":{"pairing_code":"7B21E0","code_expires_at":"2026-09-26T10:10:00.000Z"}}""")
        assertEquals(EnrollmentFrame.Code("7B21E0", "2026-09-26T10:10:00.000Z"), code)
        val env = """{"environment_id":"9b3e","name":"checkout-e2e","state":"ready","paired_devices":1,"device_limit":5}"""
        val paired = Frames.decodeEnrollment("""{"type":"paired","data":{"device_id":"7c1e09ab","device_credential":"edc_abc","environment":$env}}""")
        paired as EnrollmentFrame.Paired
        assertEquals("checkout-e2e", paired.environment!!.name)
    }

    @Test
    fun enrollmentPollStates() {
        val waiting = Frames.decodeEnrollmentState(json("""{"state":"waiting","pairing_code":"4F9C2A","code_expires_at":"2026-09-26T10:05:00.000Z"}"""))
        assertEquals(EnrollmentState.Waiting("4F9C2A", "2026-09-26T10:05:00.000Z"), waiting)
        val paired = Frames.decodeEnrollmentState(json("""{"state":"paired","device_id":"7c1e09ab","device_credential":"edc_1"}"""))
        assertEquals(EnrollmentState.Paired("7c1e09ab", "edc_1", null), paired)
    }

    @Test
    fun serviceFramesFromTheDoc() {
        val cmd = Frames.decodeService(
            """{"type":"command","id":"0192-cmd","session_id":"a3f","target":null,"command":"click","args":["@e2"],
               "attachments":[],"timeout_ms":30000,"upload_ids":["u1","u2"]}""",
        )
        assertEquals(ServiceFrame.Command("0192-cmd", "a3f", null, "click", listOf("@e2"), emptyList(), 30000, listOf("u1", "u2")), cmd)
        assertEquals(ServiceFrame.Cancel("0192-cmd"), Frames.decodeService("""{"type":"cancel","id":"0192-cmd"}"""))
        assertEquals(
            ServiceFrame.SessionStarted(null, "a3f", "si:chef", "2026-09-26T10:00:00.000Z"),
            Frames.decodeService("""{"type":"session_started","target":null,"session_id":"a3f","silicon_id":"si:chef","since":"2026-09-26T10:00:00.000Z"}"""),
        )
        assertEquals(
            ServiceFrame.SessionEnded(null, "a3f", "idle_timeout"),
            Frames.decodeService("""{"type":"session_ended","target":null,"session_id":"a3f","reason":"idle_timeout"}"""),
        )
        assertEquals(
            ServiceFrame.Takeover(null, "a3f", "Please approve Face ID", "2026-09-26T10:30:00.000Z"),
            Frames.decodeService("""{"type":"takeover","target":null,"session_id":"a3f","reason":"Please approve Face ID","expires_at":"2026-09-26T10:30:00.000Z"}"""),
        )
        assertEquals(ServiceFrame.TakeoverEnded(null, "a3f"), Frames.decodeService("""{"type":"takeover_ended","target":null,"session_id":"a3f"}"""))
        assertEquals(ServiceFrame.Refresh, Frames.decodeService("""{"type":"refresh"}"""))
        val env = Frames.decodeService("""{"type":"environment","environment":{"environment_id":"e1","name":"checkout-e2e","state":"ready","paired_devices":1,"device_limit":5}}""")
        env as ServiceFrame.Environment
        assertEquals("checkout-e2e", env.environment!!.name)
        assertEquals(5L, env.environment!!.deviceLimit)
        assertNull((Frames.decodeService("""{"type":"environment","environment":null}""") as ServiceFrame.Environment).environment)
        assertEquals(ServiceFrame.Unpaired("device_removed"), Frames.decodeService("""{"type":"unpaired","reason":"device_removed"}"""))
        assertEquals(ServiceFrame.Superseded, Frames.decodeService("""{"type":"superseded"}"""))
        assertEquals(ServiceFrame.Ping(42), Frames.decodeService("""{"type":"ping","nonce":42}"""))
    }

    @Test
    fun commandWithAttachmentsAndDefaults() {
        val cmd = Frames.decodeService(
            """{"type":"command","id":"c","session_id":"a3f","command":"display","args":["show","--image","attachment:cat.png"],
               "attachments":[{"name":"cat.png","content_type":"image/png","content_base64":"iVBORw0KGgo="}],"timeout_ms":5000,"upload_ids":[]}""",
        ) as ServiceFrame.Command
        assertEquals("cat.png", cmd.attachments.single().name)
        assertEquals("image/png", cmd.attachments.single().contentType)
        assertEquals("iVBORw0KGgo=", cmd.attachments.single().contentBase64)
        assertNull(cmd.target)
    }

    @Test
    fun unknownAndBrokenFramesDontThrow() {
        val attach = Frames.decodeService("""{"type":"attach","device_id":"x","os":"tvos","name":"Living room"}""")
        assertTrue(attach is ServiceFrame.Unknown && attach.type == "attach")
        val broken = Frames.decodeService("""{"type":"command","id":"x"}""")
        assertTrue(broken is ServiceFrame.Unknown && broken.problem != null)
        assertTrue(Frames.decodeService("not json") is ServiceFrame.Unknown)
        assertTrue(Frames.decodeService("[1,2]") is ServiceFrame.Unknown)
        // Unknown fields are ignored.
        assertEquals(ServiceFrame.Ping(1), Frames.decodeService("""{"type":"ping","nonce":1,"extra":{"a":1}}"""))
    }

    @Test
    fun helloMatchesTheDocShape() {
        val hello = DeviceFrame.Hello(
            appVersion = "1.0.0", os = "android", osVersion = "15", model = "Pixel 9", agentDeviceVersion = null,
            capabilities = listOf("screen.read", "screen.capture", "input.touch"),
            missing = listOf(MissingCapability("adb", "Wireless debugging is off. Turn it on in Developer options.")),
            setup = Setup(
                "needs_carbon",
                listOf(
                    SetupStep("accessibility", "Allow Silicon Extend to control the screen", "done"),
                    SetupStep("wireless_debugging", "Turn on wireless debugging", "needs_carbon", help = "Settings › System › Developer options › Wireless debugging"),
                ),
            ),
        )
        val encoded = Frames.encode(hello)
        assertTrue(encoded, encoded.startsWith("{\"type\":\"hello\""))
        val o = json(encoded)
        assertEquals("1.0.0", o["app_version"]!!.jsonPrimitive.content)
        assertEquals("android", o["os"]!!.jsonPrimitive.content)
        assertEquals("15", o["os_version"]!!.jsonPrimitive.content)
        assertEquals("Pixel 9", o["model"]!!.jsonPrimitive.content)
        assertEquals(3, o["capabilities"]!!.jsonArray.size)
        assertEquals("adb", o["missing"]!!.jsonArray[0].jsonObject["capability"]!!.jsonPrimitive.content)
        val setup = o["setup"]!!.jsonObject
        assertEquals("needs_carbon", setup["state"]!!.jsonPrimitive.content)
        val steps = setup["steps"]!!.jsonArray
        assertEquals("accessibility", steps[0].jsonObject["key"]!!.jsonPrimitive.content)
        assertTrue("absent help is omitted like serde's skip_serializing_if", "help" !in steps[0].jsonObject)
        assertEquals("Settings › System › Developer options › Wireless debugging", steps[1].jsonObject["help"]!!.jsonPrimitive.content)
    }

    @Test
    fun resultMatchesTheDocShape() {
        val ok = DeviceFrame.Result(
            id = "cmd-1", ok = true, output = buildJsonObject { put("x", 1) }, text = "Tapped @e2 \"Continue\"",
            files = listOf(ProducedFile("u1", "screenshot.png", "image/png", "screenshot", 184223)),
        )
        val o = json(Frames.encode(ok))
        assertEquals("result", o["type"]!!.jsonPrimitive.content)
        assertEquals("cmd-1", o["id"]!!.jsonPrimitive.content)
        assertEquals(JsonPrimitive(true), o["ok"])
        assertEquals(JsonNull, o["error"])
        assertEquals("Tapped @e2 \"Continue\"", o["text"]!!.jsonPrimitive.content)
        val f = o["files"]!!.jsonArray[0].jsonObject
        assertEquals("u1", f["upload_id"]!!.jsonPrimitive.content)
        assertEquals("image/png", f["content_type"]!!.jsonPrimitive.content)
        assertEquals("screenshot", f["kind"]!!.jsonPrimitive.content)
        assertEquals(184223L, f["size_bytes"]!!.jsonPrimitive.content.toLong())

        val failed = DeviceFrame.Result("cmd-2", false, JsonNull, "nope", CommandError("unsupported_on_device", "nope"))
        val e = json(Frames.encode(failed))
        assertEquals(JsonPrimitive(false), e["ok"])
        assertEquals("unsupported_on_device", e["error"]!!.jsonObject["code"]!!.jsonPrimitive.content)
        assertEquals("nope", e["error"]!!.jsonObject["message"]!!.jsonPrimitive.content)
        assertEquals(JsonNull, e["output"])
        assertEquals(0, e["files"]!!.jsonArray.size)
    }

    @Test
    fun bareFrames() {
        assertEquals("""{"type":"stop"}""", Frames.encode(DeviceFrame.Stop))
        assertEquals("""{"type":"takeover_done"}""", Frames.encode(DeviceFrame.TakeoverDone))
        val sp = json(Frames.encode(DeviceFrame.SetupProgress(Setup("complete", emptyList()))))
        assertEquals("setup_progress", sp["type"]!!.jsonPrimitive.content)
        assertEquals("complete", (sp["setup"] as JsonObject)["state"]!!.jsonPrimitive.content)
    }

    @Test
    fun errorEnvelopeIsLifted() {
        val o = Frames.parseObject("""{"type":"error","data":{"code":"enrollment_gone","message":"Gone"}}""")!!
        assertEquals("enrollment_gone", o["code"]!!.jsonPrimitive.content)
        assertEquals("error", o["type"]!!.jsonPrimitive.content)
    }
}

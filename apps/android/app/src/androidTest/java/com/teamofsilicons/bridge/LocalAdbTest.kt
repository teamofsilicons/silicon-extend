package com.teamofsilicons.bridge

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.teamofsilicons.bridge.adb.AdbCommand
import com.teamofsilicons.bridge.adb.AdbExecutor
import com.teamofsilicons.bridge.adb.AdbWire
import com.teamofsilicons.bridge.adb.LocalAdb
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.UUID

/** Run on the dedicated test emulator after `adb tcpip 5555`; never targets an external device. */
@RunWith(AndroidJUnit4::class)
class LocalAdbTest {
    @Test fun wirelessPairing() = runBlocking {
        val args = InstrumentationRegistry.getArguments()
        org.junit.Assume.assumeTrue("Pass the current Android pairing port and code to run TLS pairing", args.containsKey("adb_pairing_port"))
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val adb = Bridge.get(context).adb
        adb.pair(args.getString("adb_pairing_port")!!.toInt(), args.getString("adb_pairing_code")!!)
        val connected = adb.connect(args.getString("adb_connect_port")?.toInt() ?: 0)
        assertTrue(adb.lastError, connected)
        try { assertEquals("2000", adb.shell("id -u").text.trim()) }
        finally { adb.disconnect() }
    }
    @Test fun wirelessReconnect() = runBlocking {
        org.junit.Assume.assumeTrue(InstrumentationRegistry.getArguments().getString("adb_tls") == "true")
        val adb = Bridge.get(InstrumentationRegistry.getInstrumentation().targetContext).adb
        val connected = adb.connect()
        assertTrue(adb.lastError, connected)
        try {
            repeat(100) { assertEquals("2000", adb.shell("id -u").text.trim()) }
            assertTrue("Debugging must stay enabled across app restart", adb.enabled)
        }
        finally { if (InstrumentationRegistry.getArguments().getString("keep_connected") != "true") adb.disconnect() }
    }
    @Test fun realLocalDaemon() = runBlocking { withTimeout(60_000) {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val adb = Bridge.get(context).adb
        adb.disconnect()
        val connected = adb.connect(5555)
        assertTrue(adb.lastError, connected)
        val dir = File(context.cacheDir, "adb-test-${UUID.randomUUID()}").apply { mkdirs() }
        val executor = AdbExecutor(adb, dir)
        val remote = "/data/local/tmp/bridge-test-${UUID.randomUUID()}"
        try {
            repeat(40) { assertEquals("2000", adb.shell("id -u").text.trim()) }
            val apk = File(dir, "fixture.apk")
            InstrumentationRegistry.getInstrumentation().context.assets.open("fixture.apk").use { input -> apk.outputStream().use { input.copyTo(it) } }
            android.util.Log.i("BridgeAdbTest", "install")
            val installed = executor.execute("s1", AdbCommand.Install("com.teamofsilicons.bridge.adbfixture", apk.absolutePath, true), listOf(apk))
            assertTrue(installed.text.contains("Success"))
            assertTrue(adb.shell("pm path com.teamofsilicons.bridge.adbfixture").text.contains("package:"))
            executor.execute("s1", AdbCommand.Raw(listOf("uninstall", "com.teamofsilicons.bridge.adbfixture")), emptyList())
            val failure = adb.shell("printf fail >&2; exit 7", check = false)
            assertEquals(7, failure.exitCode)
            assertEquals("fail", failure.stderr.toString(Charsets.UTF_8))
            val input = File(dir, "binary.dat").apply { writeBytes(ByteArray(140000) { (it % 256).toByte() }) }
            adb.push(input, remote)
            val copy = File(dir, "copy.dat")
            adb.pull(remote, copy)
            assertArrayEquals(input.readBytes(), copy.readBytes())
            android.util.Log.i("BridgeAdbTest", "cancellation")
            val elapsed = System.nanoTime()
            assertNull(withTimeoutOrNull(250) { adb.shell("sleep 30") })
            assertTrue("A cancelled stream must unblock the command queue", (System.nanoTime() - elapsed) / 1_000_000 < 2500)
            assertEquals("alive", adb.shell("printf alive").text)
            android.util.Log.i("BridgeAdbTest", "logs")
            executor.execute("s1", AdbCommand.Logs("start"), emptyList())
            executor.execute("s1", AdbCommand.Logs("mark", "bridge-marker-123"), emptyList())
            delay(500)
            val logs = executor.execute("s1", AdbCommand.Logs("stop"), emptyList()).artifact!!
            assertTrue(logs.file.readText().contains("bridge-marker-123"))
            android.util.Log.i("BridgeAdbTest", "recording")
            executor.execute("s1", AdbCommand.Record("start", "test-video"), emptyList())
            delay(2000)
            val recording = withTimeout(15000) { executor.execute("s1", AdbCommand.Record("stop"), emptyList()).artifact!! }
            assertTrue("MP4 must contain real frames", recording.file.length() > 1000)
            assertEquals("ftyp", recording.file.inputStream().use { it.readNBytes(8).copyOfRange(4, 8).toString(Charsets.US_ASCII) })
            executor.execute("s2", AdbCommand.Logs("start"), emptyList())
            executor.execute("s2", AdbCommand.Record("start", "revoked"), emptyList())
            executor.endSession("s2")
            executor.execute("s3", AdbCommand.Record("start", "disconnect"), emptyList())
            adb.disconnect()
            delay(500)
            assertTrue(adb.connect(5555))
            val live = adb.shell("ps -A -o ARGS | grep '^screenrecord.*silicon-bridge-'", check = false)
            assertEquals("Recorders must stop when their ADB transport closes", "", live.text.trim())
            executor.endSession("s3")
            assertEquals("", adb.shell("find /data/local/tmp -maxdepth 1 -name 'silicon-bridge-*'").text.trim())
        } finally {
            executor.endAll()
            adb.shell("pm uninstall com.teamofsilicons.bridge.adbfixture", check = false)
            adb.shell("rm -f ${AdbWire.quote(remote)}")
            adb.disconnect()
            dir.deleteRecursively()
        }
    } }
}

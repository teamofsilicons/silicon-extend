package com.teamofsilicons.extend

import android.os.Build
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.teamofsilicons.extend.adb.AdbCommand
import com.teamofsilicons.extend.adb.AdbExecutor
import com.teamofsilicons.extend.adb.AdbWire
import com.teamofsilicons.extend.adb.LocalAdb
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.io.DataInputStream
import java.io.File
import java.net.InetAddress
import java.net.ServerSocket
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.UUID
import kotlin.concurrent.thread

/**
 * Android debugging on the dedicated test emulator only. Every test that touches the real daemon
 * or the app's saved debugging state needs emulator hardware and an explicit opt-in argument, so
 * running the suite on a physical device never changes its Android debugging.
 */
@RunWith(AndroidJUnit4::class)
class LocalAdbTest {
    private val args get() = InstrumentationRegistry.getArguments()
    private val context get() = InstrumentationRegistry.getInstrumentation().targetContext
    private fun emulatorOnly() = assumeTrue("Runs only on a dedicated emulator", Build.HARDWARE in setOf("ranchu", "goldfish"))

    @Test fun connectLocalForService(): Unit = runBlocking {
        assumeTrue(args.getString("service_test") == "true")
        emulatorOnly()
        val adb = Extend.get(context).adb
        // Initial setup must allow time to answer Android's RSA authorization dialog.
        val connected = adb.connect(5555, startedByCarbon = true)
        assertTrue(adb.lastError, connected)
        assertEquals("2000", adb.shell("id -u").text.trim())
        // Keep the explicit emulator connection enabled for the separately running service lane.
    }

    /** TLS lane: `-e adb_pairing_port <port> -e adb_pairing_code <code>` from Wireless debugging's pairing dialog. */
    @Test fun wirelessPairing(): Unit = runBlocking {
        assumeTrue("Pass the current Android pairing port and code to run TLS pairing", args.containsKey("adb_pairing_port"))
        val adb = Extend.get(context).adb
        adb.pair(args.getString("adb_pairing_port")!!.toInt(), args.getString("adb_pairing_code")!!)
        assertTrue("Pairing with a code switches to TLS-only connections", adb.pairedWithCode)
        assertNotNull("The paired device's GUID is kept for discovery",
            context.getSharedPreferences("extend_adb", 0).getString("guid", null))
        val connected = adb.connect(args.getString("adb_connect_port")?.toInt() ?: 0)
        assertTrue(adb.lastError, connected)
        assertEquals("2000", adb.shell("id -u").text.trim())
        if (args.getString("keep_connected") != "true") adb.disconnect()
    }

    /** `-e adb_tls true`: discovery (paired GUID only), TLS and the shell proof in a new app process. */
    @Test fun wirelessReconnect(): Unit = runBlocking {
        assumeTrue(args.getString("adb_tls") == "true")
        val adb = Extend.get(context).adb
        val connected = adb.connect()
        assertTrue(adb.lastError, connected)
        try {
            repeat(100) { assertEquals("2000", adb.shell("id -u").text.trim()) }
            assertTrue("Debugging must stay enabled across app restart", adb.enabled)
        }
        finally { if (args.getString("keep_connected") != "true") adb.disconnect() }
    }

    /**
     * `-e adb_tls true`: a decoy advertising this device's Wireless debugging name on a port that
     * isn't Android's (plain text) is one more candidate, and connecting still reaches the real one.
     * Discovery used to take whichever advertisement resolved first.
     */
    @Test fun discoveryTriesEveryCandidate(): Unit = runBlocking {
        assumeTrue(args.getString("adb_tls") == "true")
        emulatorOnly()
        val guid = context.getSharedPreferences("extend_adb", 0).getString("guid", null)
        assumeTrue("Pair Android debugging with a code first (tools/wireless-debugging-lane.py)", guid != null)
        val decoy = ServerSocket(0, 50)
        val knocks = java.util.concurrent.atomic.AtomicInteger()
        thread(isDaemon = true) {
            while (!decoy.isClosed) {
                runCatching { decoy.accept().use { knocks.incrementAndGet(); it.getOutputStream().write("not adbd\n".toByteArray()) } }
            }
        }
        val nsd = context.getSystemService(android.net.nsd.NsdManager::class.java)
        val registered = kotlinx.coroutines.CompletableDeferred<String>()
        val listener = object : android.net.nsd.NsdManager.RegistrationListener {
            override fun onServiceRegistered(info: android.net.nsd.NsdServiceInfo) { registered.complete(info.serviceName) }
            override fun onRegistrationFailed(info: android.net.nsd.NsdServiceInfo, code: Int) { registered.completeExceptionally(AssertionError("NSD registration failed: $code")) }
            override fun onServiceUnregistered(info: android.net.nsd.NsdServiceInfo) = Unit
            override fun onUnregistrationFailed(info: android.net.nsd.NsdServiceInfo, code: Int) = Unit
        }
        val info = android.net.nsd.NsdServiceInfo().apply {
            serviceName = if (guid!!.startsWith("adb-")) guid else "adb-$guid"
            serviceType = "_adb-tls-connect._tcp"
            port = decoy.localPort
        }
        nsd.registerService(info, android.net.nsd.NsdManager.PROTOCOL_DNS_SD, listener)
        try {
            val name = withTimeout(10_000) { registered.await() }
            assertTrue("the decoy carries the paired device's name: $name", com.teamofsilicons.extend.adb.AdbDiscovery.matches(name, guid))
            val ports = com.teamofsilicons.extend.adb.AdbDiscovery.ports(context, guid)
            assertTrue("the decoy is a candidate: $ports", decoy.localPort in ports)
            assertTrue("and so is Android's own service: $ports", ports.size >= 2)
            // A separate instance, so the running app's connection is left alone.
            val adb = LocalAdb(context)
            assertTrue(adb.lastError, adb.connect())
            assertEquals("2000", adb.shell("id -u").text.trim())
            android.util.Log.i("SiliconExtend", "discoveryTriesEveryCandidate: candidates $ports, decoy knocked ${knocks.get()} time(s)")
        } finally {
            runCatching { nsd.unregisterService(listener) }
            decoy.close()
        }
    }

    /** A local app posing as adbd (plain text, answers every command) must not be trusted. */
    @Test fun anImpostorDaemonIsRefused(): Unit = runBlocking {
        emulatorOnly()
        val prefs = context.getSharedPreferences("extend_adb", 0)
        val savedMode = prefs.getString("mode", null)
        try {
            // Not paired with a code (a TV's legacy port): the shell proof refuses it.
            prefs.edit().remove("mode").commit()
            FakeDaemon().use { fake ->
                // A separate instance: a failed connect never touches the app's saved state.
                val adb = LocalAdb(context)
                assertFalse(adb.connect(fake.port))
                assertFalse(adb.connected)
                assertTrue(adb.lastError, adb.lastError!!.contains("did not prove"))
                assertTrue("The impostor saw the proof command", fake.opened.any { it.contains("am") && it.contains("broadcast") })
            }
            // Paired with a code: a plain-text answer is refused before anything is sent or signed.
            prefs.edit().putString("mode", "tls").commit()
            FakeDaemon().use { fake ->
                val adb = LocalAdb(context)
                assertFalse(adb.connect(fake.port))
                assertTrue(adb.lastError, adb.lastError!!.contains("without starting TLS"))
                assertTrue("Nothing may be opened on an untrusted peer", fake.opened.isEmpty())
            }
        } finally {
            prefs.edit().apply { if (savedMode == null) remove("mode") else putString("mode", savedMode) }.commit()
        }
    }

    /** Reading the connection state never waits for a connect in progress (it holds libadb's lock for up to 8 s). */
    @Test fun connectionStateNeverBlocks(): Unit = runBlocking {
        emulatorOnly()
        // Accepts the TCP connection and never answers CNXN, so connect() waits its whole timeout.
        val silent = ServerSocket(0, 1, InetAddress.getByName("127.0.0.1"))
        val accepted = thread { runCatching { silent.accept().use { Thread.sleep(15_000) } } }
        try {
            val manager = TestManager()
            val connecting = async(Dispatchers.IO) { runCatching { manager.connect("127.0.0.1", silent.localPort) } }
            delay(1000)
            assertTrue("The connect must still be in progress", connecting.isActive)
            var started = System.nanoTime()
            assertFalse(manager.isConnected)
            var tookMs = (System.nanoTime() - started) / 1_000_000
            assertTrue("isConnected took $tookMs ms while a connect held the lock", tookMs < 200)
            val adb = LocalAdb(context)
            started = System.nanoTime()
            assertFalse(adb.connected)
            tookMs = (System.nanoTime() - started) / 1_000_000
            assertTrue("connected took $tookMs ms", tookMs < 200)
            withTimeout(20_000) { connecting.await() }
            manager.close()
        } finally { silent.close(); accepted.join(16_000) }
    }

    /** A libadb manager like the app's, with a throwaway identity. */
    private class TestManager : io.github.muntashirakon.adb.AbsAdbConnectionManager() {
        private val pair = java.security.KeyPairGenerator.getInstance("RSA").apply { initialize(2048) }.generateKeyPair()
        private val certificate = run {
            val subject = org.bouncycastle.asn1.x500.X500Name("CN=Extend test")
            val now = System.currentTimeMillis()
            org.bouncycastle.cert.jcajce.JcaX509CertificateConverter().getCertificate(
                org.bouncycastle.cert.jcajce.JcaX509v3CertificateBuilder(subject, java.math.BigInteger.ONE, java.util.Date(now - 60_000),
                    java.util.Date(now + 86_400_000), subject, pair.public)
                    .build(org.bouncycastle.operator.jcajce.JcaContentSignerBuilder("SHA256withRSA").build(pair.private)),
            )
        }
        init { setApi(Build.VERSION.SDK_INT); setTimeout(8, java.util.concurrent.TimeUnit.SECONDS) }
        override fun getPrivateKey(): java.security.PrivateKey = pair.private
        override fun getCertificate(): java.security.cert.Certificate = certificate
        override fun getDeviceName(): String = "Extend test"
    }

    @Test fun realLocalDaemon(): Unit = runBlocking { withTimeout(120_000) {
        emulatorOnly()
        assumeTrue("Pass -e local_daemon true after `adb tcpip 5555`", args.getString("local_daemon") == "true")
        val adb = Extend.get(context).adb
        val wasEnabled = adb.enabled
        adb.disconnect()
        val connected = adb.connect(5555)
        assertTrue(adb.lastError, connected)
        val dir = File(context.cacheDir, "adb-test-${UUID.randomUUID()}").apply { mkdirs() }
        val executor = AdbExecutor(adb, File(dir, "cache"), File(dir, "state"), context.packageName)
        val remote = "/data/local/tmp/extend-test-${UUID.randomUUID()}"
        val fixturePackage = "com.teamofsilicons.extend.adbfixture"
        try {
            repeat(40) { assertEquals("2000", adb.shell("id -u").text.trim()) }
            val apk = File(dir, "fixture.apk")
            InstrumentationRegistry.getInstrumentation().context.assets.open("fixture.apk").use { input -> apk.outputStream().use { input.copyTo(it) } }
            android.util.Log.i("ExtendAdbTest", "install")
            suspend fun firstInstall() = adb.shell("dumpsys package $fixturePackage | grep -m1 firstInstallTime", check = false).text.trim()
            val installed = executor.execute("s1", AdbCommand.Install(fixturePackage, apk.absolutePath, false), listOf(apk))
            assertTrue(installed.text, installed.text.contains("Success"))
            val first = firstInstall()
            delay(1100)
            executor.execute("s1", AdbCommand.Install(fixturePackage, apk.absolutePath, false), listOf(apk))
            assertEquals("install updates in place", first, firstInstall())
            delay(1100)
            val reinstalled = executor.execute("s1", AdbCommand.Install(fixturePackage, apk.absolutePath, true), listOf(apk))
            assertTrue(reinstalled.text, reinstalled.text.contains("Removed the installed copy and its data"))
            assertNotEquals("reinstall removes the app and its data first", first, firstInstall())
            assertTrue(adb.shell("pm path $fixturePackage").text.contains("package:"))
            executor.execute("s1", AdbCommand.Raw(listOf("uninstall", fixturePackage)), emptyList())

            android.util.Log.i("ExtendAdbTest", "shell output")
            val failure = executor.execute("s1", AdbCommand.Raw(listOf("shell", "echo out; echo err >&2; exit 7")), emptyList())
            assertEquals(7, failure.exitCode)
            assertEquals("out\n", failure.output!!["stdout"]!!.jsonPrimitive.content)
            assertEquals("err\n", failure.output!!["stderr"]!!.jsonPrimitive.content)
            val big = executor.execute("s1", AdbCommand.Raw(listOf("shell", "yes a | head -c 5000000")), emptyList())
            assertEquals(0, big.exitCode)
            assertEquals(AdbWire.INLINE_LIMIT, big.output!!["stdout"]!!.jsonPrimitive.content.length)
            assertEquals(5_000_000L, big.artifacts.single().file.length())
            big.artifacts.forEach { it.file.parentFile?.deleteRecursively() }

            android.util.Log.i("ExtendAdbTest", "push/pull")
            val input = File(dir, "binary.dat").apply { writeBytes(ByteArray(140000) { (it % 256).toByte() }) }
            adb.push(input, remote)
            val copy = File(dir, "copy.dat")
            adb.pull(remote, copy)
            assertArrayEquals(input.readBytes(), copy.readBytes())
            android.util.Log.i("ExtendAdbTest", "cancellation")
            val elapsed = System.nanoTime()
            assertNull(withTimeoutOrNull(250) { adb.shell("sleep 30") })
            assertTrue("A cancelled stream must unblock the command queue", (System.nanoTime() - elapsed) / 1_000_000 < 2500)
            assertEquals("alive", adb.shell("printf alive").text)

            android.util.Log.i("ExtendAdbTest", "logs after clear")
            executor.execute("s1", AdbCommand.Logs("clear"), emptyList())
            executor.execute("s1", AdbCommand.Logs("start"), emptyList())
            val marker = "extend-marker-${UUID.randomUUID()}"
            executor.execute("s1", AdbCommand.Logs("mark", marker), emptyList())
            delay(500)
            val logs = executor.execute("s1", AdbCommand.Logs("stop"), emptyList()).artifacts.single()
            val text = logs.file.readText()
            assertTrue("A marker sent after logs start must be captured", text.contains(marker))
            assertTrue("The capture starts with its own marker", text.contains("Extend log capture started"))
            executor.delivered("s1", logs)

            android.util.Log.i("ExtendAdbTest", "recording")
            executor.execute("s1", AdbCommand.Record("start", "test-video"), emptyList())
            delay(2000)
            val recording = withTimeout(15000) { executor.execute("s1", AdbCommand.Record("stop"), emptyList()).artifacts.single() }
            assertTrue("MP4 must contain real frames", recording.file.length() > 1000)
            val header = ByteArray(8)
            DataInputStream(recording.file.inputStream()).use { it.readFully(header) }
            assertEquals("ftyp", header.copyOfRange(4, 8).toString(Charsets.US_ASCII))
            executor.delivered("s1", recording)

            android.util.Log.i("ExtendAdbTest", "session end")
            executor.execute("s2", AdbCommand.Logs("start"), emptyList())
            executor.execute("s2", AdbCommand.Record("start", "revoked"), emptyList())
            executor.execute("s2", AdbCommand.Raw(listOf("shell", "nohup sh -c 'sleep 300' >/dev/null 2>&1 &")), emptyList())
            val tag = AdbWire.quote("${AdbExecutor.SESSION_VARIABLE}=s2")
            val tagged = "grep -lzxF -- $tag /proc/[0-9]*/environ 2>/dev/null | wc -l"
            // The shell returns as soon as it forked; the tag shows in /proc once the child has exec'd.
            val tagSeen = withTimeoutOrNull(5_000) { while (adb.shell(tagged, check = false).text.trim() == "0") delay(100); true }
            assertNotNull("The detached process carries its session's tag", tagSeen)
            executor.endSession("s2")
            assertEquals("Detached processes end with their session", "0", adb.shell(tagged, check = false).text.trim())

            executor.execute("s3", AdbCommand.Record("start", "disconnect"), emptyList())
            adb.disconnect()
            delay(500)
            assertTrue(adb.connect(5555))
            val live = adb.shell("ps -A -o ARGS | grep '^screenrecord.*silicon-extend-'", check = false)
            assertEquals("Recorders must stop when their ADB transport closes", "", live.text.trim())
            executor.endSession("s3")

            android.util.Log.i("ExtendAdbTest", "sweep")
            val orphan = "/data/local/tmp/silicon-extend-${UUID.randomUUID()}"
            val foreign = "/data/local/tmp/silicon-extend-${UUID.randomUUID()}"
            adb.shell("mkdir $orphan $foreign && echo ${context.packageName} >$orphan/owner && echo other.app >$foreign/owner && " +
                "touch $orphan/chunk-0.mp4 $foreign/chunk-0.mp4 && touch -t 202001010000 $orphan/owner $orphan/chunk-0.mp4 $orphan $foreign/owner $foreign/chunk-0.mp4 $foreign")
            executor.recover()
            assertEquals("An orphaned capture directory is removed", "", adb.shell("ls -d $orphan 2>/dev/null", check = false).text.trim())
            assertEquals("Another app's directory is kept", foreign, adb.shell("ls -d $foreign", check = false).text.trim())
            adb.shell("rm -rf $foreign")
        } finally {
            executor.endAll()
            adb.shell("pm uninstall $fixturePackage", check = false)
            adb.shell("rm -f ${AdbWire.quote(remote)}")
            adb.disconnect()
            if (wasEnabled) adb.connect(5555)
            dir.deleteRecursively()
        }
    } }

    /**
     * Flow control: output larger than the app's whole heap streams through to disk and stops at
     * its limit with the limit's hint. libadb used to acknowledge every packet on arrival and queue
     * it, so `head -c 270000000 /dev/zero` or a 250 MB pull crashed the app with OutOfMemoryError.
     */
    @Test fun outputLargerThanTheHeapIsStreamedNotQueued(): Unit = runBlocking { withTimeout(600_000) {
        emulatorOnly()
        assumeTrue("Pass -e local_daemon true after `adb tcpip 5555`", args.getString("local_daemon") == "true")
        val adb = Extend.get(context).adb
        assertTrue(adb.lastError, adb.connect(5555))
        val dir = File(context.cacheDir, "adb-big-${UUID.randomUUID()}").apply { mkdirs() }
        val executor = AdbExecutor(adb, File(dir, "cache"), File(dir, "state"), context.packageName)
        val remote = "/data/local/tmp/extend-big-${UUID.randomUUID()}"
        // Each transfer below is larger than the app's whole heap: queueing it in memory crashes
        // this process with OutOfMemoryError, as the unacknowledged transport did.
        val heap = Runtime.getRuntime().maxMemory()
        try {
            assertTrue("The test needs output larger than the heap ($heap bytes)", heap < 250_000_000L)

            android.util.Log.i("ExtendAdbTest", "shell beyond the limit")
            val tooMuch = runCatching {
                executor.execute("big", AdbCommand.Raw(listOf("shell", "head -c 270000000 /dev/zero")), emptyList())
            }.exceptionOrNull()
            assertTrue(tooMuch.toString(), tooMuch is AdbWire.OutputTooLarge && tooMuch.message!!.contains("more than 256 MiB"))
            assertEquals("The limit's partial output is removed", 0, File(dir, "cache").walk().count { it.isFile })

            android.util.Log.i("ExtendAdbTest", "shell within the limit")
            val big = executor.execute("big", AdbCommand.Raw(listOf("shell", "head -c 250000000 /dev/zero")), emptyList())
            assertEquals(0, big.exitCode)
            assertEquals(250_000_000L, big.artifacts.single().file.length())
            big.artifacts.forEach { it.file.parentFile?.deleteRecursively() }

            android.util.Log.i("ExtendAdbTest", "pull within the limit")
            // Sparse files: the emulator's /data is small.
            adb.shell("truncate -s 250000000 $remote")
            val copy = File(dir, "pulled.bin")
            adb.pull(remote, copy)
            assertEquals(250_000_000L, copy.length())
            copy.delete()

            android.util.Log.i("ExtendAdbTest", "pull beyond the limit")
            adb.shell("truncate -s 300000000 $remote")
            val refused = runCatching { adb.pull(remote, copy) }.exceptionOrNull()
            assertTrue(refused.toString(), refused is AdbWire.LimitExceeded && refused.message!!.contains("256 MiB transfer limit"))
            copy.delete()

            assertEquals("The connection still works", "2000", adb.shell("id -u").text.trim())
        } finally {
            adb.shell("rm -f $remote", check = false)
            dir.deleteRecursively()
        }
    } }

    /** A plain-text ADB peer that acknowledges every stream and answers it with exit status 0. */
    private class FakeDaemon : AutoCloseable {
        private val server = ServerSocket(0, 1, InetAddress.getByName("127.0.0.1"))
        val port get() = server.localPort
        val opened = java.util.concurrent.CopyOnWriteArrayList<String>()
        private val worker = thread {
            runCatching {
                server.accept().use { socket ->
                    val input = DataInputStream(socket.getInputStream())
                    val out = socket.getOutputStream()
                    fun send(command: Int, a0: Int, a1: Int, data: ByteArray?) { out.write(message(command, a0, a1, data)); out.flush() }
                    var next = 100
                    while (true) {
                        val header = ByteArray(24).also { input.readFully(it) }
                        val b = ByteBuffer.wrap(header).order(ByteOrder.LITTLE_ENDIAN)
                        val command = b.int; val arg0 = b.int; val arg1 = b.int; val length = b.int
                        val payload = ByteArray(length).also { input.readFully(it) }
                        when (command) {
                            CNXN -> send(CNXN, 0x01000000, 4096, "device::\u0000".toByteArray())
                            OPEN -> {
                                opened += String(payload).trimEnd('\u0000')
                                val remote = next++
                                send(OKAY, remote, arg0, null)
                                // shell v2: stdout frame, then exit status 0
                                val stdout = "Broadcast completed: result=0\n".toByteArray()
                                val frames = byteArrayOf(1) + AdbWire.intBytes(stdout.size) + stdout + byteArrayOf(3) + AdbWire.intBytes(1) + byteArrayOf(0)
                                send(WRTE, remote, arg0, frames)
                                send(CLSE, remote, arg0, null)
                            }
                            WRTE -> send(OKAY, arg1, arg0, null)
                        }
                    }
                }
            }
        }
        override fun close() { server.close(); worker.join(1000) }

        companion object {
            const val CNXN = 0x4e584e43
            const val OPEN = 0x4e45504f
            const val OKAY = 0x59414b4f
            const val CLSE = 0x45534c43
            const val WRTE = 0x45545257

            /** An ADB packet: 24-byte little-endian header (checksum and magic), then the payload. */
            fun message(command: Int, arg0: Int, arg1: Int, data: ByteArray?): ByteArray {
                val payload = data ?: ByteArray(0)
                return ByteBuffer.allocate(24 + payload.size).order(ByteOrder.LITTLE_ENDIAN)
                    .putInt(command).putInt(arg0).putInt(arg1).putInt(payload.size)
                    .putInt(payload.sumOf { it.toInt() and 0xff }).putInt(command.inv()).put(payload).array()
            }
        }
    }
}

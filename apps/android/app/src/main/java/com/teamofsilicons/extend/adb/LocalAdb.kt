package com.teamofsilicons.extend.adb

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Build
import android.provider.Settings
import android.util.Base64
import androidx.core.content.ContextCompat
import com.teamofsilicons.extend.security.SecretStore
import io.github.muntashirakon.adb.AbsAdbConnectionManager
import io.github.muntashirakon.adb.AdbStream
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runInterruptible
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import org.bouncycastle.asn1.x500.X500Name
import org.bouncycastle.cert.jcajce.JcaX509v3CertificateBuilder
import org.bouncycastle.cert.jcajce.JcaX509CertificateConverter
import org.bouncycastle.operator.jcajce.JcaContentSignerBuilder
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.math.BigInteger
import java.security.KeyFactory
import java.security.KeyPairGenerator
import java.security.PrivateKey
import java.security.cert.Certificate
import java.security.cert.CertificateFactory
import java.security.spec.PKCS8EncodedKeySpec
import java.util.Date
import java.util.UUID
import java.util.concurrent.TimeUnit

/**
 * Android debugging on this device, over loopback. Pairing is started locally by the Carbon,
 * never remotely.
 *
 * Nothing on loopback proves it is Android's debugging service: any installed app can listen on a
 * port and advertise it. So a port found by discovery (and any port once this device was paired
 * with a pairing code) must start TLS, and every new connection must prove it runs as Android's
 * shell before Extend uses it: it has to deliver a broadcast that only a sender holding the shell's
 * WRITE_SECURE_SETTINGS permission can deliver to this app.
 */
class LocalAdb(private val context: Context) {
    private val prefs = context.getSharedPreferences("extend_adb", Context.MODE_PRIVATE)
    private val keyStore = SecretStore(context, "extend_adb_identity")
    private val connectLock = Mutex()
    @Volatile private var manager: Manager? = null
    /** Set once the current connection proved it is Android's shell. */
    @Volatile private var verified = false
    @Volatile var lastError: String? = null
        private set

    /** Never blocks: the UI and setup checks read it while a connect may hold the transport's lock. */
    val connected: Boolean get() = verified && manager?.isConnected == true
    val enabled: Boolean get() = prefs.getBoolean("enabled", false)
    /** Paired with a pairing code (Wireless debugging), so every connection must use TLS. */
    val pairedWithCode: Boolean get() = prefs.getString("mode", null) == MODE_TLS

    /** Wireless debugging is switched off in Developer options (reconnecting can't work until it is on). */
    val wirelessDebuggingOff: Boolean
        get() = runCatching { Settings.Global.getInt(context.contentResolver, "adb_wifi_enabled", 1) == 0 }.getOrDefault(false)

    private class Manager(private val key: PrivateKey, private val certificate: Certificate) : AbsAdbConnectionManager() {
        init { setApi(Build.VERSION.SDK_INT); setHostAddress("127.0.0.1"); setTimeout(8, TimeUnit.SECONDS) }
        override fun getPrivateKey(): PrivateKey = key
        override fun getCertificate(): Certificate = certificate
        override fun getDeviceName(): String = "Silicon Extend"
    }
    private fun identity(): Manager {
        manager?.let { return it }
        var stored = keyStore.readCredential()?.split(':')
        if (stored?.size != 2) {
            val pair = KeyPairGenerator.getInstance("RSA").apply { initialize(2048) }.generateKeyPair()
            val subject = X500Name("CN=Silicon Extend")
            val now = System.currentTimeMillis()
            val certificate = JcaX509CertificateConverter().getCertificate(
                JcaX509v3CertificateBuilder(subject, BigInteger.valueOf(now), Date(now - 60_000),
                    Date(now + 10L * 365 * 86400 * 1000), subject, pair.public)
                    .build(JcaContentSignerBuilder("SHA256withRSA").build(pair.private)),
            )
            stored = listOf(pair.private.encoded, certificate.encoded).map { Base64.encodeToString(it, Base64.NO_WRAP) }
            keyStore.writeCredential(stored.joinToString(":"))
        }
        val key = KeyFactory.getInstance("RSA").generatePrivate(PKCS8EncodedKeySpec(Base64.decode(stored[0], Base64.NO_WRAP)))
        val certificate = CertificateFactory.getInstance("X.509").generateCertificate(Base64.decode(stored[1], Base64.NO_WRAP).inputStream())
        return Manager(key, certificate).also { manager = it }
    }
    suspend fun pair(port: Int, code: String) = connectLock.withLock {
        require(port in 1..65535) { "Enter the pairing port shown in Wireless debugging." }
        require(code.matches(Regex("[0-9]{6}"))) { "Enter the six-digit Android pairing code." }
        runInterruptible(Dispatchers.IO) {
            val m = identity()
            check(m.pair("127.0.0.1", port, code)) { "Android rejected the pairing code. Open a new pairing-code dialog and try again." }
            val edit = prefs.edit().putBoolean("enabled", true).putInt("port", 0).putString("mode", MODE_TLS)
            m.pairedDeviceGuid?.let { edit.putString("guid", it) } ?: edit.remove("guid")
            check(edit.commit()) { "Could not save Android debugging settings." }
        }
    }

    /**
     * Connects to [port], or discovers this device's Wireless debugging port when it is 0. Discovery
     * can return several advertisements (a stale one, or another app imitating Android): each is
     * tried in turn, and each must start TLS and pass the shell proof before it is used.
     */
    suspend fun connect(port: Int = 0): Boolean = connectLock.withLock {
        require(port in 0..65535) { "The connection port must be between 1 and 65535." }
        val m = identity()
        if (verified && m.isConnected) return@withLock true
        verified = false
        val discovered = port == 0
        val requireTls = discovered || pairedWithCode
        try {
            if (discovered) {
                val ports = AdbDiscovery.ports(context, prefs.getString("guid", null))
                if (ports.isEmpty()) {
                    lastError = if (wirelessDebuggingOff) "Wireless debugging is off. Turn it on in Developer options, then connect again."
                    else "Could not find this device's debugging port. Enter the connection port shown in Wireless debugging."
                    return@withLock false
                }
                val failures = ArrayList<Pair<Int, String>>()
                val chosen = AdbDiscovery.firstTrusted(ports, failures) { p -> attach(m, p, requireTls = true) }
                if (chosen == null) {
                    lastError = discoveryFailure(failures)
                    return@withLock false
                }
            } else {
                try {
                    attach(m, port, requireTls)
                } catch (e: CancellationException) {
                    throw e
                } catch (e: Exception) {
                    var message = e.message ?: e.javaClass.simpleName
                    if (requireTls && "TLS" in message) {
                        message += " If this is a TV's network debugging port (usually 5555), tap Disconnect Android debugging first, then connect with that port."
                    }
                    lastError = message
                    return@withLock false
                }
            }
            check(prefs.edit().putBoolean("enabled", true).putInt("port", port).putInt(KEY_CONNECTED_BOOT, bootCount(context)).commit()) {
                "Could not save Android debugging settings."
            }
            verified = true
            lastError = null
            true
        } catch (e: CancellationException) {
            withContext(NonCancellable) { runCatching { runInterruptible(Dispatchers.IO) { m.disconnect() } } }
            throw e
        } catch (e: Exception) {
            lastError = e.message ?: e.javaClass.simpleName
            runCatching { runInterruptible(Dispatchers.IO) { m.disconnect() } }
            false
        }
    }

    /** Opens the transport to [port] and has the peer prove it is Android's shell; throws (disconnected) otherwise. */
    private suspend fun attach(m: Manager, port: Int, requireTls: Boolean) {
        try {
            val ok = try {
                runInterruptible(Dispatchers.IO) {
                    m.disconnect()
                    m.setRequireTls(requireTls)
                    m.connect("127.0.0.1", port)
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                throw IOException(e.message ?: e.cause?.let { "Could not reach the debugging port $port: ${it.message ?: it.javaClass.simpleName}" } ?: e.javaClass.simpleName, e)
            }
            if (!ok) throw IOException("Android debugging did not connect on port $port within 8 seconds. Check Wireless debugging and its connection port, and approve Android's debugging prompt if it shows one.")
            provePeer(port)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            runCatching { runInterruptible(Dispatchers.IO) { m.disconnect() } }
            throw e
        }
    }

    /**
     * Has the peer on [port] prove it is Android's shell. It must deliver a broadcast carrying a
     * fresh nonce to a receiver that only accepts senders holding WRITE_SECURE_SETTINGS, which the
     * shell has and ordinary apps can't get.
     */
    private suspend fun provePeer(port: Int) {
        val nonce = UUID.randomUUID().toString()
        val received = CompletableDeferred<Unit>()
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(c: Context, intent: Intent) {
                if (intent.getStringExtra("nonce") == nonce) received.complete(Unit)
            }
        }
        ContextCompat.registerReceiver(context, receiver, IntentFilter(PROOF_ACTION), PROOF_PERMISSION, null, ContextCompat.RECEIVER_EXPORTED)
        try {
            val command = AdbWire.argv(listOf("am", "broadcast", "-a", PROOF_ACTION, "-p", context.packageName, "--es", "nonce", nonce))
            val answered = withTimeoutOrNull(10_000) {
                stream("shell,v2,raw:$command", unverified = true) { AdbWire.shell(drainingInput(it)) }
                withTimeoutOrNull(5_000) { received.await() } != null
            }
            if (answered != true) throw IOException(
                "The debugging service on port $port did not prove it is Android's own (it could not send a broadcast that needs the shell's permission), " +
                    "so Extend disconnected from it. Another app may be imitating Android debugging. Check the port shown in Wireless debugging and connect again.",
            )
        } finally {
            runCatching { context.unregisterReceiver(receiver) }
        }
    }

    suspend fun reconnect(): Boolean {
        if (!enabled || connected) return connected
        val port = prefs.getInt("port", 0)
        if (connect(port)) return true
        // Wireless debugging picks a new port each time it starts; discovery (TLS only) finds it.
        // A legacy network-debugging port (TVs) is never replaced by a discovered one.
        return port != 0 && pairedWithCode && connect(0)
    }
    suspend fun disconnect() = connectLock.withLock {
        verified = false
        runInterruptible(Dispatchers.IO) {
            manager?.disconnect()
            check(prefs.edit().putBoolean("enabled", false).remove("mode").remove("guid").remove(KEY_CONNECTED_BOOT).commit()) { "Could not save Android debugging settings." }
        }
    }
    private suspend fun <T> stream(service: String, unverified: Boolean = false, block: (AdbStream) -> T): T = try {
        runInterruptible(Dispatchers.IO) {
            val m = manager?.takeIf { it.isConnected && (verified || unverified) }
                ?: throw IOException("Android debugging isn't connected. Connect it in the Extend app's setup on this device.")
            m.openStream(service).use(block)
        }
    } catch (e: IOException) {
        // libadb wraps InterruptedException in IOException; retain structured cancellation.
        currentCoroutineContext().ensureActive()
        throw e
    }
    // libadb 3.1.1's AdbInputStream checks isClosed before draining AdbStream's buffered
    // final packet. Fast commands can therefore lose their exit frame. Read the stream
    // directly, and also preserve InputStream's unsigned-byte contract.
    private fun drainingInput(stream: AdbStream): InputStream = object : InputStream() {
        override fun read(): Int {
            val byte = ByteArray(1)
            return if (read(byte, 0, 1) < 0) -1 else byte[0].toInt() and 255
        }
        override fun read(bytes: ByteArray, offset: Int, length: Int): Int {
            if (length == 0) return 0
            return try { stream.read(bytes, offset, length) }
            catch (e: IOException) { if (stream.isClosed && stream.failure == null && e.cause !is InterruptedException) -1 else throw e }
        }
    }
    suspend fun shell(command: String, check: Boolean = true, pty: Boolean = false): AdbWire.Result {
        val result = stream("shell,v2,${if (pty) "pty" else "raw"}:$command") { AdbWire.shell(drainingInput(it)) }
        if (check && result.exitCode != 0) throw IOException("Android command exited ${result.exitCode}: ${result.text.take(2048)}")
        return result
    }
    /** A Silicon's command: output beyond the inline limit goes to files from [spill], never all into memory. */
    suspend fun shellCapture(command: String, spill: (String) -> File): AdbWire.Captured =
        stream("shell,v2,raw:$command") { AdbWire.capture(drainingInput(it), spill) }
    suspend fun logStream(file: File, onOpen: () -> Unit = {}, onReady: () -> Unit = {}) = stream("shell,v2,pty:logcat -v threadtime -T 1") { stream ->
        onOpen()
        val input = drainingInput(stream)
        file.outputStream().use { output ->
            var total = 0
            while (true) {
                val channel = input.read()
                if (channel < 0) break
                val size = AdbWire.int(AdbWire.exact(input, 4))
                if (size < 0 || size > 64 * 1024 || total + size > 16 * 1024 * 1024) break
                val bytes = AdbWire.exact(input, size)
                if (channel == 3) break
                if (channel !in 1..2) throw IOException("Unexpected log stream channel")
                output.write(bytes); output.flush()
                onReady()
                total += size
            }
        }
    }
    suspend fun push(file: File, remote: String) = stream("sync:") { stream ->
        val out = stream.openOutputStream(); val input = drainingInput(stream)
        AdbWire.packet(out, "SEND", "$remote,33152".toByteArray()) // regular file, owner read/write
        file.inputStream().use { source ->
            val buffer = ByteArray(64 * 1024)
            while (true) {
                val n = source.read(buffer)
                if (n < 0) break
                AdbWire.packet(out, "DATA", buffer.copyOf(n))
            }
        }
        out.write("DONE".toByteArray()); out.write(AdbWire.intBytes((System.currentTimeMillis() / 1000).toInt())); out.flush()
        val (id, size) = AdbWire.syncReply(input)
        if (id != "OKAY") throw IOException("ADB push failed: " + AdbWire.exact(input, size).toString(Charsets.UTF_8))
    }
    suspend fun pull(remote: String, file: File, limit: Long = PULL_LIMIT) = stream("sync:") { stream ->
        val out = stream.openOutputStream(); val input = drainingInput(stream)
        AdbWire.packet(out, "RECV", remote.toByteArray())
        file.outputStream().use { target ->
            var total = 0L
            while (true) {
                val (id, size) = AdbWire.syncReply(input)
                when (id) {
                    "DONE" -> break
                    "DATA" -> {
                        if (size !in 0..65536) throw IOException("Android sent an invalid file transfer packet ($size bytes)")
                        total += size
                        if (total > limit) throw AdbWire.LimitExceeded(
                            "$remote is larger than the ${limit / (1024 * 1024)} MiB transfer limit, so it was not pulled. " +
                                "Split it on the device (for example `split -b 200m <file> /data/local/tmp/part-`) and pull the parts.",
                        )
                        target.write(AdbWire.exact(input, size))
                    }
                    "FAIL" -> throw IOException(AdbWire.exact(input, size).toString(Charsets.UTF_8))
                    else -> throw IOException("Unexpected ADB sync response $id")
                }
            }
        }
    }

    /** The boot (Settings.Global.BOOT_COUNT) in which Android debugging last connected, or null if it never did. */
    val lastConnectedBoot: Int? get() = prefs.getInt(KEY_CONNECTED_BOOT, -1).takeIf { it >= 0 }

    companion object {
        const val PULL_LIMIT = 256L * 1024 * 1024
        private const val KEY_CONNECTED_BOOT = "connected_boot"

        /** Counts the device's boots; -1 when Android doesn't say. */
        fun bootCount(context: Context): Int =
            runCatching { Settings.Global.getInt(context.contentResolver, Settings.Global.BOOT_COUNT, -1) }.getOrDefault(-1)

        /** Every discovered candidate failed: say which and why, and what to do. */
        fun discoveryFailure(failures: List<Pair<Int, String>>): String {
            val each = failures.joinToString("; ") { (p, why) -> "port $p: ${why.trimEnd('.')}" }
            return "Found ${failures.size} Wireless debugging ${if (failures.size == 1) "advertisement" else "advertisements"} on this device, " +
                "and none was Android's own debugging service ($each). " +
                "Open Wireless debugging and enter the connection port it shows, then tap Connect."
        }
        private const val MODE_TLS = "tls"
        const val PROOF_ACTION = "com.teamofsilicons.extend.action.ADB_PEER_PROOF"
        /** Held by Android's shell (the debugging service's user) and grantable to apps only through it. */
        const val PROOF_PERMISSION = "android.permission.WRITE_SECURE_SETTINGS"
    }
}

package com.teamofsilicons.bridge.adb

import android.content.Context
import android.os.Build
import android.util.Base64
import com.teamofsilicons.bridge.security.SecretStore
import io.github.muntashirakon.adb.AbsAdbConnectionManager
import io.github.muntashirakon.adb.AdbStream
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runInterruptible
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
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
import java.util.concurrent.TimeUnit

/** Only connects to this device. Pairing is initiated locally by its owner, never remotely. */
class LocalAdb(private val context: Context) {
    private val prefs = context.getSharedPreferences("bridge_adb", Context.MODE_PRIVATE)
    private val keyStore = SecretStore(context, "bridge_adb_identity")
    private val connectLock = Mutex()
    @Volatile private var manager: Manager? = null
    @Volatile var lastError: String? = null
        private set
    val connected: Boolean get() = manager?.isConnected == true
    val enabled: Boolean get() = prefs.getBoolean("enabled", false)

    private class Manager(private val key: PrivateKey, private val certificate: Certificate) : AbsAdbConnectionManager() {
        init { setApi(Build.VERSION.SDK_INT); setHostAddress("127.0.0.1"); setTimeout(8, TimeUnit.SECONDS) }
        override fun getPrivateKey(): PrivateKey = key
        override fun getCertificate(): Certificate = certificate
        override fun getDeviceName(): String = "Silicon Bridge"
    }
    private fun identity(): Manager {
        manager?.let { return it }
        var stored = keyStore.readCredential()?.split(':')
        if (stored?.size != 2) {
            val pair = KeyPairGenerator.getInstance("RSA").apply { initialize(2048) }.generateKeyPair()
            val subject = X500Name("CN=Silicon Bridge")
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
            check(identity().pair("127.0.0.1", port, code)) { "Android rejected the pairing code. Open a new pairing-code dialog and try again." }
            check(prefs.edit().putBoolean("enabled", true).putInt("port", 0).commit()) { "Could not save Android debugging settings." }
        }
    }
    suspend fun connect(port: Int = 0): Boolean = connectLock.withLock {
        require(port in 0..65535) { "The connection port must be between 1 and 65535." }
        val resolvedPort = if (port == 0) AdbDiscovery.port(context) else port
        runInterruptible(Dispatchers.IO) {
            try {
                val m = identity()
                if (m.isConnected) return@runInterruptible true
                m.disconnect()
                if (resolvedPort == null) throw IOException("Could not find this device's debugging port. Enter the connection port shown in Wireless debugging.")
                val ok = m.connect("127.0.0.1", resolvedPort)
                if (!ok) throw IOException("Android debugging did not connect. Check Wireless debugging and its connection port.")
                check(prefs.edit().putBoolean("enabled", true).putInt("port", port).commit()) { "Could not save Android debugging settings." }
                lastError = null
                true
            } catch (e: Exception) {
                lastError = e.message
                false
            }
        }
    }
    suspend fun reconnect(): Boolean {
        if (!enabled || connected) return connected
        val port = prefs.getInt("port", 0)
        return if (connect(port)) true else if (port != 0) connect(0) else false
    }
    suspend fun disconnect() = connectLock.withLock {
        runInterruptible(Dispatchers.IO) {
            manager?.disconnect()
            check(prefs.edit().putBoolean("enabled", false).commit()) { "Could not save Android debugging settings." }
        }
    }
    private suspend fun <T> stream(service: String, block: (AdbStream) -> T): T = try {
        runInterruptible(Dispatchers.IO) {
            val m = manager?.takeIf { it.isConnected } ?: throw IOException("Connect Android debugging in the Bridge app's setup first.")
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
            catch (e: IOException) { if (stream.isClosed && e.cause !is InterruptedException) -1 else throw e }
        }
    }
    suspend fun shell(command: String, check: Boolean = true, pty: Boolean = false): AdbWire.Result {
        val result = stream("shell,v2,${if (pty) "pty" else "raw"}:$command") { AdbWire.shell(drainingInput(it)) }
        if (check && result.exitCode != 0) throw IOException("Android command exited ${result.exitCode}: ${result.text.take(2048)}")
        return result
    }
    suspend fun logStream(file: File) = stream("shell,v2,pty:logcat -v threadtime -T 1") { stream ->
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
    suspend fun pull(remote: String, file: File) = stream("sync:") { stream ->
        val out = stream.openOutputStream(); val input = drainingInput(stream)
        AdbWire.packet(out, "RECV", remote.toByteArray())
        file.outputStream().use { target ->
            var total = 0L
            while (true) {
                val (id, size) = AdbWire.syncReply(input)
                when (id) {
                    "DONE" -> break
                    "DATA" -> {
                        total += size
                        if (size !in 0..65536 || total > 256L * 1024 * 1024) throw IOException("ADB file exceeds the 256 MB transfer limit")
                        target.write(AdbWire.exact(input, size))
                    }
                    "FAIL" -> throw IOException(AdbWire.exact(input, size).toString(Charsets.UTF_8))
                    else -> throw IOException("Unexpected ADB sync response $id")
                }
            }
        }
    }
}

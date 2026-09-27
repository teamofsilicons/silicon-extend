package com.teamofsilicons.extend.adb

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.util.Log
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeoutOrNull
import java.net.NetworkInterface
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.coroutines.resume

/**
 * Finds this device's Wireless debugging port. Only advertisements for this device's own
 * addresses count, and the connection always uses loopback.
 *
 * Any installed app can advertise `_adb-tls-connect._tcp`, and mDNS can keep a stale
 * advertisement (an old port) next to the live one, so discovery returns every candidate and
 * [LocalAdb] tries each in turn: it requires TLS on it and then has the peer prove it is Android's
 * shell, and uses the first that passes.
 */
object AdbDiscovery {
    /** How long discovery listens in all. */
    const val WINDOW_MS = 5_000L
    /** Once something resolved, how long to keep listening for more before trying them. */
    const val SETTLE_MS = 1_200L
    /** More advertisements than this for one device are not plausible; the rest are ignored. */
    const val MAX_CANDIDATES = 8
    private const val TAG = "SiliconExtend"

    /**
     * Whether an advertised instance name belongs to the device that announced [guid] while
     * pairing. The saved guid is usually the full instance name (`adb-<serial>-<suffix>`). When
     * Wireless debugging restarts on a new port while its old advertisement is still cached, mDNS
     * renames the live one `adb-<serial>-<suffix> (2)`: that is the same device and must match.
     */
    fun matches(serviceName: String?, guid: String?): Boolean {
        if (guid == null) return true
        val name = serviceName ?: return false
        val base = guid.removePrefix("adb-")
        return name == "adb-$base" || name == base || name.startsWith("adb-$base (") || name.startsWith("$base (")
    }

    /** One resolved advertisement. */
    data class Advert(val name: String?, val host: String?, val port: Int)

    /**
     * Collects resolved advertisements into the ports worth trying, in the order they resolved:
     * this device's own addresses only, one entry per port, at most [MAX_CANDIDATES].
     */
    class Candidates(private val guid: String?, private val localAddresses: Set<String?>) {
        private val ports = LinkedHashSet<Int>()
        val list: List<Int> get() = ports.toList()

        /** Whether a found service is worth resolving. */
        fun wanted(name: String?): Boolean = matches(name, guid)

        /** Adds [a] if it is a candidate; returns whether it was new. */
        fun add(a: Advert): Boolean {
            if (!matches(a.name, guid) || a.host !in localAddresses || a.port !in 1..65535) return false
            if (ports.size >= MAX_CANDIDATES) return false
            return ports.add(a.port)
        }
    }

    private fun localAddresses(): Set<String?> = runCatching {
        NetworkInterface.getNetworkInterfaces().toList().flatMap { it.inetAddresses.toList() }.map { it.hostAddress }.toSet()
    }.getOrDefault(emptySet())

    /** Every candidate port this device advertises for Wireless debugging (empty when none). */
    @Suppress("DEPRECATION")
    suspend fun ports(context: Context, guid: String? = null): List<Int> {
        val nsd = context.getSystemService(NsdManager::class.java) ?: return emptyList()
        val candidates = Candidates(guid, localAddresses())
        val found = Channel<NsdServiceInfo>(Channel.UNLIMITED)
        val listener = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(type: String) = Unit
            override fun onDiscoveryStopped(type: String) = Unit
            override fun onStopDiscoveryFailed(type: String, code: Int) = Unit
            override fun onStartDiscoveryFailed(type: String, code: Int) { found.close() }
            override fun onServiceLost(info: NsdServiceInfo) = Unit
            override fun onServiceFound(info: NsdServiceInfo) {
                // After pairing, only the paired device's own service name is a candidate.
                if (candidates.wanted(info.serviceName)) found.trySend(info)
            }
        }
        try {
            nsd.discoverServices("_adb-tls-connect._tcp", NsdManager.PROTOCOL_DNS_SD, listener)
        } catch (e: Exception) {
            return emptyList()
        }
        try {
            withTimeoutOrNull(WINDOW_MS) {
                while (true) {
                    // Wait for the first candidate, then only briefly for more. Before Android 14
                    // NsdManager resolves one service at a time, so they are resolved in turn.
                    val info = if (candidates.list.isEmpty()) found.receiveCatching().getOrNull()
                    else withTimeoutOrNull(SETTLE_MS) { found.receiveCatching().getOrNull() }
                    if (info == null) break
                    val advert = resolve(nsd, info)
                    val added = advert != null && candidates.add(advert)
                    Log.i(TAG, "discovery: ${info.serviceName} → ${advert?.let { "${it.host}:${it.port}" } ?: "not resolved"}${if (added) " (candidate)" else ""}")
                    if (advert == null) continue
                    if (candidates.list.size >= MAX_CANDIDATES) break
                }
            }
        } finally {
            runCatching { nsd.stopServiceDiscovery(listener) }
            found.close()
        }
        return candidates.list
    }

    @Suppress("DEPRECATION")
    private suspend fun resolve(nsd: NsdManager, info: NsdServiceInfo): Advert? = withTimeoutOrNull(2_000) {
        suspendCancellableCoroutine { continuation ->
            val done = AtomicBoolean(false)
            try {
                nsd.resolveService(info, object : NsdManager.ResolveListener {
                    override fun onResolveFailed(info: NsdServiceInfo, code: Int) {
                        if (done.compareAndSet(false, true) && continuation.isActive) continuation.resume(null)
                    }
                    override fun onServiceResolved(info: NsdServiceInfo) {
                        if (done.compareAndSet(false, true) && continuation.isActive) {
                            continuation.resume(Advert(info.serviceName, info.host?.hostAddress, info.port))
                        }
                    }
                })
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                if (done.compareAndSet(false, true) && continuation.isActive) continuation.resume(null)
            }
        }
    }

    /**
     * Tries [ports] in order with [attempt] (connect with TLS, then the shell proof) and returns the
     * first that passes, or null with every candidate's reason in [failures]. A decoy that answers
     * first can't hide the real service behind it.
     */
    suspend fun firstTrusted(
        ports: List<Int>,
        failures: MutableList<Pair<Int, String>>,
        attempt: suspend (Int) -> Unit,
    ): Int? {
        for (port in ports) {
            try {
                attempt(port)
                return port
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                failures += port to (e.message ?: e.javaClass.simpleName)
            }
        }
        return null
    }
}

package com.teamofsilicons.extend.adb

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeoutOrNull
import java.net.NetworkInterface
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.coroutines.resume

/** Only use advertisements for this device; the actual connection always uses loopback. */
object AdbDiscovery {
    @Suppress("DEPRECATION")
    suspend fun port(context: Context): Int? = withTimeoutOrNull(5000) {
        suspendCancellableCoroutine { continuation ->
            val finished = AtomicBoolean(false)
            val nsd = context.getSystemService(NsdManager::class.java)
            val local = NetworkInterface.getNetworkInterfaces().toList()
                .flatMap { it.inetAddresses.toList() }.map { it.hostAddress }.toSet()
            lateinit var listener: NsdManager.DiscoveryListener
            fun stop() { runCatching { nsd.stopServiceDiscovery(listener) } }
            listener = object : NsdManager.DiscoveryListener {
                override fun onDiscoveryStarted(type: String) = Unit
                override fun onDiscoveryStopped(type: String) = Unit
                override fun onStopDiscoveryFailed(type: String, code: Int) = Unit
                override fun onStartDiscoveryFailed(type: String, code: Int) {
                    if (finished.compareAndSet(false, true) && continuation.isActive) continuation.resume(null)
                    stop()
                }
                override fun onServiceLost(info: NsdServiceInfo) = Unit
                override fun onServiceFound(info: NsdServiceInfo) {
                    if (!continuation.isActive) return
                    nsd.resolveService(info, object : NsdManager.ResolveListener {
                        override fun onResolveFailed(info: NsdServiceInfo, code: Int) = Unit
                        override fun onServiceResolved(info: NsdServiceInfo) {
                            if (info.host?.hostAddress in local && info.port in 1..65535 && finished.compareAndSet(false, true) && continuation.isActive) {
                                continuation.resume(info.port)
                                stop()
                            }
                        }
                    })
                }
            }
            continuation.invokeOnCancellation { finished.set(true); stop() }
            nsd.discoverServices("_adb-tls-connect._tcp", NsdManager.PROTOCOL_DNS_SD, listener)
        }
    }
}

package com.teamofsilicons.extend

import com.teamofsilicons.extend.adb.AdbDiscovery
import com.teamofsilicons.extend.adb.LocalAdb
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

/**
 * Wireless-debugging discovery keeps every advertisement for this device and tries each in turn.
 * It used to take the first one that resolved, so a stale advertisement (an old port) or another
 * app advertising `_adb-tls-connect._tcp` first kept every reconnect failing.
 */
class AdbDiscoveryTest {
    private val local = setOf("127.0.0.1", "10.0.2.16", "fe80::1")

    @Test fun everyAdvertisementForThisDeviceIsACandidateInOrder() {
        val c = AdbDiscovery.Candidates("EMULATOR36X1", local)
        // A decoy on this device with the paired name resolves first, then the real service.
        assertTrue(c.add(AdbDiscovery.Advert("adb-EMULATOR36X1 (2)", "10.0.2.16", 37_001)))
        assertTrue(c.add(AdbDiscovery.Advert("adb-EMULATOR36X1", "10.0.2.16", 41_234)))
        // Not candidates: another device's name, another host, a bad port, a repeat.
        assertTrue(!c.add(AdbDiscovery.Advert("adb-SOMEONEELSE", "10.0.2.16", 40_000)))
        assertTrue(!c.add(AdbDiscovery.Advert("adb-EMULATOR36X1", "192.168.1.50", 40_001)))
        assertTrue(!c.add(AdbDiscovery.Advert("adb-EMULATOR36X1", "10.0.2.16", 0)))
        assertTrue(!c.add(AdbDiscovery.Advert("adb-EMULATOR36X1", "10.0.2.16", 41_234)))
        assertEquals(listOf(37_001, 41_234), c.list)
        assertTrue(c.wanted("adb-EMULATOR36X1"))
        assertTrue(!c.wanted("adb-SOMEONEELSE"))
    }

    /**
     * Seen on the emulator: the saved guid is the full instance name, and after Wireless debugging
     * restarted, mDNS kept the old advertisement (a dead port) and renamed the live one "… (2)".
     * The live one used to be rejected, so reconnecting always failed on the dead port.
     */
    @Test fun theRenamedLiveAdvertisementOfThePairedDeviceMatches() {
        val guid = "adb-EMULATOR35X5X10X0-AQflKs"
        assertTrue(AdbDiscovery.matches("adb-EMULATOR35X5X10X0-AQflKs", guid))
        assertTrue(AdbDiscovery.matches("adb-EMULATOR35X5X10X0-AQflKs (2)", guid))
        assertTrue("a bare serial guid still matches", AdbDiscovery.matches("adb-EMULATOR35X5X10X0-AQflKs (2)", "EMULATOR35X5X10X0-AQflKs"))
        assertTrue(!AdbDiscovery.matches("adb-EMULATOR35X5X10X0-AQflKsX", guid))
        assertTrue(!AdbDiscovery.matches("adb-OTHER-AQflKs (2)", guid))
        val c = AdbDiscovery.Candidates(guid, local)
        assertTrue(c.add(AdbDiscovery.Advert("adb-EMULATOR35X5X10X0-AQflKs", "10.0.2.16", 42_509)))
        assertTrue(c.add(AdbDiscovery.Advert("adb-EMULATOR35X5X10X0-AQflKs (2)", "10.0.2.16", 39_845)))
        assertEquals(listOf(42_509, 39_845), c.list)
    }

    @Test fun candidatesAreCapped() {
        val c = AdbDiscovery.Candidates(null, local)
        repeat(20) { c.add(AdbDiscovery.Advert("adb-x$it", "127.0.0.1", 30_000 + it)) }
        assertEquals(AdbDiscovery.MAX_CANDIDATES, c.list.size)
    }

    @Test fun aDecoyFirstDoesNotHideTheRealService() = runBlocking {
        val tried = ArrayList<Int>()
        val failures = ArrayList<Pair<Int, String>>()
        val chosen = AdbDiscovery.firstTrusted(listOf(37_001, 41_234), failures) { port ->
            tried += port
            // The same trust checks decide: the decoy fails the shell proof, the real one passes.
            if (port == 37_001) throw IOException("The debugging service on port 37001 did not prove it is Android's own (it could not send a broadcast that needs the shell's permission), so Extend disconnected from it.")
        }
        assertEquals(41_234, chosen)
        assertEquals(listOf(37_001, 41_234), tried)
        assertEquals(listOf(37_001), failures.map { it.first })
    }

    @Test fun whenNoCandidatePassesEachReasonIsGiven() = runBlocking {
        val failures = ArrayList<Pair<Int, String>>()
        val chosen = AdbDiscovery.firstTrusted(listOf(37_001, 41_234), failures) { port ->
            if (port == 37_001) throw IOException("Connection refused.")
            throw IOException("The peer asked for RSA authentication, but this device was paired with a pairing code, so TLS is required.")
        }
        assertNull(chosen)
        val message = LocalAdb.discoveryFailure(failures)
        assertEquals(
            "Found 2 Wireless debugging advertisements on this device, and none was Android's own debugging service " +
                "(port 37001: Connection refused; port 41234: The peer asked for RSA authentication, but this device was paired with a pairing code, so TLS is required). " +
                "Open Wireless debugging and enter the connection port it shows, then tap Connect.",
            message,
        )
    }
}

package com.teamofsilicons.extend

import com.teamofsilicons.extend.adb.AdbReconnectPolicy
import org.junit.Assert.*
import org.junit.Test

class AdbReconnectPolicyTest {
    @Test fun failedReconnectsBackOffToFifteenMinutes() {
        val p = AdbReconnectPolicy()
        val waits = List(10) { p.afterFailure() }
        assertEquals(listOf(15_000L, 30_000L, 60_000L, 120_000L, 240_000L, 480_000L, 900_000L, 900_000L, 900_000L, 900_000L), waits)
        // Over a day of Wireless debugging being off, discovery runs about a hundred times, not 5760.
        var elapsed = 0L; var tries = 0
        val q = AdbReconnectPolicy()
        while (elapsed < 86_400_000L) { elapsed += q.afterFailure(); tries++ }
        assertTrue("tries=$tries", tries < 110)
    }

    @Test fun aWakeOrSuccessStartsAgainFromFifteenSeconds() {
        val p = AdbReconnectPolicy()
        repeat(5) { p.afterFailure() }
        p.reset()
        assertEquals(15_000L, p.afterFailure())
        p.afterFailure()
        assertEquals(15_000L, p.idle())
        assertEquals(15_000L, p.afterFailure())
    }
}

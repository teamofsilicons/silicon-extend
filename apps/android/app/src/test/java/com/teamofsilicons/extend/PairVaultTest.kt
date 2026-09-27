package com.teamofsilicons.extend

import com.teamofsilicons.extend.security.PairVault
import com.teamofsilicons.extend.security.StoredPair
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The sealed list of pairs, and 1.0's single credential becoming its first pair. */
class PairVaultTest {
    /** SharedPreferences in a map. */
    private class Slots : PairVault.Slots {
        val map = HashMap<String, String>()
        override fun get(key: String) = map[key]
        override fun put(key: String, value: String) { map[key] = value }
        override fun remove(key: String) { map.remove(key) }
    }

    /** A reversible "seal" that fails when the key is gone. */
    private class Sealer : PairVault.Sealer {
        var keyGone = false
        override fun seal(plain: String) = "sealed:" + plain.reversed()
        override fun open(sealed: String): String {
            check(!keyGone) { "Keystore key is gone" }
            require(sealed.startsWith("sealed:")) { "not sealed" }
            return sealed.removePrefix("sealed:").reversed()
        }
    }

    private val slots = Slots()
    private val sealer = Sealer()
    private val vault = PairVault(slots, sealer)

    @Test fun aFreshInstallHasNoPairs() {
        assertEquals(emptyList<StoredPair>(), vault.pairs { null })
        assertFalse(vault.hasPairs())
    }

    @Test fun the10CredentialBecomesTheFirstPairAndStaysAsAFallback() {
        slots.put(PairVault.KEY_LEGACY, sealer.seal("edc_one"))
        assertTrue(vault.hasPairs())
        assertEquals(listOf(StoredPair("7c1e09ab", "edc_one")), vault.pairs { "7c1e09ab" })
        assertTrue("the list is written", PairVault.KEY_PAIRS in slots.map)
        assertTrue("the single credential stays readable as a fallback", PairVault.KEY_LEGACY in slots.map)
        // Later reads come from the list, whatever the device id source says now.
        assertEquals(listOf(StoredPair("7c1e09ab", "edc_one")), vault.pairs { null })
        // The list can't be read (a corrupt value): the fallback still pairs the device.
        slots.put(PairVault.KEY_PAIRS, "garbage")
        assertEquals(listOf(StoredPair("7c1e09ab", "edc_one")), vault.pairs { "7c1e09ab" })
    }

    @Test fun aMigratedPairWithoutItsIdIsRenamedLater() {
        slots.put(PairVault.KEY_LEGACY, sealer.seal("edc_one"))
        assertEquals(listOf(StoredPair("", "edc_one")), vault.pairs { null })
        vault.rename("", "7c1e09ab")
        assertEquals(listOf(StoredPair("7c1e09ab", "edc_one")), vault.pairs { null })
    }

    @Test fun pairsAreAddedInOrderAndRemovedOneAtATime() {
        vault.add(StoredPair("a1", "edc_a"))
        vault.add(StoredPair("b2", "edc_b"))
        vault.add(StoredPair("c3", "edc_c"))
        assertEquals(listOf("a1", "b2", "c3"), vault.pairs { null }.map { it.deviceId })
        vault.add(StoredPair("b2", "edc_b2"))
        assertEquals("the same device id is replaced in place", listOf("edc_a", "edc_b2", "edc_c"), vault.pairs { null }.map { it.credential })
        vault.remove("b2")
        assertEquals(listOf("a1", "c3"), vault.pairs { null }.map { it.deviceId })
        assertFalse("credentials are sealed", slots.map.values.any { it.contains("edc_a") })
    }

    @Test fun anEndedMigratedPairNeverComesBackThroughTheFallback() {
        slots.put(PairVault.KEY_LEGACY, sealer.seal("edc_one"))
        vault.pairs { "a1" }
        vault.add(StoredPair("b2", "edc_two"))
        vault.remove("a1")
        assertNull("its credential is gone from the fallback too", slots.get(PairVault.KEY_LEGACY))
        assertEquals(listOf(StoredPair("b2", "edc_two")), vault.pairs { "a1" })
        vault.remove("b2")
        assertEquals(emptyList<StoredPair>(), vault.pairs { "a1" })
        assertTrue(slots.map.isEmpty())
    }

    @Test fun aLostKeystoreKeyForgetsEveryPair() {
        vault.add(StoredPair("a1", "edc_a"))
        slots.put(PairVault.KEY_LEGACY, sealer.seal("edc_a"))
        sealer.keyGone = true
        assertEquals(emptyList<StoredPair>(), vault.pairs { "a1" })
        assertTrue(slots.map.isEmpty())
    }

    @Test fun aStoredPairNeverPrintsItsCredential() {
        assertFalse(StoredPair("a1", "edc_secret").toString().contains("secret"))
    }
}

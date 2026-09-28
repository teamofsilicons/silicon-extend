package com.teamofsilicons.extend.security

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Keeps secrets encrypted with an AES-256-GCM key that never leaves the Android Keystore. Only the
 * ciphertext is written to app-private storage, so a copy of the app's files is useless without
 * this device's Keystore, and Android debugging (the shell user) can't read either.
 *
 * Two kinds of secret live here:
 * - the device's pairs ([pairs]): one credential per Carbon who paired this device, sealed together
 *   as one list ([PairVault]);
 * - one plain secret per namespace ([readCredential]), which Android debugging's key pair uses in
 *   its own namespace.
 *
 * (Written directly on the Keystore instead of androidx.security's EncryptedSharedPreferences,
 * which is deprecated.)
 */
class SecretStore(context: Context, namespace: String = "extend_secrets") {
    private val prefs = context.getSharedPreferences(namespace, Context.MODE_PRIVATE)

    private val vault = PairVault(
        object : PairVault.Slots {
            override fun get(key: String): String? = prefs.getString(key, null)
            override fun put(key: String, value: String) { prefs.edit().putString(key, value).commit() }
            override fun remove(key: String) { prefs.edit().remove(key).commit() }
        },
        object : PairVault.Sealer {
            override fun seal(plain: String): String = this@SecretStore.seal(plain)
            override fun open(sealed: String): String = this@SecretStore.open(sealed)
        },
    )

    // ───────────── The device's pairs ─────────────

    /**
     * Every pair of this device, in the order they were made (the app's first enrollment first).
     * A 1.0 app's single credential becomes the first pair on the first read; [legacyDeviceId]
     * names it (the device id 1.0 kept in the app's settings).
     */
    fun pairs(legacyDeviceId: () -> String? = { null }): List<StoredPair> = vault.pairs(legacyDeviceId)

    fun hasPairs(): Boolean = vault.hasPairs()

    fun credentialFor(deviceId: String): String? = vault.pairs { null }.firstOrNull { it.deviceId == deviceId }?.credential

    /** Adds a pair (or replaces the credential of the same device id). */
    fun addPair(pair: StoredPair) = vault.add(pair)

    fun removePair(deviceId: String) = vault.remove(deviceId)

    /** A migrated 1.0 pair learnt its device id from the service. */
    fun renamePair(from: String, to: String) = vault.rename(from, to)

    fun clearPairs() = vault.clear()

    // ───────────── One secret per namespace ─────────────

    fun readCredential(): String? {
        val stored = prefs.getString(KEY_CREDENTIAL, null) ?: return null
        return try {
            open(stored)
        } catch (_: Exception) {
            // The key is gone (app data restored elsewhere, Keystore reset): the secret is
            // unrecoverable.
            clearCredential()
            null
        }
    }

    fun writeCredential(credential: String) {
        prefs.edit().putString(KEY_CREDENTIAL, seal(credential)).commit()
    }

    fun clearCredential() {
        prefs.edit().remove(KEY_CREDENTIAL).commit()
    }

    // ───────────── The Keystore key ─────────────

    private fun seal(plain: String): String {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key())
        val sealed = cipher.iv + cipher.doFinal(plain.toByteArray(Charsets.UTF_8))
        return Base64.encodeToString(sealed, Base64.NO_WRAP)
    }

    private fun open(sealed: String): String {
        val bytes = Base64.decode(sealed, Base64.NO_WRAP)
        val iv = bytes.copyOfRange(0, IV_BYTES)
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(TAG_BITS, iv))
        return String(cipher.doFinal(bytes, IV_BYTES, bytes.size - IV_BYTES), Charsets.UTF_8)
    }

    private fun key(): SecretKey {
        val ks = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        (ks.getEntry(ALIAS, null) as? KeyStore.SecretKeyEntry)?.let { return it.secretKey }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
        generator.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return generator.generateKey()
    }

    private companion object {
        const val ANDROID_KEYSTORE = "AndroidKeyStore"
        const val ALIAS = "silicon_extend_device_credential"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val IV_BYTES = 12
        const val TAG_BITS = 128
        /** The single secret: 1.0's device credential, and Android debugging's key pair in its namespace. */
        const val KEY_CREDENTIAL = PairVault.KEY_LEGACY
    }
}

/** One Carbon's pair of this device. */
@Serializable
data class StoredPair(val deviceId: String, val credential: String) {
    /** Never prints the credential (it would end up in logs). */
    override fun toString(): String = "StoredPair($deviceId, ${credential.take(4)}…)"
}

/**
 * The sealed list of pairs, and the move from 1.0's single credential. Apart from the Keystore
 * ([Sealer]) and SharedPreferences ([Slots]) so JVM tests cover the format and the migration.
 *
 * The list is sealed as one value under [KEY_PAIRS]. 1.0 kept one sealed credential under
 * [KEY_LEGACY]: on the first read it becomes the first pair, and it stays there as a fallback
 * (read only when the list itself can't be) until that pair ends.
 */
class PairVault(private val slots: Slots, private val sealer: Sealer) {
    interface Slots {
        fun get(key: String): String?
        fun put(key: String, value: String)
        fun remove(key: String)
    }

    /** Seals and opens values; [open] throws when a value can't be opened (the key is gone). */
    interface Sealer {
        fun seal(plain: String): String
        fun open(sealed: String): String
    }

    private val json = Json { ignoreUnknownKeys = true }
    private val codec = ListSerializer(StoredPair.serializer())

    @Synchronized
    fun pairs(legacyDeviceId: () -> String?): List<StoredPair> {
        val sealed = slots.get(KEY_PAIRS)
        if (sealed != null) {
            runCatching { json.decodeFromString(codec, sealer.open(sealed)) }.getOrNull()?.let { return it }
        }
        // No list yet (a 1.0 app just updated), or it can't be opened: the single credential.
        val legacy = slots.get(KEY_LEGACY)?.let { runCatching { sealer.open(it) }.getOrNull() }
        if (legacy == null) {
            // Nothing can be read: every credential is unrecoverable, and the device pairs again.
            slots.remove(KEY_PAIRS)
            slots.remove(KEY_LEGACY)
            return emptyList()
        }
        val migrated = listOf(StoredPair(legacyDeviceId().orEmpty(), legacy))
        runCatching { write(migrated) }
        return migrated
    }

    @Synchronized
    fun hasPairs(): Boolean = slots.get(KEY_PAIRS) != null || slots.get(KEY_LEGACY) != null

    @Synchronized
    fun add(pair: StoredPair) {
        val current = pairs { null }
        val at = current.indexOfFirst { it.deviceId == pair.deviceId }
        write(if (at >= 0) current.toMutableList().also { it[at] = pair } else current + pair)
    }

    @Synchronized
    fun remove(deviceId: String) {
        write(pairs { null }.filter { it.deviceId != deviceId })
    }

    @Synchronized
    fun rename(from: String, to: String) {
        if (from == to) return
        write(pairs { null }.map { if (it.deviceId == from) it.copy(deviceId = to) else it })
    }

    @Synchronized
    fun clear() {
        slots.remove(KEY_PAIRS)
        slots.remove(KEY_LEGACY)
    }

    private fun write(list: List<StoredPair>) {
        if (list.isEmpty()) {
            clear()
            return
        }
        slots.put(KEY_PAIRS, sealer.seal(json.encodeToString(codec, list)))
        // The 1.0 fallback only while its credential is still one of the pairs, so an ended pair
        // can never come back through it.
        val legacy = slots.get(KEY_LEGACY)?.let { runCatching { sealer.open(it) }.getOrNull() }
        if (legacy != null && list.none { it.credential == legacy }) slots.remove(KEY_LEGACY)
    }

    companion object {
        const val KEY_PAIRS = "device_pairs"
        const val KEY_LEGACY = "device_credential"
    }
}

package com.teamofsilicons.bridge.security

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Keeps the device credential encrypted with an AES-256-GCM key that never leaves the Android
 * Keystore. Only the ciphertext is written to app-private storage, so a copy of the app's files
 * is useless without this device's Keystore.
 *
 * (Written directly on the Keystore instead of androidx.security's EncryptedSharedPreferences,
 * which is deprecated.)
 */
class SecretStore(context: Context, namespace: String = "bridge_secrets") {
    private val prefs = context.getSharedPreferences(namespace, Context.MODE_PRIVATE)

    fun readCredential(): String? {
        val stored = prefs.getString(KEY_CREDENTIAL, null) ?: return null
        return try {
            val bytes = Base64.decode(stored, Base64.NO_WRAP)
            val iv = bytes.copyOfRange(0, IV_BYTES)
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(TAG_BITS, iv))
            String(cipher.doFinal(bytes, IV_BYTES, bytes.size - IV_BYTES), Charsets.UTF_8)
        } catch (_: Exception) {
            // The key is gone (app data restored elsewhere, Keystore reset): the credential is
            // unrecoverable, so the device has to pair again.
            clearCredential()
            null
        }
    }

    fun writeCredential(credential: String) {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key())
        val sealed = cipher.iv + cipher.doFinal(credential.toByteArray(Charsets.UTF_8))
        prefs.edit().putString(KEY_CREDENTIAL, Base64.encodeToString(sealed, Base64.NO_WRAP)).commit()
    }

    fun clearCredential() {
        prefs.edit().remove(KEY_CREDENTIAL).commit()
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
        const val ALIAS = "silicon_bridge_device_credential"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val IV_BYTES = 12
        const val TAG_BITS = 128
        const val KEY_CREDENTIAL = "device_credential"
    }
}

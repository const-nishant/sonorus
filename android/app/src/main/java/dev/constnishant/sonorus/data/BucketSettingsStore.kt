package dev.constnishant.sonorus.data

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import android.util.Log
import androidx.core.content.edit
import dev.constnishant.sonorus.core.S3Settings
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

private const val TAG = "Sonorus"
private const val PREFS_NAME = "bucket_settings"
private const val ANDROID_KEYSTORE = "AndroidKeyStore"
private const val KEY_ALIAS = "sonorus_bucket_credentials"
private const val TRANSFORMATION = "AES/GCM/NoPadding"
private const val GCM_TAG_BITS = 128

private const val K_ENDPOINT = "endpoint"
private const val K_REGION = "region"
private const val K_BUCKET = "bucket"
private const val K_PATH_STYLE = "path_style"
private const val K_PREFIX = "prefix"
private const val K_ACCESS_IV = "access_iv"
private const val K_ACCESS_CT = "access_ct"
private const val K_SECRET_IV = "secret_iv"
private const val K_SECRET_CT = "secret_ct"

/** Keys that must all be present for the stored settings to be usable. */
private val REQUIRED_KEYS = listOf(
    K_ENDPOINT, K_REGION, K_BUCKET, K_PATH_STYLE, K_PREFIX,
    K_ACCESS_IV, K_ACCESS_CT, K_SECRET_IV, K_SECRET_CT,
)

/**
 * Persists the bucket settings. The plain fields live in a private SharedPreferences file; the
 * access key id and the secret are each encrypted with an AES-256-GCM key held in the Android
 * Keystore, so the secret never sits on disk in clear text.
 *
 * Writes are synchronous (`commit()`) and must be called off the main thread.
 */
class BucketSettingsStore(context: Context) {
    private val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /**
     * Returns the stored settings, or null when none are stored. If the stored credentials cannot
     * be decrypted (for example the Keystore key was lost), the stored settings are cleared and
     * null is returned. Blocking.
     */
    fun load(): S3Settings? {
        if (!hasSettings()) return null
        return try {
            val key = checkNotNull(secretKey(create = false)) { "keystore key missing" }
            S3Settings(
                endpoint = req(K_ENDPOINT),
                region = req(K_REGION),
                bucket = req(K_BUCKET),
                accessKeyId = decrypt(req(K_ACCESS_IV), req(K_ACCESS_CT), key),
                secretAccessKey = decrypt(req(K_SECRET_IV), req(K_SECRET_CT), key),
                pathStyle = prefs.getBoolean(K_PATH_STYLE, true),
                prefix = req(K_PREFIX),
            )
        } catch (e: Exception) {
            Log.w(TAG, "Stored bucket settings could not be read; clearing them", e)
            clear()
            null
        }
    }

    /**
     * Stores the settings, encrypting the credentials. Generates the Keystore key on first use.
     * Throws if the Keystore or the preferences write fails; nothing is replaced in that case.
     * Blocking.
     */
    fun save(settings: S3Settings) {
        val key = checkNotNull(secretKey(create = true)) { "keystore key unavailable" }
        val access = encrypt(settings.accessKeyId, key)
        val secret = encrypt(settings.secretAccessKey, key)
        val ok = prefs.edit()
            .putString(K_ENDPOINT, settings.endpoint)
            .putString(K_REGION, settings.region)
            .putString(K_BUCKET, settings.bucket)
            .putBoolean(K_PATH_STYLE, settings.pathStyle)
            .putString(K_PREFIX, settings.prefix)
            .putString(K_ACCESS_IV, access.first)
            .putString(K_ACCESS_CT, access.second)
            .putString(K_SECRET_IV, secret.first)
            .putString(K_SECRET_CT, secret.second)
            .commit()
        check(ok) { "failed to write bucket settings" }
    }

    /**
     * Removes the stored settings and the Keystore key. Blocking.
     */
    fun clear() {
        prefs.edit(commit = true) { clear() }
        try {
            KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }.deleteEntry(KEY_ALIAS)
        } catch (e: Exception) {
            Log.w(TAG, "Could not delete the bucket credentials key", e)
        }
    }

    /** True when a complete settings record is stored. Does not decrypt anything. */
    fun hasSettings(): Boolean = REQUIRED_KEYS.all { prefs.contains(it) }

    private fun req(key: String): String = checkNotNull(prefs.getString(key, null)) { "missing $key" }

    private fun secretKey(create: Boolean): SecretKey? {
        val keyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        (keyStore.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }
        if (!create) return null
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
        generator.init(
            KeyGenParameterSpec.Builder(
                KEY_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .setRandomizedEncryptionRequired(true)
                .build(),
        )
        return generator.generateKey()
    }

    /** Returns Base64(iv) to Base64(ciphertext). */
    private fun encrypt(plain: String, key: SecretKey): Pair<String, String> {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key)
        val ciphertext = cipher.doFinal(plain.toByteArray(Charsets.UTF_8))
        return Base64.encodeToString(cipher.iv, Base64.NO_WRAP) to
            Base64.encodeToString(ciphertext, Base64.NO_WRAP)
    }

    private fun decrypt(ivB64: String, ciphertextB64: String, key: SecretKey): String {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(
            Cipher.DECRYPT_MODE,
            key,
            GCMParameterSpec(GCM_TAG_BITS, Base64.decode(ivB64, Base64.NO_WRAP)),
        )
        return String(cipher.doFinal(Base64.decode(ciphertextB64, Base64.NO_WRAP)), Charsets.UTF_8)
    }
}

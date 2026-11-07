package dev.constnishant.sonorus.data

import android.content.Context
import dev.constnishant.sonorus.core.CacheSettings

private const val PREFS_NAME = "cache_settings"
private const val K_LIMIT_BYTES = "limit_bytes"
private const val K_TTL_DAYS = "ttl_days"
private const val K_DOWNLOAD_ON_METERED = "download_on_metered"

/** One gibibyte, in bytes. */
const val GIB: Long = 1024L * 1024 * 1024

/** The defaults: a 4 GiB cache, unpinned tracks kept 30 days, no downloads on mobile data. */
val DEFAULT_CACHE_SETTINGS = CacheSettings(
    limitBytes = 4UL * GIB.toULong(),
    ttlDays = 30u,
    downloadOnMetered = false,
)

/**
 * Persists the offline cache settings (byte limit, expiry of unpinned tracks, downloads on mobile
 * data) in a private SharedPreferences file. The values are not secret, so they are stored in
 * clear text.
 *
 * An expiry of "never" is stored as 0, which the core reads the same way as null.
 *
 * Writes are synchronous (`commit()`) and must be called off the main thread.
 */
class CacheSettingsStore(context: Context) {
    private val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /**
     * Returns the stored settings, or [DEFAULT_CACHE_SETTINGS] for any value never stored. Blocking.
     */
    fun load(): CacheSettings {
        val limit = prefs.getLong(K_LIMIT_BYTES, DEFAULT_CACHE_SETTINGS.limitBytes.toLong())
        val days = prefs.getInt(K_TTL_DAYS, DEFAULT_CACHE_SETTINGS.ttlDays?.toInt() ?: 0)
        return CacheSettings(
            limitBytes = limit.coerceAtLeast(0L).toULong(),
            ttlDays = if (days <= 0) null else days.toUInt(),
            downloadOnMetered = prefs.getBoolean(K_DOWNLOAD_ON_METERED, DEFAULT_CACHE_SETTINGS.downloadOnMetered),
        )
    }

    /**
     * Stores [settings]. Throws if the preferences write fails; the previous values are kept in
     * that case. Blocking.
     */
    fun save(settings: CacheSettings) {
        val ok = prefs.edit()
            .putLong(K_LIMIT_BYTES, settings.limitBytes.toLong())
            .putInt(K_TTL_DAYS, settings.ttlDays?.toInt() ?: 0)
            .putBoolean(K_DOWNLOAD_ON_METERED, settings.downloadOnMetered)
            .commit()
        check(ok) { "failed to write cache settings" }
    }
}

package dev.constnishant.sonorus.data

import android.content.Context
import dev.constnishant.sonorus.core.OutputSampleFormat
import dev.constnishant.sonorus.core.OutputSettings

private const val PREFS_NAME = "output_settings"
private const val K_SAMPLE_FORMAT = "sample_format"
private const val K_EXCLUSIVE = "exclusive"

/**
 * Persists the output settings (sample format and exclusive mode) in a private SharedPreferences
 * file. The values are not secret, so they are stored in clear text.
 *
 * Writes are synchronous (`commit()`) and must be called off the main thread.
 */
class OutputSettingsStore(context: Context) {
    private val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /**
     * Returns the stored settings, or the defaults (float, not exclusive) when none are stored.
     * An unknown stored format name reads as float. Blocking.
     */
    fun load(): OutputSettings {
        val format = prefs.getString(K_SAMPLE_FORMAT, null)
            ?.let { name -> OutputSampleFormat.entries.firstOrNull { it.name == name } }
            ?: OutputSampleFormat.FLOAT
        return OutputSettings(
            sampleFormat = format,
            exclusive = prefs.getBoolean(K_EXCLUSIVE, false),
        )
    }

    /**
     * Stores [settings]. Throws if the preferences write fails; the previous values are kept in
     * that case. Blocking.
     */
    fun save(settings: OutputSettings) {
        val ok = prefs.edit()
            .putString(K_SAMPLE_FORMAT, settings.sampleFormat.name)
            .putBoolean(K_EXCLUSIVE, settings.exclusive)
            .commit()
        check(ok) { "failed to write output settings" }
    }
}

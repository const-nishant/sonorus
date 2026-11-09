package dev.constnishant.sonorus.data

import android.content.Context
import dev.constnishant.sonorus.core.ReplayGainModeInfo
import dev.constnishant.sonorus.core.ReplayGainSettingsInfo

private const val PREFS_NAME = "replay_gain_settings"
private const val K_MODE = "mode"
private const val K_PREAMP_DB = "preamp_db"

/** The defaults: ReplayGain off, pre-amp 0 dB. */
val DEFAULT_REPLAY_GAIN = ReplayGainSettingsInfo(mode = ReplayGainModeInfo.OFF, preampDb = 0f)

/**
 * Persists the ReplayGain settings (mode and pre-amp in dB) in a private SharedPreferences file.
 * The values are not secret, so they are stored in clear text.
 *
 * The repository reads these when it creates the core and writes them on every change, so the
 * core always applies the saved values. Writes are synchronous (`commit()`) and must be called off
 * the main thread.
 */
class ReplayGainSettingsStore(context: Context) {
    private val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /**
     * Returns the stored settings, or [DEFAULT_REPLAY_GAIN] for any value never stored. An unknown
     * stored mode name reads as the default mode. Blocking.
     */
    fun load(): ReplayGainSettingsInfo {
        val mode = prefs.getString(K_MODE, null)
            ?.let { name -> ReplayGainModeInfo.entries.firstOrNull { it.name == name } }
            ?: DEFAULT_REPLAY_GAIN.mode
        return ReplayGainSettingsInfo(
            mode = mode,
            preampDb = prefs.getFloat(K_PREAMP_DB, DEFAULT_REPLAY_GAIN.preampDb),
        )
    }

    /**
     * Stores [settings]. Throws if the preferences write fails; the previous values are kept in
     * that case. Blocking.
     */
    fun save(settings: ReplayGainSettingsInfo) {
        val ok = prefs.edit()
            .putString(K_MODE, settings.mode.name)
            .putFloat(K_PREAMP_DB, settings.preampDb)
            .commit()
        check(ok) { "failed to write the ReplayGain settings" }
    }
}

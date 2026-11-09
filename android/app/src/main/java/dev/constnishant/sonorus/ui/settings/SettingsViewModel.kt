package dev.constnishant.sonorus.ui.settings

import android.app.Application
import androidx.annotation.StringRes
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.BuildConfig
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.CacheSettings
import dev.constnishant.sonorus.core.CacheUsageInfo
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.LibraryStatsInfo
import dev.constnishant.sonorus.core.OutputInfo
import dev.constnishant.sonorus.core.OutputSampleFormat
import dev.constnishant.sonorus.core.OutputSettings
import dev.constnishant.sonorus.core.ReplayGainModeInfo
import dev.constnishant.sonorus.core.ReplayGainSettingsInfo
import dev.constnishant.sonorus.core.S3Settings
import dev.constnishant.sonorus.core.coreVersion
import dev.constnishant.sonorus.data.DEFAULT_CACHE_SETTINGS
import dev.constnishant.sonorus.data.DEFAULT_REPLAY_GAIN
import dev.constnishant.sonorus.data.ScanUi
import dev.constnishant.sonorus.data.SonorusRepository
import dev.constnishant.sonorus.data.userMessage
import dev.constnishant.sonorus.ui.components.formatBytes
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Bucket provider presets. Picking one prefills the endpoint pattern and the region hint. */
enum class BucketProvider(
    @StringRes val labelRes: Int,
    val endpointPattern: String,
    val regionHint: String,
) {
    /** Backblaze B2: replace `<region>` in the endpoint. */
    B2(R.string.provider_b2, "https://s3.<region>.backblazeb2.com", "us-west-004"),

    /** Cloudflare R2: replace `<account-id>` in the endpoint. */
    R2(R.string.provider_r2, "https://<account-id>.r2.cloudflarestorage.com", "auto"),

    /** Any other S3-compatible service. Nothing is prefilled. */
    CUSTOM(R.string.provider_custom, "", ""),
}

/** The bucket form. Holds the secret in memory only while the screen is alive. */
data class BucketForm(
    val provider: BucketProvider = BucketProvider.CUSTOM,
    val endpoint: String = "",
    val region: String = "",
    val bucket: String = "",
    val accessKeyId: String = "",
    val secretAccessKey: String = "",
    val pathStyle: Boolean = true,
    val prefix: String = "",
)

/** Everything the settings screen shows. */
data class SettingsUiState(
    val form: BucketForm = BucketForm(),
    val hasSettings: Boolean = false,
    val saving: Boolean = false,
    /** Result of the last save or disconnect, shown inline. */
    val bucketMessage: String? = null,
    val bucketFailed: Boolean = false,
    val stats: LibraryStatsInfo? = null,
    val statsError: String? = null,
    val syncing: Boolean = false,
    /** Result of the last sync, shown inline. */
    val syncMessage: String? = null,
    val syncFailed: Boolean = false,
    /** Saved output settings: sample format and exclusive mode. */
    val output: OutputSettings = OutputSettings(sampleFormat = OutputSampleFormat.FLOAT, exclusive = false),
    /** The output stream the core has open, or null when nothing is open. Refreshed every second. */
    val outputInfo: OutputInfo? = null,
    /** Whether audio goes to a connected USB DAC bit-perfect. Applies on the next stream open. */
    val bitPerfect: Boolean = true,
    /** Name of the connected USB DAC, or null when none is connected. */
    val dacName: String? = null,
    /** Saved ReplayGain settings: mode and pre-amp. Applies from the next song. */
    val replayGain: ReplayGainSettingsInfo = DEFAULT_REPLAY_GAIN,
    /** Saved offline cache settings. */
    val cache: CacheSettings = DEFAULT_CACHE_SETTINGS,
    /** Cache usage, or null until the first successful read. */
    val cacheUsage: CacheUsageInfo? = null,
    /** Result of the last clear, or a failed read, shown inline. */
    val cacheMessage: String? = null,
    val cacheFailed: Boolean = false,
    val clearing: Boolean = false,
    val version: String = "",
)

/** The provider preset whose endpoint pattern matches [endpoint]; CUSTOM when none does. */
private fun providerFor(endpoint: String): BucketProvider = when {
    endpoint.contains("backblazeb2.com") -> BucketProvider.B2
    endpoint.contains("r2.cloudflarestorage.com") -> BucketProvider.R2
    else -> BucketProvider.CUSTOM
}

/**
 * The bucket form prefilled from the build's SONORUS_S3_* values, or null when the build has none.
 * Only debug builds carry these (see `app/build.gradle.kts`); release builds leave them empty.
 */
private fun buildDefaults(): BucketForm? {
    val endpoint = BuildConfig.DEFAULT_S3_ENDPOINT
    if (endpoint.isBlank()) return null
    return BucketForm(
        provider = providerFor(endpoint),
        endpoint = endpoint,
        region = BuildConfig.DEFAULT_S3_REGION,
        bucket = BuildConfig.DEFAULT_S3_BUCKET,
        accessKeyId = BuildConfig.DEFAULT_S3_ACCESS_KEY_ID,
        secretAccessKey = BuildConfig.DEFAULT_S3_SECRET_ACCESS_KEY,
        prefix = BuildConfig.DEFAULT_S3_PREFIX,
    )
}

/**
 * State and actions for the settings screen. Core and settings access goes through the repository,
 * which runs it on IO. Field values are never logged.
 */
class SettingsViewModel(
    application: Application,
    private val repository: SonorusRepository,
) : AndroidViewModel(application) {
    private val context: Application = application
    private val _state = MutableStateFlow(SettingsUiState())

    /** The screen state. */
    val state: StateFlow<SettingsUiState> = _state.asStateFlow()

    /** Library scan state, straight from the repository. */
    val scan: StateFlow<ScanUi> = repository.events.scan

    init {
        viewModelScope.launch {
            val saved = withContext(Dispatchers.IO) { repository.settings.load() }
            if (saved == null) {
                buildDefaults()?.let { defaults -> _state.update { it.copy(form = defaults) } }
            } else {
                _state.update {
                    it.copy(
                        hasSettings = true,
                        form = BucketForm(
                            provider = providerFor(saved.endpoint),
                            endpoint = saved.endpoint,
                            region = saved.region,
                            bucket = saved.bucket,
                            accessKeyId = saved.accessKeyId,
                            secretAccessKey = saved.secretAccessKey,
                            pathStyle = saved.pathStyle,
                            prefix = saved.prefix,
                        ),
                    )
                }
            }
        }
        viewModelScope.launch {
            val output = withContext(Dispatchers.IO) { repository.output.load() }
            _state.update { it.copy(output = output) }
        }
        viewModelScope.launch {
            while (isActive) {
                val info = try {
                    repository.outputInfo()
                } catch (e: CoreException) {
                    null
                }
                _state.update { it.copy(outputInfo = info) }
                delay(1_000)
            }
        }
        viewModelScope.launch {
            val replayGain = repository.replayGainSettings()
            _state.update { it.copy(replayGain = replayGain) }
        }
        viewModelScope.launch {
            val enabled = withContext(Dispatchers.IO) { repository.output.bitPerfect() }
            _state.update { it.copy(bitPerfect = enabled) }
        }
        viewModelScope.launch {
            repository.dac.collect { dac -> _state.update { it.copy(dacName = dac?.name) } }
        }
        viewModelScope.launch {
            val version = withContext(Dispatchers.IO) { coreVersion() }
            _state.update { it.copy(version = version) }
        }
        viewModelScope.launch {
            val cache = repository.cacheSettings()
            _state.update { it.copy(cache = cache) }
            refreshCacheUsage()
        }
        viewModelScope.launch {
            refreshStats()
            repository.events.libraryChanged.collect { refreshStats() }
        }
    }

    /** Applies a preset: fills the endpoint pattern and region hint. Custom changes nothing. */
    fun selectProvider(provider: BucketProvider) {
        _state.update {
            val form = it.form
            val updated = if (provider == BucketProvider.CUSTOM) {
                form.copy(provider = provider)
            } else {
                form.copy(
                    provider = provider,
                    endpoint = provider.endpointPattern,
                    region = provider.regionHint,
                )
            }
            it.copy(form = updated)
        }
    }

    /** Sets the endpoint; the provider chip follows what the endpoint looks like. */
    fun setEndpoint(value: String) {
        updateForm { copy(endpoint = value, provider = providerFor(value)) }
    }

    /** Applies [change] to the bucket form. */
    fun updateForm(change: BucketForm.() -> BucketForm) {
        _state.update { it.copy(form = it.form.change()) }
    }

    /**
     * Validates and stores the bucket settings and connects the core to them. The result is shown
     * inline as "Connected" or the rejection message.
     */
    fun saveAndConnect() {
        val form = _state.value.form
        _state.update { it.copy(saving = true, bucketMessage = null, bucketFailed = false) }
        viewModelScope.launch {
            val settings = S3Settings(
                endpoint = form.endpoint.trim(),
                region = form.region.trim(),
                bucket = form.bucket.trim(),
                accessKeyId = form.accessKeyId.trim(),
                secretAccessKey = form.secretAccessKey.trim(),
                pathStyle = form.pathStyle,
                prefix = form.prefix.trim(),
            )
            var stored = false
            try {
                repository.configureBucket(settings)
                stored = true
                // Settings are stored now; one small listing proves the credentials work.
                val found = repository.call { it.checkBackend() }
                val where = settings.prefix.trim('/').ifEmpty { null }
                _state.update {
                    it.copy(
                        saving = false,
                        hasSettings = true,
                        bucketMessage = when {
                            found -> context.getString(R.string.settings_connected)
                            where != null -> context.getString(R.string.settings_connected_no_files, where)
                            else -> context.getString(R.string.settings_connected_empty)
                        },
                        bucketFailed = !found,
                    )
                }
            } catch (e: CoreException) {
                _state.update {
                    it.copy(
                        saving = false,
                        hasSettings = it.hasSettings || stored,
                        bucketMessage = e.userMessage(context, R.string.error_bucket_connect),
                        bucketFailed = true,
                    )
                }
            } catch (e: Exception) {
                // Keystore or preferences failure while storing the settings (the core already
                // accepted them). The exception text is not shown: it may name key material.
                _state.update {
                    it.copy(
                        saving = false,
                        bucketMessage = context.getString(R.string.error_bucket_store),
                        bucketFailed = true,
                    )
                }
            }
        }
    }

    /**
     * Removes the stored bucket settings. The core keeps its current backend until the next launch.
     */
    fun disconnect() {
        viewModelScope.launch {
            repository.disconnectBucket()
            _state.update {
                it.copy(
                    hasSettings = false,
                    bucketMessage = context.getString(R.string.settings_disconnected),
                    bucketFailed = false,
                )
            }
        }
    }

    /** Selects the sample format and applies it. Playback stops if the value changed. */
    fun setSampleFormat(format: OutputSampleFormat) {
        applyOutput(_state.value.output.copy(sampleFormat = format))
    }

    /** Turns exclusive mode on or off and applies it. Playback stops if the value changed. */
    fun setExclusive(exclusive: Boolean) {
        applyOutput(_state.value.output.copy(exclusive = exclusive))
    }

    /**
     * Turns bit-perfect output to a USB DAC on or off. Saved at once; it applies on the next stream
     * open, so playback does not stop. If the save fails, the previous value is shown again.
     */
    fun setBitPerfect(enabled: Boolean) {
        val previous = _state.value.bitPerfect
        _state.update { it.copy(bitPerfect = enabled) }
        viewModelScope.launch {
            try {
                repository.setBitPerfect(enabled)
            } catch (e: IllegalStateException) {
                _state.update { it.copy(bitPerfect = previous) }
                repository.events.emitError(context.getString(R.string.error_save_bit_perfect))
            }
        }
    }

    /**
     * Shows [settings] at once, then applies them in the core. If the core rejects them, the saved
     * settings are shown again and the reason is shown as a message.
     */
    private fun applyOutput(settings: OutputSettings) {
        _state.update { it.copy(output = settings) }
        viewModelScope.launch {
            try {
                repository.setOutputSettings(settings)
            } catch (e: CoreException) {
                val saved = withContext(Dispatchers.IO) { repository.output.load() }
                _state.update { it.copy(output = saved) }
                repository.events.emitError(e.userMessage(context, R.string.error_output_settings))
            }
        }
    }

    /** Selects the ReplayGain mode (off, track or album) and applies it from the next song. */
    fun setReplayGainMode(mode: ReplayGainModeInfo) {
        applyReplayGain(_state.value.replayGain.copy(mode = mode))
    }

    /** Sets the ReplayGain pre-amp in dB and applies it from the next song. */
    fun setReplayGainPreamp(db: Float) {
        applyReplayGain(_state.value.replayGain.copy(preampDb = db))
    }

    /**
     * Shows [settings] at once, then saves and applies them. The repository saves before it applies,
     * so a failed save leaves the core unchanged; the previous settings are shown again and the
     * failure is shown as an error.
     */
    private fun applyReplayGain(settings: ReplayGainSettingsInfo) {
        val previous = _state.value.replayGain
        _state.update { it.copy(replayGain = settings) }
        viewModelScope.launch {
            try {
                repository.setReplayGain(settings)
            } catch (e: IllegalStateException) {
                _state.update { it.copy(replayGain = previous) }
                repository.events.emitError(context.getString(R.string.error_replay_gain_save))
            }
        }
    }

    /** Sets the cache size limit in bytes. */
    fun setCacheLimit(limitBytes: ULong) {
        applyCache(_state.value.cache.copy(limitBytes = limitBytes))
    }

    /** Sets how long unpinned cached songs are kept; null keeps them forever. */
    fun setKeepDays(days: UInt?) {
        applyCache(_state.value.cache.copy(ttlDays = days))
    }

    /** Turns downloads on mobile data on or off. Off means downloads wait for Wi-Fi. */
    fun setDownloadOnMetered(enabled: Boolean) {
        applyCache(_state.value.cache.copy(downloadOnMetered = enabled))
    }

    /**
     * Shows [settings] at once, then applies and saves them. If the save fails, the previous
     * settings are shown again and the reason is shown as an error.
     */
    private fun applyCache(settings: CacheSettings) {
        val previous = _state.value.cache
        _state.update { it.copy(cache = settings) }
        viewModelScope.launch {
            try {
                repository.setCacheSettings(settings)
                refreshCacheUsage()
            } catch (e: CoreException) {
                revertCache(previous)
            } catch (e: IllegalStateException) {
                revertCache(previous)
            }
        }
    }

    private fun revertCache(previous: CacheSettings) {
        _state.update { it.copy(cache = previous) }
        repository.events.emitError(context.getString(R.string.error_cache_save))
    }

    /** Deletes the unpinned cached songs and shows how much space was freed. */
    fun clearCache() {
        _state.update { it.copy(clearing = true, cacheMessage = null, cacheFailed = false) }
        viewModelScope.launch {
            try {
                val freed = repository.clearCache()
                _state.update {
                    it.copy(
                        clearing = false,
                        cacheMessage = context.getString(R.string.cache_freed, formatBytes(context, freed)),
                        cacheFailed = false,
                    )
                }
            } catch (e: CoreException) {
                _state.update {
                    it.copy(
                        clearing = false,
                        cacheMessage = e.userMessage(context, R.string.error_cache_clear),
                        cacheFailed = true,
                    )
                }
            }
            refreshCacheUsage()
        }
    }

    /** Starts a library scan. Does nothing while one is running. */
    fun startScan() {
        repository.startScan()
    }

    /** Syncs playlists and favorites with the bucket and shows the summary or the error. */
    fun syncUserData() {
        _state.update { it.copy(syncing = true, syncMessage = null, syncFailed = false) }
        viewModelScope.launch {
            try {
                val summary = repository.syncUserData()
                _state.update {
                    it.copy(
                        syncing = false,
                        syncMessage = context.getString(
                            R.string.sync_applied,
                            context.resources.getQuantityString(
                                R.plurals.sync_playlists_count,
                                summary.playlistsApplied.toInt(),
                                summary.playlistsApplied.toInt(),
                            ),
                            context.resources.getQuantityString(
                                R.plurals.sync_favorites_count,
                                summary.favoritesApplied.toInt(),
                                summary.favoritesApplied.toInt(),
                            ),
                        ),
                        syncFailed = false,
                    )
                }
            } catch (e: CoreException) {
                _state.update {
                    it.copy(
                        syncing = false,
                        syncMessage = e.userMessage(context, R.string.error_sync),
                        syncFailed = true,
                    )
                }
            }
        }
    }

    /** Reads how much the cache holds. A failure is shown inline. */
    private suspend fun refreshCacheUsage() {
        try {
            val usage = repository.cacheUsage()
            _state.update { it.copy(cacheUsage = usage) }
        } catch (e: CoreException) {
            _state.update {
                it.copy(cacheMessage = e.userMessage(context, R.string.error_cache_read), cacheFailed = true)
            }
        }
    }

    private suspend fun refreshStats() {
        try {
            val stats = repository.call { it.stats() }
            _state.update { it.copy(stats = stats, statsError = null) }
        } catch (e: CoreException) {
            _state.update { it.copy(statsError = e.userMessage(context, R.string.error_stats)) }
        }
    }
}

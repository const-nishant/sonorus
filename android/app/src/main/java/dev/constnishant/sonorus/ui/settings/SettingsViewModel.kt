package dev.constnishant.sonorus.ui.settings

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.BuildConfig
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.LibraryStatsInfo
import dev.constnishant.sonorus.core.S3Settings
import dev.constnishant.sonorus.core.coreVersion
import dev.constnishant.sonorus.data.ScanUi
import dev.constnishant.sonorus.data.SonorusRepository
import dev.constnishant.sonorus.data.userMessage
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Bucket provider presets. Picking one prefills the endpoint pattern and the region hint. */
enum class BucketProvider(val label: String, val endpointPattern: String, val regionHint: String) {
    /** Backblaze B2: replace `<region>` in the endpoint. */
    B2("Backblaze B2", "https://s3.<region>.backblazeb2.com", "us-west-004"),

    /** Cloudflare R2: replace `<account-id>` in the endpoint. */
    R2("Cloudflare R2", "https://<account-id>.r2.cloudflarestorage.com", "auto"),

    /** Any other S3-compatible service. Nothing is prefilled. */
    CUSTOM("Custom", "", ""),
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
class SettingsViewModel(private val repository: SonorusRepository) : ViewModel() {
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
            val version = withContext(Dispatchers.IO) { coreVersion() }
            _state.update { it.copy(version = version) }
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
                            found -> "Connected"
                            where != null -> "Connected, but there are no files under \"$where\""
                            else -> "Connected, but the bucket is empty"
                        },
                        bucketFailed = !found,
                    )
                }
            } catch (e: CoreException) {
                _state.update {
                    it.copy(
                        saving = false,
                        hasSettings = it.hasSettings || stored,
                        bucketMessage = e.userMessage("Could not connect to the bucket."),
                        bucketFailed = true,
                    )
                }
            } catch (e: Exception) {
                // Keystore or preferences failure while storing the settings (the core already
                // accepted them). The exception text is not shown: it may name key material.
                _state.update {
                    it.copy(
                        saving = false,
                        bucketMessage = "Could not store the bucket settings securely.",
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
                    bucketMessage = "Disconnected. The change takes effect after restart.",
                    bucketFailed = false,
                )
            }
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
                        syncMessage = "Applied ${summary.playlistsApplied} playlists, " +
                            "${summary.favoritesApplied} favorites",
                        syncFailed = false,
                    )
                }
            } catch (e: CoreException) {
                _state.update {
                    it.copy(
                        syncing = false,
                        syncMessage = e.userMessage("Sync failed."),
                        syncFailed = true,
                    )
                }
            }
        }
    }

    private suspend fun refreshStats() {
        try {
            val stats = repository.call { it.stats() }
            _state.update { it.copy(stats = stats, statsError = null) }
        } catch (e: CoreException) {
            _state.update { it.copy(statsError = e.userMessage("Could not read library stats.")) }
        }
    }
}

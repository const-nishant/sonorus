package dev.constnishant.sonorus.data

import android.content.Context
import dev.constnishant.sonorus.core.CoreConfig
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.OutputInfo
import dev.constnishant.sonorus.core.OutputSettings
import dev.constnishant.sonorus.core.S3Settings
import dev.constnishant.sonorus.core.SonorusCore
import dev.constnishant.sonorus.core.SyncSummary
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Entry point to the Rust core for the UI layer.
 *
 * The core is opened in the background as soon as the repository is created. Blocking core calls
 * run on [Dispatchers.IO]: use [call] from coroutines that need a result, and [command] for
 * fire-and-forget player commands. Core events are exposed through [events].
 */
class SonorusRepository(private val app: Context) {
    /** Persisted bucket settings (credentials encrypted with the Keystore). */
    val settings = BucketSettingsStore(app)

    /** Persisted output settings (sample format and exclusive mode). */
    val output = OutputSettingsStore(app)

    /** UI state built from core events. */
    val events = CoreEvents()

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val _configured = MutableStateFlow(false)

    /**
     * True when the core has a bucket backend. Set from the core after it opens, and after a
     * successful [configureBucket]. Note that [disconnectBucket] does not change it: the core keeps
     * its backend until the next launch.
     */
    val configured: StateFlow<Boolean> = _configured.asStateFlow()

    private val core: Deferred<SonorusCore> = scope.async {
        SonorusCore(
            CoreConfig(
                dataDir = File(app.filesDir, "sonorus").absolutePath,
                s3 = settings.load(),
                localMusicDir = null,
            ),
        ).also {
            it.setListener(events)
            it.setOutputSettings(output.load())
            _configured.value = it.isConfigured()
        }
    }

    /**
     * Runs [block] against the core on [Dispatchers.IO] and returns its result. Exceptions (for
     * example [CoreException]) propagate to the caller.
     */
    suspend fun <T> call(block: (SonorusCore) -> T): T =
        withContext(Dispatchers.IO) { block(core.await()) }

    /**
     * Runs a player or transport command in the background without waiting for it. A
     * [CoreException] is reported through [CoreEvents.errors] instead of being thrown.
     */
    fun command(block: (SonorusCore) -> Unit) {
        scope.launch {
            try {
                block(core.await())
            } catch (e: CoreException) {
                events.emitError(e.userMessage("Something went wrong."))
            }
        }
    }

    /**
     * Validates and applies bucket settings in the core, then persists them. Throws a
     * [CoreException] when the settings are rejected; nothing is saved in that case.
     */
    suspend fun configureBucket(s: S3Settings) {
        call { sonorus ->
            sonorus.configureS3(s)
            settings.save(s)
        }
        _configured.value = true
    }

    /**
     * Removes the stored bucket settings. The core keeps its current backend until the app
     * restarts, so the change takes effect on next launch.
     */
    suspend fun disconnectBucket() {
        withContext(Dispatchers.IO) { settings.clear() }
    }

    /**
     * Starts a library scan in the background. Does nothing if one is already running. The result
     * arrives through [CoreEvents.scan] and [CoreEvents.libraryChanged].
     */
    fun startScan() {
        if (events.scan.value.running) return
        events.scanStarted()
        scope.launch {
            try {
                core.await().scanLibrary()
            } catch (e: CoreException) {
                // Failures during the scan already arrived as a ScanFailed event; this also
                // covers the ones before it starts, which send no event. Shown once, in scan state.
                events.scanFailed(e.userMessage("The scan failed."))
            }
        }
    }

    /**
     * Syncs playlists and favorites with the bucket, then notifies [CoreEvents.libraryChanged].
     * Throws a [CoreException] on failure.
     */
    suspend fun syncUserData(): SyncSummary {
        val summary = call { it.syncUserData() }
        notifyLibraryChanged()
        return summary
    }

    /**
     * Applies output settings in the core and persists them. Does nothing when they equal the saved
     * settings. Changing them makes the core stop playback and clear the queue, so the playback
     * state is reset here too. Throws a [CoreException] when the core rejects the change; nothing
     * is saved in that case.
     */
    suspend fun setOutputSettings(settings: OutputSettings) {
        if (settings == withContext(Dispatchers.IO) { output.load() }) return
        call { it.setOutputSettings(settings) }
        withContext(Dispatchers.IO) { output.save(settings) }
        events.resetPlayback()
    }

    /** The output stream the core has open, or null when nothing is open. */
    suspend fun outputInfo(): OutputInfo? = call { it.outputInfo() }

    /**
     * Tells observers that the catalogue changed. Call this after playlist or favorite edits.
     */
    fun notifyLibraryChanged() {
        events.emitLibraryChanged()
    }
}

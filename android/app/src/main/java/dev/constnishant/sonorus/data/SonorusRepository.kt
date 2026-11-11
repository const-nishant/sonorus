package dev.constnishant.sonorus.data

import android.content.Context
import android.content.Intent
import android.database.Cursor
import android.net.Uri
import android.provider.OpenableColumns
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.CacheSettings
import dev.constnishant.sonorus.core.CacheUsageInfo
import dev.constnishant.sonorus.core.CoreConfig
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.DownloadStatus
import dev.constnishant.sonorus.core.OutputInfo
import dev.constnishant.sonorus.core.OutputSettings
import dev.constnishant.sonorus.core.ReplayGainSettingsInfo
import dev.constnishant.sonorus.core.S3Settings
import dev.constnishant.sonorus.core.SignalPathInfo
import dev.constnishant.sonorus.core.SleepTimerRequest
import dev.constnishant.sonorus.core.SonorusCore
import dev.constnishant.sonorus.core.SyncSummary
import dev.constnishant.sonorus.core.UploadJobInfo
import dev.constnishant.sonorus.core.UploadState
import dev.constnishant.sonorus.data.audio.UsbAudioRouter
import dev.constnishant.sonorus.data.audio.UsbDac
import dev.constnishant.sonorus.playback.AudioFocusGuard
import java.io.File
import java.io.IOException
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
 * The outcome of [SonorusRepository.prepareUploads]: how many files were queued, how many were
 * already in the bucket or the queue, and why each other file was skipped.
 */
data class PrepareResult(
    val queued: Int,
    val duplicates: Int,
    /** One user-facing reason per skipped file. */
    val skipped: List<String>,
)

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

    /** Persisted offline cache settings (limit, expiry, downloads on mobile data). */
    val cache = CacheSettingsStore(app)

    /** Persisted ReplayGain settings (mode and pre-amp). Applied to the core when it opens. */
    val replayGain = ReplayGainSettingsStore(app)

    /** Persisted recent search queries, newest first (at most [RECENT_SEARCH_LIMIT]). */
    val recentSearches = RecentSearchesStore(app)

    /** UI state built from core events. */
    val events = CoreEvents()

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /**
     * Chooses the output route for each stream open: bit-perfect to a USB DAC when enabled, the
     * mixer otherwise. Playback pauses when the DAC is unplugged so audio never jumps to the phone
     * speaker.
     */
    private val router = UsbAudioRouter(
        app,
        bitPerfectEnabled = { output.bitPerfect() },
        onDacDetached = { command { it.pause() } },
    )

    /**
     * Holds audio focus while the core plays, for every source of play (the app's screens and the
     * media session alike), and pauses on focus loss or when the output becomes noisy.
     */
    private val audioFocus = AudioFocusGuard(app, events.playback) { block -> command(block) }

    /** The connected USB DAC, or null when none is connected. */
    val dac: StateFlow<UsbDac?> get() = router.dac

    /** The signal path of the last stream open, or null before the first one. */
    val signalPath: StateFlow<SignalPathInfo?> get() = router.signalPath

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
            it.setOutputRouter(router)
            it.setListener(events)
            it.setOutputSettings(output.load())
            it.setCacheSettings(cache.load())
            it.setReplayGain(replayGain.load())
            _configured.value = it.isConfigured()
        }
    }

    /**
     * Sends the network state to the core, which pauses or resumes downloads. Commands wait for the
     * core, so the first report is applied once it is open.
     */
    private val networkMonitor = NetworkMonitor(app) { connected, metered ->
        command { it.setNetworkState(connected, metered) }
    }

    init {
        networkMonitor.start()
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
                events.emitError(e.userMessage(app, R.string.error_generic))
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
                events.scanFailed(e.userMessage(app, R.string.error_scan))
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

    /**
     * Stores the bit-perfect setting. It applies on the next stream open; the current stream is not
     * reopened. Throws if the setting cannot be saved.
     */
    suspend fun setBitPerfect(enabled: Boolean) {
        withContext(Dispatchers.IO) { output.setBitPerfect(enabled) }
    }

    /** The output stream the core has open, or null when nothing is open. */
    suspend fun outputInfo(): OutputInfo? = call { it.outputInfo() }

    /** The saved offline cache settings. Reads the preferences; blocking reads run on IO. */
    suspend fun cacheSettings(): CacheSettings = withContext(Dispatchers.IO) { cache.load() }

    /**
     * Applies the offline cache settings in the core and persists them. Throws if the preferences
     * cannot be written; the core keeps the new settings in that case until the app restarts.
     */
    suspend fun setCacheSettings(settings: CacheSettings) {
        call { it.setCacheSettings(settings) }
        withContext(Dispatchers.IO) { cache.save(settings) }
    }

    /** The saved ReplayGain settings. Reads the preferences; blocking reads run on IO. */
    suspend fun replayGainSettings(): ReplayGainSettingsInfo =
        withContext(Dispatchers.IO) { replayGain.load() }

    /**
     * Stores the ReplayGain settings and applies them in the core. They take effect from the next
     * track; the current one keeps its gain. Nothing is applied when the save fails, in which case
     * it throws if the preferences cannot be written.
     */
    suspend fun setReplayGain(settings: ReplayGainSettingsInfo) {
        withContext(Dispatchers.IO) { replayGain.save(settings) }
        call { it.setReplayGain(settings) }
    }

    /**
     * Sets, replaces or clears the sleep timer, then reads the timer state back from the core into
     * [events]. The read-back makes the countdown show even when no player runs yet, because the
     * core sends no event in that case. Throws a [CoreException] when the request is rejected.
     */
    suspend fun setSleepTimer(request: SleepTimerRequest) {
        val info = call { sonorus ->
            sonorus.setSleepTimer(request)
            sonorus.sleepTimer()
        }
        events.applySleepTimer(info.remainingMs, info.endOfTrack)
    }

    /** How much the offline cache holds, and the settings in effect. */
    suspend fun cacheUsage(): CacheUsageInfo = call { it.cacheUsage() }

    /**
     * Deletes the cached tracks that are not pinned. Returns the bytes freed. Pinned albums and
     * playlists stay. Throws a [CoreException].
     */
    suspend fun clearCache(): ULong = call { it.clearCache() }

    /** Pins or unpins an album for offline download. Throws a [CoreException]. */
    suspend fun setAlbumPinned(albumId: Long, pinned: Boolean) {
        call { it.setAlbumPinned(albumId, pinned) }
    }

    /** Pins or unpins a playlist for offline download. Throws a [CoreException]. */
    suspend fun setPlaylistPinned(playlistId: Long, pinned: Boolean) {
        call { it.setPlaylistPinned(playlistId, pinned) }
    }

    /** Whether the album is pinned. Throws a [CoreException]. */
    suspend fun isAlbumPinned(albumId: Long): Boolean = call { it.isAlbumPinned(albumId) }

    /** Whether the playlist is pinned. Throws a [CoreException]. */
    suspend fun isPlaylistPinned(playlistId: Long): Boolean = call { it.isPlaylistPinned(playlistId) }

    /**
     * How much of the album is on this device, including its pin state. Throws a [CoreException]
     * (for example NotFound for an unknown album).
     */
    suspend fun downloadStatusAlbum(albumId: Long): DownloadStatus = call { it.downloadStatusAlbum(albumId) }

    /** How much of the playlist is on this device, including its pin state. Throws a [CoreException]. */
    suspend fun downloadStatusPlaylist(playlistId: Long): DownloadStatus =
        call { it.downloadStatusPlaylist(playlistId) }

    /**
     * Tells observers that the catalogue changed. Call this after playlist or favorite edits.
     */
    fun notifyLibraryChanged() {
        events.emitLibraryChanged()
    }

    /**
     * Adds local audio files to the upload queue. Each file is read and hashed by the core; files
     * that are already in the bucket or the queue are counted as duplicates. A file that cannot be
     * added is skipped with its reason, and the others are still added. Blocking reads run on
     * [Dispatchers.IO].
     */
    suspend fun prepareUploads(uris: List<Uri>): PrepareResult = withContext(Dispatchers.IO) {
        val sonorus = core.await()
        var queued = 0
        var duplicates = 0
        val skipped = mutableListOf<String>()
        for (uri in uris) {
            val name = displayName(uri)
            try {
                takeReadPermission(uri)
                val pfd = app.contentResolver.openFileDescriptor(uri, "r")
                    ?: throw IOException("no descriptor for $name")
                // The core owns the descriptor from here on, on every path.
                val job = sonorus.uploadPrepare(pfd.detachFd(), uri.toString(), name)
                if (job.state == UploadState.DUPLICATE) duplicates++ else queued++
            } catch (e: CoreException) {
                skipped += e.userMessage(app, R.string.error_add_file, name)
            } catch (e: IOException) {
                skipped += app.getString(R.string.error_read_file, name)
            } catch (e: SecurityException) {
                skipped += app.getString(R.string.error_read_file, name)
            }
        }
        PrepareResult(queued, duplicates, skipped)
    }

    /** All upload jobs, newest first. */
    suspend fun uploads(): List<UploadJobInfo> = call { it.uploads() }

    /** True when at least one upload is queued or was interrupted while uploading. */
    suspend fun hasPendingUploads(): Boolean = try {
        uploads().any { it.state == UploadState.QUEUED || it.state == UploadState.UPLOADING }
    } catch (e: CoreException) {
        false
    }

    /** Deletes the finished upload jobs (done, duplicate or cancelled). Returns how many. */
    suspend fun clearFinishedUploads(): UInt = call { it.clearFinishedUploads() }

    /**
     * Cancels an upload job. A running job stops after its current part and then reports
     * [UploadEvent.Finished]; a queued or failed job is cancelled now. Runs in the background; a
     * failure is shown through [CoreEvents.errors].
     */
    fun cancelUpload(id: Long) {
        command { sonorus ->
            sonorus.uploadCancel(id)
            events.emitUploadsChanged()
        }
    }

    /**
     * Runs upload job [jobId] from the descriptor [fd], which the core takes ownership of. Blocks
     * for the whole upload; the returned job is `Done`, `Failed`, `Cancelled` or `Duplicate`.
     */
    suspend fun runUpload(jobId: Long, fd: Int): UploadJobInfo = call { it.uploadRun(jobId, fd) }

    /**
     * Cancels job [jobId] and waits until the core has done so, then tells the upload screens.
     * Throws a [CoreException].
     */
    suspend fun cancelUploadNow(jobId: Long) {
        call { it.uploadCancel(jobId) }
        events.emitUploadsChanged()
    }

    /** The name of a picked file: its display name, else the last segment of its URI. */
    private fun displayName(uri: Uri): String {
        val queried = try {
            app.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
                ?.use { cursor: Cursor ->
                    if (cursor.moveToFirst()) cursor.getString(0) else null
                }
        } catch (e: SecurityException) {
            null
        }
        return queried?.takeIf { it.isNotBlank() }
            ?: uri.lastPathSegment?.substringAfterLast('/')?.takeIf { it.isNotBlank() }
            ?: app.getString(R.string.file_name_fallback)
    }

    /**
     * Keeps read access to [uri] across restarts, so an interrupted upload can be resumed. Providers
     * that do not allow it leave the access for this session only.
     */
    private fun takeReadPermission(uri: Uri) {
        try {
            app.contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
        } catch (e: SecurityException) {
            // Not persistable: the source may be unreadable after a restart. The job is then
            // cancelled when it runs.
        }
    }
}

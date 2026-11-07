package dev.constnishant.sonorus.data

import dev.constnishant.sonorus.core.CoreEvent
import dev.constnishant.sonorus.core.EventListener
import dev.constnishant.sonorus.core.PlaybackState
import dev.constnishant.sonorus.core.QueueItem
import dev.constnishant.sonorus.core.RepeatMode
import dev.constnishant.sonorus.core.ScanProgressInfo
import dev.constnishant.sonorus.core.ScanSummary
import dev.constnishant.sonorus.core.StreamInfoRecord
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.core.UploadState
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/** The retry the core is waiting on after a network drop. */
data class Recovery(val attempt: UInt, val retryInMs: ULong)

/** Playback state as the UI sees it. Built from core events by [CoreEvents]. */
data class PlaybackUi(
    val state: PlaybackState = PlaybackState.IDLE,
    val track: TrackInfo? = null,
    val entryId: ULong? = null,
    val positionMs: Long = 0,
    val durationMs: Long? = null,
    val stream: StreamInfoRecord? = null,
    val queue: List<QueueItem> = emptyList(),
    val shuffle: Boolean = false,
    val repeat: RepeatMode = RepeatMode.OFF,
    val recovery: Recovery? = null,
)

/**
 * Offline download worker state as the UI sees it. [doneTracks] and [totalTracks] count the
 * pinned tracks across all pins, not one album. [active] is true from a progress event until the
 * worker reports idle.
 */
data class DownloadsUi(
    val active: Boolean = false,
    val doneTracks: UInt = 0u,
    val totalTracks: UInt = 0u,
    val pendingTracks: UInt = 0u,
)

/** Library scan state as the UI sees it. */
data class ScanUi(
    val running: Boolean = false,
    val progress: ScanProgressInfo? = null,
    val lastSummary: ScanSummary? = null,
    val error: String? = null,
)

/** Upload queue changes as the UI sees them. Built from core events by [CoreEvents]. */
sealed interface UploadEvent {
    /** A running upload stored more bytes. */
    data class Progress(val jobId: Long, val bytesDone: ULong, val total: ULong) : UploadEvent

    /** An upload run ended in [state]: done, failed (resumable) or cancelled. */
    data class Finished(val jobId: Long, val state: UploadState) : UploadEvent

    /** The queue changed outside a run, for example a job was cancelled while not running. */
    data object Changed : UploadEvent
}

/**
 * Receives core events on a Rust thread and turns them into UI state flows.
 *
 * [onEvent] only updates thread-safe flows and returns; it never calls back into the core.
 * Errors and library-changed notifications are one-shot flows with a small buffer that drops the
 * oldest entry on overflow.
 */
class CoreEvents : EventListener {
    private val _playback = MutableStateFlow(PlaybackUi())

    /** Current playback state. Updated from the core's event thread. */
    val playback: StateFlow<PlaybackUi> = _playback.asStateFlow()

    private val _scan = MutableStateFlow(ScanUi())

    /** Current library scan state. */
    val scan: StateFlow<ScanUi> = _scan.asStateFlow()

    private val _errors = MutableSharedFlow<String>(
        extraBufferCapacity = 16,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )

    /** Human-readable error messages to show once (playback errors, failed commands). */
    val errors: SharedFlow<String> = _errors.asSharedFlow()

    private val _libraryChanged = MutableSharedFlow<Unit>(
        extraBufferCapacity = 16,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )

    /** Emits when the catalogue changed (scan finished, sync applied, playlists edited). */
    val libraryChanged: SharedFlow<Unit> = _libraryChanged.asSharedFlow()

    private val _downloads = MutableStateFlow(DownloadsUi())

    /**
     * Offline download worker state. Every download progress event changes [DownloadsUi.doneTracks],
     * so collectors see one emission per finished track.
     */
    val downloads: StateFlow<DownloadsUi> = _downloads.asStateFlow()

    private val _uploadProgress = MutableStateFlow<Map<Long, Pair<ULong, ULong>>>(emptyMap())

    /**
     * Latest `bytesDone` to `total` for each upload job that is running. An entry is removed when
     * the job's run ends.
     */
    val uploadProgress: StateFlow<Map<Long, Pair<ULong, ULong>>> = _uploadProgress.asStateFlow()

    private val _uploadEvents = MutableSharedFlow<UploadEvent>(
        extraBufferCapacity = 64,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )

    /** Upload progress and run ends, and queue changes made outside a run. */
    val uploadEvents: SharedFlow<UploadEvent> = _uploadEvents.asSharedFlow()

    /** Applies a core event. Called on the core's thread; must stay fast and non-blocking. */
    override fun onEvent(event: CoreEvent) {
        when (event) {
            is CoreEvent.StateChanged -> _playback.update { it.copy(state = event.state) }
            is CoreEvent.TrackChanged -> _playback.update {
                it.copy(
                    track = event.track,
                    entryId = event.entryId,
                    positionMs = 0,
                    durationMs = event.track?.durationMs?.toLong(),
                    stream = null,
                    recovery = null,
                )
            }
            is CoreEvent.TrackLoaded -> _playback.update {
                if (event.trackId == it.track?.id) it.copy(stream = event.stream) else it
            }
            is CoreEvent.Position -> _playback.update {
                val current = it.track
                if (current != null && event.trackId != current.id) {
                    it
                } else {
                    it.copy(
                        positionMs = event.positionMs.toLong(),
                        durationMs = event.durationMs?.toLong() ?: it.durationMs,
                        recovery = null,
                    )
                }
            }
            is CoreEvent.Recovering -> _playback.update {
                it.copy(recovery = Recovery(event.attempt, event.retryInMs))
            }
            is CoreEvent.PlaybackError -> _errors.tryEmit(event.message)
            is CoreEvent.QueueChanged -> _playback.update {
                it.copy(queue = event.items, shuffle = event.shuffle, repeat = event.repeat)
            }
            is CoreEvent.TrackEnded -> Unit
            is CoreEvent.ScanProgress -> _scan.update {
                it.copy(running = true, progress = event.progress)
            }
            is CoreEvent.ScanFinished -> {
                _scan.update { it.copy(running = false, lastSummary = event.summary, error = null) }
                _libraryChanged.tryEmit(Unit)
            }
            // The repository replaces this raw text with a user message when `scanLibrary` throws;
            // whichever of the two arrives first, the user message is the one that stays.
            is CoreEvent.ScanFailed -> _scan.update {
                it.copy(running = false, error = it.error ?: event.message)
            }
            is CoreEvent.UploadProgress -> {
                _uploadProgress.update { it + (event.jobId to (event.bytesDone to event.total)) }
                _uploadEvents.tryEmit(UploadEvent.Progress(event.jobId, event.bytesDone, event.total))
            }
            is CoreEvent.UploadFinished -> {
                _uploadProgress.update { it - event.jobId }
                _uploadEvents.tryEmit(UploadEvent.Finished(event.jobId, event.state))
            }
            is CoreEvent.DownloadProgress -> _downloads.update {
                it.copy(
                    active = true,
                    doneTracks = event.doneTracks,
                    totalTracks = event.totalTracks,
                )
            }
            is CoreEvent.DownloadsIdle -> _downloads.update {
                it.copy(active = false, pendingTracks = event.pendingTracks)
            }
        }
    }

    /** Marks a library scan as started and clears the previous error. */
    fun scanStarted() {
        _scan.update { it.copy(running = true, error = null) }
    }

    /**
     * Marks a scan as failed. Used when `scanLibrary` throws before it starts (not configured,
     * already running): the core sends no `ScanFailed` event in that case.
     */
    internal fun scanFailed(message: String) {
        _scan.update { it.copy(running = false, error = message) }
    }

    /**
     * Resets playback state to its initial value. Used when the core drops its player without
     * sending an event, for example when the output settings change.
     */
    internal fun resetPlayback() {
        _playback.value = PlaybackUi()
    }

    /** Publishes an error message to [errors]. */
    internal fun emitError(message: String) {
        _errors.tryEmit(message)
    }

    /** Publishes a library-changed notification to [libraryChanged]. */
    internal fun emitLibraryChanged() {
        _libraryChanged.tryEmit(Unit)
    }

    /** Publishes [UploadEvent.Changed] to [uploadEvents]. */
    internal fun emitUploadsChanged() {
        _uploadEvents.tryEmit(UploadEvent.Changed)
    }
}

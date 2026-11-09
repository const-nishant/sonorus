package dev.constnishant.sonorus.ui.player

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.RepeatMode
import dev.constnishant.sonorus.core.SignalPathInfo
import dev.constnishant.sonorus.core.SleepTimerRequest
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.data.PlaybackUi
import dev.constnishant.sonorus.data.SleepTimerUi
import dev.constnishant.sonorus.data.SonorusRepository
import dev.constnishant.sonorus.data.audio.UsbDac
import dev.constnishant.sonorus.data.userMessage
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

/** How long an undo waits for the core to report a re-added queue entry. */
private const val RESTORE_TIMEOUT_MS = 5_000L

/** A favorite flag the user set for [trackId], shown until the track changes. */
data class FavoriteOverride(val trackId: Long, val favorite: Boolean)

/**
 * State and commands for the Now Playing screen and its queue sheet. Player commands are
 * fire-and-forget through the repository; favorites and track lookups await the core on IO.
 */
class NowPlayingViewModel(
    application: Application,
    private val repository: SonorusRepository,
) : AndroidViewModel(application) {
    private val context: Application = application
    /** Playback state from the core. */
    val playback: StateFlow<PlaybackUi> = repository.events.playback

    /** The signal path of the last stream open, or null before the first one. */
    val signalPath: StateFlow<SignalPathInfo?> = repository.signalPath

    /** The connected USB DAC, or null when none is connected. */
    val dac: StateFlow<UsbDac?> = repository.dac

    /** The sleep timer: the deadline of a timed sleep, or end-of-track. */
    val sleepTimer: StateFlow<SleepTimerUi> = repository.events.sleepTimer

    private val _favoriteOverride = MutableStateFlow<FavoriteOverride?>(null)

    /** Optimistic favorite flag for the current track, or null to use the track's own flag. */
    val favoriteOverride: StateFlow<FavoriteOverride?> = _favoriteOverride.asStateFlow()

    private val _tracks = MutableStateFlow<Map<Long, TrackInfo?>>(emptyMap())

    /**
     * Track info for queue entries, keyed by track id. A missing key means "still loading"; a
     * null value means the lookup failed.
     */
    val tracks: StateFlow<Map<Long, TrackInfo?>> = _tracks.asStateFlow()

    /** Track ids already requested. Only touched on the main thread. */
    private val requested = HashSet<Long>()

    init {
        // A new track starts with the flag from its own snapshot again.
        viewModelScope.launch {
            playback.map { it.track?.id }.distinctUntilChanged().collect {
                _favoriteOverride.value = null
            }
        }
    }

    /** Plays when the play button shows play, pauses when it shows pause. */
    fun togglePlayPause() {
        val pause = playback.value.state.showsPause()
        repository.command { if (pause) it.pause() else it.play() }
    }

    /**
     * Sets, replaces or clears the sleep timer. The countdown changes once the core accepts the
     * request. A rejected request is shown through the error flow.
     */
    fun setSleepTimer(request: SleepTimerRequest) {
        viewModelScope.launch {
            try {
                repository.setSleepTimer(request)
            } catch (e: CoreException) {
                repository.events.emitError(e.userMessage(context, R.string.error_sleep_timer))
            }
        }
    }

    /** Skips to the next queue entry. */
    fun next() {
        repository.command { it.next() }
    }

    /** Goes to the previous queue entry. */
    fun previous() {
        repository.command { it.previous() }
    }

    /** Seeks the current track to [positionMs] (negative values are clamped to zero). */
    fun seek(positionMs: Long) {
        val target = positionMs.coerceAtLeast(0L).toULong()
        repository.command { it.seek(target) }
    }

    /** Turns shuffle on or off. */
    fun toggleShuffle() {
        val on = !playback.value.shuffle
        repository.command { it.setShuffle(on) }
    }

    /** Cycles the repeat mode: off, then all, then one, then off. */
    fun cycleRepeat() {
        val next = when (playback.value.repeat) {
            RepeatMode.OFF -> RepeatMode.ALL
            RepeatMode.ALL -> RepeatMode.ONE
            RepeatMode.ONE -> RepeatMode.OFF
        }
        repository.command { it.setRepeat(next) }
    }

    /** Starts playing the queue entry [entryId]. */
    fun jumpTo(entryId: ULong) {
        repository.command { it.jumpTo(entryId) }
    }

    /** Removes the queue entry [entryId]. */
    fun removeFromQueue(entryId: ULong) {
        repository.command { it.removeFromQueue(entryId) }
    }

    /** Moves the queue entry [entryId] to position [toIndex] in the queue. */
    fun moveInQueue(entryId: ULong, toIndex: Int) {
        val to = toIndex.toUInt()
        repository.command { it.moveInQueue(entryId, to) }
    }

    /**
     * Puts [trackId] back in the queue at [index] after a removal was undone. The core only adds to
     * the end, so the new entry is found in the queue state and then moved to [index].
     */
    fun restoreToQueue(trackId: Long, index: Int) {
        viewModelScope.launch {
            val known = playback.value.queue.map { it.entryId }.toSet()
            try {
                repository.call { it.append(listOf(trackId)) }
            } catch (e: CoreException) {
                repository.events.emitError(e.userMessage(context, R.string.error_generic))
                return@launch
            }
            val added = withTimeoutOrNull(RESTORE_TIMEOUT_MS) {
                playback.first { state -> state.queue.any { it.entryId !in known } }
                    .queue.last { it.entryId !in known }
            } ?: return@launch
            moveInQueue(added.entryId, index)
        }
    }

    /**
     * Sets the favorite flag of [trackId]. The new value shows at once; if the core rejects the
     * change, the previous value is restored and the error is shown.
     */
    fun setFavorite(trackId: Long, favorite: Boolean) {
        val optimistic = FavoriteOverride(trackId, favorite)
        _favoriteOverride.value = optimistic
        viewModelScope.launch {
            try {
                repository.call { it.setFavorite(trackId, favorite) }
                repository.notifyLibraryChanged()
            } catch (e: CoreException) {
                if (_favoriteOverride.value == optimistic) {
                    _favoriteOverride.value = FavoriteOverride(trackId, !favorite)
                }
                repository.events.emitError(e.userMessage(context, R.string.error_favorite))
            }
        }
    }

    /**
     * Looks up the tracks behind [trackIds] for the queue sheet. Each id is requested once;
     * lookups run one after another on IO and failures are stored as null.
     */
    fun resolveTracks(trackIds: List<Long>) {
        val fresh = trackIds.filter { requested.add(it) }
        if (fresh.isEmpty()) return
        viewModelScope.launch {
            for (id in fresh) {
                val track = try {
                    repository.call { it.track(id) }
                } catch (e: CoreException) {
                    null
                }
                _tracks.update { it + (id to track) }
            }
        }
    }
}

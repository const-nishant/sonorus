package dev.constnishant.sonorus.ui.screens

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.PlaylistEntryInfo
import dev.constnishant.sonorus.core.PlaylistInfo
import dev.constnishant.sonorus.data.DownloadsUi
import dev.constnishant.sonorus.data.SonorusRepository
import dev.constnishant.sonorus.data.userMessage
import dev.constnishant.sonorus.ui.components.PinUi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** A playlist with its entries in order. */
data class PlaylistData(val playlist: PlaylistInfo, val entries: List<PlaylistEntryInfo>)

/**
 * Backs the Playlist screen: loads the playlist and its entries, starts playback, and edits the
 * entries (remove, move, rename, delete).
 */
class PlaylistViewModel(
    application: Application,
    private val repository: SonorusRepository,
    private val playlistId: Long,
) : AndroidViewModel(application) {
    private val context: Application = application
    private val _state = MutableStateFlow<LoadState<PlaylistData>>(LoadState.Loading)

    /** The playlist, or the reason it could not be loaded. */
    val state: StateFlow<LoadState<PlaylistData>> = _state.asStateFlow()

    private val _pin = MutableStateFlow(PinUi())

    /** Whether the playlist is kept for offline listening, and how much of it is on this device. */
    val pin: StateFlow<PinUi> = _pin.asStateFlow()

    /** The offline download worker state, for the status line under the header. */
    val downloads: StateFlow<DownloadsUi> = repository.events.downloads

    init {
        viewModelScope.launch {
            repository.libraryChangeSignals().collectLatest {
                _state.refresh(context) {
                    repository.call { core ->
                        val playlist = core.playlists().firstOrNull { it.id == playlistId }
                            ?: throw CoreException.NotFound("playlist $playlistId")
                        PlaylistData(playlist, core.playlistEntries(playlistId))
                    }
                }
                refreshPin()
            }
        }
        // Every download progress or idle event may change the playlist's status. The first value
        // of the state flow also loads it.
        viewModelScope.launch {
            repository.events.downloads.collectLatest { refreshPin() }
        }
    }

    /** Reads the playlist's pin and cache status. A failed read keeps the last one. */
    private suspend fun refreshPin() {
        try {
            val status = repository.downloadStatusPlaylist(playlistId)
            _pin.value = PinUi(pinned = status.pinned, status = status)
        } catch (e: CoreException) {
            // Keep the last known state.
        }
    }

    /**
     * Keeps the playlist for offline listening, or removes that. The toggle shows the new value at
     * once; a failure is shown as a message and the real state is read back.
     */
    fun setPinned(pinned: Boolean) {
        _pin.update { it.copy(pinned = pinned) }
        viewModelScope.launch {
            try {
                repository.setPlaylistPinned(playlistId, pinned)
            } catch (e: CoreException) {
                repository.events.emitError(e.userMessage(context, R.string.error_download))
            }
            refreshPin()
        }
    }

    /** Plays the playlist from the start, with shuffle on or off. */
    fun playAll(shuffle: Boolean) {
        repository.command { core ->
            core.setShuffle(shuffle)
            core.playPlaylist(playlistId, 0u)
        }
    }

    /** Plays the playlist starting at the entry with [position]. */
    fun playFrom(position: UInt) {
        repository.command { core -> core.playPlaylist(playlistId, position) }
    }

    /** Removes the entry at [position]. */
    fun remove(position: UInt) {
        repository.command { core ->
            core.removeFromPlaylist(playlistId, position)
            repository.notifyLibraryChanged()
        }
    }

    /** Moves the entry at [from] to [to]. */
    fun move(from: UInt, to: UInt) {
        repository.command { core ->
            core.moveInPlaylist(playlistId, from, to)
            repository.notifyLibraryChanged()
        }
    }

    /**
     * Puts [trackId] back at [position] after a removal was undone. The core only adds to the end,
     * so the track is appended and then moved to [position].
     */
    fun restoreToPlaylist(trackId: Long, position: UInt) {
        repository.command { core ->
            core.addToPlaylist(playlistId, listOf(trackId))
            val last = core.playlistEntries(playlistId).lastIndex.toUInt()
            core.moveInPlaylist(playlistId, last, position)
            repository.notifyLibraryChanged()
        }
    }

    /** Renames the playlist to [name]. */
    fun rename(name: String) {
        repository.command { core ->
            core.renamePlaylist(playlistId, name)
            repository.notifyLibraryChanged()
        }
    }

    /** Deletes the playlist. Its tracks stay in the library. */
    fun delete() {
        repository.command { core ->
            core.deletePlaylist(playlistId)
            repository.notifyLibraryChanged()
        }
    }
}

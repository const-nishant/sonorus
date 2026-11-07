package dev.constnishant.sonorus.ui.screens

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.PlaylistEntryInfo
import dev.constnishant.sonorus.core.PlaylistInfo
import dev.constnishant.sonorus.data.SonorusRepository
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch

/** A playlist with its entries in order. */
data class PlaylistData(val playlist: PlaylistInfo, val entries: List<PlaylistEntryInfo>)

/**
 * Backs the Playlist screen: loads the playlist and its entries, starts playback, and edits the
 * entries (remove, move, rename, delete).
 */
class PlaylistViewModel(
    private val repository: SonorusRepository,
    private val playlistId: Long,
) : ViewModel() {
    private val _state = MutableStateFlow<LoadState<PlaylistData>>(LoadState.Loading)

    /** The playlist, or the reason it could not be loaded. */
    val state: StateFlow<LoadState<PlaylistData>> = _state.asStateFlow()

    init {
        viewModelScope.launch {
            repository.libraryChangeSignals().collectLatest {
                _state.refresh {
                    repository.call { core ->
                        val playlist = core.playlists().firstOrNull { it.id == playlistId }
                            ?: throw CoreException.NotFound("playlist $playlistId")
                        PlaylistData(playlist, core.playlistEntries(playlistId))
                    }
                }
            }
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

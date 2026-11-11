package dev.constnishant.sonorus.ui.screens

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.core.SmartPlaylistKind
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.data.SonorusRepository
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch

/** The most tracks a smart playlist lists. */
const val SMART_PLAYLIST_LIMIT = 500u

/**
 * Backs the Smart playlist screen: loads the tracks of one built-in smart playlist and starts
 * playback from them. The list reloads whenever the catalogue changes.
 */
class SmartPlaylistViewModel(
    application: Application,
    private val repository: SonorusRepository,
    private val kind: SmartPlaylistKind,
) : AndroidViewModel(application) {
    private val context: Application = application
    private val _state = MutableStateFlow<LoadState<List<TrackInfo>>>(LoadState.Loading)

    /** The tracks of the playlist, or the reason they could not be loaded. */
    val state: StateFlow<LoadState<List<TrackInfo>>> = _state.asStateFlow()

    init {
        viewModelScope.launch {
            repository.libraryChangeSignals().collectLatest {
                _state.refresh(context) {
                    repository.call { core -> core.smartPlaylist(kind, SMART_PLAYLIST_LIMIT) }
                }
            }
        }
    }

    /** Replaces the queue with the whole list and starts it from the first track, with shuffle on or off. */
    fun playAll(shuffle: Boolean) {
        val ids = loadedIds()
        if (ids.isEmpty()) return
        repository.command { core ->
            core.setShuffle(shuffle)
            core.playTracks(ids, 0u)
        }
    }

    /** Replaces the queue with the list and starts it from the track at [index] (0-based). */
    fun playFrom(index: Int) {
        val ids = loadedIds()
        if (ids.isEmpty()) return
        repository.command { core -> core.playTracks(ids, index.toUInt()) }
    }

    /** The ids of the loaded tracks in list order, or none while they are not loaded. */
    private fun loadedIds(): List<Long> =
        (_state.value as? LoadState.Ready)?.value?.map { it.id }.orEmpty()
}

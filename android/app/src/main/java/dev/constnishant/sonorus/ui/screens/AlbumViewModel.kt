package dev.constnishant.sonorus.ui.screens

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.core.AlbumInfo
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.data.SonorusRepository
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch

/** An album with its tracks in disc and track order. */
data class AlbumData(val album: AlbumInfo, val tracks: List<TrackInfo>)

/** Backs the Album screen: loads the album and its tracks and starts playback from it. */
class AlbumViewModel(
    private val repository: SonorusRepository,
    private val albumId: Long,
) : ViewModel() {
    private val _state = MutableStateFlow<LoadState<AlbumData>>(LoadState.Loading)

    /** The album, or the reason it could not be loaded. */
    val state: StateFlow<LoadState<AlbumData>> = _state.asStateFlow()

    init {
        viewModelScope.launch {
            repository.libraryChangeSignals().collectLatest {
                _state.refresh {
                    repository.call { core -> AlbumData(core.album(albumId), core.albumTracks(albumId)) }
                }
            }
        }
    }

    /** Plays the whole album from the first track, with shuffle on or off. */
    fun playAll(shuffle: Boolean) {
        repository.command { core ->
            core.setShuffle(shuffle)
            core.playAlbum(albumId, 0u)
        }
    }

    /** Plays the album starting at track [index] (0-based) of the list. */
    fun playFrom(index: Int) {
        repository.command { core -> core.playAlbum(albumId, index.toUInt()) }
    }
}

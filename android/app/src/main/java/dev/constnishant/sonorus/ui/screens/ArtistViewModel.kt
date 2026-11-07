package dev.constnishant.sonorus.ui.screens

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.core.AlbumInfo
import dev.constnishant.sonorus.core.ArtistInfo
import dev.constnishant.sonorus.data.SonorusRepository
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch

/** An artist with their albums. */
data class ArtistData(val artist: ArtistInfo, val albums: List<AlbumInfo>)

/** Backs the Artist screen: loads the artist and their albums, and plays all their tracks. */
class ArtistViewModel(
    application: Application,
    private val repository: SonorusRepository,
    private val artistId: Long,
) : AndroidViewModel(application) {
    private val context: Application = application
    private val _state = MutableStateFlow<LoadState<ArtistData>>(LoadState.Loading)

    /** The artist, or the reason it could not be loaded. */
    val state: StateFlow<LoadState<ArtistData>> = _state.asStateFlow()

    init {
        viewModelScope.launch {
            repository.libraryChangeSignals().collectLatest {
                _state.refresh(context) {
                    repository.call { core -> ArtistData(core.artist(artistId), core.artistAlbums(artistId)) }
                }
            }
        }
    }

    /** Plays every track of every album by the artist, album by album, from the start. */
    fun playAll() {
        repository.command { core ->
            val ids = core.artistAlbums(artistId)
                .flatMap { album -> core.albumTracks(album.id) }
                .map { track -> track.id }
            if (ids.isNotEmpty()) {
                core.playTracks(ids, 0u)
            }
        }
    }
}

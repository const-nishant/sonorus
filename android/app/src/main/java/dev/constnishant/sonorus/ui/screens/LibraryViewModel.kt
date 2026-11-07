package dev.constnishant.sonorus.ui.screens

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.core.AlbumInfo
import dev.constnishant.sonorus.core.AlbumSort
import dev.constnishant.sonorus.core.ArtistInfo
import dev.constnishant.sonorus.core.PlaylistInfo
import dev.constnishant.sonorus.data.SonorusRepository
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch

/**
 * Backs the Library screen: albums (sorted), artists and playlists, plus playlist create, rename
 * and delete. Lists reload whenever the catalogue changes.
 */
class LibraryViewModel(private val repository: SonorusRepository) : ViewModel() {
    private val _sort = MutableStateFlow(AlbumSort.ARTIST)

    /** The album sort order. */
    val sort: StateFlow<AlbumSort> = _sort.asStateFlow()

    private val _albums = MutableStateFlow<LoadState<List<AlbumInfo>>>(LoadState.Loading)

    /** Albums in [sort] order. */
    val albums: StateFlow<LoadState<List<AlbumInfo>>> = _albums.asStateFlow()

    private val _artists = MutableStateFlow<LoadState<List<ArtistInfo>>>(LoadState.Loading)

    /** All artists. */
    val artists: StateFlow<LoadState<List<ArtistInfo>>> = _artists.asStateFlow()

    private val _playlists = MutableStateFlow<LoadState<List<PlaylistInfo>>>(LoadState.Loading)

    /** All playlists. */
    val playlists: StateFlow<LoadState<List<PlaylistInfo>>> = _playlists.asStateFlow()

    init {
        viewModelScope.launch {
            combine(_sort, repository.libraryChangeSignals()) { sort, _ -> sort }
                .collectLatest { sort ->
                    _albums.refresh { repository.call { core -> core.albums(sort, 2000u, 0u) } }
                }
        }
        viewModelScope.launch {
            repository.libraryChangeSignals().collectLatest {
                _artists.refresh { repository.call { core -> core.artists(2000u, 0u) } }
                _playlists.refresh { repository.call { core -> core.playlists() } }
            }
        }
    }

    /** Changes the album sort order and reloads the albums. */
    fun setSort(sort: AlbumSort) {
        _sort.value = sort
    }

    /** Creates an empty playlist called [name]. */
    fun createPlaylist(name: String) {
        repository.command { core ->
            core.createPlaylist(name)
            repository.notifyLibraryChanged()
        }
    }

    /** Renames the playlist [id] to [name]. */
    fun renamePlaylist(id: Long, name: String) {
        repository.command { core ->
            core.renamePlaylist(id, name)
            repository.notifyLibraryChanged()
        }
    }

    /** Deletes the playlist [id]. Its tracks stay in the library. */
    fun deletePlaylist(id: Long) {
        repository.command { core ->
            core.deletePlaylist(id)
            repository.notifyLibraryChanged()
        }
    }
}

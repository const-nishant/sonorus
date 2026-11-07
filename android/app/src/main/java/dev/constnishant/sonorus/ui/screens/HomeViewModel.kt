package dev.constnishant.sonorus.ui.screens

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.core.AlbumInfo
import dev.constnishant.sonorus.core.AlbumSort
import dev.constnishant.sonorus.core.LibraryStatsInfo
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.data.SonorusRepository
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch

/** The sections of the Home screen. */
data class HomeData(
    val stats: LibraryStatsInfo,
    val recentTracks: List<TrackInfo>,
    val recentAlbums: List<AlbumInfo>,
    val recentlyAdded: List<AlbumInfo>,
) {
    /** True when the library has no tracks. */
    val isEmpty: Boolean get() = stats.tracks == 0uL
}

/** Loads the Home sections and reloads them whenever the catalogue changes. */
class HomeViewModel(
    application: Application,
    private val repository: SonorusRepository,
) : AndroidViewModel(application) {
    private val context: Application = application
    private val _state = MutableStateFlow<LoadState<HomeData>>(LoadState.Loading)

    /** The Home sections, or the reason they could not be loaded. */
    val state: StateFlow<LoadState<HomeData>> = _state.asStateFlow()

    init {
        viewModelScope.launch {
            repository.libraryChangeSignals().collectLatest {
                _state.refresh(context) {
                    repository.call { core ->
                        HomeData(
                            stats = core.stats(),
                            recentTracks = core.recentlyPlayed(20u),
                            recentAlbums = core.recentAlbums(20u),
                            recentlyAdded = core.albums(AlbumSort.RECENTLY_ADDED, 20u, 0u),
                        )
                    }
                }
            }
        }
    }
}

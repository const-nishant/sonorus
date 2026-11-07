package dev.constnishant.sonorus.ui.screens

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.AlbumInfo
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.TrackInfo
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

/** An album with its tracks in disc and track order. */
data class AlbumData(val album: AlbumInfo, val tracks: List<TrackInfo>)

/** Backs the Album screen: loads the album and its tracks and starts playback from it. */
class AlbumViewModel(
    application: Application,
    private val repository: SonorusRepository,
    private val albumId: Long,
) : AndroidViewModel(application) {
    private val context: Application = application
    private val _state = MutableStateFlow<LoadState<AlbumData>>(LoadState.Loading)

    /** The album, or the reason it could not be loaded. */
    val state: StateFlow<LoadState<AlbumData>> = _state.asStateFlow()

    private val _pin = MutableStateFlow(PinUi())

    /** Whether the album is kept for offline listening, and how much of it is on this device. */
    val pin: StateFlow<PinUi> = _pin.asStateFlow()

    /** The offline download worker state, for the status line under the header. */
    val downloads: StateFlow<DownloadsUi> = repository.events.downloads

    init {
        viewModelScope.launch {
            repository.libraryChangeSignals().collectLatest {
                _state.refresh(context) {
                    repository.call { core -> AlbumData(core.album(albumId), core.albumTracks(albumId)) }
                }
                refreshPin()
            }
        }
        // Every download progress or idle event may change the album's status. The first value
        // of the state flow also loads it.
        viewModelScope.launch {
            repository.events.downloads.collectLatest { refreshPin() }
        }
    }

    /** Reads the album's pin and cache status. A failed read keeps the last one. */
    private suspend fun refreshPin() {
        try {
            val status = repository.downloadStatusAlbum(albumId)
            _pin.value = PinUi(pinned = status.pinned, status = status)
        } catch (e: CoreException) {
            // Keep the last known state.
        }
    }

    /**
     * Keeps the album for offline listening, or removes that. The toggle shows the new value at
     * once; a failure is shown as a message and the real state is read back.
     */
    fun setPinned(pinned: Boolean) {
        _pin.update { it.copy(pinned = pinned) }
        viewModelScope.launch {
            try {
                repository.setAlbumPinned(albumId, pinned)
            } catch (e: CoreException) {
                repository.events.emitError(e.userMessage(context, R.string.error_download))
            }
            refreshPin()
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

package dev.constnishant.sonorus.ui.components

import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.platform.LocalContext
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.data.SonorusRepository
import dev.constnishant.sonorus.data.repository
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/**
 * The track menu actions shared by every track list: play next, add to queue, favorite, and
 * playlist membership. Fire-and-forget actions report failures to the snackbar through the
 * repository. Create one with [rememberTrackActions].
 */
class TrackActions(
    private val repository: SonorusRepository,
    private val scope: CoroutineScope,
) {
    /** Inserts [ids] right after the current track in the queue. */
    fun playNext(ids: List<Long>) {
        repository.command { core -> core.playNext(ids) }
    }

    /** Appends [ids] to the end of the queue. */
    fun addToQueue(ids: List<Long>) {
        repository.command { core -> core.append(ids) }
    }

    /**
     * Sets the favorite flag of [track], then notifies library observers. [onError] runs on the
     * main thread when the change was rejected, so the caller can revert its optimistic state.
     */
    fun setFavorite(track: TrackInfo, favorite: Boolean, onError: () -> Unit = {}) {
        scope.launch {
            try {
                repository.call { core -> core.setFavorite(track.id, favorite) }
                repository.notifyLibraryChanged()
            } catch (e: CoreException) {
                onError()
                repository.events.emitError(e.message?.takeIf { it.isNotBlank() } ?: e.toString())
            }
        }
    }

    /** Adds [ids] to the playlist [playlistId]. */
    fun addToPlaylist(playlistId: Long, ids: List<Long>) {
        repository.command { core ->
            core.addToPlaylist(playlistId, ids)
            repository.notifyLibraryChanged()
        }
    }

    /** Creates a playlist called [name] containing [ids]. */
    fun createPlaylistAndAdd(name: String, ids: List<Long>) {
        repository.command { core ->
            val playlist = core.createPlaylist(name)
            repository.notifyLibraryChanged()
            core.addToPlaylist(playlist.id, ids)
            repository.notifyLibraryChanged()
        }
    }
}

/** A [TrackActions] bound to the app repository and the composition's coroutine scope. */
@Composable
fun rememberTrackActions(): TrackActions {
    val repository = LocalContext.current.repository
    val scope = rememberCoroutineScope()
    return remember(repository, scope) { TrackActions(repository, scope) }
}

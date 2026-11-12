package dev.constnishant.sonorus.ui.components

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.data.SonorusRepository
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.data.userMessage
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/**
 * Opens the Manage bucket screen at a bucket folder path (relative to the prefix, `""` for the
 * root). The app shell provides it; without a provider it does nothing.
 */
val LocalOpenBucketFolder = compositionLocalOf<(String) -> Unit> { {} }

/** True while a bucket is configured. The bucket menu items are shown only then. */
@Composable
fun rememberBucketConfigured(): State<Boolean> {
    val repository = LocalContext.current.repository
    return repository.configured.collectAsStateWithLifecycle()
}

/** The "Show in bucket" actions of a track or album menu. Create one with [rememberBucketMenuActions]. */
class BucketMenuActions(
    private val repository: SonorusRepository,
    private val context: Context,
    private val scope: CoroutineScope,
    private val open: (String) -> Unit,
) {
    /** Opens the bucket folder that holds [track]. */
    fun showTrack(track: TrackInfo) {
        scope.launch { open(repository.bucketFolderOf(track.key)) }
    }

    /** Opens the bucket folder that holds album [albumId]. A failed read is shown as a message. */
    fun showAlbum(albumId: Long) {
        scope.launch {
            try {
                open(repository.bucketFolderOfAlbum(albumId))
            } catch (e: CoreException) {
                repository.events.emitError(e.userMessage(context, R.string.error_generic))
            }
        }
    }
}

/** A [BucketMenuActions] bound to the app repository, the navigation provider and the composition's scope. */
@Composable
fun rememberBucketMenuActions(): BucketMenuActions {
    val repository = LocalContext.current.repository
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val open = LocalOpenBucketFolder.current
    return remember(repository, context, scope, open) { BucketMenuActions(repository, context, scope, open) }
}

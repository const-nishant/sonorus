@file:OptIn(ExperimentalMaterial3Api::class)

package dev.constnishant.sonorus.ui.screens

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.TrendingUp
import androidx.compose.material.icons.rounded.ErrorOutline
import androidx.compose.material.icons.rounded.Favorite
import androidx.compose.material.icons.rounded.HighQuality
import androidx.compose.material.icons.rounded.History
import androidx.compose.material.icons.rounded.HourglassEmpty
import androidx.compose.material.icons.rounded.NewReleases
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.Shuffle
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModelProvider.AndroidViewModelFactory.Companion.APPLICATION_KEY
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.SmartPlaylistKind
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.BackButton
import dev.constnishant.sonorus.ui.components.CenteredContent
import dev.constnishant.sonorus.ui.components.EmptyState
import dev.constnishant.sonorus.ui.components.LoadingContent
import dev.constnishant.sonorus.ui.components.TrackActions
import dev.constnishant.sonorus.ui.components.TrackRow
import dev.constnishant.sonorus.ui.components.rememberCurrentTrackId
import dev.constnishant.sonorus.ui.components.rememberTrackActions
import dev.constnishant.sonorus.ui.components.songsText

/** The smart playlists in the order the Library tab lists them. */
internal val SMART_PLAYLISTS: List<SmartPlaylistKind> = listOf(
    SmartPlaylistKind.MOST_PLAYED30_DAYS,
    SmartPlaylistKind.MOST_PLAYED_ALL_TIME,
    SmartPlaylistKind.RECENTLY_ADDED,
    SmartPlaylistKind.HI_RES,
    SmartPlaylistKind.NEVER_PLAYED,
    SmartPlaylistKind.FAVORITES,
)

/** The icon of a smart playlist. */
internal fun SmartPlaylistKind.icon(): ImageVector = when (this) {
    SmartPlaylistKind.MOST_PLAYED30_DAYS -> Icons.AutoMirrored.Rounded.TrendingUp
    SmartPlaylistKind.MOST_PLAYED_ALL_TIME -> Icons.Rounded.History
    SmartPlaylistKind.RECENTLY_ADDED -> Icons.Rounded.NewReleases
    SmartPlaylistKind.HI_RES -> Icons.Rounded.HighQuality
    SmartPlaylistKind.NEVER_PLAYED -> Icons.Rounded.HourglassEmpty
    SmartPlaylistKind.FAVORITES -> Icons.Rounded.Favorite
}

/** The name of a smart playlist. */
@StringRes
internal fun SmartPlaylistKind.titleRes(): Int = when (this) {
    SmartPlaylistKind.MOST_PLAYED30_DAYS -> R.string.smart_most_played_30_title
    SmartPlaylistKind.MOST_PLAYED_ALL_TIME -> R.string.smart_most_played_all_title
    SmartPlaylistKind.RECENTLY_ADDED -> R.string.smart_recently_added_title
    SmartPlaylistKind.HI_RES -> R.string.smart_hi_res_title
    SmartPlaylistKind.NEVER_PLAYED -> R.string.smart_never_played_title
    SmartPlaylistKind.FAVORITES -> R.string.smart_favorites_title
}

/** The one-line description of a smart playlist, shown in the Library tab. */
@StringRes
internal fun SmartPlaylistKind.descriptionRes(): Int = when (this) {
    SmartPlaylistKind.MOST_PLAYED30_DAYS -> R.string.smart_most_played_30_desc
    SmartPlaylistKind.MOST_PLAYED_ALL_TIME -> R.string.smart_most_played_all_desc
    SmartPlaylistKind.RECENTLY_ADDED -> R.string.smart_recently_added_desc
    SmartPlaylistKind.HI_RES -> R.string.smart_hi_res_desc
    SmartPlaylistKind.NEVER_PLAYED -> R.string.smart_never_played_desc
    SmartPlaylistKind.FAVORITES -> R.string.smart_favorites_desc
}

/** The message shown when a smart playlist has no tracks. */
@StringRes
private fun SmartPlaylistKind.emptyMessageRes(): Int = when (this) {
    SmartPlaylistKind.MOST_PLAYED30_DAYS -> R.string.smart_empty_most_played_30
    SmartPlaylistKind.MOST_PLAYED_ALL_TIME -> R.string.smart_empty_most_played_all
    SmartPlaylistKind.RECENTLY_ADDED -> R.string.smart_empty_recently_added
    SmartPlaylistKind.HI_RES -> R.string.quality_empty_hi_res
    SmartPlaylistKind.NEVER_PLAYED -> R.string.smart_empty_never_played
    SmartPlaylistKind.FAVORITES -> R.string.smart_empty_favorites
}

/**
 * A read-only list of one smart playlist: a header with the song count and Play / Shuffle, then
 * the tracks. Tapping a track plays the list from that track. Reloads when the library changes.
 *
 * @param kind the smart playlist to show.
 * @param onBack leaves the screen.
 * @param modifier applied to the root layout.
 */
@Composable
fun SmartPlaylistScreen(kind: SmartPlaylistKind, onBack: () -> Unit, modifier: Modifier = Modifier) {
    val repository = LocalContext.current.repository
    val viewModel: SmartPlaylistViewModel = viewModel(
        key = "smart-${kind.name}",
        factory = viewModelFactory {
            initializer { SmartPlaylistViewModel(checkNotNull(this[APPLICATION_KEY]), repository, kind) }
        },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    val currentId by rememberCurrentTrackId()
    val actions = rememberTrackActions()

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(stringResource(kind.titleRes()), maxLines = 1, overflow = TextOverflow.Ellipsis) },
                navigationIcon = { BackButton(onBack) },
            )
        },
    ) { padding ->
        when (val s = state) {
            LoadState.Loading -> LoadingContent(Modifier.padding(padding))
            is LoadState.Failed -> CenteredContent(Modifier.padding(padding)) {
                EmptyState(Icons.Rounded.ErrorOutline, stringResource(R.string.smart_open_failed), s.message)
            }
            is LoadState.Ready -> SmartPlaylistContent(
                kind = kind,
                tracks = s.value,
                currentId = currentId,
                actions = actions,
                onPlayAll = viewModel::playAll,
                onPlayFrom = viewModel::playFrom,
                modifier = Modifier.padding(padding),
            )
        }
    }
}

/** The loaded smart playlist: the count with Play and Shuffle, then the read-only track list. */
@Composable
private fun SmartPlaylistContent(
    kind: SmartPlaylistKind,
    tracks: List<TrackInfo>,
    currentId: Long?,
    actions: TrackActions,
    onPlayAll: (Boolean) -> Unit,
    onPlayFrom: (Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    LazyColumn(modifier.fillMaxSize()) {
        item(key = "header") {
            Column(
                Modifier
                    .fillMaxWidth()
                    .padding(16.dp),
            ) {
                Text(
                    songsText(tracks.size),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.height(12.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalAlignment = Alignment.CenterVertically) {
                    Button(onClick = { onPlayAll(false) }, enabled = tracks.isNotEmpty()) {
                        Icon(Icons.Rounded.PlayArrow, contentDescription = null, modifier = Modifier.size(ButtonDefaults.IconSize))
                        Spacer(Modifier.size(ButtonDefaults.IconSpacing))
                        Text(stringResource(R.string.action_play))
                    }
                    OutlinedButton(onClick = { onPlayAll(true) }, enabled = tracks.isNotEmpty()) {
                        Icon(Icons.Rounded.Shuffle, contentDescription = null, modifier = Modifier.size(ButtonDefaults.IconSize))
                        Spacer(Modifier.size(ButtonDefaults.IconSpacing))
                        Text(stringResource(R.string.action_shuffle))
                    }
                }
            }
        }
        if (tracks.isEmpty()) {
            item(key = "empty") {
                EmptyState(
                    kind.icon(),
                    stringResource(kind.titleRes()),
                    stringResource(kind.emptyMessageRes()),
                )
            }
        }
        itemsIndexed(tracks, key = { _, track -> track.id }) { index, track ->
            TrackRow(
                track = track,
                onClick = { onPlayFrom(index) },
                showArtwork = true,
                isCurrent = track.id == currentId,
                actions = actions,
            )
        }
    }
}

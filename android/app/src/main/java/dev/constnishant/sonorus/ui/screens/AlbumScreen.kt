@file:OptIn(ExperimentalMaterial3Api::class)

package dev.constnishant.sonorus.ui.screens

import androidx.compose.foundation.clickable
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
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.ErrorOutline
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.Shuffle
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ButtonDefaults
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.Artwork
import dev.constnishant.sonorus.ui.components.BackButton
import dev.constnishant.sonorus.ui.components.CenteredContent
import dev.constnishant.sonorus.ui.components.EmptyState
import dev.constnishant.sonorus.ui.components.LoadingContent
import dev.constnishant.sonorus.ui.components.TrackActions
import dev.constnishant.sonorus.ui.components.TrackRow
import dev.constnishant.sonorus.ui.components.formatTotal
import dev.constnishant.sonorus.ui.components.isHiRes
import dev.constnishant.sonorus.ui.components.qualityLabel
import dev.constnishant.sonorus.ui.components.rememberCurrentTrackId
import dev.constnishant.sonorus.ui.components.rememberTrackActions

/**
 * The Album screen: cover and details, Play / Shuffle, and the track list grouped under disc
 * headers when the album spans more than one disc.
 *
 * @param albumId the album to show.
 * @param onArtist opens the album artist.
 * @param onBack leaves the screen.
 * @param modifier applied to the root layout.
 */
@Composable
fun AlbumScreen(albumId: Long, onArtist: (Long) -> Unit, onBack: () -> Unit, modifier: Modifier = Modifier) {
    val repository = LocalContext.current.repository
    val viewModel: AlbumViewModel = viewModel(
        key = "album-$albumId",
        factory = viewModelFactory { initializer { AlbumViewModel(repository, albumId) } },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    val currentId by rememberCurrentTrackId()
    val actions = rememberTrackActions()
    val title = (state as? LoadState.Ready)?.value?.album?.title.orEmpty()

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(title, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                navigationIcon = { BackButton(onBack) },
            )
        },
    ) { padding ->
        when (val s = state) {
            LoadState.Loading -> LoadingContent(Modifier.padding(padding))
            is LoadState.Failed -> CenteredContent(Modifier.padding(padding)) {
                EmptyState(Icons.Rounded.ErrorOutline, "Couldn't open album", s.message)
            }
            is LoadState.Ready -> AlbumContent(
                data = s.value,
                currentId = currentId,
                actions = actions,
                onArtist = onArtist,
                onPlayAll = viewModel::playAll,
                onPlayFrom = viewModel::playFrom,
                modifier = Modifier.padding(padding),
            )
        }
    }
}

/** The loaded album: header and the track list. */
@Composable
private fun AlbumContent(
    data: AlbumData,
    currentId: Long?,
    actions: TrackActions,
    onArtist: (Long) -> Unit,
    onPlayAll: (Boolean) -> Unit,
    onPlayFrom: (Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    val album = data.album
    val tracks = data.tracks
    val multiDisc = tracks.mapNotNull { it.discNo }.distinct().size > 1
    val first = tracks.firstOrNull()

    LazyColumn(modifier.fillMaxSize()) {
        item(key = "header") {
            Column(
                Modifier
                    .fillMaxWidth()
                    .padding(16.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Artwork(
                    path = album.coverPath,
                    contentDescription = album.title,
                    modifier = Modifier.size(200.dp),
                    cornerRadius = 12.dp,
                )
                Spacer(Modifier.height(16.dp))
                Text(
                    album.title,
                    style = MaterialTheme.typography.headlineSmall,
                    textAlign = TextAlign.Center,
                )
                Text(
                    album.artist,
                    style = MaterialTheme.typography.titleSmall,
                    color = MaterialTheme.colorScheme.primary,
                    textAlign = TextAlign.Center,
                    modifier = Modifier
                        .clickable { onArtist(album.artistId) }
                        .padding(vertical = 4.dp),
                )
                Text(
                    listOfNotNull(
                        album.year?.toString(),
                        "${album.trackCount} songs",
                        formatTotal(album.durationMs.toLong()),
                    ).joinToString(" · "),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
                val quality = first?.qualityLabel()
                if (first != null && quality != null) {
                    Text(
                        quality,
                        style = MaterialTheme.typography.labelMedium,
                        color = if (first.isHiRes()) {
                            MaterialTheme.colorScheme.tertiary
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                    )
                }
                Spacer(Modifier.height(16.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    Button(onClick = { onPlayAll(false) }, enabled = tracks.isNotEmpty()) {
                        Icon(Icons.Rounded.PlayArrow, contentDescription = null, modifier = Modifier.size(ButtonDefaults.IconSize))
                        Spacer(Modifier.size(ButtonDefaults.IconSpacing))
                        Text("Play")
                    }
                    OutlinedButton(onClick = { onPlayAll(true) }, enabled = tracks.isNotEmpty()) {
                        Icon(Icons.Rounded.Shuffle, contentDescription = null, modifier = Modifier.size(ButtonDefaults.IconSize))
                        Spacer(Modifier.size(ButtonDefaults.IconSpacing))
                        Text("Shuffle")
                    }
                }
            }
        }
        tracks.forEachIndexed { index, track ->
            if (multiDisc && (index == 0 || tracks[index - 1].discNo != track.discNo)) {
                item(key = "disc-$index") {
                    Text(
                        track.discNo?.let { "Disc $it" } ?: "Other tracks",
                        style = MaterialTheme.typography.titleSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                    )
                }
            }
            item(key = "track-${track.id}") {
                TrackRow(
                    track = track,
                    onClick = { onPlayFrom(index) },
                    number = track.trackNo?.toInt() ?: (index + 1),
                    isCurrent = track.id == currentId,
                    actions = actions,
                )
            }
        }
    }
}

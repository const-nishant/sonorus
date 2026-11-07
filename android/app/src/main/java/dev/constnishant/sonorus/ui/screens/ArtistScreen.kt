@file:OptIn(ExperimentalMaterial3Api::class)

package dev.constnishant.sonorus.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Album
import androidx.compose.material.icons.rounded.ErrorOutline
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.AlbumCard
import dev.constnishant.sonorus.ui.components.BackButton
import dev.constnishant.sonorus.ui.components.CenteredContent
import dev.constnishant.sonorus.ui.components.EmptyState
import dev.constnishant.sonorus.ui.components.LoadingContent

/**
 * The Artist screen: name, album and track counts, a Play all button, and the album grid.
 *
 * @param artistId the artist to show.
 * @param onAlbum opens an album.
 * @param onBack leaves the screen.
 * @param modifier applied to the root layout.
 */
@Composable
fun ArtistScreen(artistId: Long, onAlbum: (Long) -> Unit, onBack: () -> Unit, modifier: Modifier = Modifier) {
    val repository = LocalContext.current.repository
    val viewModel: ArtistViewModel = viewModel(
        key = "artist-$artistId",
        factory = viewModelFactory { initializer { ArtistViewModel(repository, artistId) } },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    val title = (state as? LoadState.Ready)?.value?.artist?.name.orEmpty()

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
                EmptyState(Icons.Rounded.ErrorOutline, "Couldn't open artist", s.message)
            }
            is LoadState.Ready -> ArtistContent(
                data = s.value,
                onAlbum = onAlbum,
                onPlayAll = viewModel::playAll,
                modifier = Modifier.padding(padding),
            )
        }
    }
}

/** The loaded artist: header, then the album grid. */
@Composable
private fun ArtistContent(
    data: ArtistData,
    onAlbum: (Long) -> Unit,
    onPlayAll: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val artist = data.artist
    LazyVerticalGrid(
        columns = GridCells.Adaptive(150.dp),
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        item(key = "header", span = { GridItemSpan(maxLineSpan) }) {
            Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
                Text(
                    "${artist.albumCount} albums · ${artist.trackCount} tracks",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.height(12.dp))
                Button(
                    onClick = onPlayAll,
                    enabled = artist.trackCount > 0u,
                ) {
                    Icon(Icons.Rounded.PlayArrow, contentDescription = null, modifier = Modifier.size(ButtonDefaults.IconSize))
                    Spacer(Modifier.size(ButtonDefaults.IconSpacing))
                    Text("Play all")
                }
            }
        }
        if (data.albums.isEmpty()) {
            item(key = "empty", span = { GridItemSpan(maxLineSpan) }) {
                EmptyState(
                    Icons.Rounded.Album,
                    "No albums",
                    "This artist has no albums in the library.",
                )
            }
        }
        items(data.albums, key = { it.id }) { album ->
            AlbumCard(album = album, onClick = { onAlbum(album.id) })
        }
    }
}

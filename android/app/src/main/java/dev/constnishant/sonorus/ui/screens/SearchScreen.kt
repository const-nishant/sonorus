@file:OptIn(ExperimentalMaterial3Api::class)

package dev.constnishant.sonorus.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Clear
import androidx.compose.material.icons.rounded.ErrorOutline
import androidx.compose.material.icons.rounded.Person
import androidx.compose.material.icons.rounded.Search
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.constnishant.sonorus.core.SearchResult
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.AlbumCard
import dev.constnishant.sonorus.ui.components.CenteredContent
import dev.constnishant.sonorus.ui.components.EmptyState
import dev.constnishant.sonorus.ui.components.LoadingContent
import dev.constnishant.sonorus.ui.components.SectionTitle
import dev.constnishant.sonorus.ui.components.TrackActions
import dev.constnishant.sonorus.ui.components.TrackRow
import dev.constnishant.sonorus.ui.components.rememberCurrentTrackId
import dev.constnishant.sonorus.ui.components.rememberTrackActions

/**
 * The Search screen. The field is focused on entry; results are grouped into songs, albums and
 * artists. Tapping a song starts playback of all the song results from that one.
 *
 * @param onAlbum opens an album from the results.
 * @param onArtist opens an artist from the results.
 * @param modifier applied to the root layout.
 */
@Composable
fun SearchScreen(onAlbum: (Long) -> Unit, onArtist: (Long) -> Unit, modifier: Modifier = Modifier) {
    val repository = LocalContext.current.repository
    val viewModel: SearchViewModel = viewModel(
        factory = viewModelFactory { initializer { SearchViewModel(repository) } },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    val currentId by rememberCurrentTrackId()
    val actions = rememberTrackActions()
    val focusRequester = remember { FocusRequester() }

    LaunchedEffect(Unit) {
        focusRequester.requestFocus()
    }

    Scaffold(
        modifier = modifier,
        topBar = { TopAppBar(title = { Text("Search") }) },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            OutlinedTextField(
                value = state.query,
                onValueChange = viewModel::onQueryChange,
                singleLine = true,
                placeholder = { Text("Songs, albums, artists") },
                leadingIcon = { Icon(Icons.Rounded.Search, contentDescription = null) },
                trailingIcon = {
                    if (state.query.isNotEmpty()) {
                        IconButton(onClick = { viewModel.onQueryChange("") }) {
                            Icon(Icons.Rounded.Clear, contentDescription = "Clear search")
                        }
                    }
                },
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp)
                    .focusRequester(focusRequester),
            )

            val trimmed = state.term
            if (state.query.isBlank()) {
                CenteredContent {
                    EmptyState(
                        Icons.Rounded.Search,
                        "Search your library",
                        "Find songs, albums and artists.",
                    )
                }
            } else {
                when (val results = state.results) {
                    LoadState.Loading -> LoadingContent()
                    is LoadState.Failed -> CenteredContent {
                        EmptyState(Icons.Rounded.ErrorOutline, "Search failed", results.message)
                    }
                    is LoadState.Ready -> {
                        val result = results.value
                        val empty = result.tracks.isEmpty() && result.albums.isEmpty() && result.artists.isEmpty()
                        if (!empty) {
                            SearchResults(
                                result = result,
                                currentId = currentId,
                                onAlbum = onAlbum,
                                onArtist = onArtist,
                                onPlayTrack = { index ->
                                    val ids = result.tracks.map { it.id }
                                    repository.command { core -> core.playTracks(ids, index.toUInt()) }
                                },
                                actions = actions,
                            )
                        } else if (trimmed == state.query.trim()) {
                            CenteredContent {
                                EmptyState(
                                    Icons.Rounded.Search,
                                    "No results for \"$trimmed\"",
                                    "Try a different spelling or fewer words.",
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

/** The grouped result list: songs, then albums (a shelf), then artists. */
@Composable
private fun SearchResults(
    result: SearchResult,
    currentId: Long?,
    onAlbum: (Long) -> Unit,
    onArtist: (Long) -> Unit,
    onPlayTrack: (Int) -> Unit,
    actions: TrackActions,
) {
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 16.dp)) {
        if (result.tracks.isNotEmpty()) {
            item(key = "songs-title") { SectionTitle("Songs") }
            itemsIndexed(result.tracks, key = { _, track -> "track-${track.id}" }) { index, track ->
                TrackRow(
                    track = track,
                    onClick = { onPlayTrack(index) },
                    showArtwork = true,
                    isCurrent = track.id == currentId,
                    actions = actions,
                )
            }
        }
        if (result.albums.isNotEmpty()) {
            item(key = "albums-title") { SectionTitle("Albums") }
            item(key = "albums-row") {
                LazyRow(
                    contentPadding = PaddingValues(horizontal = 16.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    items(result.albums, key = { it.id }) { album ->
                        AlbumCard(
                            album = album,
                            onClick = { onAlbum(album.id) },
                            modifier = Modifier.width(140.dp),
                        )
                    }
                }
            }
            item(key = "albums-gap") { Spacer(Modifier.height(8.dp)) }
        }
        if (result.artists.isNotEmpty()) {
            item(key = "artists-title") { SectionTitle("Artists") }
            items(result.artists, key = { "artist-${it.id}" }) { artist ->
                ListItem(
                    modifier = Modifier.clickable { onArtist(artist.id) },
                    headlineContent = {
                        Text(artist.name, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    },
                    supportingContent = {
                        Text("${artist.albumCount} albums · ${artist.trackCount} tracks")
                    },
                    leadingContent = { Icon(Icons.Rounded.Person, contentDescription = null) },
                )
            }
        }
    }
}

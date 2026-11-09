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
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Cloud
import androidx.compose.material.icons.rounded.ErrorOutline
import androidx.compose.material.icons.rounded.LibraryMusic
import androidx.compose.material.icons.rounded.Settings
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModelProvider.AndroidViewModelFactory.Companion.APPLICATION_KEY
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.AlbumInfo
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.AlbumCard
import dev.constnishant.sonorus.ui.components.Artwork
import dev.constnishant.sonorus.ui.components.CenteredContent
import dev.constnishant.sonorus.ui.components.EmptyState
import dev.constnishant.sonorus.ui.components.LoadingContent
import dev.constnishant.sonorus.ui.components.Section
import dev.constnishant.sonorus.ui.components.albumsText
import dev.constnishant.sonorus.ui.components.rememberCurrentTrackId
import dev.constnishant.sonorus.ui.components.tracksText

/**
 * The Home screen: recently played tracks and albums, recently added albums, and library totals.
 * When the library is empty it asks the user to connect a bucket or scan one instead.
 *
 * @param onAlbum opens the album with the given id.
 * @param onOpenSettings opens Settings (top bar button and the empty-library prompts).
 * @param modifier applied to the root layout.
 */
@Composable
fun HomeScreen(onAlbum: (Long) -> Unit, onOpenSettings: () -> Unit, modifier: Modifier = Modifier) {
    val repository = LocalContext.current.repository
    val viewModel: HomeViewModel = viewModel(
        factory = viewModelFactory { initializer { HomeViewModel(checkNotNull(this[APPLICATION_KEY]), repository) } },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    val configured by repository.configured.collectAsStateWithLifecycle()
    val scanning by viewModel.scanning.collectAsStateWithLifecycle()
    val currentId by rememberCurrentTrackId()

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.app_name)) },
                actions = {
                    IconButton(onClick = onOpenSettings) {
                        Icon(Icons.Rounded.Settings, contentDescription = stringResource(R.string.cd_settings))
                    }
                },
            )
        },
    ) { padding ->
        PullToRefreshBox(
            isRefreshing = scanning,
            onRefresh = viewModel::refresh,
            modifier = Modifier.padding(padding),
        ) {
            when (val s = state) {
                LoadState.Loading -> LoadingContent()
                is LoadState.Failed -> CenteredContent {
                    EmptyState(Icons.Rounded.ErrorOutline, stringResource(R.string.home_load_failed), s.message)
                }
                is LoadState.Ready -> {
                    val data = s.value
                    if (data.isEmpty) {
                        CenteredContent {
                            if (configured) {
                                EmptyState(
                                    icon = Icons.Rounded.LibraryMusic,
                                    title = stringResource(R.string.home_empty_title),
                                    message = stringResource(R.string.scan_bucket_hint),
                                    action = { Button(onClick = onOpenSettings) { Text(stringResource(R.string.action_open_settings)) } },
                                )
                            } else {
                                EmptyState(
                                    icon = Icons.Rounded.Cloud,
                                    title = stringResource(R.string.home_connect_title),
                                    message = stringResource(R.string.home_connect_message),
                                    action = { Button(onClick = onOpenSettings) { Text(stringResource(R.string.action_open_settings)) } },
                                )
                            }
                        }
                    } else {
                        HomeContent(
                            data = data,
                            currentId = currentId,
                            onAlbum = onAlbum,
                            onPlayTrack = { index ->
                                val ids = data.recentTracks.map { it.id }
                                repository.command { core -> core.playTracks(ids, index.toUInt()) }
                            },
                        )
                    }
                }
            }
        }
    }
}

/** The populated Home layout: the three shelves that have content, then the totals. */
@Composable
private fun HomeContent(
    data: HomeData,
    currentId: Long?,
    onAlbum: (Long) -> Unit,
    onPlayTrack: (Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    LazyColumn(
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(vertical = 16.dp),
        verticalArrangement = Arrangement.spacedBy(24.dp),
    ) {
        if (data.recentTracks.isNotEmpty()) {
            item(key = "recent-tracks") {
                Section(stringResource(R.string.home_recently_played)) {
                    LazyRow(
                        state = rememberShelfState(data.recentTracks.firstOrNull()?.id),
                        contentPadding = PaddingValues(horizontal = 16.dp),
                        horizontalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        itemsIndexed(data.recentTracks, key = { _, track -> track.id }) { index, track ->
                            RecentTrackCard(
                                track = track,
                                isCurrent = track.id == currentId,
                                onClick = { onPlayTrack(index) },
                            )
                        }
                    }
                }
            }
        }
        if (data.recentAlbums.isNotEmpty()) {
            item(key = "recent-albums") {
                Section(stringResource(R.string.home_recently_played_albums)) {
                    AlbumRow(data.recentAlbums, onAlbum)
                }
            }
        }
        if (data.recentlyAdded.isNotEmpty()) {
            item(key = "recently-added") {
                Section(stringResource(R.string.home_recently_added)) {
                    AlbumRow(data.recentlyAdded, onAlbum)
                }
            }
        }
        item(key = "stats") {
            Text(
                tracksText(data.stats.tracks.toInt()) + " · " + albumsText(data.stats.albums.toInt()),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
            )
        }
    }
}

/**
 * Scroll state for a horizontal shelf. A keyed lazy row keeps its first visible item in place when
 * items are inserted before it, so a shelf that fills while a scan runs would open scrolled past
 * the newest entry. When [firstKey] changes and the user has not scrolled away, jump back to it.
 */
@Composable
private fun rememberShelfState(firstKey: Any?): LazyListState {
    val state = rememberLazyListState()
    LaunchedEffect(firstKey) {
        if (state.firstVisibleItemIndex <= 1) state.scrollToItem(0)
    }
    return state
}

/** A horizontal shelf of album cards. */
@Composable
private fun AlbumRow(albums: List<AlbumInfo>, onAlbum: (Long) -> Unit) {
    LazyRow(
        state = rememberShelfState(albums.firstOrNull()?.id),
        contentPadding = PaddingValues(horizontal = 16.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        items(albums, key = { it.id }) { album ->
            AlbumCard(
                album = album,
                onClick = { onAlbum(album.id) },
                modifier = Modifier.width(140.dp),
            )
        }
    }
}

/** A small square cover with the track title, for the "Recently played" shelf. */
@Composable
private fun RecentTrackCard(track: TrackInfo, isCurrent: Boolean, onClick: () -> Unit) {
    Column(Modifier.width(120.dp).clickable(role = Role.Button, onClick = onClick)) {
        Artwork(
            path = track.coverPath,
            contentDescription = track.album,
            modifier = Modifier.size(120.dp),
            shape = MaterialTheme.shapes.small,
        )
        Spacer(Modifier.height(4.dp))
        Text(
            track.title,
            style = MaterialTheme.typography.bodyMedium,
            color = if (isCurrent) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        Text(
            track.artist,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

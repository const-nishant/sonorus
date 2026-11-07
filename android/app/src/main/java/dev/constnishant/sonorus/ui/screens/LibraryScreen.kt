@file:OptIn(ExperimentalMaterial3Api::class)

package dev.constnishant.sonorus.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items as gridItems
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.QueueMusic
import androidx.compose.material.icons.automirrored.rounded.Sort
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.rounded.Album
import androidx.compose.material.icons.rounded.Check
import androidx.compose.material.icons.rounded.Delete
import androidx.compose.material.icons.rounded.Edit
import androidx.compose.material.icons.rounded.ErrorOutline
import androidx.compose.material.icons.rounded.MoreVert
import androidx.compose.material.icons.rounded.Person
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.PrimaryTabRow
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
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
import dev.constnishant.sonorus.core.AlbumInfo
import dev.constnishant.sonorus.core.AlbumSort
import dev.constnishant.sonorus.core.ArtistInfo
import dev.constnishant.sonorus.core.PlaylistInfo
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.AlbumCard
import dev.constnishant.sonorus.ui.components.Artwork
import dev.constnishant.sonorus.ui.components.CenteredContent
import dev.constnishant.sonorus.ui.components.ConfirmDialog
import dev.constnishant.sonorus.ui.components.EmptyState
import dev.constnishant.sonorus.ui.components.LoadingContent
import dev.constnishant.sonorus.ui.components.PlaylistNameDialog
import dev.constnishant.sonorus.ui.components.albumsText
import dev.constnishant.sonorus.ui.components.formatTotal
import dev.constnishant.sonorus.ui.components.tracksText

private val LIBRARY_TABS = listOf(
    R.string.library_tab_albums,
    R.string.library_tab_artists,
    R.string.library_tab_playlists,
)

/**
 * The Library screen with three tabs: albums (sortable grid), artists, and playlists. Playlists
 * can be created from the floating button and renamed or deleted from each row's menu.
 *
 * @param onAlbum opens an album.
 * @param onArtist opens an artist.
 * @param onPlaylist opens a playlist.
 * @param modifier applied to the root layout.
 */
@Composable
fun LibraryScreen(
    onAlbum: (Long) -> Unit,
    onArtist: (Long) -> Unit,
    onPlaylist: (Long) -> Unit,
    modifier: Modifier = Modifier,
) {
    val repository = LocalContext.current.repository
    val viewModel: LibraryViewModel = viewModel(
        factory = viewModelFactory { initializer { LibraryViewModel(checkNotNull(this[APPLICATION_KEY]), repository) } },
    )
    val albums by viewModel.albums.collectAsStateWithLifecycle()
    val sort by viewModel.sort.collectAsStateWithLifecycle()
    val artists by viewModel.artists.collectAsStateWithLifecycle()
    val playlists by viewModel.playlists.collectAsStateWithLifecycle()
    var tab by rememberSaveable { mutableIntStateOf(0) }
    var creating by rememberSaveable { mutableStateOf(false) }
    var renaming by remember { mutableStateOf<PlaylistInfo?>(null) }
    var deleting by remember { mutableStateOf<PlaylistInfo?>(null) }

    Scaffold(
        modifier = modifier,
        topBar = { TopAppBar(title = { Text(stringResource(R.string.library_title)) }) },
        floatingActionButton = {
            if (tab == 2) {
                FloatingActionButton(onClick = { creating = true }) {
                    Icon(Icons.Rounded.Add, contentDescription = stringResource(R.string.cd_new_playlist))
                }
            }
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            PrimaryTabRow(selectedTabIndex = tab) {
                LIBRARY_TABS.forEachIndexed { index, title ->
                    Tab(
                        selected = tab == index,
                        onClick = { tab = index },
                        text = { Text(stringResource(title)) },
                    )
                }
            }
            when (tab) {
                0 -> AlbumsTab(
                    state = albums,
                    sort = sort,
                    onSort = viewModel::setSort,
                    onAlbum = onAlbum,
                )
                1 -> ArtistsTab(state = artists, onArtist = onArtist)
                else -> PlaylistsTab(
                    state = playlists,
                    onPlaylist = onPlaylist,
                    onRename = { renaming = it },
                    onDelete = { deleting = it },
                )
            }
        }
    }

    if (creating) {
        PlaylistNameDialog(
            title = stringResource(R.string.playlist_new_title),
            initialName = "",
            confirmLabel = stringResource(R.string.action_create),
            onConfirm = { name ->
                viewModel.createPlaylist(name)
                creating = false
            },
            onDismiss = { creating = false },
        )
    }
    renaming?.let { playlist ->
        PlaylistNameDialog(
            title = stringResource(R.string.playlist_rename_title),
            initialName = playlist.name,
            confirmLabel = stringResource(R.string.action_rename),
            onConfirm = { name ->
                viewModel.renamePlaylist(playlist.id, name)
                renaming = null
            },
            onDismiss = { renaming = null },
        )
    }
    deleting?.let { playlist ->
        ConfirmDialog(
            title = stringResource(R.string.playlist_delete_title),
            message = stringResource(R.string.playlist_delete_message, playlist.name),
            confirmLabel = stringResource(R.string.action_delete),
            onConfirm = {
                viewModel.deletePlaylist(playlist.id)
                deleting = null
            },
            onDismiss = { deleting = null },
        )
    }
}

/** The Albums tab: a sort menu above an adaptive grid of covers. */
@Composable
private fun AlbumsTab(
    state: LoadState<List<AlbumInfo>>,
    sort: AlbumSort,
    onSort: (AlbumSort) -> Unit,
    onAlbum: (Long) -> Unit,
) {
    when (state) {
        LoadState.Loading -> LoadingContent()
        is LoadState.Failed -> CenteredContent {
            EmptyState(Icons.Rounded.ErrorOutline, stringResource(R.string.library_albums_failed), state.message)
        }
        is LoadState.Ready -> if (state.value.isEmpty()) {
            CenteredContent {
                EmptyState(
                    Icons.Rounded.Album,
                    stringResource(R.string.library_no_albums),
                    stringResource(R.string.scan_bucket_hint),
                )
            }
        } else {
            Column(Modifier.fillMaxSize()) {
                SortMenu(sort = sort, onSort = onSort)
                LazyVerticalGrid(
                    columns = GridCells.Adaptive(150.dp),
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(16.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    gridItems(state.value, key = { it.id }) { album ->
                        AlbumCard(album = album, onClick = { onAlbum(album.id) })
                    }
                }
            }
        }
    }
}

/** A right-aligned button that opens the album sort menu. */
@Composable
private fun SortMenu(sort: AlbumSort, onSort: (AlbumSort) -> Unit) {
    var open by remember { mutableStateOf(false) }
    Row(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 8.dp),
        horizontalArrangement = Arrangement.End,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            stringResource(R.string.library_sorted_by, sort.label()),
            style = MaterialTheme.typography.labelLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Box {
            IconButton(onClick = { open = true }) {
                Icon(Icons.AutoMirrored.Rounded.Sort, contentDescription = stringResource(R.string.cd_sort_albums))
            }
            DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                AlbumSort.entries.forEach { option ->
                    DropdownMenuItem(
                        text = { Text(option.label()) },
                        trailingIcon = {
                            if (option == sort) {
                                Icon(Icons.Rounded.Check, contentDescription = stringResource(R.string.cd_selected))
                            }
                        },
                        onClick = {
                            open = false
                            onSort(option)
                        },
                    )
                }
            }
        }
    }
}

/** The Artists tab: one row per artist with album and track counts. */
@Composable
private fun ArtistsTab(state: LoadState<List<ArtistInfo>>, onArtist: (Long) -> Unit) {
    when (state) {
        LoadState.Loading -> LoadingContent()
        is LoadState.Failed -> CenteredContent {
            EmptyState(Icons.Rounded.ErrorOutline, stringResource(R.string.library_artists_failed), state.message)
        }
        is LoadState.Ready -> if (state.value.isEmpty()) {
            CenteredContent {
                EmptyState(
                    Icons.Rounded.Person,
                    stringResource(R.string.library_no_artists),
                    stringResource(R.string.scan_bucket_hint),
                )
            }
        } else {
            LazyColumn(Modifier.fillMaxSize()) {
                items(state.value, key = { it.id }) { artist ->
                    ListItem(
                        modifier = Modifier.clickable { onArtist(artist.id) },
                        headlineContent = {
                            Text(artist.name, maxLines = 1, overflow = TextOverflow.Ellipsis)
                        },
                        supportingContent = {
                            Text(albumsText(artist.albumCount.toInt()) + " · " + tracksText(artist.trackCount.toInt()))
                        },
                        leadingContent = { Icon(Icons.Rounded.Person, contentDescription = null) },
                    )
                }
            }
        }
    }
}

/** The Playlists tab: cover, name, track count and total length, with a per-row menu. */
@Composable
private fun PlaylistsTab(
    state: LoadState<List<PlaylistInfo>>,
    onPlaylist: (Long) -> Unit,
    onRename: (PlaylistInfo) -> Unit,
    onDelete: (PlaylistInfo) -> Unit,
) {
    when (state) {
        LoadState.Loading -> LoadingContent()
        is LoadState.Failed -> CenteredContent {
            EmptyState(Icons.Rounded.ErrorOutline, stringResource(R.string.library_playlists_failed), state.message)
        }
        is LoadState.Ready -> if (state.value.isEmpty()) {
            CenteredContent {
                EmptyState(
                    Icons.AutoMirrored.Rounded.QueueMusic,
                    stringResource(R.string.library_no_playlists),
                    stringResource(R.string.library_playlists_hint),
                )
            }
        } else {
            LazyColumn(Modifier.fillMaxSize()) {
                items(state.value, key = { it.id }) { playlist ->
                    PlaylistRow(
                        playlist = playlist,
                        onClick = { onPlaylist(playlist.id) },
                        onRename = { onRename(playlist) },
                        onDelete = { onDelete(playlist) },
                    )
                }
            }
        }
    }
}

/** One playlist row with an overflow menu for rename and delete. */
@Composable
private fun PlaylistRow(
    playlist: PlaylistInfo,
    onClick: () -> Unit,
    onRename: () -> Unit,
    onDelete: () -> Unit,
) {
    var open by remember { mutableStateOf(false) }
    ListItem(
        modifier = Modifier.clickable(onClick = onClick),
        leadingContent = {
            Artwork(
                path = playlist.coverPath,
                contentDescription = null,
                modifier = Modifier.size(56.dp),
                shape = MaterialTheme.shapes.small,
            )
        },
        headlineContent = { Text(playlist.name, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        supportingContent = {
            Text(
                tracksText(playlist.trackCount.toInt()) + " · " +
                    formatTotal(LocalContext.current, playlist.durationMs.toLong()),
            )
        },
        trailingContent = {
            Box {
                IconButton(onClick = { open = true }) {
                    Icon(Icons.Rounded.MoreVert, contentDescription = stringResource(R.string.cd_playlist_options))
                }
                DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.action_rename)) },
                        leadingIcon = { Icon(Icons.Rounded.Edit, contentDescription = null) },
                        onClick = {
                            open = false
                            onRename()
                        },
                    )
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.action_delete)) },
                        leadingIcon = { Icon(Icons.Rounded.Delete, contentDescription = null) },
                        onClick = {
                            open = false
                            onDelete()
                        },
                    )
                }
            }
        },
    )
}

/** Display name of a sort order. */
@Composable
private fun AlbumSort.label(): String = stringResource(
    when (this) {
        AlbumSort.ARTIST -> R.string.sort_artist
        AlbumSort.TITLE -> R.string.sort_title
        AlbumSort.YEAR -> R.string.sort_year
        AlbumSort.RECENTLY_ADDED -> R.string.sort_recently_added
    },
)

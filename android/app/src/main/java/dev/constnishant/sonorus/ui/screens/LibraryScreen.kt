@file:OptIn(ExperimentalMaterial3Api::class)

package dev.constnishant.sonorus.ui.screens

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items as gridItems
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.CircleShape
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
import androidx.compose.material3.CircularProgressIndicator
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
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
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
import dev.constnishant.sonorus.core.SmartPlaylistKind
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.AlbumCard
import dev.constnishant.sonorus.ui.components.Artwork
import dev.constnishant.sonorus.ui.components.CenteredContent
import dev.constnishant.sonorus.ui.components.ConfirmDialog
import dev.constnishant.sonorus.ui.components.EmptyState
import dev.constnishant.sonorus.ui.components.LoadingContent
import dev.constnishant.sonorus.ui.components.PlaylistNameDialog
import dev.constnishant.sonorus.ui.components.SectionTitle
import dev.constnishant.sonorus.ui.components.albumsText
import dev.constnishant.sonorus.ui.components.formatTotal
import dev.constnishant.sonorus.ui.components.tracksText
import kotlinx.coroutines.launch

private val LIBRARY_TABS = listOf(
    R.string.library_tab_albums,
    R.string.library_tab_artists,
    R.string.library_tab_playlists,
    R.string.library_tab_folders,
)

/** The index of the Playlists tab in [LIBRARY_TABS]. */
private const val PLAYLISTS_PAGE = 2

/** The index of the Folders tab in [LIBRARY_TABS]. */
private const val FOLDERS_PAGE = 3

/** Artists lists longer than this get the A–Z index. */
private const val FAST_SCROLL_MIN_ARTISTS = 30

/** The index letters, top to bottom: '#' for names that do not start with a letter, then A to Z. */
private val FAST_SCROLL_LETTERS: List<Char> = listOf('#') + ('A'..'Z').toList()

/**
 * The Library screen with four swipeable tabs: albums (sortable grid), artists, playlists (with the
 * smart playlists above the user's own, which can be created, renamed and deleted), and folders.
 * Pulling down starts a library scan. System back on the Folders tab goes up one folder.
 *
 * @param onAlbum opens an album.
 * @param onArtist opens an artist.
 * @param onPlaylist opens a playlist.
 * @param onSmartPlaylist opens a smart playlist.
 * @param modifier applied to the root layout.
 */
@Composable
fun LibraryScreen(
    onAlbum: (Long) -> Unit,
    onArtist: (Long) -> Unit,
    onPlaylist: (Long) -> Unit,
    onSmartPlaylist: (SmartPlaylistKind) -> Unit,
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
    val scanning by viewModel.scanning.collectAsStateWithLifecycle()
    val foldersViewModel: FoldersViewModel = viewModel(
        factory = viewModelFactory { initializer { FoldersViewModel(checkNotNull(this[APPLICATION_KEY]), repository) } },
    )
    val folderPath by foldersViewModel.path.collectAsStateWithLifecycle()
    var savedPage by rememberSaveable { mutableIntStateOf(0) }
    val pagerState = rememberPagerState(initialPage = savedPage, pageCount = { LIBRARY_TABS.size })
    val scope = rememberCoroutineScope()
    LaunchedEffect(pagerState.currentPage) { savedPage = pagerState.currentPage }
    var creating by rememberSaveable { mutableStateOf(false) }
    var renaming by remember { mutableStateOf<PlaylistInfo?>(null) }
    var deleting by remember { mutableStateOf<PlaylistInfo?>(null) }
    // Back on the Folders tab goes up one folder; at the root it leaves the screen as usual.
    BackHandler(enabled = pagerState.currentPage == FOLDERS_PAGE && folderPath.isNotEmpty()) {
        foldersViewModel.up()
    }

    Scaffold(
        modifier = modifier,
        topBar = { TopAppBar(title = { Text(stringResource(R.string.library_title)) }) },
        floatingActionButton = {
            if (pagerState.currentPage == PLAYLISTS_PAGE) {
                FloatingActionButton(onClick = { creating = true }) {
                    Icon(Icons.Rounded.Add, contentDescription = stringResource(R.string.cd_new_playlist))
                }
            }
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            PrimaryTabRow(selectedTabIndex = pagerState.currentPage) {
                LIBRARY_TABS.forEachIndexed { index, title ->
                    Tab(
                        selected = pagerState.currentPage == index,
                        onClick = { scope.launch { pagerState.animateScrollToPage(index) } },
                        text = { Text(stringResource(title)) },
                    )
                }
            }
            PullToRefreshBox(
                isRefreshing = scanning,
                onRefresh = viewModel::refresh,
                modifier = Modifier.weight(1f),
            ) {
                HorizontalPager(
                    state = pagerState,
                    modifier = Modifier.fillMaxSize(),
                ) { page ->
                    when (page) {
                        0 -> AlbumsTab(
                            state = albums,
                            sort = sort,
                            onSort = viewModel::setSort,
                            onAlbum = onAlbum,
                        )
                        1 -> ArtistsTab(state = artists, onArtist = onArtist)
                        PLAYLISTS_PAGE -> PlaylistsTab(
                            state = playlists,
                            onPlaylist = onPlaylist,
                            onSmartPlaylist = onSmartPlaylist,
                            onRename = { renaming = it },
                            onDelete = { deleting = it },
                        )
                        else -> FoldersTab(viewModel = foldersViewModel)
                    }
                }
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

/**
 * The Artists tab: one row per artist with album and track counts. Lists longer than
 * [FAST_SCROLL_MIN_ARTISTS] get an A–Z index along the end edge.
 */
@Composable
private fun ArtistsTab(state: LoadState<List<ArtistInfo>>, onArtist: (Long) -> Unit) {
    val listState = rememberLazyListState()
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
            Box(Modifier.fillMaxSize()) {
                LazyColumn(state = listState, modifier = Modifier.fillMaxSize()) {
                    items(state.value, key = { it.id }) { artist ->
                        ListItem(
                            modifier = Modifier.clickable(role = Role.Button) { onArtist(artist.id) },
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
                if (state.value.size > FAST_SCROLL_MIN_ARTISTS) {
                    FastScrollIndex(names = state.value.map { it.name }, listState = listState)
                }
            }
        }
    }
}

/**
 * The A–Z index along the end edge of the artists list. Tapping or dragging a letter scrolls to the
 * first artist for it, and a bubble shows the letter while dragging.
 */
@Composable
private fun BoxScope.FastScrollIndex(names: List<String>, listState: LazyListState) {
    val scope = rememberCoroutineScope()
    val haptics = LocalHapticFeedback.current
    val label = stringResource(R.string.cd_fast_scroll)
    var shown by remember { mutableStateOf<Char?>(null) }
    var dragging by remember { mutableStateOf(false) }
    var stripHeight by remember { mutableIntStateOf(0) }

    fun letterAt(y: Float): Char {
        if (stripHeight <= 0) return FAST_SCROLL_LETTERS.first()
        val cell = stripHeight / FAST_SCROLL_LETTERS.size.toFloat()
        return FAST_SCROLL_LETTERS[(y / cell).toInt().coerceIn(0, FAST_SCROLL_LETTERS.lastIndex)]
    }

    fun select(letter: Char, force: Boolean) {
        if (letter == shown && !force) return
        if (letter != shown) haptics.performHapticFeedback(HapticFeedbackType.TextHandleMove)
        shown = letter
        val index = artistLetterIndex(names, letter)
        if (index >= 0) scope.launch { listState.scrollToItem(index) }
    }

    val stripWidth = 24.dp
    val bubbleSize = 56.dp
    shown?.takeIf { dragging }?.let { letter ->
        Box(
            Modifier
                .align(Alignment.TopEnd)
                .offset {
                    val cell = stripHeight / FAST_SCROLL_LETTERS.size
                    val index = FAST_SCROLL_LETTERS.indexOf(letter).coerceAtLeast(0)
                    IntOffset(
                        x = -(stripWidth + 8.dp + bubbleSize).roundToPx(),
                        y = (index * cell + cell / 2 - bubbleSize.roundToPx() / 2).coerceAtLeast(0),
                    )
                }
                .size(bubbleSize)
                .clip(CircleShape)
                .background(MaterialTheme.colorScheme.primaryContainer),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                letter.toString(),
                style = MaterialTheme.typography.headlineSmall,
                color = MaterialTheme.colorScheme.onPrimaryContainer,
            )
        }
    }

    Column(
        Modifier
            .align(Alignment.TopEnd)
            .fillMaxHeight()
            .width(stripWidth)
            .onSizeChanged { stripHeight = it.height }
            .semantics { contentDescription = label }
            .pointerInput(names) {
                detectTapGestures { offset -> select(letterAt(offset.y), force = true) }
            }
            .pointerInput(names) {
                detectVerticalDragGestures(
                    onDragStart = { offset ->
                        dragging = true
                        select(letterAt(offset.y), force = true)
                    },
                    onVerticalDrag = { change, _ ->
                        change.consume()
                        select(letterAt(change.position.y), force = false)
                    },
                    onDragEnd = { dragging = false },
                    onDragCancel = { dragging = false },
                )
            },
    ) {
        FAST_SCROLL_LETTERS.forEach { letter ->
            Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
                Text(
                    letter.toString(),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

/**
 * The index of the first artist for [letter] in [names] (already in the core's sort order). '#'
 * matches the first artist whose name does not start with a letter. A letter with no artists falls
 * back to the next letter that has some. Returns -1 when there is nothing to jump to.
 */
private fun artistLetterIndex(names: List<String>, letter: Char): Int {
    val exact = names.indexOfFirst { artistIndexLetter(it) == letter }
    if (exact >= 0 || letter == '#') return exact
    return names.indexOfFirst { artistIndexLetter(it) > letter }
}

/**
 * The index letter of an artist name: its first character upper-cased when it is an ASCII letter,
 * otherwise '#'. Matches the core's `ORDER BY name COLLATE NOCASE`, which folds ASCII case only and
 * does not skip a leading "The".
 */
private fun artistIndexLetter(name: String): Char {
    val first = name.firstOrNull() ?: return '#'
    return when (first) {
        in 'a'..'z' -> first.uppercaseChar()
        in 'A'..'Z' -> first
        else -> '#'
    }
}

/**
 * The Playlists tab: the smart playlists under "Smart playlists", then the user's playlists under
 * "Your playlists" (cover, name, track count and total length, with a per-row menu).
 */
@Composable
private fun PlaylistsTab(
    state: LoadState<List<PlaylistInfo>>,
    onPlaylist: (Long) -> Unit,
    onSmartPlaylist: (SmartPlaylistKind) -> Unit,
    onRename: (PlaylistInfo) -> Unit,
    onDelete: (PlaylistInfo) -> Unit,
) {
    LazyColumn(Modifier.fillMaxSize()) {
        item(key = "smart-title") { SectionTitle(stringResource(R.string.library_smart_playlists)) }
        items(SMART_PLAYLISTS, key = { it.name }) { kind ->
            ListItem(
                modifier = Modifier.clickable(role = Role.Button) { onSmartPlaylist(kind) },
                leadingContent = { Icon(kind.icon(), contentDescription = null) },
                headlineContent = { Text(stringResource(kind.titleRes()), maxLines = 1, overflow = TextOverflow.Ellipsis) },
                supportingContent = { Text(stringResource(kind.descriptionRes())) },
            )
        }
        item(key = "your-title") { SectionTitle(stringResource(R.string.library_your_playlists)) }
        when (state) {
            LoadState.Loading -> item(key = "playlists-loading") {
                Box(Modifier.fillMaxWidth().padding(32.dp), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator()
                }
            }
            is LoadState.Failed -> item(key = "playlists-failed") {
                EmptyState(Icons.Rounded.ErrorOutline, stringResource(R.string.library_playlists_failed), state.message)
            }
            is LoadState.Ready -> if (state.value.isEmpty()) {
                item(key = "playlists-empty") {
                    EmptyState(
                        Icons.AutoMirrored.Rounded.QueueMusic,
                        stringResource(R.string.library_no_playlists),
                        stringResource(R.string.library_playlists_hint),
                    )
                }
            } else {
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
        modifier = Modifier.clickable(role = Role.Button, onClick = onClick),
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

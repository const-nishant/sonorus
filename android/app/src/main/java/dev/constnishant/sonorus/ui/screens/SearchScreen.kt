@file:OptIn(ExperimentalMaterial3Api::class)

package dev.constnishant.sonorus.ui.screens

import androidx.annotation.StringRes
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
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
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Clear
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.ErrorOutline
import androidx.compose.material.icons.rounded.History
import androidx.compose.material.icons.rounded.MusicNote
import androidx.compose.material.icons.rounded.Person
import androidx.compose.material.icons.rounded.Search
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModelProvider.AndroidViewModelFactory.Companion.APPLICATION_KEY
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.QualityFilterInfo
import dev.constnishant.sonorus.core.SearchResult
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.AlbumCard
import dev.constnishant.sonorus.ui.components.CenteredContent
import dev.constnishant.sonorus.ui.components.EmptyState
import dev.constnishant.sonorus.ui.components.LoadingContent
import dev.constnishant.sonorus.ui.components.SectionTitle
import dev.constnishant.sonorus.ui.components.TrackActions
import dev.constnishant.sonorus.ui.components.TrackRow
import dev.constnishant.sonorus.ui.components.albumsText
import dev.constnishant.sonorus.ui.components.rememberCurrentTrackId
import dev.constnishant.sonorus.ui.components.rememberTrackActions
import dev.constnishant.sonorus.ui.components.tracksText

/** The quality filters in the order their chips are shown. */
private val QUALITY_FILTERS = listOf(
    QualityFilterInfo.ANY,
    QualityFilterInfo.HI_RES,
    QualityFilterInfo.LOSSLESS,
    QualityFilterInfo.LOSSY,
)

/**
 * The Search screen. The field is focused on entry, and quality chips (All, Hi-res, Lossless,
 * Lossy) filter the results. With a blank query the screen shows the recent searches, or with a
 * quality chip the tracks of that quality. Results are grouped into songs, albums and artists.
 * Tapping a song starts playback of all the song results from that one.
 *
 * @param onAlbum opens an album from the results.
 * @param onArtist opens an artist from the results.
 * @param modifier applied to the root layout.
 */
@Composable
fun SearchScreen(onAlbum: (Long) -> Unit, onArtist: (Long) -> Unit, modifier: Modifier = Modifier) {
    val repository = LocalContext.current.repository
    val viewModel: SearchViewModel = viewModel(
        factory = viewModelFactory { initializer { SearchViewModel(checkNotNull(this[APPLICATION_KEY]), repository) } },
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
        topBar = { TopAppBar(title = { Text(stringResource(R.string.search_title)) }) },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            OutlinedTextField(
                value = state.query,
                onValueChange = viewModel::onQueryChange,
                singleLine = true,
                placeholder = { Text(stringResource(R.string.search_placeholder)) },
                leadingIcon = { Icon(Icons.Rounded.Search, contentDescription = null) },
                trailingIcon = {
                    if (state.query.isNotEmpty()) {
                        IconButton(onClick = { viewModel.onQueryChange("") }) {
                            Icon(Icons.Rounded.Clear, contentDescription = stringResource(R.string.cd_clear_search))
                        }
                    }
                },
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                keyboardActions = KeyboardActions(onSearch = { viewModel.onSearchSubmit() }),
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp)
                    .focusRequester(focusRequester),
            )
            QualityFilterRow(selected = state.filter, onSelect = viewModel::onFilterChange)

            val browsing = state.query.isBlank()
            when {
                browsing && state.filter == QualityFilterInfo.ANY -> {
                    if (state.recent.isEmpty()) {
                        CenteredContent {
                            EmptyState(
                                Icons.Rounded.Search,
                                stringResource(R.string.search_intro_title),
                                stringResource(R.string.search_intro_message),
                            )
                        }
                    } else {
                        RecentSearches(
                            recent = state.recent,
                            onOpen = viewModel::onQueryChange,
                            onRemove = viewModel::removeRecent,
                            onClear = viewModel::clearRecent,
                        )
                    }
                }
                browsing -> QualityTracks(
                    filter = state.filter,
                    // Results for an older query are not shown while the new settled query loads.
                    results = if (state.term.isEmpty()) state.results else LoadState.Loading,
                    currentId = currentId,
                    actions = actions,
                    onPlayTrack = { tracks, index ->
                        val ids = tracks.map { it.id }
                        repository.command { core -> core.playTracks(ids, index.toUInt()) }
                    },
                )
                else -> when (val results = state.results) {
                    LoadState.Loading -> LoadingContent()
                    is LoadState.Failed -> CenteredContent {
                        EmptyState(Icons.Rounded.ErrorOutline, stringResource(R.string.search_failed), results.message)
                    }
                    is LoadState.Ready -> {
                        val result = results.value
                        val empty = result.tracks.isEmpty() && result.albums.isEmpty() && result.artists.isEmpty()
                        if (!empty) {
                            SearchResults(
                                result = result,
                                currentId = currentId,
                                onAlbum = {
                                    viewModel.onResultOpened()
                                    onAlbum(it)
                                },
                                onArtist = {
                                    viewModel.onResultOpened()
                                    onArtist(it)
                                },
                                onPlayTrack = { index ->
                                    viewModel.onResultOpened()
                                    val ids = result.tracks.map { it.id }
                                    repository.command { core -> core.playTracks(ids, index.toUInt()) }
                                },
                                actions = actions,
                            )
                        } else if (state.term == state.query.trim()) {
                            CenteredContent {
                                EmptyState(
                                    Icons.Rounded.Search,
                                    stringResource(R.string.search_no_results_title, state.term),
                                    stringResource(R.string.search_no_results_message),
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

/** A single-select row of quality chips: All, Hi-res, Lossless and Lossy. */
@Composable
private fun QualityFilterRow(selected: QualityFilterInfo, onSelect: (QualityFilterInfo) -> Unit) {
    LazyRow(
        contentPadding = PaddingValues(horizontal = 16.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        modifier = Modifier.padding(bottom = 8.dp),
    ) {
        items(QUALITY_FILTERS) { option ->
            FilterChip(
                selected = option == selected,
                onClick = { onSelect(option) },
                label = { Text(stringResource(option.chipLabel())) },
            )
        }
    }
}

/**
 * The recent searches, shown for a blank query with the All filter. Tapping one searches for it;
 * its X removes it, and Clear empties the list.
 *
 * @param recent the saved queries, newest first.
 * @param onOpen searches for a recent query.
 * @param onRemove removes one query.
 * @param onClear removes every query.
 */
@Composable
private fun RecentSearches(
    recent: List<String>,
    onOpen: (String) -> Unit,
    onRemove: (String) -> Unit,
    onClear: () -> Unit,
) {
    Column(Modifier.fillMaxSize()) {
        Row(
            Modifier
                .fillMaxWidth()
                .padding(start = 16.dp, end = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                stringResource(R.string.search_recent_title),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.weight(1f),
            )
            TextButton(onClick = onClear) {
                Text(stringResource(R.string.action_clear))
            }
        }
        LazyColumn(Modifier.fillMaxSize()) {
            items(recent, key = { it }) { text ->
                ListItem(
                    modifier = Modifier.clickable(role = Role.Button) { onOpen(text) },
                    leadingContent = { Icon(Icons.Rounded.History, contentDescription = null) },
                    headlineContent = { Text(text, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                    trailingContent = {
                        IconButton(onClick = { onRemove(text) }) {
                            Icon(Icons.Rounded.Close, contentDescription = stringResource(R.string.action_remove))
                        }
                    },
                )
            }
        }
    }
}

/**
 * The tracks of one quality, listed for a blank query with a quality chip. Shows the quality as a
 * header, or an empty state when the library has no such track.
 *
 * @param filter the quality shown; never [QualityFilterInfo.ANY] in practice, but it has a title.
 * @param results the loaded tracks, or the state while they load.
 * @param onPlayTrack plays the list from the given track position.
 */
@Composable
private fun QualityTracks(
    filter: QualityFilterInfo,
    results: LoadState<SearchResult>,
    currentId: Long?,
    actions: TrackActions,
    onPlayTrack: (List<TrackInfo>, Int) -> Unit,
) {
    when (results) {
        LoadState.Loading -> LoadingContent()
        is LoadState.Failed -> CenteredContent {
            EmptyState(Icons.Rounded.ErrorOutline, stringResource(R.string.search_failed), results.message)
        }
        is LoadState.Ready -> {
            val tracks = results.value.tracks
            if (tracks.isEmpty()) {
                CenteredContent {
                    EmptyState(
                        Icons.Rounded.MusicNote,
                        stringResource(filter.headerTitle()),
                        stringResource(filter.emptyMessage()),
                    )
                }
            } else {
                LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 16.dp)) {
                    item(key = "quality-title") { SectionTitle(stringResource(filter.headerTitle())) }
                    itemsIndexed(tracks, key = { _, track -> "track-${track.id}" }) { index, track ->
                        TrackRow(
                            track = track,
                            onClick = { onPlayTrack(tracks, index) },
                            showArtwork = true,
                            isCurrent = track.id == currentId,
                            actions = actions,
                        )
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
            item(key = "songs-title") { SectionTitle(stringResource(R.string.section_songs)) }
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
            item(key = "albums-title") { SectionTitle(stringResource(R.string.section_albums)) }
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
            item(key = "artists-title") { SectionTitle(stringResource(R.string.section_artists)) }
            items(result.artists, key = { "artist-${it.id}" }) { artist ->
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
    }
}

/** The label of a quality chip. */
@StringRes
private fun QualityFilterInfo.chipLabel(): Int = when (this) {
    QualityFilterInfo.ANY -> R.string.search_filter_all
    QualityFilterInfo.HI_RES -> R.string.search_filter_hi_res
    QualityFilterInfo.LOSSLESS -> R.string.search_filter_lossless
    QualityFilterInfo.LOSSY -> R.string.search_filter_lossy
}

/** The header over the tracks of a quality filter. */
@StringRes
private fun QualityFilterInfo.headerTitle(): Int = when (this) {
    QualityFilterInfo.ANY -> R.string.search_filter_all
    QualityFilterInfo.HI_RES -> R.string.search_hi_res_tracks
    QualityFilterInfo.LOSSLESS -> R.string.search_lossless_tracks
    QualityFilterInfo.LOSSY -> R.string.search_lossy_tracks
}

/** The message shown when the library has no track of a quality filter. */
@StringRes
private fun QualityFilterInfo.emptyMessage(): Int = when (this) {
    QualityFilterInfo.ANY -> R.string.search_empty_all
    QualityFilterInfo.HI_RES -> R.string.quality_empty_hi_res
    QualityFilterInfo.LOSSLESS -> R.string.quality_empty_lossless
    QualityFilterInfo.LOSSY -> R.string.quality_empty_lossy
}

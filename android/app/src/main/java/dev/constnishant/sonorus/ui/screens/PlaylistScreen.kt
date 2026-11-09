@file:OptIn(ExperimentalMaterial3Api::class)

package dev.constnishant.sonorus.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.QueueMusic
import androidx.compose.material.icons.rounded.Delete
import androidx.compose.material.icons.rounded.DragHandle
import androidx.compose.material.icons.rounded.Edit
import androidx.compose.material.icons.rounded.ErrorOutline
import androidx.compose.material.icons.rounded.MoreVert
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.RemoveCircleOutline
import androidx.compose.material.icons.rounded.Shuffle
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModelProvider.AndroidViewModelFactory.Companion.APPLICATION_KEY
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.PlaylistEntryInfo
import dev.constnishant.sonorus.data.DownloadsUi
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.BackButton
import dev.constnishant.sonorus.ui.components.CenteredContent
import dev.constnishant.sonorus.ui.components.ConfirmDialog
import dev.constnishant.sonorus.ui.components.DownloadStatusText
import dev.constnishant.sonorus.ui.components.DownloadToggle
import dev.constnishant.sonorus.ui.components.EmptyState
import dev.constnishant.sonorus.ui.components.LoadingContent
import dev.constnishant.sonorus.ui.components.PinUi
import dev.constnishant.sonorus.ui.components.PlaylistNameDialog
import dev.constnishant.sonorus.ui.components.SwipeRemoveBox
import dev.constnishant.sonorus.ui.components.TrackActions
import dev.constnishant.sonorus.ui.components.TrackRow
import dev.constnishant.sonorus.ui.components.formatTotal
import dev.constnishant.sonorus.ui.components.rememberCurrentTrackId
import dev.constnishant.sonorus.ui.components.rememberTrackActions
import dev.constnishant.sonorus.ui.components.songsText
import kotlinx.coroutines.launch
import sh.calvin.reorderable.ReorderableItem
import sh.calvin.reorderable.rememberReorderableLazyListState

/**
 * The Playlist screen. Lists the entries in order: catalogued tracks play from that entry on;
 * entries whose track is missing from the library are shown as unavailable. Long-pressing an entry
 * or dragging its handle reorders the playlist, and swiping an entry to the start removes it with
 * an undo message. The top bar menu renames or deletes the playlist.
 *
 * @param playlistId the playlist to show.
 * @param onBack leaves the screen (also after the playlist is deleted).
 * @param modifier applied to the root layout.
 */
@Composable
fun PlaylistScreen(playlistId: Long, onBack: () -> Unit, modifier: Modifier = Modifier) {
    val repository = LocalContext.current.repository
    val viewModel: PlaylistViewModel = viewModel(
        key = "playlist-$playlistId",
        factory = viewModelFactory { initializer { PlaylistViewModel(checkNotNull(this[APPLICATION_KEY]), repository, playlistId) } },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    val pin by viewModel.pin.collectAsStateWithLifecycle()
    val downloads by viewModel.downloads.collectAsStateWithLifecycle()
    val currentId by rememberCurrentTrackId()
    val actions = rememberTrackActions()
    val data = (state as? LoadState.Ready)?.value
    val scope = rememberCoroutineScope()
    val snackbarHostState = remember { SnackbarHostState() }
    val undoLabel = stringResource(R.string.action_undo)
    var menuOpen by remember { mutableStateOf(false) }
    var renaming by remember { mutableStateOf(false) }
    var deleting by remember { mutableStateOf(false) }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        data?.playlist?.name ?: stringResource(R.string.playlist_fallback_title),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                },
                navigationIcon = { BackButton(onBack) },
                actions = {
                    if (data != null) {
                        Box {
                            IconButton(onClick = { menuOpen = true }) {
                                Icon(Icons.Rounded.MoreVert, contentDescription = stringResource(R.string.cd_playlist_options))
                            }
                            DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                                DropdownMenuItem(
                                    text = { Text(stringResource(R.string.action_rename)) },
                                    leadingIcon = { Icon(Icons.Rounded.Edit, contentDescription = null) },
                                    onClick = {
                                        menuOpen = false
                                        renaming = true
                                    },
                                )
                                DropdownMenuItem(
                                    text = { Text(stringResource(R.string.action_delete)) },
                                    leadingIcon = { Icon(Icons.Rounded.Delete, contentDescription = null) },
                                    onClick = {
                                        menuOpen = false
                                        deleting = true
                                    },
                                )
                            }
                        }
                    }
                },
            )
        },
        snackbarHost = { SnackbarHost(snackbarHostState) },
    ) { padding ->
        when (val s = state) {
            LoadState.Loading -> LoadingContent(Modifier.padding(padding))
            is LoadState.Failed -> CenteredContent(Modifier.padding(padding)) {
                EmptyState(Icons.Rounded.ErrorOutline, stringResource(R.string.playlist_open_failed), s.message)
            }
            is LoadState.Ready -> PlaylistContent(
                data = s.value,
                currentId = currentId,
                actions = actions,
                onPlayAll = viewModel::playAll,
                onPlayFrom = viewModel::playFrom,
                onRemove = viewModel::remove,
                onMove = viewModel::move,
                onSwipeRemoved = { message, trackId, position ->
                    scope.launch {
                        val result = snackbarHostState.showSnackbar(
                            message = message,
                            actionLabel = if (trackId != null) undoLabel else null,
                            duration = SnackbarDuration.Long,
                        )
                        if (result == SnackbarResult.ActionPerformed && trackId != null) {
                            viewModel.restoreToPlaylist(trackId, position.toUInt())
                        }
                    }
                },
                pin = pin,
                downloads = downloads,
                onSetPinned = viewModel::setPinned,
                modifier = Modifier.padding(padding),
            )
        }
    }

    if (renaming && data != null) {
        PlaylistNameDialog(
            title = stringResource(R.string.playlist_rename_title),
            initialName = data.playlist.name,
            confirmLabel = stringResource(R.string.action_rename),
            onConfirm = { name ->
                viewModel.rename(name)
                renaming = false
            },
            onDismiss = { renaming = false },
        )
    }
    if (deleting && data != null) {
        ConfirmDialog(
            title = stringResource(R.string.playlist_delete_title),
            message = stringResource(R.string.playlist_delete_message, data.playlist.name),
            confirmLabel = stringResource(R.string.action_delete),
            onConfirm = {
                deleting = false
                viewModel.delete()
                onBack()
            },
            onDismiss = { deleting = false },
        )
    }
}

/**
 * A playlist entry as the list shows it. [key] stays the same while the entry is moved, and is
 * unique even when the same track appears more than once.
 */
private data class EntryRow(val key: String, val entry: PlaylistEntryInfo)

/**
 * The entries with keys built from the track key and the occurrence number of that track, so
 * duplicate tracks get distinct keys.
 */
private fun List<PlaylistEntryInfo>.withRowKeys(): List<EntryRow> {
    val seen = HashMap<String, Int>()
    return map { entry ->
        val occurrence = (seen[entry.trackKey] ?: 0) + 1
        seen[entry.trackKey] = occurrence
        EntryRow("${entry.trackKey}#$occurrence", entry)
    }
}

/**
 * The loaded playlist: header with Play / Shuffle, then the entries. The entries are reordered
 * live while dragging and sent to the core once when the drag ends. A swipe removes an entry at
 * once and reports it through [onSwipeRemoved] so the screen can offer undo.
 */
@Composable
private fun PlaylistContent(
    data: PlaylistData,
    currentId: Long?,
    actions: TrackActions,
    onPlayAll: (Boolean) -> Unit,
    onPlayFrom: (UInt) -> Unit,
    onRemove: (UInt) -> Unit,
    onMove: (UInt, UInt) -> Unit,
    onSwipeRemoved: (message: String, trackId: Long?, position: Int) -> Unit,
    pin: PinUi,
    downloads: DownloadsUi,
    onSetPinned: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
) {
    val playlist = data.playlist
    val listState = rememberLazyListState()
    val haptics = LocalHapticFeedback.current
    // The rows shown and reordered live. They follow the core except while a drag is in progress.
    var rows by remember { mutableStateOf(data.entries.withRowKeys()) }
    var dragging by remember { mutableStateOf(false) }
    var draggedKey by remember { mutableStateOf<String?>(null) }
    var dragOrigin by remember { mutableIntStateOf(-1) }

    LaunchedEffect(data.entries) {
        if (!dragging) rows = data.entries.withRowKeys()
    }

    val reorderState = rememberReorderableLazyListState(listState) { from, to ->
        val fromIndex = rows.indexOfFirst { it.key == from.key }
        val toIndex = rows.indexOfFirst { it.key == to.key }
        if (fromIndex >= 0 && toIndex >= 0 && fromIndex != toIndex) {
            rows = rows.toMutableList().apply { add(toIndex, removeAt(fromIndex)) }
            haptics.performHapticFeedback(HapticFeedbackType.TextHandleMove)
        }
    }

    fun startDrag(key: String) {
        dragging = true
        draggedKey = key
        dragOrigin = rows.indexOfFirst { it.key == key }
        haptics.performHapticFeedback(HapticFeedbackType.TextHandleMove)
    }

    fun finishDrag() {
        dragging = false
        val key = draggedKey ?: return
        draggedKey = null
        val to = rows.indexOfFirst { it.key == key }
        if (dragOrigin >= 0 && to >= 0 && to != dragOrigin) {
            onMove(dragOrigin.toUInt(), to.toUInt())
        }
    }

    LazyColumn(modifier.fillMaxSize(), state = listState) {
        item(key = "header") {
            Column(
                Modifier
                    .fillMaxWidth()
                    .padding(16.dp),
            ) {
                Text(
                    songsText(playlist.trackCount.toInt()) + " · " + formatTotal(LocalContext.current, playlist.durationMs.toLong()),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.height(12.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    Button(onClick = { onPlayAll(false) }, enabled = data.entries.isNotEmpty()) {
                        Icon(Icons.Rounded.PlayArrow, contentDescription = null, modifier = Modifier.size(ButtonDefaults.IconSize))
                        Spacer(Modifier.size(ButtonDefaults.IconSpacing))
                        Text(stringResource(R.string.action_play))
                    }
                    OutlinedButton(onClick = { onPlayAll(true) }, enabled = data.entries.isNotEmpty()) {
                        Icon(Icons.Rounded.Shuffle, contentDescription = null, modifier = Modifier.size(ButtonDefaults.IconSize))
                        Spacer(Modifier.size(ButtonDefaults.IconSpacing))
                        Text(stringResource(R.string.action_shuffle))
                    }
                    DownloadToggle(state = pin, onSetPinned = onSetPinned)
                }
                DownloadStatusText(state = pin, downloads = downloads)
            }
        }
        if (rows.isEmpty()) {
            item(key = "empty") {
                EmptyState(
                    Icons.AutoMirrored.Rounded.QueueMusic,
                    stringResource(R.string.playlist_empty_title),
                    stringResource(R.string.playlist_empty_message),
                )
            }
        }
        items(rows, key = { it.key }) { row ->
            ReorderableItem(reorderState, key = row.key) { isDragging ->
                val entry = row.entry
                val track = entry.track
                val removedMessage = stringResource(
                    R.string.snackbar_removed,
                    track?.title ?: entry.trackKey.substringAfterLast('/'),
                )
                val menu: @Composable ColumnScope.(() -> Unit) -> Unit = { dismiss ->
                    EntryMenuItems(
                        onRemove = { onRemove(entry.position) },
                        onDismiss = dismiss,
                    )
                }
                SwipeRemoveBox(
                    onRemove = {
                        val index = rows.indexOfFirst { it.key == row.key }.coerceAtLeast(0)
                        rows = rows.filterNot { it.key == row.key }
                        onRemove(index.toUInt())
                        onSwipeRemoved(removedMessage, track?.id, index)
                    },
                ) {
                    Surface(
                        color = if (isDragging) {
                            MaterialTheme.colorScheme.surfaceContainerHigh
                        } else {
                            MaterialTheme.colorScheme.surface
                        },
                        shadowElevation = if (isDragging) 8.dp else 0.dp,
                    ) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Box(
                                Modifier
                                    .weight(1f)
                                    .longPressDraggableHandle(
                                        onDragStarted = { startDrag(row.key) },
                                        onDragStopped = { finishDrag() },
                                    ),
                            ) {
                                if (track != null) {
                                    TrackRow(
                                        track = track,
                                        onClick = { onPlayFrom(entry.position) },
                                        showArtwork = true,
                                        isCurrent = track.id == currentId,
                                        actions = actions,
                                        extraMenuItems = menu,
                                    )
                                } else {
                                    UnavailableEntry(entry = entry, menu = menu)
                                }
                            }
                            Box(
                                Modifier
                                    .size(48.dp)
                                    .draggableHandle(
                                        onDragStarted = { startDrag(row.key) },
                                        onDragStopped = { finishDrag() },
                                    ),
                                contentAlignment = Alignment.Center,
                            ) {
                                Icon(
                                    Icons.Rounded.DragHandle,
                                    contentDescription = stringResource(R.string.cd_drag_to_reorder),
                                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

/** Playlist-specific menu items: remove. Entries are reordered by dragging them. */
@Composable
private fun ColumnScope.EntryMenuItems(
    onRemove: () -> Unit,
    onDismiss: () -> Unit,
) {
    DropdownMenuItem(
        text = { Text(stringResource(R.string.action_remove_from_playlist)) },
        leadingIcon = { Icon(Icons.Rounded.RemoveCircleOutline, contentDescription = null) },
        onClick = {
            onDismiss()
            onRemove()
        },
    )
}

/**
 * A playlist entry whose track is not in the library. Not tappable, but it keeps its menu so the
 * entry can still be removed.
 */
@Composable
private fun UnavailableEntry(
    entry: PlaylistEntryInfo,
    menu: @Composable ColumnScope.(() -> Unit) -> Unit,
) {
    var menuOpen by remember { mutableStateOf(false) }
    val fileName = entry.trackKey.substringAfterLast('/')
    ListItem(
        modifier = Modifier.alpha(0.6f),
        leadingContent = {
            Icon(Icons.Rounded.ErrorOutline, contentDescription = null)
        },
        headlineContent = {
            Text(stringResource(R.string.playlist_unavailable_title, fileName), maxLines = 1, overflow = TextOverflow.Ellipsis)
        },
        supportingContent = { Text(stringResource(R.string.playlist_unavailable_message)) },
        trailingContent = {
            Box {
                IconButton(onClick = { menuOpen = true }) {
                    Icon(Icons.Rounded.MoreVert, contentDescription = stringResource(R.string.cd_more_options))
                }
                DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                    menu { menuOpen = false }
                }
            }
        },
    )
}

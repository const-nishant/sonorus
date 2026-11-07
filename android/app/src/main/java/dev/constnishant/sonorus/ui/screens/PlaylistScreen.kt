@file:OptIn(ExperimentalMaterial3Api::class)

package dev.constnishant.sonorus.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.ColumnScope
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
import androidx.compose.material.icons.automirrored.rounded.QueueMusic
import androidx.compose.material.icons.rounded.ArrowDownward
import androidx.compose.material.icons.rounded.ArrowUpward
import androidx.compose.material.icons.rounded.Delete
import androidx.compose.material.icons.rounded.Edit
import androidx.compose.material.icons.rounded.ErrorOutline
import androidx.compose.material.icons.rounded.MoreVert
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.RemoveCircleOutline
import androidx.compose.material.icons.rounded.Shuffle
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.constnishant.sonorus.core.PlaylistEntryInfo
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.BackButton
import dev.constnishant.sonorus.ui.components.CenteredContent
import dev.constnishant.sonorus.ui.components.ConfirmDialog
import dev.constnishant.sonorus.ui.components.EmptyState
import dev.constnishant.sonorus.ui.components.LoadingContent
import dev.constnishant.sonorus.ui.components.PlaylistNameDialog
import dev.constnishant.sonorus.ui.components.TrackActions
import dev.constnishant.sonorus.ui.components.TrackRow
import dev.constnishant.sonorus.ui.components.formatTotal
import dev.constnishant.sonorus.ui.components.rememberCurrentTrackId
import dev.constnishant.sonorus.ui.components.rememberTrackActions

/**
 * The Playlist screen. Lists the entries in order: catalogued tracks play from that entry on;
 * entries whose track is missing from the library are shown as unavailable. Each entry's menu
 * can remove it or move it up or down. The top bar menu renames or deletes the playlist.
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
        factory = viewModelFactory { initializer { PlaylistViewModel(repository, playlistId) } },
    )
    val state by viewModel.state.collectAsStateWithLifecycle()
    val currentId by rememberCurrentTrackId()
    val actions = rememberTrackActions()
    val data = (state as? LoadState.Ready)?.value
    var menuOpen by remember { mutableStateOf(false) }
    var renaming by remember { mutableStateOf(false) }
    var deleting by remember { mutableStateOf(false) }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(data?.playlist?.name ?: "Playlist", maxLines = 1, overflow = TextOverflow.Ellipsis) },
                navigationIcon = { BackButton(onBack) },
                actions = {
                    if (data != null) {
                        Box {
                            IconButton(onClick = { menuOpen = true }) {
                                Icon(Icons.Rounded.MoreVert, contentDescription = "Playlist options")
                            }
                            DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                                DropdownMenuItem(
                                    text = { Text("Rename") },
                                    leadingIcon = { Icon(Icons.Rounded.Edit, contentDescription = null) },
                                    onClick = {
                                        menuOpen = false
                                        renaming = true
                                    },
                                )
                                DropdownMenuItem(
                                    text = { Text("Delete") },
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
    ) { padding ->
        when (val s = state) {
            LoadState.Loading -> LoadingContent(Modifier.padding(padding))
            is LoadState.Failed -> CenteredContent(Modifier.padding(padding)) {
                EmptyState(Icons.Rounded.ErrorOutline, "Couldn't open playlist", s.message)
            }
            is LoadState.Ready -> PlaylistContent(
                data = s.value,
                currentId = currentId,
                actions = actions,
                onPlayAll = viewModel::playAll,
                onPlayFrom = viewModel::playFrom,
                onRemove = viewModel::remove,
                onMove = viewModel::move,
                modifier = Modifier.padding(padding),
            )
        }
    }

    if (renaming && data != null) {
        PlaylistNameDialog(
            title = "Rename playlist",
            initialName = data.playlist.name,
            confirmLabel = "Rename",
            onConfirm = { name ->
                viewModel.rename(name)
                renaming = false
            },
            onDismiss = { renaming = false },
        )
    }
    if (deleting && data != null) {
        ConfirmDialog(
            title = "Delete playlist?",
            message = "\"${data.playlist.name}\" will be removed. Its tracks stay in your library.",
            confirmLabel = "Delete",
            onConfirm = {
                deleting = false
                viewModel.delete()
                onBack()
            },
            onDismiss = { deleting = false },
        )
    }
}

/** The loaded playlist: header with Play / Shuffle, then the entries. */
@Composable
private fun PlaylistContent(
    data: PlaylistData,
    currentId: Long?,
    actions: TrackActions,
    onPlayAll: (Boolean) -> Unit,
    onPlayFrom: (UInt) -> Unit,
    onRemove: (UInt) -> Unit,
    onMove: (UInt, UInt) -> Unit,
    modifier: Modifier = Modifier,
) {
    val playlist = data.playlist
    val entries = data.entries
    val lastIndex = entries.lastIndex

    LazyColumn(modifier.fillMaxSize()) {
        item(key = "header") {
            Column(
                Modifier
                    .fillMaxWidth()
                    .padding(16.dp),
            ) {
                Text(
                    "${playlist.trackCount} songs · ${formatTotal(playlist.durationMs.toLong())}",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.height(12.dp))
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    Button(onClick = { onPlayAll(false) }, enabled = entries.isNotEmpty()) {
                        Icon(Icons.Rounded.PlayArrow, contentDescription = null, modifier = Modifier.size(ButtonDefaults.IconSize))
                        Spacer(Modifier.size(ButtonDefaults.IconSpacing))
                        Text("Play")
                    }
                    OutlinedButton(onClick = { onPlayAll(true) }, enabled = entries.isNotEmpty()) {
                        Icon(Icons.Rounded.Shuffle, contentDescription = null, modifier = Modifier.size(ButtonDefaults.IconSize))
                        Spacer(Modifier.size(ButtonDefaults.IconSpacing))
                        Text("Shuffle")
                    }
                }
            }
        }
        if (entries.isEmpty()) {
            item(key = "empty") {
                EmptyState(
                    Icons.AutoMirrored.Rounded.QueueMusic,
                    "This playlist is empty",
                    "Add songs from any track menu with \"Add to playlist…\".",
                )
            }
        }
        itemsIndexed(entries, key = { _, entry -> "pos-${entry.position}" }) { index, entry ->
            val canMoveUp = index > 0
            val canMoveDown = index < lastIndex
            val menu: @Composable ColumnScope.(() -> Unit) -> Unit = { dismiss ->
                EntryMenuItems(
                    canMoveUp = canMoveUp,
                    canMoveDown = canMoveDown,
                    onMoveUp = { onMove(entry.position, entries[index - 1].position) },
                    onMoveDown = { onMove(entry.position, entries[index + 1].position) },
                    onRemove = { onRemove(entry.position) },
                    onDismiss = dismiss,
                )
            }
            val track = entry.track
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
    }
}

/** Playlist-specific menu items: move up, move down, remove. */
@Composable
private fun ColumnScope.EntryMenuItems(
    canMoveUp: Boolean,
    canMoveDown: Boolean,
    onMoveUp: () -> Unit,
    onMoveDown: () -> Unit,
    onRemove: () -> Unit,
    onDismiss: () -> Unit,
) {
    DropdownMenuItem(
        text = { Text("Move up") },
        leadingIcon = { Icon(Icons.Rounded.ArrowUpward, contentDescription = null) },
        enabled = canMoveUp,
        onClick = {
            onDismiss()
            onMoveUp()
        },
    )
    DropdownMenuItem(
        text = { Text("Move down") },
        leadingIcon = { Icon(Icons.Rounded.ArrowDownward, contentDescription = null) },
        enabled = canMoveDown,
        onClick = {
            onDismiss()
            onMoveDown()
        },
    )
    DropdownMenuItem(
        text = { Text("Remove from playlist") },
        leadingIcon = { Icon(Icons.Rounded.RemoveCircleOutline, contentDescription = null) },
        onClick = {
            onDismiss()
            onRemove()
        },
    )
}

/**
 * A playlist entry whose track is not in the library. Not tappable, but it keeps its menu so the
 * entry can still be moved or removed.
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
            Text("Unavailable — $fileName", maxLines = 1, overflow = TextOverflow.Ellipsis)
        },
        supportingContent = { Text("Not found in the library") },
        trailingContent = {
            Box {
                IconButton(onClick = { menuOpen = true }) {
                    Icon(Icons.Rounded.MoreVert, contentDescription = "More options")
                }
                DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                    menu { menuOpen = false }
                }
            }
        },
    )
}

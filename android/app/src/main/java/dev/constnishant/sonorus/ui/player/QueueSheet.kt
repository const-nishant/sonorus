package dev.constnishant.sonorus.ui.player

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.QueueMusic
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.DragHandle
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
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
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.ui.components.SwipeRemoveBox
import kotlinx.coroutines.launch
import sh.calvin.reorderable.ReorderableCollectionItemScope
import sh.calvin.reorderable.ReorderableItem
import sh.calvin.reorderable.rememberReorderableLazyListState

/**
 * The play queue as a bottom sheet. Tapping an entry jumps to it. Long-pressing a row or dragging
 * its handle reorders the queue; swiping a row to the start removes it, with an undo message. Track
 * names come from [viewModel].
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun QueueSheet(viewModel: NowPlayingViewModel, onDismiss: () -> Unit) {
    val playback by viewModel.playback.collectAsStateWithLifecycle()
    val tracks by viewModel.tracks.collectAsStateWithLifecycle()
    val haptics = LocalHapticFeedback.current
    val scope = rememberCoroutineScope()
    val snackbarHostState = remember { SnackbarHostState() }
    val undoLabel = stringResource(R.string.action_undo)
    val listState = rememberLazyListState()
    // The list shown and reordered live. It follows the core except while a drag is in progress.
    var rows by remember { mutableStateOf(playback.queue) }
    var dragging by remember { mutableStateOf(false) }
    var draggedKey by remember { mutableStateOf<Long?>(null) }
    var dragOrigin by remember { mutableIntStateOf(-1) }

    LaunchedEffect(playback.queue) {
        viewModel.resolveTracks(playback.queue.map { it.trackId })
        if (!dragging) rows = playback.queue
    }

    val reorderState = rememberReorderableLazyListState(listState) { from, to ->
        val fromIndex = rows.indexOfFirst { it.entryId.toLong() == from.key }
        val toIndex = rows.indexOfFirst { it.entryId.toLong() == to.key }
        if (fromIndex >= 0 && toIndex >= 0 && fromIndex != toIndex) {
            rows = rows.toMutableList().apply { add(toIndex, removeAt(fromIndex)) }
            haptics.performHapticFeedback(HapticFeedbackType.TextHandleMove)
        }
    }

    fun startDrag(key: Long) {
        dragging = true
        draggedKey = key
        dragOrigin = rows.indexOfFirst { it.entryId.toLong() == key }
        haptics.performHapticFeedback(HapticFeedbackType.TextHandleMove)
    }

    fun finishDrag() {
        dragging = false
        val key = draggedKey ?: return
        draggedKey = null
        val index = rows.indexOfFirst { it.entryId.toLong() == key }
        if (index >= 0 && index != dragOrigin) {
            viewModel.moveInQueue(rows[index].entryId, index)
        }
    }

    ModalBottomSheet(onDismissRequest = onDismiss) {
        Text(
            text = stringResource(R.string.now_playing_up_next),
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
        )
        if (rows.isEmpty()) {
            Text(
                text = stringResource(R.string.queue_empty),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 16.dp),
            )
        }
        LazyColumn(
            state = listState,
            modifier = Modifier.fillMaxWidth().heightIn(max = 600.dp),
        ) {
            itemsIndexed(
                items = rows,
                // Lazy keys must be Bundle-storable: ULong is not, so the id is passed as a Long.
                key = { _, item -> item.entryId.toLong() },
            ) { _, item ->
                ReorderableItem(reorderState, key = item.entryId.toLong()) { isDragging ->
                    val track = tracks[item.trackId]
                    val known = tracks.containsKey(item.trackId)
                    val removedMessage = stringResource(
                        R.string.snackbar_removed,
                        track?.title ?: stringResource(R.string.file_name_fallback),
                    )
                    // Removes the entry at once and offers undo; used by the swipe and the close button.
                    val remove: () -> Unit = {
                        val index = rows.indexOfFirst { it.entryId == item.entryId }.coerceAtLeast(0)
                        rows = rows.filterNot { it.entryId == item.entryId }
                        viewModel.removeFromQueue(item.entryId)
                        scope.launch {
                            val result = snackbarHostState.showSnackbar(
                                message = removedMessage,
                                actionLabel = undoLabel,
                                duration = SnackbarDuration.Long,
                            )
                            if (result == SnackbarResult.ActionPerformed) {
                                viewModel.restoreToQueue(item.trackId, index)
                            }
                        }
                    }
                    Column {
                        SwipeRemoveBox(onRemove = remove) {
                            QueueRow(
                                title = when {
                                    track != null -> track.title
                                    known -> stringResource(R.string.queue_unavailable)
                                    else -> stringResource(R.string.queue_loading)
                                },
                                subtitle = track?.artist,
                                isCurrent = item.entryId == playback.entryId,
                                isDragging = isDragging,
                                onJump = { viewModel.jumpTo(item.entryId) },
                                onRemove = remove,
                                onDragStart = { startDrag(item.entryId.toLong()) },
                                onDragStop = { finishDrag() },
                            )
                        }
                        HorizontalDivider()
                    }
                }
            }
        }
        SnackbarHost(
            hostState = snackbarHostState,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        )
    }
}

/**
 * One queue entry. Tapping the title jumps to it; long-pressing the title or dragging the handle at
 * the end reorders it, and the close button removes it. A dragged row is raised.
 */
@Composable
private fun ReorderableCollectionItemScope.QueueRow(
    title: String,
    subtitle: String?,
    isCurrent: Boolean,
    isDragging: Boolean,
    onJump: () -> Unit,
    onRemove: () -> Unit,
    onDragStart: () -> Unit,
    onDragStop: () -> Unit,
) {
    val accent = MaterialTheme.colorScheme.primary
    Surface(
        color = if (isDragging) {
            MaterialTheme.colorScheme.surfaceContainerHigh
        } else {
            MaterialTheme.colorScheme.surfaceContainerLow
        },
        shadowElevation = if (isDragging) 8.dp else 0.dp,
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Row(
                modifier = Modifier
                    .weight(1f)
                    .longPressDraggableHandle(
                        onDragStarted = { onDragStart() },
                        onDragStopped = onDragStop,
                    )
                    .clickable(role = Role.Button, onClick = onJump)
                    .padding(start = 24.dp, top = 4.dp, bottom = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (isCurrent) {
                    Icon(
                        imageVector = Icons.AutoMirrored.Rounded.QueueMusic,
                        contentDescription = stringResource(R.string.cd_now_playing),
                        tint = accent,
                        modifier = Modifier.size(20.dp),
                    )
                    Spacer(Modifier.width(12.dp))
                }
                Column(Modifier.weight(1f)) {
                    Text(
                        text = title,
                        style = MaterialTheme.typography.bodyLarge,
                        color = if (isCurrent) accent else MaterialTheme.colorScheme.onSurface,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (!subtitle.isNullOrEmpty()) {
                        Text(
                            text = subtitle,
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
            }
            IconButton(onClick = onRemove) {
                Icon(Icons.Rounded.Close, contentDescription = stringResource(R.string.cd_remove_from_queue))
            }
            Box(
                Modifier
                    .size(48.dp)
                    .draggableHandle(
                        onDragStarted = { onDragStart() },
                        onDragStopped = onDragStop,
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

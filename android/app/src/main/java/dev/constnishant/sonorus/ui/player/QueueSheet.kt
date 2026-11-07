package dev.constnishant.sonorus.ui.player

import androidx.compose.foundation.clickable
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
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.ArrowDownward
import androidx.compose.material.icons.rounded.ArrowUpward
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.QueueMusic
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle

/**
 * The play queue as a bottom sheet. Tapping an entry jumps to it; the buttons on each row move it
 * one place up or down or remove it. Track names come from [viewModel].
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun QueueSheet(viewModel: NowPlayingViewModel, onDismiss: () -> Unit) {
    val playback by viewModel.playback.collectAsStateWithLifecycle()
    val tracks by viewModel.tracks.collectAsStateWithLifecycle()

    LaunchedEffect(playback.queue) {
        viewModel.resolveTracks(playback.queue.map { it.trackId })
    }

    ModalBottomSheet(onDismissRequest = onDismiss) {
        Text(
            text = "Up next",
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
        )
        if (playback.queue.isEmpty()) {
            Text(
                text = "The queue is empty.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 24.dp, vertical = 16.dp),
            )
        }
        LazyColumn(modifier = Modifier.fillMaxWidth().heightIn(max = 600.dp)) {
            itemsIndexed(
                items = playback.queue,
                key = { _, item -> item.entryId },
            ) { index, item ->
                val track = tracks[item.trackId]
                val known = tracks.containsKey(item.trackId)
                QueueRow(
                    title = when {
                        track != null -> track.title
                        known -> "Unavailable"
                        else -> "Loading..."
                    },
                    subtitle = track?.artist,
                    isCurrent = item.entryId == playback.entryId,
                    canMoveUp = index > 0,
                    canMoveDown = index < playback.queue.lastIndex,
                    onJump = { viewModel.jumpTo(item.entryId) },
                    onRemove = { viewModel.removeFromQueue(item.entryId) },
                    onMoveUp = { viewModel.moveInQueue(item.entryId, index - 1) },
                    onMoveDown = { viewModel.moveInQueue(item.entryId, index + 1) },
                )
                HorizontalDivider()
            }
        }
    }
}

/** One queue entry: tap to jump; trailing buttons move it up or down or remove it. */
@Composable
private fun QueueRow(
    title: String,
    subtitle: String?,
    isCurrent: Boolean,
    canMoveUp: Boolean,
    canMoveDown: Boolean,
    onJump: () -> Unit,
    onRemove: () -> Unit,
    onMoveUp: () -> Unit,
    onMoveDown: () -> Unit,
) {
    val accent = MaterialTheme.colorScheme.primary
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onJump)
            .padding(start = 24.dp, end = 8.dp, top = 4.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (isCurrent) {
            Icon(
                imageVector = Icons.Rounded.QueueMusic,
                contentDescription = "Now playing",
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
        IconButton(onClick = onMoveUp, enabled = canMoveUp) {
            Icon(Icons.Rounded.ArrowUpward, contentDescription = "Move up")
        }
        IconButton(onClick = onMoveDown, enabled = canMoveDown) {
            Icon(Icons.Rounded.ArrowDownward, contentDescription = "Move down")
        }
        IconButton(onClick = onRemove) {
            Icon(Icons.Rounded.Close, contentDescription = "Remove from queue")
        }
    }
}

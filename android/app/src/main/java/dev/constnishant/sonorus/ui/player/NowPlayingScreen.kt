package dev.constnishant.sonorus.ui.player

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Favorite
import androidx.compose.material.icons.rounded.FavoriteBorder
import androidx.compose.material.icons.rounded.KeyboardArrowDown
import androidx.compose.material.icons.rounded.Pause
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.QueueMusic
import androidx.compose.material.icons.rounded.Repeat
import androidx.compose.material.icons.rounded.RepeatOne
import androidx.compose.material.icons.rounded.Shuffle
import androidx.compose.material.icons.rounded.SkipNext
import androidx.compose.material.icons.rounded.SkipPrevious
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Slider
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.constnishant.sonorus.core.RepeatMode
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.data.PlaybackUi
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.Artwork
import dev.constnishant.sonorus.ui.components.formatDuration
import dev.constnishant.sonorus.ui.components.isHiRes
import dev.constnishant.sonorus.ui.components.qualityLabel

/**
 * Full-screen player. [onBack] collapses it, [onAlbum] and [onArtist] open the album or artist of
 * the current track. Shows "Nothing is playing" when there is no current track.
 */
@Composable
fun NowPlayingScreen(
    onBack: () -> Unit,
    onAlbum: (Long) -> Unit,
    onArtist: (Long) -> Unit,
    modifier: Modifier = Modifier,
) {
    val repository = LocalContext.current.repository
    val viewModel: NowPlayingViewModel = viewModel { NowPlayingViewModel(repository) }
    val playback by viewModel.playback.collectAsStateWithLifecycle()
    val track = playback.track
    var showQueue by rememberSaveable { mutableStateOf(false) }

    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 24.dp, vertical = 8.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            IconButton(onClick = onBack) {
                Icon(Icons.Rounded.KeyboardArrowDown, contentDescription = "Collapse")
            }
            Text(
                text = "Now playing",
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.size(48.dp))
        }

        if (track == null) {
            Box(
                modifier = Modifier.fillMaxWidth().height(400.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = "Nothing is playing",
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        } else {
            NowPlayingBody(
                playback = playback,
                track = track,
                viewModel = viewModel,
                onAlbum = onAlbum,
                onArtist = onArtist,
            )
        }

        TextButton(onClick = { showQueue = true }) {
            Icon(Icons.Rounded.QueueMusic, contentDescription = null)
            Spacer(Modifier.width(8.dp))
            Text("Up next")
        }
    }

    if (showQueue) {
        QueueSheet(viewModel = viewModel, onDismiss = { showQueue = false })
    }
}

/** Everything below the top bar when a track is loaded. */
@Composable
private fun NowPlayingBody(
    playback: PlaybackUi,
    track: TrackInfo,
    viewModel: NowPlayingViewModel,
    onAlbum: (Long) -> Unit,
    onArtist: (Long) -> Unit,
) {
    val favoriteOverride by viewModel.favoriteOverride.collectAsStateWithLifecycle()
    val favorite = favoriteOverride?.takeIf { it.trackId == track.id }?.favorite ?: track.isFavorite
    val hiRes = track.isHiRes()
    val quality = playback.stream?.label ?: track.qualityLabel()
    val busy = playback.state.isBusy()
    val playing = playback.state.showsPause()
    val signalPath by viewModel.signalPath.collectAsStateWithLifecycle()
    val dac by viewModel.dac.collectAsStateWithLifecycle()
    var showSignalPath by rememberSaveable { mutableStateOf(false) }
    val pathChip = signalPath?.takeIf { it.error == null }

    Spacer(Modifier.height(8.dp))
    Artwork(
        path = track.coverPath,
        contentDescription = null,
        modifier = Modifier.fillMaxWidth().aspectRatio(1f),
        cornerRadius = 16.dp,
    )
    Spacer(Modifier.height(24.dp))

    playback.recovery?.let { recovery ->
        val seconds = (recovery.retryInMs + 999UL) / 1000UL
        Surface(
            modifier = Modifier.fillMaxWidth().padding(bottom = 16.dp),
            shape = RoundedCornerShape(12.dp),
            color = MaterialTheme.colorScheme.errorContainer,
            contentColor = MaterialTheme.colorScheme.onErrorContainer,
        ) {
            Text(
                text = "Connection lost — retrying in ${seconds}s (attempt ${recovery.attempt})",
                modifier = Modifier.padding(12.dp),
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }

    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.Top,
    ) {
        Column(Modifier.weight(1f)) {
            Text(
                text = track.title,
                style = MaterialTheme.typography.titleLarge,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = track.artist,
                style = MaterialTheme.typography.titleMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.clickable { onArtist(track.artistId) },
            )
            Text(
                text = track.album,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.clickable { onAlbum(track.albumId) },
            )
        }
        IconButton(onClick = { viewModel.setFavorite(track.id, !favorite) }) {
            Icon(
                imageVector = if (favorite) Icons.Rounded.Favorite else Icons.Rounded.FavoriteBorder,
                contentDescription = if (favorite) "Remove from favorites" else "Add to favorites",
                tint = if (favorite) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }

    if (quality != null || pathChip != null) {
        Spacer(Modifier.height(8.dp))
        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (quality != null) {
                val tint = if (hiRes) MaterialTheme.colorScheme.tertiary else MaterialTheme.colorScheme.onSurfaceVariant
                Surface(
                    shape = RoundedCornerShape(50),
                    color = Color.Transparent,
                    contentColor = tint,
                    border = BorderStroke(1.dp, tint),
                ) {
                    Text(
                        text = quality,
                        modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
                        style = MaterialTheme.typography.labelMedium,
                    )
                }
            }
            if (pathChip != null) {
                SignalPathChip(path = pathChip, onClick = { showSignalPath = true })
            }
        }
    }

    if (showSignalPath && pathChip != null) {
        SignalPathDialog(
            path = pathChip,
            dac = dac,
            onDismiss = { showSignalPath = false },
        )
    }

    Spacer(Modifier.height(16.dp))
    SeekBar(playback = playback, onSeek = viewModel::seek)
    Spacer(Modifier.height(8.dp))

    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        IconButton(onClick = viewModel::toggleShuffle) {
            Icon(
                Icons.Rounded.Shuffle,
                contentDescription = "Shuffle",
                tint = if (playback.shuffle) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        IconButton(onClick = viewModel::previous, modifier = Modifier.size(56.dp)) {
            Icon(Icons.Rounded.SkipPrevious, contentDescription = "Previous", modifier = Modifier.size(36.dp))
        }
        FilledIconButton(
            onClick = viewModel::togglePlayPause,
            modifier = Modifier.size(64.dp),
        ) {
            if (busy) {
                CircularProgressIndicator(
                    modifier = Modifier.size(28.dp),
                    strokeWidth = 3.dp,
                    color = MaterialTheme.colorScheme.onPrimary,
                )
            } else {
                Icon(
                    imageVector = if (playing) Icons.Rounded.Pause else Icons.Rounded.PlayArrow,
                    contentDescription = if (playing) "Pause" else "Play",
                    modifier = Modifier.size(36.dp),
                )
            }
        }
        IconButton(onClick = viewModel::next, modifier = Modifier.size(56.dp)) {
            Icon(Icons.Rounded.SkipNext, contentDescription = "Next", modifier = Modifier.size(36.dp))
        }
        IconButton(onClick = viewModel::cycleRepeat) {
            val repeatOn = playback.repeat != RepeatMode.OFF
            Icon(
                imageVector = if (playback.repeat == RepeatMode.ONE) Icons.Rounded.RepeatOne else Icons.Rounded.Repeat,
                contentDescription = "Repeat",
                tint = if (repeatOn) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/**
 * Seek slider with elapsed and remaining time. While the user drags, the slider shows the drag
 * position and ignores position updates; the seek happens when the drag ends.
 */
@Composable
private fun SeekBar(playback: PlaybackUi, onSeek: (Long) -> Unit) {
    val duration = playback.durationMs
    val max = (duration ?: 0L).coerceAtLeast(1L).toFloat()
    var dragging by remember { mutableStateOf(false) }
    var dragMs by remember { mutableFloatStateOf(0f) }
    val shownMs = if (dragging) dragMs.toLong() else playback.positionMs

    Slider(
        value = (if (dragging) dragMs else playback.positionMs.toFloat()).coerceIn(0f, max),
        onValueChange = {
            dragging = true
            dragMs = it
        },
        onValueChangeFinished = {
            if (dragging) onSeek(dragMs.toLong())
            dragging = false
        },
        valueRange = 0f..max,
        enabled = duration != null,
    )
    Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
        Text(
            text = formatDuration(shownMs),
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            text = if (duration == null) {
                formatDuration(null)
            } else {
                "-" + formatDuration((duration - shownMs).coerceAtLeast(0L))
            },
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.End,
        )
    }
}

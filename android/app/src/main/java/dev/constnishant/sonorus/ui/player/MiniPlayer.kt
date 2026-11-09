package dev.constnishant.sonorus.ui.player

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.spring
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Pause
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.SkipNext
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.PlaybackState
import dev.constnishant.sonorus.data.PlaybackUi
import dev.constnishant.sonorus.ui.components.Artwork
import dev.constnishant.sonorus.ui.components.animatedDominant
import dev.constnishant.sonorus.ui.components.rememberArtworkColors
import kotlinx.coroutines.launch
import kotlin.math.abs

/** True when the play button should show pause: playing, or about to play. */
internal fun PlaybackState.showsPause(): Boolean = this == PlaybackState.PLAYING || isBusy()

/** True while the core is opening or buffering audio. */
internal fun PlaybackState.isBusy(): Boolean =
    this == PlaybackState.LOADING || this == PlaybackState.BUFFERING

/**
 * Compact playback bar shown above the bottom navigation while a track is loaded. Tapping the
 * bar calls [onOpen]; the buttons call [onPlayPause], [onNext] and [onPrevious]. A horizontal
 * swipe past a third of the width calls [onNext] (left) or [onPrevious] (right). Renders nothing
 * when there is no current track.
 */
@Composable
fun MiniPlayer(
    playback: PlaybackUi,
    onOpen: () -> Unit,
    onPlayPause: () -> Unit,
    onNext: () -> Unit,
    onPrevious: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val artwork = rememberArtworkColors(playback.track?.coverPath)
    val dominant by artwork.animatedDominant(MaterialTheme.colorScheme.surfaceVariant)
    val track = playback.track ?: return
    val duration = playback.durationMs
    val progress = if (duration != null && duration > 0) {
        (playback.positionMs.toFloat() / duration.toFloat()).coerceIn(0f, 1f)
    } else {
        0f
    }

    val scope = rememberCoroutineScope()
    val swipeX = remember { Animatable(0f) }
    val currentNext by rememberUpdatedState(onNext)
    val currentPrevious by rememberUpdatedState(onPrevious)

    Surface(
        onClick = onOpen,
        modifier = modifier
            .fillMaxWidth()
            .pointerInput(Unit) {
                detectHorizontalDragGestures(
                    onDragStart = { scope.launch { swipeX.stop() } },
                    onDragEnd = {
                        val dx = swipeX.value
                        if (abs(dx) > size.width / 3f) {
                            if (dx < 0f) currentNext() else currentPrevious()
                        }
                        scope.launch { swipeX.animateTo(0f, spring(stiffness = Spring.StiffnessMediumLow)) }
                    },
                    onDragCancel = { scope.launch { swipeX.animateTo(0f) } },
                    onHorizontalDrag = { change, amount ->
                        change.consume()
                        scope.launch { swipeX.snapTo(swipeX.value + amount) }
                    },
                )
            }
            .graphicsLayer { translationX = swipeX.value },
        color = MaterialTheme.colorScheme.surfaceContainer,
    ) {
        Box(Modifier.fillMaxWidth().height(64.dp)) {
            Box(Modifier.matchParentSize().background(dominant.copy(alpha = 0.6f)))
            LinearProgressIndicator(
                progress = { progress },
                modifier = Modifier.align(Alignment.TopCenter).fillMaxWidth().height(2.dp),
            )
            Row(
                modifier = Modifier.fillMaxSize().padding(horizontal = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Artwork(
                    path = track.coverPath,
                    contentDescription = null,
                    modifier = Modifier.size(48.dp),
                    shape = MaterialTheme.shapes.small,
                )
                Spacer(Modifier.width(12.dp))
                Column(Modifier.weight(1f)) {
                    Text(
                        text = track.title,
                        style = MaterialTheme.typography.titleSmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        text = track.artist,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                IconButton(onClick = onPlayPause) {
                    Box(contentAlignment = Alignment.Center) {
                        if (playback.state.isBusy()) {
                            CircularProgressIndicator(
                                modifier = Modifier.size(32.dp),
                                strokeWidth = 2.dp,
                            )
                        }
                        val pause = playback.state.showsPause()
                        Icon(
                            imageVector = if (pause) Icons.Rounded.Pause else Icons.Rounded.PlayArrow,
                            contentDescription = if (pause) {
                                stringResource(R.string.cd_pause)
                            } else {
                                stringResource(R.string.cd_play)
                            },
                        )
                    }
                }
                IconButton(onClick = onNext) {
                    Icon(Icons.Rounded.SkipNext, contentDescription = stringResource(R.string.cd_next))
                }
            }
        }
    }
}

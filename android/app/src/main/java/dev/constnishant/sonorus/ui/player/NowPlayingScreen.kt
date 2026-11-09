package dev.constnishant.sonorus.ui.player

import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.AnimationVector1D
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.spring
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.animation.scaleOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectVerticalDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.QueueMusic
import androidx.compose.material.icons.outlined.Bedtime
import androidx.compose.material.icons.rounded.Bedtime
import androidx.compose.material.icons.rounded.Favorite
import androidx.compose.material.icons.rounded.FavoriteBorder
import androidx.compose.material.icons.rounded.KeyboardArrowDown
import androidx.compose.material.icons.rounded.Pause
import androidx.compose.material.icons.rounded.PlayArrow
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
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.blur
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.RectangleShape
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.util.VelocityTracker
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModelProvider.AndroidViewModelFactory.Companion.APPLICATION_KEY
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.RepeatMode
import dev.constnishant.sonorus.core.ReplayGainModeInfo
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.data.PlaybackUi
import dev.constnishant.sonorus.data.SleepTimerUi
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.Artwork
import dev.constnishant.sonorus.ui.components.SonorusChip
import dev.constnishant.sonorus.ui.components.animatedDominant
import dev.constnishant.sonorus.ui.components.formatDuration
import dev.constnishant.sonorus.ui.components.isHiRes
import dev.constnishant.sonorus.ui.components.qualityLabel
import dev.constnishant.sonorus.ui.components.rememberArtworkColors
import dev.constnishant.sonorus.ui.theme.tabular
import kotlin.math.abs
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/** Drag distance, in dp, past which releasing the screen dismisses it. */
private val DismissDistance = 150.dp

/** Downward fling speed, in dp per second, that dismisses the screen on release. */
private val DismissFling = 600.dp

/** Movement, in px, after which a drag on the cover commits to horizontal or vertical. */
private const val SwipeLockPx = 16f

/**
 * Full-screen player. [onBack] collapses it (also used when the screen is swiped down), [onAlbum]
 * and [onArtist] open the album or artist of the current track. Shows "Nothing is playing" when
 * there is no current track.
 */
@Composable
fun NowPlayingScreen(
    onBack: () -> Unit,
    onAlbum: (Long) -> Unit,
    onArtist: (Long) -> Unit,
    modifier: Modifier = Modifier,
) {
    val repository = LocalContext.current.repository
    val viewModel: NowPlayingViewModel = viewModel { NowPlayingViewModel(checkNotNull(this[APPLICATION_KEY]), repository) }
    val playback by viewModel.playback.collectAsStateWithLifecycle()
    val sleepTimer by viewModel.sleepTimer.collectAsStateWithLifecycle()
    val track = playback.track
    var showQueue by rememberSaveable { mutableStateOf(false) }
    var showSleepPicker by rememberSaveable { mutableStateOf(false) }
    // The saved ReplayGain mode, for the signal path dialog. Read from the settings store on entry.
    val replayGainMode by produceState(initialValue = ReplayGainModeInfo.OFF) {
        value = repository.replayGainSettings().mode
    }
    val haptic = LocalHapticFeedback.current
    val scope = rememberCoroutineScope()
    val dismissY = remember { Animatable(0f) }
    val artwork = rememberArtworkColors(track?.coverPath)
    val dominant by artwork.animatedDominant(MaterialTheme.colorScheme.surfaceVariant)
    val background = MaterialTheme.colorScheme.background
    val coverPath = track?.coverPath
    val onSkip: (Boolean) -> Unit = { forward ->
        haptic.performHapticFeedback(HapticFeedbackType.TextHandleMove)
        if (forward) viewModel.next() else viewModel.previous()
    }

    Box(modifier.fillMaxSize()) {
        // Backdrop: the blurred cover, then a fade from its dominant color into the background.
        if (coverPath != null) {
            Artwork(
                path = coverPath,
                contentDescription = null,
                modifier = Modifier.fillMaxSize().blur(48.dp).alpha(0.35f),
                shape = RectangleShape,
            )
        }
        Box(
            Modifier
                .fillMaxSize()
                .background(
                    Brush.verticalGradient(
                        listOf(dominant.copy(alpha = 0.85f), background),
                    ),
                ),
        )

        BoxWithConstraints(
            Modifier
                .fillMaxSize()
                .windowInsetsPadding(WindowInsets.safeDrawing)
                .graphicsLayer { translationY = dismissY.value },
        ) {
            val landscape = maxWidth > maxHeight
            val scroll = rememberScrollState()
            Column(Modifier.fillMaxSize().padding(horizontal = 24.dp, vertical = 8.dp)) {
                NowPlayingTopBar(
                    onBack = onBack,
                    dismissY = dismissY,
                    sleepTimer = sleepTimer,
                    onSleepTimer = { showSleepPicker = true },
                )

                when {
                    track == null -> Column(Modifier.weight(1f).verticalScroll(scroll)) {
                        Box(
                            modifier = Modifier.fillMaxWidth().height(400.dp),
                            contentAlignment = Alignment.Center,
                        ) {
                            Text(
                                text = stringResource(R.string.now_playing_empty),
                                style = MaterialTheme.typography.bodyLarge,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        UpNextButton(onClick = { showQueue = true })
                    }

                    landscape -> Row(Modifier.weight(1f)) {
                        CoverArt(
                            path = track.coverPath,
                            dismissY = dismissY,
                            onSkip = onSkip,
                            onDismiss = onBack,
                            modifier = Modifier.fillMaxHeight().padding(vertical = 8.dp).aspectRatio(1f),
                        )
                        Spacer(Modifier.width(24.dp))
                        Column(Modifier.weight(1f).verticalScroll(scroll)) {
                            NowPlayingDetails(
                                playback = playback,
                                track = track,
                                viewModel = viewModel,
                                sleepTimer = sleepTimer,
                                replayGainMode = replayGainMode,
                                onSleepTimer = { showSleepPicker = true },
                                onAlbum = onAlbum,
                                onArtist = onArtist,
                            )
                            UpNextButton(onClick = { showQueue = true })
                        }
                    }

                    else -> Column(Modifier.weight(1f).verticalScroll(scroll)) {
                        Spacer(Modifier.height(8.dp))
                        CoverArt(
                            path = track.coverPath,
                            dismissY = dismissY,
                            onSkip = onSkip,
                            onDismiss = onBack,
                            modifier = Modifier.fillMaxWidth().aspectRatio(1f),
                        )
                        Spacer(Modifier.height(24.dp))
                        NowPlayingDetails(
                            playback = playback,
                            track = track,
                            viewModel = viewModel,
                            sleepTimer = sleepTimer,
                            replayGainMode = replayGainMode,
                            onSleepTimer = { showSleepPicker = true },
                            onAlbum = onAlbum,
                            onArtist = onArtist,
                        )
                        UpNextButton(onClick = { showQueue = true })
                    }
                }
            }
        }
    }

    if (showQueue) {
        QueueSheet(viewModel = viewModel, onDismiss = { showQueue = false })
    }
    if (showSleepPicker) {
        SleepTimerSheet(
            active = sleepTimer.active,
            onSelect = { request ->
                showSleepPicker = false
                viewModel.setSleepTimer(request)
            },
            onDismiss = { showSleepPicker = false },
        )
    }
}

/**
 * The collapse button, title row and sleep timer button. Dragging it down moves the whole screen
 * with the finger; releasing past 150 dp, or flinging down, calls [onBack], otherwise the screen
 * springs back. [onSleepTimer] opens the sleep timer picker; the icon is filled while a timer is set.
 */
@Composable
private fun NowPlayingTopBar(
    onBack: () -> Unit,
    dismissY: Animatable<Float, AnimationVector1D>,
    sleepTimer: SleepTimerUi,
    onSleepTimer: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val currentOnBack by rememberUpdatedState(onBack)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .pointerInput(Unit) {
                val velocity = VelocityTracker()
                detectVerticalDragGestures(
                    onDragStart = {
                        velocity.resetTracking()
                        scope.launch { dismissY.stop() }
                    },
                    onDragEnd = {
                        scope.settleDismiss(
                            dismissY = dismissY,
                            velocityY = velocity.calculateVelocity().y,
                            distancePx = DismissDistance.toPx(),
                            flingPx = DismissFling.toPx(),
                            onDismiss = { currentOnBack() },
                        )
                    },
                    onDragCancel = { scope.launch { dismissY.animateTo(0f) } },
                    onVerticalDrag = { change, amount ->
                        change.consume()
                        velocity.addPosition(change.uptimeMillis, change.position)
                        scope.launch { dismissY.snapTo((dismissY.value + amount).coerceAtLeast(0f)) }
                    },
                )
            },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        IconButton(onClick = onBack) {
            Icon(Icons.Rounded.KeyboardArrowDown, contentDescription = stringResource(R.string.cd_collapse))
        }
        Text(
            text = stringResource(R.string.now_playing_title),
            style = MaterialTheme.typography.labelLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        IconButton(onClick = onSleepTimer) {
            Icon(
                imageVector = if (sleepTimer.active) Icons.Rounded.Bedtime else Icons.Outlined.Bedtime,
                contentDescription = stringResource(R.string.cd_sleep_timer),
                tint = if (sleepTimer.active) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/**
 * The big cover. A horizontal drag moves it and skips a track when released past a quarter of
 * its width (left for next, right for previous). A vertical drag moves [dismissY] down, and
 * [onDismiss] is called when released past 150 dp or flung down. The first 16 px of a drag pick
 * its axis, so the two gestures never fight.
 */
@Composable
private fun CoverArt(
    path: String?,
    dismissY: Animatable<Float, AnimationVector1D>,
    onSkip: (forward: Boolean) -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val shape = MaterialTheme.shapes.extraLarge
    val scope = rememberCoroutineScope()
    val swipeX = remember { Animatable(0f) }
    var widthPx by remember { mutableFloatStateOf(1f) }
    val currentSkip by rememberUpdatedState(onSkip)
    val currentDismiss by rememberUpdatedState(onDismiss)

    Artwork(
        path = path,
        contentDescription = null,
        modifier = modifier
            .shadow(24.dp, shape)
            .onSizeChanged { widthPx = it.width.toFloat() }
            // pointerInput sits before graphicsLayer so drag positions ignore the cover's own offset.
            .pointerInput(Unit) {
                val velocity = VelocityTracker()
                var horizontal: Boolean? = null
                var totalX = 0f
                var totalY = 0f

                fun follow(dx: Float, dy: Float) {
                    if (horizontal == true) {
                        scope.launch { swipeX.snapTo(swipeX.value + dx) }
                    } else if (horizontal == false) {
                        scope.launch { dismissY.snapTo((dismissY.value + dy).coerceAtLeast(0f)) }
                    }
                }

                detectDragGestures(
                    onDragStart = {
                        velocity.resetTracking()
                        horizontal = null
                        totalX = 0f
                        totalY = 0f
                        scope.launch {
                            swipeX.stop()
                            dismissY.stop()
                        }
                    },
                    onDrag = { change, drag ->
                        change.consume()
                        velocity.addPosition(change.uptimeMillis, change.position)
                        if (horizontal == null) {
                            totalX += drag.x
                            totalY += drag.y
                            if (abs(totalX) >= SwipeLockPx || abs(totalY) >= SwipeLockPx) {
                                val isHorizontal = abs(totalX) >= abs(totalY)
                                horizontal = isHorizontal
                                if (isHorizontal) follow(totalX, 0f) else follow(0f, totalY)
                            }
                        } else {
                            follow(drag.x, drag.y)
                        }
                    },
                    onDragEnd = {
                        when (horizontal) {
                            true -> scope.launch {
                                val dx = swipeX.value
                                if (abs(dx) > widthPx / 4f) currentSkip(dx < 0f)
                                swipeX.animateTo(0f, spring(stiffness = Spring.StiffnessMediumLow))
                            }
                            false -> scope.settleDismiss(
                                dismissY = dismissY,
                                velocityY = velocity.calculateVelocity().y,
                                distancePx = DismissDistance.toPx(),
                                flingPx = DismissFling.toPx(),
                                onDismiss = { currentDismiss() },
                            )
                            null -> Unit
                        }
                    },
                    onDragCancel = {
                        scope.launch { swipeX.animateTo(0f) }
                        scope.launch { dismissY.animateTo(0f) }
                    },
                )
            }
            .graphicsLayer {
                translationX = swipeX.value
                alpha = 1f - 0.35f * (abs(swipeX.value) / widthPx).coerceAtMost(1f)
            },
        shape = shape,
    )
}

/**
 * Finishes a swipe down: calls [onDismiss] when [dismissY] is past [distancePx] or the release
 * speed [velocityY] (px per second, positive is down) is above [flingPx]; otherwise springs back.
 */
private fun CoroutineScope.settleDismiss(
    dismissY: Animatable<Float, AnimationVector1D>,
    velocityY: Float,
    distancePx: Float,
    flingPx: Float,
    onDismiss: () -> Unit,
) {
    launch {
        if (dismissY.value > distancePx || velocityY > flingPx) {
            onDismiss()
        } else {
            dismissY.animateTo(0f, spring(stiffness = Spring.StiffnessMediumLow))
        }
    }
}

/** The "Up next" button that opens the queue sheet. */
@Composable
private fun UpNextButton(onClick: () -> Unit) {
    TextButton(onClick = onClick) {
        Icon(Icons.AutoMirrored.Rounded.QueueMusic, contentDescription = null)
        Spacer(Modifier.width(8.dp))
        Text(stringResource(R.string.now_playing_up_next))
    }
}

/**
 * Everything below the cover when a track is loaded: recovery notice, title and links, quality
 * chips (with the sleep countdown chip while a timer is set), seek bar and transport controls.
 * [replayGainMode] is passed to the signal path dialog; [onSleepTimer] opens the sleep timer picker.
 */
@Composable
private fun NowPlayingDetails(
    playback: PlaybackUi,
    track: TrackInfo,
    viewModel: NowPlayingViewModel,
    sleepTimer: SleepTimerUi,
    replayGainMode: ReplayGainModeInfo,
    onSleepTimer: () -> Unit,
    onAlbum: (Long) -> Unit,
    onArtist: (Long) -> Unit,
) {
    val haptic = LocalHapticFeedback.current
    val favoriteOverride by viewModel.favoriteOverride.collectAsStateWithLifecycle()
    val favorite = favoriteOverride?.takeIf { it.trackId == track.id }?.favorite ?: track.isFavorite
    val hiRes = track.isHiRes()
    val quality = playback.stream?.label ?: track.qualityLabel(LocalContext.current)
    val busy = playback.state.isBusy()
    val playing = playback.state.showsPause()
    val signalPath by viewModel.signalPath.collectAsStateWithLifecycle()
    val dac by viewModel.dac.collectAsStateWithLifecycle()
    var showSignalPath by rememberSaveable { mutableStateOf(false) }
    val pathChip = signalPath?.takeIf { it.error == null }

    playback.recovery?.let { recovery ->
        val seconds = (recovery.retryInMs + 999UL) / 1000UL
        Surface(
            modifier = Modifier.fillMaxWidth().padding(bottom = 16.dp),
            shape = MaterialTheme.shapes.medium,
            color = MaterialTheme.colorScheme.errorContainer,
            contentColor = MaterialTheme.colorScheme.onErrorContainer,
        ) {
            Text(
                text = stringResource(
                    R.string.now_playing_retrying,
                    seconds.toLong(),
                    recovery.attempt.toLong(),
                ),
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
                modifier = Modifier.clickable(role = Role.Button) { onArtist(track.artistId) },
            )
            Text(
                text = track.album,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.clickable(role = Role.Button) { onAlbum(track.albumId) },
            )
        }
        IconButton(onClick = { viewModel.setFavorite(track.id, !favorite) }) {
            Icon(
                imageVector = if (favorite) Icons.Rounded.Favorite else Icons.Rounded.FavoriteBorder,
                contentDescription = if (favorite) {
                    stringResource(R.string.cd_remove_from_favorites)
                } else {
                    stringResource(R.string.cd_add_to_favorites)
                },
                tint = if (favorite) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }

    if (quality != null || pathChip != null || sleepTimer.active) {
        Spacer(Modifier.height(8.dp))
        Row(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            if (quality != null) {
                SonorusChip(text = quality, accent = hiRes)
            }
            if (pathChip != null) {
                SignalPathChip(path = pathChip, onClick = { showSignalPath = true })
            }
            if (sleepTimer.active) {
                SleepChip(timer = sleepTimer, onClick = onSleepTimer)
            }
        }
    }

    if (showSignalPath && pathChip != null) {
        SignalPathDialog(
            path = pathChip,
            dac = dac,
            replayGainMode = replayGainMode,
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
                contentDescription = stringResource(R.string.cd_shuffle),
                tint = if (playback.shuffle) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        IconButton(
            onClick = {
                haptic.performHapticFeedback(HapticFeedbackType.TextHandleMove)
                viewModel.previous()
            },
            modifier = Modifier.size(56.dp),
        ) {
            Icon(
                Icons.Rounded.SkipPrevious,
                contentDescription = stringResource(R.string.cd_previous),
                modifier = Modifier.size(36.dp),
            )
        }
        FilledIconButton(
            onClick = {
                haptic.performHapticFeedback(HapticFeedbackType.TextHandleMove)
                viewModel.togglePlayPause()
            },
            modifier = Modifier.size(64.dp),
        ) {
            if (busy) {
                CircularProgressIndicator(
                    modifier = Modifier.size(28.dp),
                    strokeWidth = 3.dp,
                    color = MaterialTheme.colorScheme.onPrimary,
                )
            } else {
                AnimatedContent(
                    targetState = playing,
                    transitionSpec = {
                        (scaleIn() + fadeIn()) togetherWith (scaleOut() + fadeOut())
                    },
                    label = "play-pause",
                ) { isPlaying ->
                    Icon(
                        imageVector = if (isPlaying) Icons.Rounded.Pause else Icons.Rounded.PlayArrow,
                        contentDescription = if (isPlaying) {
                            stringResource(R.string.cd_pause)
                        } else {
                            stringResource(R.string.cd_play)
                        },
                        modifier = Modifier.size(36.dp),
                    )
                }
            }
        }
        IconButton(
            onClick = {
                haptic.performHapticFeedback(HapticFeedbackType.TextHandleMove)
                viewModel.next()
            },
            modifier = Modifier.size(56.dp),
        ) {
            Icon(
                Icons.Rounded.SkipNext,
                contentDescription = stringResource(R.string.cd_next),
                modifier = Modifier.size(36.dp),
            )
        }
        IconButton(onClick = viewModel::cycleRepeat) {
            val repeatOn = playback.repeat != RepeatMode.OFF
            Icon(
                imageVector = if (playback.repeat == RepeatMode.ONE) Icons.Rounded.RepeatOne else Icons.Rounded.Repeat,
                contentDescription = stringResource(R.string.cd_repeat),
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
    val haptic = LocalHapticFeedback.current
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
            if (dragging) {
                haptic.performHapticFeedback(HapticFeedbackType.TextHandleMove)
                onSeek(dragMs.toLong())
            }
            dragging = false
        },
        valueRange = 0f..max,
        enabled = duration != null,
    )
    Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
        Text(
            text = formatDuration(shownMs),
            style = MaterialTheme.typography.labelMedium.tabular(),
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            text = if (duration == null) {
                formatDuration(null)
            } else {
                "-" + formatDuration((duration - shownMs).coerceAtLeast(0L))
            },
            style = MaterialTheme.typography.labelMedium.tabular(),
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.End,
        )
    }
}

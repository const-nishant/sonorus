package dev.constnishant.sonorus.ui.player

import android.os.SystemClock
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.animateScrollBy
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.BlendMode
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.CompositingStrategy
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.PointerEventPass
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.LyricLineInfo
import dev.constnishant.sonorus.ui.theme.Dimens
import kotlinx.coroutines.delay

/** How long, in ms, after the last touch before auto-scroll re-centers the current line. */
private const val AUTO_SCROLL_RESUME_MS = 4_000L

/** Height of the fade at the top and bottom edges of the synced lyrics. */
private val EdgeFade = 48.dp

/**
 * The lyrics in place of the cover. [lyrics] picks the body: a spinner while loading, a centered
 * message when there are none, scrollable text for plain lyrics, or timed lines for synced lyrics.
 * [positionMs] is the core position and [playing] tells whether it is moving; the synced view
 * moves the highlight between core updates. [onSeek] is called with the start time of a tapped line.
 */
@Composable
internal fun LyricsPane(
    lyrics: LyricsUi,
    positionMs: Long,
    playing: Boolean,
    onSeek: (Long) -> Unit,
    modifier: Modifier = Modifier,
) {
    when (lyrics) {
        LyricsUi.Loading -> Box(modifier, contentAlignment = Alignment.Center) {
            CircularProgressIndicator(modifier = Modifier.size(32.dp))
        }

        LyricsUi.None -> Box(
            modifier.padding(horizontal = Dimens.ScreenPadding),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                text = stringResource(R.string.lyrics_none),
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
        }

        is LyricsUi.Plain -> PlainLyrics(text = lyrics.text, modifier = modifier)

        is LyricsUi.Synced -> SyncedLyrics(
            lines = lyrics.lines,
            positionMs = positionMs,
            playing = playing,
            onSeek = onSeek,
            modifier = modifier,
        )
    }
}

/** Plain lyrics as one scrollable block of text, starting at the top. */
@Composable
private fun PlainLyrics(text: String, modifier: Modifier) {
    Text(
        text = text,
        modifier = modifier
            .verticalScroll(rememberScrollState())
            .padding(horizontal = Dimens.ScreenPadding, vertical = 8.dp),
        style = MaterialTheme.typography.bodyLarge.copy(lineHeight = 28.sp),
        color = MaterialTheme.colorScheme.onSurface,
    )
}

/**
 * A position the synced view counts forward from. A new instance restarts the count: the core
 * position on every update, or a tapped line.
 */
private class PlaybackAnchor(val ms: Long)

/**
 * Timed lyrics with the current line centered and highlighted. The highlight follows the frame
 * clock while [playing] and snaps to the core position otherwise. Auto-scroll is paused while the
 * user touches the list and for [AUTO_SCROLL_RESUME_MS] after the last touch. Tapping a line seeks
 * to it and resumes auto-scroll.
 */
@Composable
private fun SyncedLyrics(
    lines: List<LyricLineInfo>,
    positionMs: Long,
    playing: Boolean,
    onSeek: (Long) -> Unit,
    modifier: Modifier,
) {
    // The count starts from the core position, or from a tapped line until the core reports again.
    val anchor = remember(positionMs) { mutableStateOf(PlaybackAnchor(positionMs)) }
    val shown = remember { mutableLongStateOf(positionMs) }
    LaunchedEffect(anchor.value, playing) {
        val base = anchor.value.ms
        shown.longValue = base
        if (!playing) return@LaunchedEffect
        var start = -1L
        while (true) {
            withFrameNanos { frame ->
                if (start < 0L) start = frame
                shown.longValue = base + (frame - start) / 1_000_000L
            }
        }
    }
    val currentIndex by remember(lines) {
        derivedStateOf { lines.currentLineIndex(shown.longValue) }
    }

    val listState = rememberLazyListState()
    var touching by remember { mutableStateOf(false) }
    var lastReleaseMs by remember { mutableLongStateOf(0L) }
    // Re-centers on each line change, once the user has been idle for the resume delay. Declared
    // after the anchor effect so it reads the position that effect has just set.
    LaunchedEffect(currentIndex, touching, lastReleaseMs) {
        if (touching || lines.currentLineIndex(shown.longValue) < 0) return@LaunchedEffect
        val wait = lastReleaseMs + AUTO_SCROLL_RESUME_MS - SystemClock.uptimeMillis()
        if (wait > 0L) delay(wait)
        centerLine(listState, lines.currentLineIndex(shown.longValue))
    }

    val fadePx = with(LocalDensity.current) { EdgeFade.toPx() }
    BoxWithConstraints(modifier) {
        // Half the viewport above and below lets the first and last lines reach the center.
        val halfHeight = maxHeight / 2
        LazyColumn(
            state = listState,
            modifier = Modifier
                .fillMaxSize()
                .fadeEdges(fadePx)
                .pointerInput(Unit) {
                    awaitEachGesture {
                        awaitFirstDown(requireUnconsumed = false, pass = PointerEventPass.Initial)
                        touching = true
                        var pressed = true
                        while (pressed) {
                            pressed = awaitPointerEvent(PointerEventPass.Initial).changes.any { it.pressed }
                        }
                        touching = false
                        lastReleaseMs = SystemClock.uptimeMillis()
                    }
                },
            contentPadding = PaddingValues(vertical = halfHeight),
        ) {
            itemsIndexed(lines) { index, line ->
                LyricLineText(
                    line = line,
                    current = index == currentIndex,
                    onClick = {
                        val ms = line.timeMs.toLong()
                        anchor.value = PlaybackAnchor(ms)
                        onSeek(ms)
                        lastReleaseMs = 0L
                    },
                )
            }
        }
    }
}

/**
 * One synced line. The current line is larger and fully opaque; the others are dimmed. Empty
 * lines are instrumental gaps and show a faint note. The whole line is a button that seeks.
 */
@Composable
private fun LyricLineText(
    line: LyricLineInfo,
    current: Boolean,
    onClick: () -> Unit,
) {
    val onSurface = MaterialTheme.colorScheme.onSurface
    val instrumental = line.text.isBlank()
    val color by animateColorAsState(
        targetValue = when {
            current -> onSurface
            instrumental -> onSurface.copy(alpha = 0.25f)
            else -> onSurface.copy(alpha = 0.45f)
        },
        label = "lyric-color",
    )
    val scale by animateFloatAsState(
        targetValue = if (current) 1f else 0.96f,
        label = "lyric-scale",
    )
    val nowPlaying = stringResource(R.string.cd_now_playing)
    Text(
        text = if (instrumental) stringResource(R.string.lyrics_instrumental) else line.text,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Dimens.ScreenPadding, vertical = 14.dp)
            .graphicsLayer {
                scaleX = scale
                scaleY = scale
            }
            .clickable(
                role = Role.Button,
                onClickLabel = stringResource(R.string.lyrics_play_from_here),
                onClick = onClick,
            )
            .semantics {
                if (current) stateDescription = nowPlaying
            },
        style = if (current) {
            MaterialTheme.typography.headlineSmall
        } else {
            MaterialTheme.typography.titleLarge
        },
        color = color,
        textAlign = TextAlign.Center,
    )
}

/**
 * Index of the last line whose start time is at or before [positionMs], or -1 when [positionMs]
 * is before the first line. Assumes lines in time order.
 */
private fun List<LyricLineInfo>.currentLineIndex(positionMs: Long): Int {
    val target = positionMs.coerceAtLeast(0L).toULong()
    return indexOfLast { it.timeMs <= target }
}

/**
 * Scrolls so the line at [index] sits in the middle of the list. The line is first brought on
 * screen if it is off screen, then the remaining distance is animated.
 */
private suspend fun centerLine(state: LazyListState, index: Int) {
    if (index < 0) return
    if (state.layoutInfo.visibleItemsInfo.none { it.index == index }) {
        state.scrollToItem(index)
    }
    val info = state.layoutInfo
    val item = info.visibleItemsInfo.firstOrNull { it.index == index } ?: return
    val itemCenter = item.offset + item.size / 2
    val viewportCenter = info.viewportSize.height / 2
    state.animateScrollBy((itemCenter - viewportCenter).toFloat())
}

/**
 * Fades the top and bottom [fadePx] of the content to transparent with a vertical gradient mask.
 * The content is drawn offscreen so the mask only clears pixels and does not touch the backdrop.
 */
private fun Modifier.fadeEdges(fadePx: Float): Modifier =
    graphicsLayer { compositingStrategy = CompositingStrategy.Offscreen }
        .drawWithContent {
            drawContent()
            if (size.height > 0f) {
                val edge = (fadePx / size.height).coerceIn(0f, 0.5f)
                drawRect(
                    brush = Brush.verticalGradient(
                        colorStops = arrayOf(
                            0f to Color.Transparent,
                            edge to Color.Black,
                            (1f - edge) to Color.Black,
                            1f to Color.Transparent,
                        ),
                        startY = 0f,
                        endY = size.height,
                    ),
                    blendMode = BlendMode.DstIn,
                )
            }
        }

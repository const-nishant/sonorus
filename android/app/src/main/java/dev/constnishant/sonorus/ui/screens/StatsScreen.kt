@file:OptIn(ExperimentalMaterial3Api::class)

package dev.constnishant.sonorus.ui.screens

import androidx.annotation.StringRes
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.ErrorOutline
import androidx.compose.material.icons.rounded.Insights
import androidx.compose.material.icons.rounded.Person
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModelProvider.AndroidViewModelFactory.Companion.APPLICATION_KEY
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.ListeningStatsInfo
import dev.constnishant.sonorus.core.StatsRangeInfo
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.Artwork
import dev.constnishant.sonorus.ui.components.BackButton
import dev.constnishant.sonorus.ui.components.CenteredContent
import dev.constnishant.sonorus.ui.components.EmptyState
import dev.constnishant.sonorus.ui.components.LoadingContent
import dev.constnishant.sonorus.ui.components.SectionTitle
import dev.constnishant.sonorus.ui.components.albumsText
import dev.constnishant.sonorus.ui.components.formatTotal
import dev.constnishant.sonorus.ui.components.tracksText
import dev.constnishant.sonorus.ui.theme.tabular

/** The ranges in the order their chips are shown. */
private val STATS_RANGES = listOf(
    StatsRangeInfo.WEEK,
    StatsRangeInfo.MONTH,
    StatsRangeInfo.YEAR,
    StatsRangeInfo.ALL_TIME,
)

/**
 * The Listening stats screen: a range selector (default Month), the listening time, plays and
 * distinct songs, then the top 10 songs, albums and artists. Each row has a bar scaled to the
 * largest entry of its section. Tapping a song plays it; tapping an album or artist opens it.
 *
 * @param onAlbum opens an album.
 * @param onArtist opens an artist.
 * @param onBack leaves the screen.
 * @param modifier applied to the root layout.
 */
@Composable
fun StatsScreen(
    onAlbum: (Long) -> Unit,
    onArtist: (Long) -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val repository = LocalContext.current.repository
    val viewModel: StatsViewModel = viewModel(
        factory = viewModelFactory { initializer { StatsViewModel(checkNotNull(this[APPLICATION_KEY]), repository) } },
    )
    val range by viewModel.range.collectAsStateWithLifecycle()
    val state by viewModel.state.collectAsStateWithLifecycle()

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.stats_title)) },
                navigationIcon = { BackButton(onBack) },
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            RangeChips(selected = range, onSelect = viewModel::setRange)
            Box(Modifier.weight(1f)) {
                when (val s = state) {
                    LoadState.Loading -> LoadingContent()
                    is LoadState.Failed -> CenteredContent {
                        EmptyState(Icons.Rounded.ErrorOutline, stringResource(R.string.stats_load_failed), s.message)
                    }
                    is LoadState.Ready -> StatsContent(
                        stats = s.value,
                        onPlayTrack = viewModel::playTrack,
                        onAlbum = onAlbum,
                        onArtist = onArtist,
                    )
                }
            }
        }
    }
}

/** A row of single-select chips for the stats window: Week, Month, Year and All time. */
@Composable
private fun RangeChips(selected: StatsRangeInfo, onSelect: (StatsRangeInfo) -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState())
            .padding(horizontal = 16.dp, vertical = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        STATS_RANGES.forEach { range ->
            FilterChip(
                selected = range == selected,
                onClick = { onSelect(range) },
                label = { Text(stringResource(range.labelRes())) },
            )
        }
    }
}

/**
 * The loaded stats: the summary, then the three top lists. With no plays in the window it shows an
 * empty state instead of the lists.
 */
@Composable
private fun StatsContent(
    stats: ListeningStatsInfo,
    onPlayTrack: (Long) -> Unit,
    onAlbum: (Long) -> Unit,
    onArtist: (Long) -> Unit,
) {
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 16.dp)) {
        item(key = "summary") { Summary(stats) }
        if (stats.plays == 0uL) {
            item(key = "empty") {
                EmptyState(
                    Icons.Rounded.Insights,
                    stringResource(R.string.stats_empty_title),
                    stringResource(R.string.stats_empty_message),
                )
            }
            return@LazyColumn
        }
        if (stats.topTracks.isNotEmpty()) {
            val max = stats.topTracks.maxOfOrNull { it.playedMs } ?: 0uL
            item(key = "tracks-title") { SectionTitle(stringResource(R.string.stats_top_songs)) }
            itemsIndexed(stats.topTracks, key = { _, entry -> "track-${entry.track.id}" }) { index, entry ->
                StatRow(
                    rank = index + 1,
                    leading = { Artwork(entry.track.coverPath, contentDescription = null, modifier = Modifier.size(48.dp), shape = MaterialTheme.shapes.small) },
                    title = entry.track.title,
                    subtitle = "${entry.track.artist} · ${entry.track.album}",
                    plays = entry.plays,
                    playedMs = entry.playedMs,
                    max = max,
                    onClick = { onPlayTrack(entry.track.id) },
                )
            }
        }
        if (stats.topAlbums.isNotEmpty()) {
            val max = stats.topAlbums.maxOfOrNull { it.playedMs } ?: 0uL
            item(key = "albums-title") { SectionTitle(stringResource(R.string.stats_top_albums)) }
            itemsIndexed(stats.topAlbums, key = { _, entry -> "album-${entry.album.id}" }) { index, entry ->
                StatRow(
                    rank = index + 1,
                    leading = { Artwork(entry.album.coverPath, contentDescription = null, modifier = Modifier.size(48.dp), shape = MaterialTheme.shapes.small) },
                    title = entry.album.title,
                    subtitle = entry.album.artist,
                    plays = entry.plays,
                    playedMs = entry.playedMs,
                    max = max,
                    onClick = { onAlbum(entry.album.id) },
                )
            }
        }
        if (stats.topArtists.isNotEmpty()) {
            val max = stats.topArtists.maxOfOrNull { it.playedMs } ?: 0uL
            item(key = "artists-title") { SectionTitle(stringResource(R.string.stats_top_artists)) }
            itemsIndexed(stats.topArtists, key = { _, entry -> "artist-${entry.artist.id}" }) { index, entry ->
                StatRow(
                    rank = index + 1,
                    leading = { ArtistIcon() },
                    title = entry.artist.name,
                    subtitle = albumsText(entry.artist.albumCount.toInt()) + " · " + tracksText(entry.artist.trackCount.toInt()),
                    plays = entry.plays,
                    playedMs = entry.playedMs,
                    max = max,
                    onClick = { onArtist(entry.artist.id) },
                )
            }
        }
    }
}

/** The three headline figures: listening time, plays and distinct songs. */
@Composable
private fun Summary(stats: ListeningStatsInfo) {
    val context = LocalContext.current
    Row(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        SummaryFigure(
            value = formatTotal(context, stats.totalPlayedMs.toLong()),
            label = stringResource(R.string.stats_listening_time),
            modifier = Modifier.weight(1f),
        )
        SummaryFigure(
            value = stats.plays.toString(),
            label = stringResource(R.string.stats_plays_label),
            modifier = Modifier.weight(1f),
        )
        SummaryFigure(
            value = stats.distinctTracks.toString(),
            label = stringResource(R.string.stats_songs_label),
            modifier = Modifier.weight(1f),
        )
    }
}

/** One big number with its small label under it. */
@Composable
private fun SummaryFigure(value: String, label: String, modifier: Modifier = Modifier) {
    Column(modifier, horizontalAlignment = Alignment.CenterHorizontally) {
        Text(
            value,
            style = MaterialTheme.typography.headlineSmall,
            color = MaterialTheme.colorScheme.primary,
            textAlign = TextAlign.Center,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
        Text(
            label,
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
    }
}

/**
 * One ranked entry: the rank, the leading picture, the title and subtitle with the usage bar under
 * them, then the plays and the listening time on the right.
 *
 * @param rank the 1-based position in its section.
 * @param leading the cover, or the artist icon.
 * @param max the largest played time in the section; the bar is scaled to it.
 * @param onClick called when the row is tapped.
 */
@Composable
private fun StatRow(
    rank: Int,
    leading: @Composable () -> Unit,
    title: String,
    subtitle: String,
    plays: ULong,
    playedMs: ULong,
    max: ULong,
    onClick: () -> Unit,
) {
    val context = LocalContext.current
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            rank.toString(),
            style = MaterialTheme.typography.bodyMedium.tabular(),
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
            modifier = Modifier.width(28.dp),
        )
        leading()
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(
                subtitle,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Spacer(Modifier.height(6.dp))
            UsageBar(fraction = usageShare(playedMs, max))
        }
        Spacer(Modifier.width(12.dp))
        Column(horizontalAlignment = Alignment.End) {
            Text(playsText(plays.toInt()), style = MaterialTheme.typography.labelLarge, maxLines = 1)
            Text(
                formatTotal(context, playedMs.toLong()),
                style = MaterialTheme.typography.bodySmall.tabular(),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
            )
        }
    }
}

/**
 * A thin horizontal bar: 4 dp tall and rounded, the primary colour over a surfaceContainerHighest
 * track. [fraction] is the filled share from 0 to 1.
 */
@Composable
private fun UsageBar(fraction: Float) {
    Box(
        Modifier
            .fillMaxWidth()
            .height(4.dp)
            .clip(MaterialTheme.shapes.extraSmall)
            .background(MaterialTheme.colorScheme.surfaceContainerHighest),
    ) {
        Box(
            Modifier
                .fillMaxWidth(fraction.coerceIn(0f, 1f))
                .fillMaxHeight()
                .background(MaterialTheme.colorScheme.primary),
        )
    }
}

/** The leading picture of an artist row: the person icon on a rounded tile, as the Artists tab shows. */
@Composable
private fun ArtistIcon() {
    Box(
        Modifier
            .size(48.dp)
            .clip(MaterialTheme.shapes.small)
            .background(MaterialTheme.colorScheme.surfaceVariant),
        contentAlignment = Alignment.Center,
    ) {
        Icon(Icons.Rounded.Person, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

/** A play count such as "1 play" or "12 plays". */
@Composable
private fun playsText(count: Int): String = pluralStringResource(R.plurals.plays_count, count, count)

/** The chip label of a stats window. */
@StringRes
private fun StatsRangeInfo.labelRes(): Int = when (this) {
    StatsRangeInfo.WEEK -> R.string.stats_range_week
    StatsRangeInfo.MONTH -> R.string.stats_range_month
    StatsRangeInfo.YEAR -> R.string.stats_range_year
    StatsRangeInfo.ALL_TIME -> R.string.stats_range_all
}

/** [value] as a share of [max], from 0 to 1. It is 0 when [max] is zero. */
internal fun usageShare(value: ULong, max: ULong): Float =
    if (max == 0uL) 0f else value.toFloat() / max.toFloat()

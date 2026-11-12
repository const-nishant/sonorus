package dev.constnishant.sonorus.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.PlaylistAdd
import androidx.compose.material.icons.rounded.Edit
import androidx.compose.material.icons.rounded.Favorite
import androidx.compose.material.icons.rounded.FavoriteBorder
import androidx.compose.material.icons.rounded.Folder
import androidx.compose.material.icons.rounded.MoreVert
import androidx.compose.material.icons.rounded.Queue
import androidx.compose.material.icons.rounded.QueuePlayNext
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.State
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.theme.tabular
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map

/**
 * One track in a list. Shows the title, artist (and album when [showArtwork]), duration, a
 * favorite toggle, and an overflow menu with "Play next", "Add to queue" and "Add to playlist…". When a
 * bucket is configured the menu also has "Edit tags…" and "Show in bucket".
 *
 * @param number leading track number, shown when [showArtwork] is false.
 * @param showArtwork shows the 48dp cover instead of [number] and adds the album to the subtitle.
 * @param isCurrent true for the track that is playing; its title is drawn in the primary colour.
 * @param actions the menu and favorite actions.
 * @param extraMenuItems optional items appended to the overflow menu (for example playlist entry
 * actions). The callback closes the menu.
 */
@Composable
fun TrackRow(
    track: TrackInfo,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    number: Int? = null,
    showArtwork: Boolean = false,
    isCurrent: Boolean = false,
    actions: TrackActions,
    extraMenuItems: (@Composable ColumnScope.(onDismiss: () -> Unit) -> Unit)? = null,
) {
    var favorite by remember(track.id) { mutableStateOf(track.isFavorite) }
    LaunchedEffect(track.isFavorite) { favorite = track.isFavorite }
    var menuOpen by remember { mutableStateOf(false) }
    var addOpen by remember { mutableStateOf(false) }
    var editOpen by remember { mutableStateOf(false) }
    val configured by rememberBucketConfigured()
    val bucket = rememberBucketMenuActions()
    val primary = MaterialTheme.colorScheme.primary
    val leading: (@Composable () -> Unit)? = if (showArtwork || number != null) {
        { TrackLeading(track, number, showArtwork) }
    } else {
        null
    }

    ListItem(
        modifier = modifier.clickable(role = Role.Button, onClick = onClick),
        leadingContent = leading,
        headlineContent = {
            Text(
                track.title,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                color = if (isCurrent) primary else Color.Unspecified,
            )
        },
        supportingContent = {
            Text(
                if (showArtwork) "${track.artist} · ${track.album}" else track.artist,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        },
        trailingContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (track.isHiRes()) {
                    Text(
                        stringResource(R.string.badge_hi_res),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.tertiary,
                    )
                    Spacer(Modifier.width(4.dp))
                }
                Text(
                    formatDuration(track.durationMs?.toLong()),
                    style = MaterialTheme.typography.bodySmall.tabular(),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                IconButton(onClick = {
                    val next = !favorite
                    favorite = next
                    actions.setFavorite(track, next, onError = { favorite = !next })
                }) {
                    Icon(
                        if (favorite) Icons.Rounded.Favorite else Icons.Rounded.FavoriteBorder,
                        contentDescription = if (favorite) {
                            stringResource(R.string.cd_remove_from_favorites)
                        } else {
                            stringResource(R.string.cd_add_to_favorites)
                        },
                        tint = if (favorite) primary else MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Box {
                    IconButton(onClick = { menuOpen = true }) {
                        Icon(Icons.Rounded.MoreVert, contentDescription = stringResource(R.string.cd_more_options))
                    }
                    DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.action_play_next)) },
                            leadingIcon = { Icon(Icons.Rounded.QueuePlayNext, contentDescription = null) },
                            onClick = {
                                menuOpen = false
                                actions.playNext(listOf(track.id))
                            },
                        )
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.action_add_to_queue)) },
                            leadingIcon = { Icon(Icons.Rounded.Queue, contentDescription = null) },
                            onClick = {
                                menuOpen = false
                                actions.addToQueue(listOf(track.id))
                            },
                        )
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.action_add_to_playlist)) },
                            leadingIcon = { Icon(Icons.AutoMirrored.Rounded.PlaylistAdd, contentDescription = null) },
                            onClick = {
                                menuOpen = false
                                addOpen = true
                            },
                        )
                        if (configured) {
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.action_edit_tags)) },
                                leadingIcon = { Icon(Icons.Rounded.Edit, contentDescription = null) },
                                onClick = {
                                    menuOpen = false
                                    editOpen = true
                                },
                            )
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.action_show_in_bucket)) },
                                leadingIcon = { Icon(Icons.Rounded.Folder, contentDescription = null) },
                                onClick = {
                                    menuOpen = false
                                    bucket.showTrack(track)
                                },
                            )
                        }
                        extraMenuItems?.invoke(this) { menuOpen = false }
                    }
                }
            }
        },
    )

    if (addOpen) {
        AddToPlaylistDialog(
            trackIds = listOf(track.id),
            onDismiss = { addOpen = false },
            actions = actions,
        )
    }
    if (editOpen) {
        TrackTagsDialog(trackId = track.id, onDismiss = { editOpen = false })
    }
}

/** The leading slot: the cover, or the track number. */
@Composable
private fun TrackLeading(track: TrackInfo, number: Int?, showArtwork: Boolean) {
    if (showArtwork) {
        Artwork(track.coverPath, contentDescription = null, modifier = Modifier.size(48.dp), shape = MaterialTheme.shapes.small)
    } else if (number != null) {
        Box(Modifier.width(32.dp), contentAlignment = Alignment.Center) {
            Text(
                number.toString(),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/**
 * The current track id from the playback state, or null when nothing is loaded. Recomposes only
 * when the track changes, not on every position update.
 */
@Composable
fun rememberCurrentTrackId(): State<Long?> {
    val repository = LocalContext.current.repository
    val ids = remember(repository) {
        repository.events.playback.map { it.track?.id }.distinctUntilChanged()
    }
    return ids.collectAsStateWithLifecycle(initialValue = null)
}

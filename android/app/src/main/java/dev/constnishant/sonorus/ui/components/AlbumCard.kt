package dev.constnishant.sonorus.ui.components

import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Edit
import androidx.compose.material.icons.rounded.Folder
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.AlbumInfo

/**
 * A square album cover with the title and "artist · year" underneath. The card fills the width
 * that [modifier] gives it. Long-pressing it opens "Edit album tags…" and "Show in bucket" when a
 * bucket is configured.
 *
 * @param album the album to show.
 * @param onClick called when the card is tapped.
 */
@Composable
fun AlbumCard(
    album: AlbumInfo,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val subtitle = listOfNotNull(album.artist, album.year?.toString()).joinToString(" · ")
    val configured by rememberBucketConfigured()
    val bucket = rememberBucketMenuActions()
    var menuOpen by remember { mutableStateOf(false) }
    var editOpen by remember { mutableStateOf(false) }
    val onLongClick: (() -> Unit)? = if (configured) ({ menuOpen = true }) else null

    Box(modifier) {
        Column(Modifier.combinedClickable(role = Role.Button, onClick = onClick, onLongClick = onLongClick)) {
            Artwork(
                path = album.coverPath,
                contentDescription = album.title,
                modifier = Modifier
                    .fillMaxWidth()
                    .aspectRatio(1f),
                shape = MaterialTheme.shapes.small,
            )
            Spacer(Modifier.height(6.dp))
            Text(
                album.title,
                style = MaterialTheme.typography.titleSmall,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                subtitle,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
            DropdownMenuItem(
                text = { Text(stringResource(R.string.action_edit_album_tags)) },
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
                    bucket.showAlbum(album.id)
                },
            )
        }
    }
    if (editOpen) {
        AlbumTagsDialog(albumId = album.id, onDismiss = { editOpen = false })
    }
}

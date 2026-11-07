package dev.constnishant.sonorus.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.constnishant.sonorus.core.AlbumInfo

/**
 * A square album cover with the title and "artist · year" underneath. The card fills the width
 * that [modifier] gives it.
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
    Column(modifier.clickable(onClick = onClick)) {
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
}

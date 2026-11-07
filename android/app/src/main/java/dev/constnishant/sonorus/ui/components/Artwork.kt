package dev.constnishant.sonorus.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.MusicNote
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.layout.ContentScale
import coil3.compose.SubcomposeAsyncImage
import java.io.File

/**
 * Cover art from a local file path (as returned by the core), with a music-note placeholder when
 * there is no cover or it fails to load. Size it with [modifier].
 */
@Composable
fun Artwork(
    path: String?,
    contentDescription: String?,
    modifier: Modifier = Modifier,
    shape: Shape = MaterialTheme.shapes.small,
) {
    Box(modifier.clip(shape).background(MaterialTheme.colorScheme.surfaceVariant)) {
        if (path != null) {
            SubcomposeAsyncImage(
                model = File(path),
                contentDescription = contentDescription,
                contentScale = ContentScale.Crop,
                modifier = Modifier.fillMaxSize(),
                error = { Placeholder() },
                loading = { Placeholder() },
            )
        } else {
            Placeholder()
        }
    }
}

@Composable
private fun Placeholder() {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Icon(
            Icons.Rounded.MusicNote,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.fillMaxWidth(0.4f),
        )
    }
}

package dev.constnishant.sonorus.ui.components

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.minimumInteractiveComponentSize
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import dev.constnishant.sonorus.core.TrackInfo

/**
 * Outlined pill with a label. [accent] draws it in the gold tertiary colour (used for hi-res
 * quality); otherwise it uses the muted variant colour. When [onClick] is set the chip is a button
 * with a 48dp touch target.
 */
@Composable
fun SonorusChip(
    text: String,
    accent: Boolean = false,
    onClick: (() -> Unit)? = null,
    leadingIcon: ImageVector? = null,
) {
    val tint = if (accent) MaterialTheme.colorScheme.tertiary else MaterialTheme.colorScheme.onSurfaceVariant
    val clickModifier = if (onClick != null) {
        Modifier
            .minimumInteractiveComponentSize()
            .clickable(role = Role.Button, onClick = onClick)
    } else {
        Modifier
    }
    Box(modifier = clickModifier, contentAlignment = Alignment.Center) {
        Surface(
            shape = CircleShape,
            color = Color.Transparent,
            contentColor = tint,
            border = BorderStroke(1.dp, tint),
        ) {
            Row(
                modifier = Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                if (leadingIcon != null) {
                    Icon(leadingIcon, contentDescription = null, modifier = Modifier.size(16.dp))
                }
                Text(text = text, style = MaterialTheme.typography.labelMedium)
            }
        }
    }
}

/** Quality chip for a track, such as "FLAC 24/96"; nothing when the codec is unknown. */
@Composable
fun QualityBadge(track: TrackInfo) {
    val label = track.qualityLabel(LocalContext.current) ?: return
    SonorusChip(text = label, accent = track.isHiRes())
}

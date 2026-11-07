@file:OptIn(ExperimentalMaterial3Api::class)

package dev.constnishant.sonorus.ui.components

import android.content.Context
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.DownloadDone
import androidx.compose.material.icons.rounded.DownloadForOffline
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedIconToggleButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.DownloadStatus
import dev.constnishant.sonorus.data.DownloadsUi

/**
 * The offline state of one album or playlist as its screen shows it. [status] is null until the
 * first status read succeeds; the toggle is disabled until then.
 */
data class PinUi(
    val pinned: Boolean = false,
    val status: DownloadStatus? = null,
) {
    /** True when every catalogued track is completely in the offline cache. */
    val complete: Boolean
        get() = status != null && status.cachedTracks >= status.totalTracks
}

/**
 * The "Download for offline" toggle for a header action row. Removing a download that is complete
 * asks first; the songs stay in the bucket and still stream.
 *
 * @param state the current pin state.
 * @param onSetPinned called with the new pinned value once the user has chosen it.
 * @param modifier applied to the toggle.
 */
@Composable
fun DownloadToggle(
    state: PinUi,
    onSetPinned: (Boolean) -> Unit,
    modifier: Modifier = Modifier,
) {
    var confirming by remember { mutableStateOf(false) }
    OutlinedIconToggleButton(
        checked = state.pinned,
        onCheckedChange = { wantPinned ->
            if (!wantPinned && state.complete) confirming = true else onSetPinned(wantPinned)
        },
        enabled = state.status != null,
        modifier = modifier,
    ) {
        Icon(
            imageVector = if (state.pinned) Icons.Rounded.DownloadDone else Icons.Rounded.DownloadForOffline,
            contentDescription = if (state.pinned) {
                stringResource(R.string.cd_remove_download)
            } else {
                stringResource(R.string.cd_download_offline)
            },
        )
    }
    if (confirming) {
        ConfirmDialog(
            title = stringResource(R.string.download_remove_title),
            message = stringResource(R.string.download_remove_message),
            confirmLabel = stringResource(R.string.action_remove),
            onConfirm = {
                confirming = false
                onSetPinned(false)
            },
            onDismiss = { confirming = false },
        )
    }
}

/**
 * The one-line download status under the header, or null when the album or playlist is not
 * pinned or its status is not loaded yet.
 *
 * "Downloaded" when every track is cached; "Downloading 3 of 12" while the worker is busy;
 * "Waiting for Wi-Fi · 3 of 12" when idle with tracks pending; otherwise "Queued · 3 of 12". The
 * cached size is appended when there is one.
 */
fun downloadStatusText(context: Context, state: PinUi, downloads: DownloadsUi): String? {
    val status = state.status ?: return null
    if (!status.pinned) return null
    val counts = context.getString(R.string.download_counts, status.cachedTracks.toInt(), status.totalTracks.toInt())
    val base = when {
        state.complete -> context.getString(R.string.download_downloaded)
        downloads.active -> context.getString(R.string.download_downloading, counts)
        downloads.pendingTracks > 0u -> context.getString(R.string.download_waiting) + " · " + counts
        else -> context.getString(R.string.download_queued) + " · " + counts
    }
    return if (status.cachedBytes > 0u) {
        "$base · ${formatBytes(context, status.cachedBytes)}"
    } else {
        base
    }
}

/** The status line from [downloadStatusText]. Renders nothing when there is no status to show. */
@Composable
fun DownloadStatusText(
    state: PinUi,
    downloads: DownloadsUi,
    modifier: Modifier = Modifier,
) {
    val text = downloadStatusText(LocalContext.current, state, downloads) ?: return
    Text(
        text = text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = modifier.padding(top = 8.dp),
    )
}

@file:OptIn(ExperimentalMaterial3Api::class)

package dev.constnishant.sonorus.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.ChevronRight
import androidx.compose.material.icons.rounded.ErrorOutline
import androidx.compose.material.icons.rounded.Folder
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.CatalogueFolderInfo
import dev.constnishant.sonorus.ui.components.CenteredContent
import dev.constnishant.sonorus.ui.components.EmptyState
import dev.constnishant.sonorus.ui.components.LoadingContent
import dev.constnishant.sonorus.ui.components.TrackActions
import dev.constnishant.sonorus.ui.components.TrackRow
import dev.constnishant.sonorus.ui.components.rememberCurrentTrackId
import dev.constnishant.sonorus.ui.components.rememberTrackActions
import dev.constnishant.sonorus.ui.components.songsText

/**
 * The Folders tab of the Library: a breadcrumb row for the current folder, then the folders directly
 * inside it (tapping one opens it) and the tracks directly inside it (tapping one plays the folder
 * from there). A failed load shows the usual error state, with the breadcrumbs still usable.
 *
 * @param viewModel the folder being viewed. The Library screen also reads its path for the back button.
 * @param modifier applied to the root layout.
 */
@Composable
internal fun FoldersTab(viewModel: FoldersViewModel, modifier: Modifier = Modifier) {
    val path by viewModel.path.collectAsStateWithLifecycle()
    val state by viewModel.state.collectAsStateWithLifecycle()
    val currentId by rememberCurrentTrackId()
    val actions = rememberTrackActions()

    Column(modifier.fillMaxSize()) {
        Breadcrumbs(path = path, onOpen = viewModel::open)
        when (val s = state) {
            LoadState.Loading -> LoadingContent(Modifier.weight(1f))
            is LoadState.Failed -> CenteredContent(Modifier.weight(1f)) {
                EmptyState(Icons.Rounded.ErrorOutline, stringResource(R.string.folders_failed), s.message)
            }
            is LoadState.Ready -> FolderListing(
                folder = s.value,
                currentId = currentId,
                actions = actions,
                onOpen = viewModel::open,
                onPlayFrom = viewModel::playFrom,
                modifier = Modifier.weight(1f),
            )
        }
    }
}

/**
 * The path as a row of buttons, "All folders" first, then each folder down to the current one,
 * separated by chevrons. Each button opens that level.
 *
 * @param path the current folder: `""` for the root, else a path ending with `/`.
 * @param onOpen opens the folder with the given path.
 */
@Composable
private fun Breadcrumbs(path: String, onOpen: (String) -> Unit) {
    val segments = path.split('/').filter { it.isNotEmpty() }
    Row(
        Modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState())
            .padding(horizontal = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        TextButton(onClick = { onOpen("") }) {
            Text(stringResource(R.string.folders_all))
        }
        segments.forEachIndexed { index, name ->
            Icon(
                Icons.Rounded.ChevronRight,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            TextButton(onClick = { onOpen(segments.take(index + 1).joinToString("") { "$it/" }) }) {
                Text(name)
            }
        }
    }
}

/**
 * The listing of one folder: its child folders, then its tracks. An empty folder shows an empty
 * state.
 *
 * @param folder the folder to list.
 * @param currentId the playing track, drawn in the primary colour.
 * @param onOpen opens a child folder by path.
 * @param onPlayFrom plays the folder's tracks from the given index.
 */
@Composable
private fun FolderListing(
    folder: CatalogueFolderInfo,
    currentId: Long?,
    actions: TrackActions,
    onOpen: (String) -> Unit,
    onPlayFrom: (Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    LazyColumn(modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 16.dp)) {
        items(folder.folders, key = { "folder-${it.path}" }) { entry ->
            ListItem(
                modifier = Modifier.clickable(role = Role.Button) { onOpen(entry.path) },
                leadingContent = { Icon(Icons.Rounded.Folder, contentDescription = null) },
                headlineContent = { Text(entry.name, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                supportingContent = { Text(songsText(entry.trackCount.toInt())) },
            )
        }
        itemsIndexed(folder.tracks, key = { _, track -> "track-${track.id}" }) { index, track ->
            TrackRow(
                track = track,
                onClick = { onPlayFrom(index) },
                showArtwork = true,
                isCurrent = track.id == currentId,
                actions = actions,
            )
        }
        if (folder.folders.isEmpty() && folder.tracks.isEmpty()) {
            item(key = "empty") {
                EmptyState(
                    Icons.Rounded.Folder,
                    stringResource(R.string.folders_empty_title),
                    stringResource(R.string.folders_empty_message),
                )
            }
        }
    }
}

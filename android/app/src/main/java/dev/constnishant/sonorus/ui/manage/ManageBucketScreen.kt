@file:OptIn(ExperimentalMaterial3Api::class)

package dev.constnishant.sonorus.ui.manage

import android.content.Context
import android.text.format.DateUtils
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.clickable
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.ChevronRight
import androidx.compose.material.icons.automirrored.rounded.DriveFileMove
import androidx.compose.material.icons.rounded.Description
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.Delete
import androidx.compose.material.icons.rounded.DriveFileRenameOutline
import androidx.compose.material.icons.rounded.Edit
import androidx.compose.material.icons.rounded.ErrorOutline
import androidx.compose.material.icons.rounded.Folder
import androidx.compose.material.icons.rounded.FolderOpen
import androidx.compose.material.icons.rounded.MusicNote
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.SelectAll
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.toggleableState
import androidx.compose.ui.state.ToggleableState
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModelProvider.AndroidViewModelFactory.Companion.APPLICATION_KEY
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.BucketEntryInfo
import dev.constnishant.sonorus.core.BucketFolderInfo
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.BackButton
import dev.constnishant.sonorus.ui.components.EmptyState
import dev.constnishant.sonorus.ui.components.LoadingContent
import dev.constnishant.sonorus.ui.components.TrackTagsDialog
import dev.constnishant.sonorus.ui.components.formatBytes
import dev.constnishant.sonorus.ui.screens.LoadState

/**
 * The Manage bucket screen: the folders and files of one bucket folder, with the breadcrumbs above
 * them. Folders open on tap; files open a details sheet. Long-press starts a selection, which can
 * be renamed (one file), moved or deleted. System back goes up one folder, then leaves the screen.
 *
 * @param initialPath the folder to open first, relative to the bucket prefix (`""` for the root).
 * @param onBack leaves the screen from the root.
 * @param modifier applied to the root layout.
 */
@Composable
fun ManageBucketScreen(initialPath: String, onBack: () -> Unit, modifier: Modifier = Modifier) {
    val repository = LocalContext.current.repository
    val viewModel: ManageBucketViewModel = viewModel(
        factory = viewModelFactory {
            initializer { ManageBucketViewModel(checkNotNull(this[APPLICATION_KEY]), repository, initialPath) }
        },
    )
    val path by viewModel.path.collectAsStateWithLifecycle()
    val state by viewModel.state.collectAsStateWithLifecycle()
    val refreshing by viewModel.refreshing.collectAsStateWithLifecycle()
    val selection by viewModel.selection.collectAsStateWithLifecycle()
    val dialog by viewModel.dialog.collectAsStateWithLifecycle()
    val sheet by viewModel.sheet.collectAsStateWithLifecycle()
    val busy by viewModel.busy.collectAsStateWithLifecycle()
    val snackbarHostState = remember { SnackbarHostState() }
    val detailsLabel = stringResource(R.string.manage_details)
    val selectionMode = selection.isNotEmpty()
    val listing = (state as? LoadState.Ready)?.value
    val chosen = listing?.let { it.folders + it.files }?.filter { it.key in selection }.orEmpty()

    LaunchedEffect(viewModel) {
        viewModel.results.collect { result ->
            val action = snackbarHostState.showSnackbar(
                message = result.message,
                actionLabel = if (result.failures.isEmpty()) null else detailsLabel,
            )
            if (action == SnackbarResult.ActionPerformed) {
                viewModel.showDialog(ManageDialog.Failures(result.failures))
            }
        }
    }

    // Back clears the selection first, then goes up a folder. At the root it leaves the screen.
    BackHandler(enabled = selectionMode || path.isNotEmpty()) {
        if (selectionMode) viewModel.clearSelection() else viewModel.up()
    }

    Scaffold(
        modifier = modifier,
        topBar = {
            if (selectionMode) {
                SelectionBar(
                    count = chosen.size,
                    onClear = viewModel::clearSelection,
                    onSelectAll = viewModel::selectAll,
                    // Rename works on one file: the core renames files only.
                    renameEntry = chosen.singleOrNull()?.takeUnless { it.isFolder },
                    onRename = { viewModel.showDialog(ManageDialog.Rename(it)) },
                    onMove = { viewModel.showDialog(ManageDialog.Move(chosen)) },
                    onDelete = { viewModel.showDialog(ManageDialog.Delete(chosen)) },
                )
            } else {
                TopAppBar(
                    title = { Text(stringResource(R.string.manage_title)) },
                    navigationIcon = {
                        BackButton(onClick = { if (path.isEmpty()) onBack() else viewModel.up() })
                    },
                )
            }
        },
        snackbarHost = { SnackbarHost(snackbarHostState) },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            ManageBreadcrumbs(path = path, onOpen = viewModel::open)
            PullToRefreshBox(
                isRefreshing = refreshing,
                onRefresh = viewModel::refresh,
                modifier = Modifier.weight(1f),
            ) {
                when (val s = state) {
                    LoadState.Loading -> LoadingContent(Modifier.fillMaxSize())
                    is LoadState.Failed -> LazyColumn(Modifier.fillMaxSize()) {
                        item(key = "failed") {
                            EmptyState(
                                Icons.Rounded.ErrorOutline,
                                stringResource(R.string.manage_load_failed),
                                s.message,
                            )
                        }
                    }
                    is LoadState.Ready -> BucketListing(
                        folder = s.value,
                        selection = selection,
                        onTap = viewModel::tap,
                        onLongPress = viewModel::longPress,
                    )
                }
            }
        }
    }

    sheet?.let { entry ->
        ModalBottomSheet(onDismissRequest = viewModel::dismissSheet) {
            FileSheet(
                entry = entry,
                onRename = { viewModel.showFromSheet(ManageDialog.Rename(entry)) },
                onMove = { viewModel.showFromSheet(ManageDialog.Move(listOf(entry))) },
                onEditTags = { entry.trackId?.let { viewModel.showFromSheet(ManageDialog.Tags(it)) } },
                onDelete = { viewModel.showFromSheet(ManageDialog.Delete(listOf(entry))) },
                onPlay = { viewModel.playFromSheet(entry) },
            )
        }
    }

    when (val d = dialog) {
        is ManageDialog.Rename -> RenameDialog(
            entry = d.entry,
            onRename = { viewModel.rename(d.entry, it) },
            onDismiss = viewModel::dismissDialog,
        )
        is ManageDialog.Move -> MoveDialog(
            currentPath = path,
            entries = d.entries,
            onMove = { viewModel.move(d.entries, it) },
            onDismiss = viewModel::dismissDialog,
        )
        is ManageDialog.Delete -> DeleteDialog(
            entries = d.entries,
            onDelete = { viewModel.delete(d.entries) },
            onDismiss = viewModel::dismissDialog,
        )
        is ManageDialog.Tags -> TrackTagsDialog(
            trackId = d.trackId,
            onDismiss = {
                viewModel.dismissDialog()
                viewModel.refresh()
            },
        )
        is ManageDialog.Failures -> FailuresDialog(
            failures = d.failures,
            onDismiss = viewModel::dismissDialog,
        )
        null -> Unit
    }

    busy?.let { BusyDialog(it) }
}

/**
 * The top bar in selection mode: a close button, "N selected", Select all, and the actions for
 * the selection. [renameEntry] is set only when exactly one file is selected, and Rename acts on it.
 */
@Composable
private fun SelectionBar(
    count: Int,
    onClear: () -> Unit,
    onSelectAll: () -> Unit,
    renameEntry: BucketEntryInfo?,
    onRename: (BucketEntryInfo) -> Unit,
    onMove: () -> Unit,
    onDelete: () -> Unit,
) {
    TopAppBar(
        title = { Text(pluralStringResource(R.plurals.manage_selected_count, count, count)) },
        navigationIcon = {
            IconButton(onClick = onClear) {
                Icon(Icons.Rounded.Close, contentDescription = stringResource(R.string.manage_clear_selection))
            }
        },
        actions = {
            IconButton(onClick = onSelectAll) {
                Icon(Icons.Rounded.SelectAll, contentDescription = stringResource(R.string.manage_select_all))
            }
            if (renameEntry != null) {
                IconButton(onClick = { onRename(renameEntry) }) {
                    Icon(Icons.Rounded.DriveFileRenameOutline, contentDescription = stringResource(R.string.action_rename))
                }
            }
            IconButton(onClick = onMove) {
                Icon(Icons.AutoMirrored.Rounded.DriveFileMove, contentDescription = stringResource(R.string.manage_move))
            }
            IconButton(onClick = onDelete) {
                Icon(Icons.Rounded.Delete, contentDescription = stringResource(R.string.action_delete))
            }
        },
        colors = TopAppBarDefaults.topAppBarColors(
            containerColor = MaterialTheme.colorScheme.secondaryContainer,
            titleContentColor = MaterialTheme.colorScheme.onSecondaryContainer,
            navigationIconContentColor = MaterialTheme.colorScheme.onSecondaryContainer,
            actionIconContentColor = MaterialTheme.colorScheme.onSecondaryContainer,
        ),
    )
}

/** The folders of [folder] first, then its files. An empty folder shows an empty state. */
@Composable
private fun BucketListing(
    folder: BucketFolderInfo,
    selection: Set<String>,
    onTap: (BucketEntryInfo) -> Unit,
    onLongPress: (BucketEntryInfo) -> Unit,
) {
    val selectionMode = selection.isNotEmpty()
    LazyColumn(Modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 16.dp)) {
        items(folder.folders, key = { "folder-${it.key}" }) { entry ->
            EntryRow(
                entry = entry,
                selectionMode = selectionMode,
                selected = entry.key in selection,
                onClick = { onTap(entry) },
                onLongClick = { onLongPress(entry) },
            )
        }
        items(folder.files, key = { "file-${it.key}" }) { entry ->
            EntryRow(
                entry = entry,
                selectionMode = selectionMode,
                selected = entry.key in selection,
                onClick = { onTap(entry) },
                onLongClick = { onLongPress(entry) },
            )
        }
        if (folder.folders.isEmpty() && folder.files.isEmpty()) {
            item(key = "empty") {
                EmptyState(
                    Icons.Rounded.FolderOpen,
                    stringResource(R.string.manage_empty_title),
                    stringResource(R.string.manage_empty_message),
                )
            }
        }
    }
}

/**
 * One folder or file. A folder shows a chevron; a file shows its size and modified date. In
 * selection mode the row is a checkbox that toggles on tap.
 */
@Composable
private fun EntryRow(
    entry: BucketEntryInfo,
    selectionMode: Boolean,
    selected: Boolean,
    onClick: () -> Unit,
    onLongClick: () -> Unit,
) {
    val context = LocalContext.current
    val icon = when {
        entry.isFolder -> Icons.Rounded.Folder
        entry.trackId != null -> Icons.Rounded.MusicNote
        else -> Icons.Rounded.Description
    }
    val iconDescription = when {
        entry.isFolder -> stringResource(R.string.manage_cd_folder)
        entry.trackId != null -> stringResource(R.string.manage_cd_track)
        else -> stringResource(R.string.manage_cd_file)
    }
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 56.dp)
            .combinedClickable(
                role = if (selectionMode) Role.Checkbox else Role.Button,
                onClick = onClick,
                onLongClick = onLongClick,
            )
            .semantics { if (selectionMode) toggleableState = ToggleableState(selected) }
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Icon(icon, contentDescription = iconDescription, tint = MaterialTheme.colorScheme.onSurfaceVariant)
        Column(Modifier.weight(1f)) {
            Text(entry.name, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (!entry.isFolder) {
                Text(
                    sizeAndDate(context, entry),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        if (selectionMode) {
            Checkbox(checked = selected, onCheckedChange = null)
        } else if (entry.isFolder) {
            Icon(Icons.Rounded.ChevronRight, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

/**
 * The details sheet of one file: its key, size and modified date, then its actions. Edit tags and
 * Play are passed only for a catalogued track.
 */
@Composable
private fun FileSheet(
    entry: BucketEntryInfo,
    onRename: () -> Unit,
    onMove: () -> Unit,
    onEditTags: () -> Unit,
    onDelete: () -> Unit,
    onPlay: () -> Unit,
) {
    val context = LocalContext.current
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp).padding(bottom = 24.dp)) {
        Text(entry.name, style = MaterialTheme.typography.titleMedium, maxLines = 2, overflow = TextOverflow.Ellipsis)
        Spacer(Modifier.height(12.dp))
        DetailLine(stringResource(R.string.manage_detail_key), entry.key)
        DetailLine(stringResource(R.string.manage_detail_size), formatBytes(context, entry.size))
        entry.modifiedMs?.let { DetailLine(stringResource(R.string.manage_detail_modified), modifiedText(context, it)) }
        Spacer(Modifier.height(12.dp))
        SheetAction(Icons.Rounded.DriveFileRenameOutline, stringResource(R.string.action_rename), onRename)
        SheetAction(Icons.AutoMirrored.Rounded.DriveFileMove, stringResource(R.string.manage_move), onMove)
        if (entry.trackId != null) {
            SheetAction(Icons.Rounded.Edit, stringResource(R.string.action_edit_tags), onEditTags)
        }
        SheetAction(
            Icons.Rounded.Delete,
            stringResource(R.string.action_delete),
            onDelete,
            color = MaterialTheme.colorScheme.error,
        )
        if (entry.trackId != null) {
            SheetAction(Icons.Rounded.PlayArrow, stringResource(R.string.action_play), onPlay)
        }
    }
}

/** A label above a value in the file sheet. */
@Composable
private fun DetailLine(label: String, value: String) {
    Column(Modifier.padding(vertical = 4.dp)) {
        Text(label, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(value, style = MaterialTheme.typography.bodyMedium)
    }
}

/** One action in the file sheet: an icon and a label, 48dp tall. */
@Composable
private fun SheetAction(
    icon: ImageVector,
    label: String,
    onClick: () -> Unit,
    color: Color = MaterialTheme.colorScheme.onSurface,
) {
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, contentDescription = null, tint = color)
        Spacer(Modifier.width(16.dp))
        Text(label, style = MaterialTheme.typography.bodyLarge, color = color)
    }
}

/** "1.2 MB · 4 Mar 2025" for a file row; the size alone when the date is unknown. */
private fun sizeAndDate(context: Context, entry: BucketEntryInfo): String {
    val size = formatBytes(context, entry.size)
    val modified = entry.modifiedMs?.let {
        DateUtils.formatDateTime(
            context,
            it,
            DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_SHOW_YEAR or DateUtils.FORMAT_ABBREV_MONTH,
        )
    }
    return listOfNotNull(size, modified).joinToString(" · ")
}

/** A full date and time for the file sheet. */
private fun modifiedText(context: Context, ms: Long): String = DateUtils.formatDateTime(
    context,
    ms,
    DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_SHOW_YEAR or DateUtils.FORMAT_ABBREV_MONTH or DateUtils.FORMAT_SHOW_TIME,
)

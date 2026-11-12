package dev.constnishant.sonorus.ui.manage

import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.ChevronRight
import androidx.compose.material.icons.rounded.Folder
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.BucketEntryInfo
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.BucketFolderInfo
import dev.constnishant.sonorus.core.FailureInfo
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.data.userMessage
import dev.constnishant.sonorus.ui.screens.LoadState

/**
 * The rename dialog for one file. The name starts with the base name selected, without the
 * extension. Renaming is offered only when the name changed, and must not be blank or contain
 * `/`. Folders keep their whole name selected because the core renames files only.
 */
@Composable
fun RenameDialog(entry: BucketEntryInfo, onRename: (String) -> Unit, onDismiss: () -> Unit) {
    var value by remember(entry.key) {
        mutableStateOf(TextFieldValue(entry.name, selection = TextRange(0, baseNameLength(entry.name))))
    }
    val edited = value.text != entry.name
    val trimmed = value.text.trim()
    val error: String? = when {
        !edited -> null
        trimmed.isEmpty() -> stringResource(R.string.manage_rename_empty)
        trimmed.contains('/') -> stringResource(R.string.manage_rename_slash)
        trimmed == entry.name -> stringResource(R.string.manage_rename_same)
        else -> null
    }
    val focus = remember { FocusRequester() }
    LaunchedEffect(entry.key) { focus.requestFocus() }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.manage_rename_title)) },
        text = {
            OutlinedTextField(
                value = value,
                onValueChange = { value = it },
                label = { Text(stringResource(R.string.manage_rename_field)) },
                singleLine = true,
                isError = error != null,
                supportingText = { if (error != null) Text(error) },
                modifier = Modifier.fillMaxWidth().focusRequester(focus),
            )
        },
        confirmButton = {
            TextButton(enabled = edited && error == null, onClick = { onRename(trimmed) }) {
                Text(stringResource(R.string.action_rename))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_cancel)) }
        },
    )
}

/** The length of a file name without its extension (the whole name when it has none). */
private fun baseNameLength(name: String): Int {
    val dot = name.lastIndexOf('.')
    return if (dot > 0) dot else name.length
}

/**
 * The folder picker for a move. It browses the bucket's folders from [currentPath] with the same
 * listing as the screen. The current folder cannot be the target, and neither can a folder being
 * moved or one of its subfolders; "Move here" stays disabled for those.
 *
 * @param currentPath the folder the screen shows.
 * @param entries the entries being moved.
 * @param onMove called with the chosen target folder path.
 */
@Composable
fun MoveDialog(
    currentPath: String,
    entries: List<BucketEntryInfo>,
    onMove: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    val repository = LocalContext.current.repository
    val context = LocalContext.current
    var target by rememberSaveable(currentPath) { mutableStateOf(currentPath) }
    var listing by remember { mutableStateOf<LoadState<BucketFolderInfo>>(LoadState.Loading) }

    LaunchedEffect(target) {
        listing = LoadState.Loading
        listing = try {
            LoadState.Ready(repository.browseBucket(target))
        } catch (e: CoreException) {
            LoadState.Failed(e.userMessage(context, R.string.error_load_generic))
        }
    }

    val invalid = invalidTarget(target, currentPath, entries)
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.manage_move_title)) },
        text = {
            Column {
                ManageBreadcrumbs(path = target, onOpen = { target = it })
                if (invalid) {
                    Text(
                        stringResource(R.string.manage_move_pick_other),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier.padding(vertical = 4.dp),
                    )
                }
                when (val s = listing) {
                    LoadState.Loading -> Box(Modifier.fillMaxWidth().height(200.dp), contentAlignment = Alignment.Center) {
                        CircularProgressIndicator(Modifier.size(32.dp))
                    }
                    is LoadState.Failed -> Text(s.message, color = MaterialTheme.colorScheme.error)
                    is LoadState.Ready -> if (s.value.folders.isEmpty()) {
                        Text(
                            stringResource(R.string.manage_move_no_folders),
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(vertical = 16.dp),
                        )
                    } else {
                        LazyColumn(Modifier.fillMaxWidth().height(260.dp)) {
                            items(s.value.folders, key = { it.key }) { folder ->
                                ListItem(
                                    modifier = Modifier.clickable(role = Role.Button) { target = folder.path },
                                    leadingContent = {
                                        Icon(Icons.Rounded.Folder, contentDescription = stringResource(R.string.manage_cd_folder))
                                    },
                                    headlineContent = { Text(folder.name, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                                )
                            }
                        }
                    }
                }
            }
        },
        confirmButton = {
            TextButton(enabled = !invalid && listing !is LoadState.Loading, onClick = { onMove(target) }) {
                Text(stringResource(R.string.manage_move_here))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_cancel)) }
        },
    )
}

/** True when [target] cannot receive [entries]: the current folder, or a moved folder itself or a subfolder of it. */
internal fun invalidTarget(target: String, currentPath: String, entries: List<BucketEntryInfo>): Boolean =
    target == currentPath || entries.any { it.isFolder && target.startsWith(it.path) }

/**
 * Confirms deleting [entries]. Files alone need one tap. When a folder is included, the user must
 * type the folder's name (one selected folder) or the number of items (anything else) to enable
 * Delete. Delete is shown in the error colour.
 */
@Composable
fun DeleteDialog(entries: List<BucketEntryInfo>, onDelete: () -> Unit, onDismiss: () -> Unit) {
    val count = entries.size
    val folderIncluded = entries.any { it.isFolder }
    val single = entries.singleOrNull()
    val expected = when {
        !folderIncluded -> ""
        single != null -> single.name
        else -> count.toString()
    }
    var typed by remember(expected) { mutableStateOf("") }
    val confirmed = !folderIncluded || typed == expected

    val message = when {
        !folderIncluded -> pluralStringResource(R.plurals.manage_delete_files_message, count, count)
        single != null -> stringResource(R.string.manage_delete_folder_message, single.name)
        else -> pluralStringResource(R.plurals.manage_delete_mixed_message, count, count)
    }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.manage_delete_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(message)
                if (folderIncluded) {
                    OutlinedTextField(
                        value = typed,
                        onValueChange = { typed = it },
                        label = {
                            Text(
                                if (single != null) {
                                    stringResource(R.string.manage_delete_type_name, single.name)
                                } else {
                                    stringResource(R.string.manage_delete_type_count, count)
                                },
                            )
                        },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
        },
        confirmButton = {
            TextButton(
                enabled = confirmed,
                onClick = onDelete,
                colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error),
            ) { Text(stringResource(R.string.action_delete)) }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_cancel)) }
        },
    )
}

/** The keys and reasons of the objects a partly failed operation could not change. */
@Composable
fun FailuresDialog(failures: List<FailureInfo>, onDismiss: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.manage_failures_title)) },
        text = {
            LazyColumn(Modifier.fillMaxWidth().height(260.dp)) {
                items(failures, key = { it.key }) { failure ->
                    Column(Modifier.padding(vertical = 6.dp)) {
                        Text(failure.key, style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium)
                        Text(
                            failure.message,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        },
        confirmButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_close)) }
        },
    )
}

/** The non-dismissable progress dialog shown while a bucket change runs. */
@Composable
fun BusyDialog(busy: ManageBusy) {
    val text = when (busy) {
        ManageBusy.Renaming -> stringResource(R.string.manage_busy_renaming)
        is ManageBusy.Moving -> pluralStringResource(R.plurals.manage_busy_moving, busy.count, busy.count)
        is ManageBusy.Deleting -> pluralStringResource(R.plurals.manage_busy_deleting, busy.count, busy.count)
    }
    AlertDialog(
        onDismissRequest = {},
        properties = DialogProperties(dismissOnBackPress = false, dismissOnClickOutside = false),
        text = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 3.dp)
                Spacer(Modifier.width(16.dp))
                Text(text)
            }
        },
        confirmButton = {},
    )
}

/**
 * The folder path as buttons: Root first, then each folder down to [path], separated by chevrons.
 * Each button opens that level through [onOpen] (a path with a trailing `/`, or `""` for the root).
 */
@Composable
fun ManageBreadcrumbs(path: String, onOpen: (String) -> Unit) {
    val segments = path.split('/').filter { it.isNotEmpty() }
    Row(
        Modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState()),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        TextButton(onClick = { onOpen("") }) {
            Text(stringResource(R.string.manage_root))
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

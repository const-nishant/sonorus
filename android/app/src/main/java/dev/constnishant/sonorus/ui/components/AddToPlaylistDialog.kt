package dev.constnishant.sonorus.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.rounded.LibraryMusic
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.PlaylistInfo
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.screens.LoadState
import dev.constnishant.sonorus.ui.screens.displayMessage

/**
 * Dialog that adds [trackIds] to an existing playlist or to a new one. Loads the playlists when
 * shown. Closes after an action is chosen.
 *
 * @param trackIds the tracks to add.
 * @param onDismiss called when the dialog closes (cancel or after an action).
 * @param actions performs the add.
 */
@Composable
fun AddToPlaylistDialog(
    trackIds: List<Long>,
    onDismiss: () -> Unit,
    actions: TrackActions,
) {
    val repository = LocalContext.current.repository
    val playlists by produceState<LoadState<List<PlaylistInfo>>>(LoadState.Loading, repository) {
        value = try {
            LoadState.Ready(repository.call { core -> core.playlists() })
        } catch (e: CoreException) {
            LoadState.Failed(e.displayMessage())
        }
    }
    var creating by remember { mutableStateOf(false) }
    var newName by remember { mutableStateOf("") }
    val trimmed = newName.trim()

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Add to playlist") },
        text = {
            Column(
                Modifier
                    .fillMaxWidth()
                    .heightIn(max = 360.dp)
                    .verticalScroll(rememberScrollState()),
            ) {
                when (val state = playlists) {
                    LoadState.Loading -> CircularProgressIndicator(Modifier.size(32.dp))
                    is LoadState.Failed -> Text(state.message, color = MaterialTheme.colorScheme.error)
                    is LoadState.Ready -> state.value.forEach { playlist ->
                        Row(
                            Modifier
                                .fillMaxWidth()
                                .clickable {
                                    actions.addToPlaylist(playlist.id, trackIds)
                                    onDismiss()
                                }
                                .padding(vertical = 12.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Icon(Icons.Rounded.LibraryMusic, contentDescription = null)
                            Spacer(Modifier.size(16.dp))
                            Column {
                                Text(playlist.name, maxLines = 1)
                                Text(
                                    "${playlist.trackCount} tracks",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                    }
                }
                Row(
                    Modifier
                        .fillMaxWidth()
                        .clickable { creating = true }
                        .padding(vertical = 12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(Icons.Rounded.Add, contentDescription = null)
                    Spacer(Modifier.size(16.dp))
                    Text("New playlist")
                }
                if (creating) {
                    OutlinedTextField(
                        value = newName,
                        onValueChange = { newName = it },
                        label = { Text("Playlist name") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
        },
        confirmButton = {
            if (creating) {
                TextButton(
                    enabled = trimmed.isNotEmpty(),
                    onClick = {
                        actions.createPlaylistAndAdd(trimmed, trackIds)
                        onDismiss()
                    },
                ) { Text("Create") }
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text("Cancel") }
        },
    )
}

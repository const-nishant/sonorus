package dev.constnishant.sonorus.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalResources
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.AlbumInfo
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.TagEditInfo
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.data.userMessage
import kotlinx.coroutines.launch

/** Largest value a year, track number or disc number may have in the tag forms. */
private const val MAX_TAG_NUMBER = 9999

/** One field of a tag form: its label, its value when the form opens, and whether it takes a number. */
private class TagField(val label: String, val initial: String, val number: Boolean = false)

/** A [TagEditInfo] with only the given fields set; every other field keeps its value (null). */
private fun editOf(
    title: String? = null,
    artist: String? = null,
    album: String? = null,
    albumArtist: String? = null,
    year: Int? = null,
    trackNo: UInt? = null,
    discNo: UInt? = null,
    genre: String? = null,
): TagEditInfo = TagEditInfo(
    title = title,
    artist = artist,
    album = album,
    albumArtist = albumArtist,
    year = year,
    trackNo = trackNo,
    discNo = discNo,
    genre = genre,
)

/** The number a number field holds, or 0 (which removes the tag) when it is blank. */
private fun numberOf(text: String): Int = text.trim().toIntOrNull() ?: 0

/** True when a number field holds something other than a blank or a number from 0 to 9999. */
private fun invalidNumber(text: String): Boolean =
    text.isNotBlank() && text.trim().toIntOrNull()?.let { it in 0..MAX_TAG_NUMBER } != true

/**
 * Track tag form: title, artist, album, album artist, year, track number, disc number and genre.
 * It opens with the track's current values and sends only the fields the user changed. A blank
 * text field removes its tag, and a blank number removes the number. The form is disabled while
 * the tags are saved. A failure is shown in the form and the dialog stays open.
 *
 * @param trackId the catalogued track to edit.
 * @param onDismiss called on cancel, outside tap, or after a successful save.
 */
@Composable
fun TrackTagsDialog(trackId: Long, onDismiss: () -> Unit) {
    val repository = LocalContext.current.repository
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var track by remember(trackId) { mutableStateOf<TrackInfo?>(null) }
    var loadError by remember(trackId) { mutableStateOf<String?>(null) }
    var saving by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }

    LaunchedEffect(trackId) {
        try {
            track = repository.track(trackId)
        } catch (e: CoreException) {
            loadError = e.userMessage(context, R.string.error_load_generic)
        }
    }

    val loaded = track
    if (loaded == null) {
        AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text(stringResource(R.string.tags_title)) },
            text = {
                val message = loadError
                if (message != null) {
                    Text(message, color = MaterialTheme.colorScheme.error)
                } else {
                    CircularProgressIndicator(Modifier.size(32.dp))
                }
            },
            confirmButton = {
                TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_close)) }
            },
        )
        return
    }

    TagFormDialog(
        title = stringResource(R.string.tags_title),
        fields = listOf(
            TagField(stringResource(R.string.tags_field_title), loaded.title),
            TagField(stringResource(R.string.tags_field_artist), loaded.artist),
            TagField(stringResource(R.string.tags_field_album), loaded.album),
            TagField(stringResource(R.string.tags_field_album_artist), loaded.albumArtist),
            TagField(stringResource(R.string.tags_field_year), loaded.year?.toString().orEmpty(), number = true),
            TagField(stringResource(R.string.tags_field_track_no), loaded.trackNo?.toString().orEmpty(), number = true),
            TagField(stringResource(R.string.tags_field_disc_no), loaded.discNo?.toString().orEmpty(), number = true),
            TagField(stringResource(R.string.tags_field_genre), loaded.genre.orEmpty()),
        ),
        saving = saving,
        progress = null,
        error = error,
        onSave = { values, fields ->
            val edit = changedEdit(fields, values) { index, text ->
                when (index) {
                    0 -> editOf(title = text.trim())
                    1 -> editOf(artist = text.trim())
                    2 -> editOf(album = text.trim())
                    3 -> editOf(albumArtist = text.trim())
                    4 -> editOf(year = numberOf(text))
                    5 -> editOf(trackNo = numberOf(text).toUInt())
                    6 -> editOf(discNo = numberOf(text).toUInt())
                    else -> editOf(genre = text.trim())
                }
            }
            if (edit == null) {
                onDismiss()
                return@TagFormDialog
            }
            saving = true
            error = null
            scope.launch {
                try {
                    repository.editTags(trackId, edit)
                    onDismiss()
                } catch (e: CoreException) {
                    error = e.userMessage(context, R.string.error_save_tags)
                    saving = false
                }
            }
        },
        onDismiss = onDismiss,
    )
}

/**
 * Album tag form: album, album artist, year and genre. Each field applies to every track of the
 * album, one edit per track, and the form shows "Saving 3 of 12…" while it runs. The genre starts
 * from the first track's genre. A failed track is counted and the dialog stays open.
 *
 * @param albumId the album to edit.
 * @param onDismiss called on cancel, outside tap, or after every track is saved.
 */
@Composable
fun AlbumTagsDialog(albumId: Long, onDismiss: () -> Unit) {
    val repository = LocalContext.current.repository
    val context = LocalContext.current
    val resources = LocalResources.current
    val scope = rememberCoroutineScope()
    var loaded by remember(albumId) { mutableStateOf<Pair<AlbumInfo, List<TrackInfo>>?>(null) }
    var loadError by remember(albumId) { mutableStateOf<String?>(null) }
    var saving by remember { mutableStateOf(false) }
    var done by remember { mutableIntStateOf(0) }
    var total by remember { mutableIntStateOf(0) }
    var error by remember { mutableStateOf<String?>(null) }

    LaunchedEffect(albumId) {
        try {
            loaded = repository.call { core -> core.album(albumId) to core.albumTracks(albumId) }
        } catch (e: CoreException) {
            loadError = e.userMessage(context, R.string.error_load_generic)
        }
    }

    val current = loaded
    if (current == null) {
        AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text(stringResource(R.string.tags_album_title)) },
            text = {
                val message = loadError
                if (message != null) {
                    Text(message, color = MaterialTheme.colorScheme.error)
                } else {
                    CircularProgressIndicator(Modifier.size(32.dp))
                }
            },
            confirmButton = {
                TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_close)) }
            },
        )
        return
    }
    val (album, tracks) = current

    TagFormDialog(
        title = stringResource(R.string.tags_album_title),
        fields = listOf(
            TagField(stringResource(R.string.tags_field_album), album.title),
            TagField(stringResource(R.string.tags_field_album_artist), album.artist),
            TagField(stringResource(R.string.tags_field_year), album.year?.toString().orEmpty(), number = true),
            TagField(stringResource(R.string.tags_field_genre), tracks.firstOrNull()?.genre.orEmpty()),
        ),
        saving = saving,
        progress = if (saving) Pair(done, total) else null,
        error = error,
        onSave = { values, fields ->
            val edit = changedEdit(fields, values) { index, text ->
                when (index) {
                    0 -> editOf(album = text.trim())
                    1 -> editOf(albumArtist = text.trim())
                    2 -> editOf(year = numberOf(text))
                    else -> editOf(genre = text.trim())
                }
            }
            if (edit == null) {
                onDismiss()
                return@TagFormDialog
            }
            saving = true
            error = null
            done = 0
            total = tracks.size
            scope.launch {
                var failures = 0
                var firstFailure: String? = null
                for (track in tracks) {
                    try {
                        repository.editTags(track.id, edit)
                    } catch (e: CoreException) {
                        failures++
                        if (firstFailure == null) firstFailure = e.userMessage(context, R.string.error_save_tags)
                    }
                    done++
                }
                if (failures == 0) {
                    onDismiss()
                } else {
                    error = resources.getQuantityString(
                        R.plurals.tags_album_partial,
                        tracks.size,
                        failures,
                        tracks.size,
                        firstFailure.orEmpty(),
                    )
                    saving = false
                }
            }
        },
        onDismiss = onDismiss,
    )
}

/**
 * The fields that differ from their starting value, mapped by [build] to a [TagEditInfo]. Returns
 * null when nothing changed. [build] gets the field index and its text.
 */
private fun changedEdit(
    fields: List<TagField>,
    values: List<String>,
    build: (Int, String) -> TagEditInfo,
): TagEditInfo? {
    var edit: TagEditInfo? = null
    fields.forEachIndexed { index, field ->
        val text = values[index]
        if (text.trim() != field.initial.trim()) {
            edit = merge(edit, build(index, text))
        }
    }
    return edit
}

/** Combines two partial edits: the fields set in [extra] are copied over [base]. */
private fun merge(base: TagEditInfo?, extra: TagEditInfo): TagEditInfo {
    if (base == null) return extra
    return TagEditInfo(
        title = extra.title ?: base.title,
        artist = extra.artist ?: base.artist,
        album = extra.album ?: base.album,
        albumArtist = extra.albumArtist ?: base.albumArtist,
        year = extra.year ?: base.year,
        trackNo = extra.trackNo ?: base.trackNo,
        discNo = extra.discNo ?: base.discNo,
        genre = extra.genre ?: base.genre,
    )
}

/**
 * The shared form of the tag dialogs. The values start from each field's initial value. Save is
 * enabled while every number is valid; [onSave] receives the values and the fields in order.
 * While [saving], the form is disabled and [progress] (done of total) or a spinner shows.
 */
@Composable
private fun TagFormDialog(
    title: String,
    fields: List<TagField>,
    saving: Boolean,
    progress: Pair<Int, Int>?,
    error: String?,
    onSave: (List<String>, List<TagField>) -> Unit,
    onDismiss: () -> Unit,
) {
    val values = remember(fields) { mutableStateListOf<String>().apply { addAll(fields.map { it.initial }) } }
    val valid = fields.indices.none { fields[it].number && invalidNumber(values[it]) }
    AlertDialog(
        onDismissRequest = { if (!saving) onDismiss() },
        properties = DialogProperties(dismissOnBackPress = !saving, dismissOnClickOutside = !saving),
        title = { Text(title) },
        text = {
            Column(
                Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                if (saving) {
                    if (progress != null) {
                        LinearProgressIndicator(
                            progress = { progress.first.toFloat() / progress.second.coerceAtLeast(1) },
                            modifier = Modifier.fillMaxWidth(),
                        )
                        Text(stringResource(R.string.tags_saving_progress, progress.first, progress.second))
                    } else {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                            Spacer(Modifier.width(12.dp))
                            Text(stringResource(R.string.tags_saving))
                        }
                    }
                }
                fields.forEachIndexed { index, field ->
                    val invalid = field.number && invalidNumber(values[index])
                    OutlinedTextField(
                        value = values[index],
                        onValueChange = { values[index] = it },
                        label = { Text(field.label) },
                        singleLine = true,
                        enabled = !saving,
                        isError = invalid,
                        supportingText = {
                            Text(
                                if (invalid) {
                                    stringResource(R.string.tags_number_error, MAX_TAG_NUMBER)
                                } else if (field.number) {
                                    stringResource(R.string.tags_number_hint)
                                } else {
                                    ""
                                },
                            )
                        },
                        keyboardOptions = if (field.number) {
                            KeyboardOptions(keyboardType = KeyboardType.Number)
                        } else {
                            KeyboardOptions(autoCorrectEnabled = false)
                        },
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
                if (error != null) {
                    Text(error, color = MaterialTheme.colorScheme.error)
                }
            }
        },
        confirmButton = {
            TextButton(
                enabled = !saving && valid,
                onClick = { onSave(values.toList(), fields) },
            ) { Text(stringResource(R.string.action_save)) }
        },
        dismissButton = {
            TextButton(enabled = !saving, onClick = onDismiss) { Text(stringResource(R.string.action_cancel)) }
        },
    )
}

@file:OptIn(ExperimentalMaterial3Api::class)

package dev.constnishant.sonorus.ui.uploads

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.CloudUpload
import androidx.compose.material.icons.rounded.ErrorOutline
import androidx.compose.material.icons.rounded.Refresh
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.ViewModelProvider.AndroidViewModelFactory.Companion.APPLICATION_KEY
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.UploadJobInfo
import dev.constnishant.sonorus.core.UploadState
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.BackButton
import dev.constnishant.sonorus.ui.components.CenteredContent
import dev.constnishant.sonorus.ui.components.EmptyState
import dev.constnishant.sonorus.ui.components.LoadingContent
import dev.constnishant.sonorus.ui.screens.LoadState
import dev.constnishant.sonorus.ui.theme.tabular
import dev.constnishant.sonorus.upload.UploadService

/**
 * The uploads screen: the upload queue with each job's state and progress. "Add music" picks audio
 * files, copies them to the bucket in the background and shows a summary of what was queued.
 *
 * @param onBack leaves the screen.
 * @param modifier applied to the root layout.
 */
@Composable
fun UploadsScreen(onBack: () -> Unit, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val repository = context.repository
    val viewModel: UploadsViewModel = viewModel { UploadsViewModel(checkNotNull(this[APPLICATION_KEY]), repository) }
    val state by viewModel.state.collectAsStateWithLifecycle()
    val progress by viewModel.progress.collectAsStateWithLifecycle()
    val preparing by viewModel.preparing.collectAsStateWithLifecycle()
    val prefix by viewModel.prefix.collectAsStateWithLifecycle()
    val configured by repository.configured.collectAsStateWithLifecycle()
    val snackbarHostState = remember { SnackbarHostState() }

    LaunchedEffect(viewModel) {
        viewModel.messages.collect { snackbarHostState.showSnackbar(it) }
    }

    // Asked once per visit to this screen, before the first pick.
    var askedForNotifications by rememberSaveable { mutableStateOf(false) }
    var pendingUris by remember { mutableStateOf<List<Uri>>(emptyList()) }
    val addFiles = { uris: List<Uri> ->
        viewModel.addFiles(uris) { UploadService.start(context) }
    }
    val notificationPermission = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) {
        // Uploads go ahead whether or not notifications were allowed.
        val uris = pendingUris
        pendingUris = emptyList()
        if (uris.isNotEmpty()) addFiles(uris)
    }
    val picker = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenMultipleDocuments(),
    ) { uris ->
        if (uris.isEmpty()) return@rememberLauncherForActivityResult
        if (!askedForNotifications && !notificationsGranted(context)) {
            askedForNotifications = true
            pendingUris = uris
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        } else {
            addFiles(uris)
        }
    }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.uploads_title)) },
                navigationIcon = { BackButton(onBack) },
                actions = {
                    TextButton(onClick = viewModel::clearFinished) { Text(stringResource(R.string.action_clear_finished)) }
                },
            )
        },
        floatingActionButton = {
            if (configured) {
                ExtendedFloatingActionButton(
                    onClick = { picker.launch(arrayOf("audio/*")) },
                    icon = { Icon(Icons.Rounded.Add, contentDescription = null) },
                    text = { Text(stringResource(R.string.action_add_music)) },
                )
            }
        },
        snackbarHost = { SnackbarHost(snackbarHostState) },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            Notices(preparing = preparing, configured = configured)
            when (val s = state) {
                LoadState.Loading -> LoadingContent(Modifier.weight(1f))
                is LoadState.Failed -> CenteredContent(Modifier.weight(1f)) {
                    EmptyState(Icons.Rounded.ErrorOutline, stringResource(R.string.uploads_load_failed), s.message)
                }
                is LoadState.Ready -> if (s.value.isEmpty()) {
                    CenteredContent(Modifier.weight(1f)) {
                        EmptyState(
                            icon = Icons.Rounded.CloudUpload,
                            title = stringResource(R.string.uploads_empty_title),
                            message = stringResource(R.string.uploads_empty_message),
                        )
                    }
                } else {
                    LazyColumn(
                        modifier = Modifier.weight(1f),
                        // Room below the last row for the "Add music" button.
                        contentPadding = PaddingValues(bottom = 96.dp),
                    ) {
                        items(s.value, key = { it.id }) { job ->
                            UploadRow(
                                job = job,
                                folderKey = folderKey(job.key, prefix),
                                progress = progress[job.id],
                                onCancel = { viewModel.cancel(job.id) },
                                onRetry = {
                                    UploadService.start(context, retryJobId = job.id)
                                    viewModel.reload()
                                },
                            )
                        }
                    }
                }
            }
        }
    }
}

/** The "Reading files…" line while files are added, and the notice when no bucket is set. */
@Composable
private fun Notices(preparing: Boolean, configured: Boolean) {
    if (preparing) {
        LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
        Text(
            text = stringResource(R.string.uploads_reading),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
        )
    }
    if (!configured) {
        Text(
            text = stringResource(R.string.error_not_configured),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.padding(16.dp),
        )
    }
}

/** One upload job: name, object key, size and state, with cancel and retry actions. */
@Composable
private fun UploadRow(
    job: UploadJobInfo,
    folderKey: String,
    progress: Pair<ULong, ULong>?,
    onCancel: () -> Unit,
    onRetry: () -> Unit,
) {
    val context = LocalContext.current
    val running = job.state == UploadState.UPLOADING || (job.state == UploadState.QUEUED && progress != null)
    val fraction = uploadFraction(job, progress)
    val canCancel = job.state == UploadState.QUEUED ||
        job.state == UploadState.UPLOADING ||
        job.state == UploadState.FAILED
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(start = 16.dp, end = 8.dp, top = 8.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Column(Modifier.weight(1f)) {
            Text(
                text = job.displayName,
                style = MaterialTheme.typography.bodyLarge,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                text = folderKey,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.MiddleEllipsis,
            )
            Text(
                text = formatMegabytes(context, job.size) + " · " + stateLine(job, running, fraction),
                style = MaterialTheme.typography.bodySmall.tabular(),
                color = if (job.state == UploadState.FAILED) {
                    MaterialTheme.colorScheme.error
                } else {
                    MaterialTheme.colorScheme.onSurfaceVariant
                },
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            if (running && fraction != null) {
                LinearProgressIndicator(
                    progress = { fraction.toFloat() },
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(top = 4.dp),
                )
            }
        }
        if (canCancel) {
            IconButton(onClick = onCancel) {
                Icon(Icons.Rounded.Close, contentDescription = stringResource(R.string.cd_cancel_upload))
            }
        }
        if (job.state == UploadState.FAILED) {
            IconButton(onClick = onRetry) {
                Icon(Icons.Rounded.Refresh, contentDescription = stringResource(R.string.cd_retry_upload))
            }
        }
    }
}

/** The state of [job] as a short phrase, such as "Uploading 42%" or "Failed: <error>". */
@Composable
private fun stateLine(job: UploadJobInfo, running: Boolean, fraction: Double?): String = when (job.state) {
    UploadState.QUEUED -> if (running) uploadingLine(fraction) else stringResource(R.string.upload_state_queued)
    UploadState.UPLOADING -> uploadingLine(fraction)
    UploadState.DONE -> stringResource(R.string.upload_state_uploaded)
    UploadState.DUPLICATE -> stringResource(R.string.upload_state_duplicate)
    UploadState.FAILED -> stringResource(
        R.string.upload_state_failed,
        job.error ?: stringResource(R.string.upload_unknown_error),
    )
    UploadState.CANCELLED -> stringResource(R.string.upload_state_cancelled)
}

@Composable
private fun uploadingLine(fraction: Double?): String =
    if (fraction == null) {
        stringResource(R.string.upload_state_uploading)
    } else {
        stringResource(R.string.upload_state_uploading_percent, (fraction * 100).toInt())
    }

/**
 * Share of [job] stored: from the latest progress event when there is one, else from the job
 * itself while it is uploading. Null when unknown.
 */
private fun uploadFraction(job: UploadJobInfo, progress: Pair<ULong, ULong>?): Double? {
    if (progress != null && progress.second > 0UL) {
        return progress.first.toDouble() / progress.second.toDouble()
    }
    return if (job.state == UploadState.UPLOADING && job.size > 0UL) {
        job.bytesDone.toDouble() / job.size.toDouble()
    } else {
        null
    }
}

/** [key] without the bucket's folder [prefix], as it is shown in the list. */
private fun folderKey(key: String, prefix: String): String {
    val folder = prefix.trim('/').let { if (it.isEmpty()) "" else "$it/" }
    return key.removePrefix(folder)
}

/** A size in megabytes, such as "4.2 MB". */
private fun formatMegabytes(context: Context, bytes: ULong): String =
    context.getString(R.string.upload_size_mb, "%.1f".format(bytes.toDouble() / 1_000_000.0))

/** True when the app may post notifications (Android 13 and later need the permission). */
private fun notificationsGranted(context: Context): Boolean =
    ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
        PackageManager.PERMISSION_GRANTED

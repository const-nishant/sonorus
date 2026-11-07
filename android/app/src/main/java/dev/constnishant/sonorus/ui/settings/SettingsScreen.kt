package dev.constnishant.sonorus.ui.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.ArrowBack
import androidx.compose.material.icons.rounded.Visibility
import androidx.compose.material.icons.rounded.VisibilityOff
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.constnishant.sonorus.core.ScanProgressInfo
import dev.constnishant.sonorus.data.ScanUi
import dev.constnishant.sonorus.data.repository

/**
 * Settings: bucket connection, library scan and stats, playlist and favorite sync, and about
 * information. [onBack] returns to the previous screen.
 */
@Composable
fun SettingsScreen(onBack: () -> Unit, modifier: Modifier = Modifier) {
    val repository = LocalContext.current.repository
    val viewModel: SettingsViewModel = viewModel { SettingsViewModel(repository) }
    val state by viewModel.state.collectAsStateWithLifecycle()
    val scan by viewModel.scan.collectAsStateWithLifecycle()
    var showSecret by rememberSaveable { mutableStateOf(false) }

    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = onBack) {
                Icon(Icons.AutoMirrored.Rounded.ArrowBack, contentDescription = "Back")
            }
            Text(text = "Settings", style = MaterialTheme.typography.titleLarge)
        }

        BucketSection(
            state = state,
            showSecret = showSecret,
            onToggleSecret = { showSecret = !showSecret },
            viewModel = viewModel,
        )
        LibrarySection(state = state, scan = scan, onScan = viewModel::startScan)
        SyncSection(state = state, onSync = viewModel::syncUserData)
        AboutSection(version = state.version)
    }
}

@Composable
private fun SectionTitle(text: String) {
    Spacer(Modifier.height(8.dp))
    Text(text = text, style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.primary)
}

@Composable
private fun BucketSection(
    state: SettingsUiState,
    showSecret: Boolean,
    onToggleSecret: () -> Unit,
    viewModel: SettingsViewModel,
) {
    val form = state.form
    SectionTitle("Bucket")
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        BucketProvider.entries.forEach { provider ->
            FilterChip(
                selected = form.provider == provider,
                onClick = { viewModel.selectProvider(provider) },
                label = { Text(provider.label) },
            )
        }
    }
    OutlinedTextField(
        value = form.endpoint,
        onValueChange = viewModel::setEndpoint,
        label = { Text("Endpoint") },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, autoCorrectEnabled = false),
        modifier = Modifier.fillMaxWidth(),
    )
    OutlinedTextField(
        value = form.region,
        onValueChange = { value -> viewModel.updateForm { copy(region = value) } },
        label = { Text("Region (optional)") },
        singleLine = true,
        keyboardOptions = KeyboardOptions(autoCorrectEnabled = false),
        modifier = Modifier.fillMaxWidth(),
    )
    OutlinedTextField(
        value = form.bucket,
        onValueChange = { value -> viewModel.updateForm { copy(bucket = value) } },
        label = { Text("Bucket") },
        singleLine = true,
        keyboardOptions = KeyboardOptions(autoCorrectEnabled = false),
        modifier = Modifier.fillMaxWidth(),
    )
    OutlinedTextField(
        value = form.accessKeyId,
        onValueChange = { value -> viewModel.updateForm { copy(accessKeyId = value) } },
        label = { Text("Access key ID") },
        singleLine = true,
        keyboardOptions = KeyboardOptions(autoCorrectEnabled = false),
        modifier = Modifier.fillMaxWidth(),
    )
    OutlinedTextField(
        value = form.secretAccessKey,
        onValueChange = { value -> viewModel.updateForm { copy(secretAccessKey = value) } },
        label = { Text("Secret access key") },
        singleLine = true,
        visualTransformation = if (showSecret) VisualTransformation.None else PasswordVisualTransformation(),
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false),
        trailingIcon = {
            IconButton(onClick = onToggleSecret) {
                Icon(
                    imageVector = if (showSecret) Icons.Rounded.VisibilityOff else Icons.Rounded.Visibility,
                    contentDescription = if (showSecret) "Hide secret" else "Show secret",
                )
            }
        },
        modifier = Modifier.fillMaxWidth(),
    )
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Column(Modifier.weight(1f)) {
            Text(text = "Path-style URLs", style = MaterialTheme.typography.bodyLarge)
            Text(
                text = "On for Backblaze B2 and Cloudflare R2",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Switch(
            checked = form.pathStyle,
            onCheckedChange = { value -> viewModel.updateForm { copy(pathStyle = value) } },
        )
    }
    OutlinedTextField(
        value = form.prefix,
        onValueChange = { value -> viewModel.updateForm { copy(prefix = value) } },
        label = { Text("Folder prefix (optional)") },
        singleLine = true,
        keyboardOptions = KeyboardOptions(autoCorrectEnabled = false),
        modifier = Modifier.fillMaxWidth(),
    )

    Button(
        onClick = viewModel::saveAndConnect,
        enabled = !state.saving,
        modifier = Modifier.fillMaxWidth(),
    ) {
        if (state.saving) {
            CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
            Spacer(Modifier.width(8.dp))
        }
        Text("Save & connect")
    }
    state.bucketMessage?.let { message ->
        Text(
            text = message,
            style = MaterialTheme.typography.bodyMedium,
            color = if (state.bucketFailed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary,
        )
    }
    if (state.hasSettings) {
        TextButton(onClick = viewModel::disconnect) {
            Text("Disconnect")
        }
    }
}

@Composable
private fun LibrarySection(state: SettingsUiState, scan: ScanUi, onScan: () -> Unit) {
    SectionTitle("Library")
    val stats = state.stats
    Text(
        text = when {
            stats != null ->
                "${stats.tracks} tracks · ${stats.albums} albums · " +
                    "%.2f GB · %.1f h".format(
                        stats.totalBytes.toDouble() / 1e9,
                        stats.totalDurationMs.toDouble() / 3_600_000.0,
                    )
            state.statsError != null -> state.statsError
            else -> "Loading library stats..."
        },
        style = MaterialTheme.typography.bodyMedium,
        color = if (state.statsError != null && stats == null) {
            MaterialTheme.colorScheme.error
        } else {
            MaterialTheme.colorScheme.onSurfaceVariant
        },
    )

    Button(onClick = onScan, enabled = !scan.running) {
        Text("Scan bucket")
    }
    if (scan.running) {
        val progress = scan.progress
        if (progress != null) {
            LinearProgressIndicator(
                progress = { progressFraction(progress) },
                modifier = Modifier.fillMaxWidth(),
            )
            Text(
                text = progressLine(progress),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
        }
    } else {
        scan.lastSummary?.let { summary ->
            Text(
                text = "Last scan: " + progressLine(summary.progress),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
    scan.error?.let { message ->
        Text(
            text = message,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.error,
            maxLines = 3,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun SyncSection(state: SettingsUiState, onSync: () -> Unit) {
    SectionTitle("Sync")
    Button(onClick = onSync, enabled = !state.syncing) {
        if (state.syncing) {
            CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
            Spacer(Modifier.width(8.dp))
        }
        Text("Sync playlists & favorites")
    }
    state.syncMessage?.let { message ->
        Text(
            text = message,
            style = MaterialTheme.typography.bodyMedium,
            color = if (state.syncFailed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary,
        )
    }
}

@Composable
private fun AboutSection(version: String) {
    SectionTitle("About")
    Text(text = "Sonorus $version", style = MaterialTheme.typography.bodyMedium)
    Text(
        text = "Audio output: silent test output until Phase 10",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

/** Fraction of a scan done, 0 when the total is not known yet. */
private fun progressFraction(p: ScanProgressInfo): Float =
    if (p.total == 0u) 0f else p.done.toFloat() / p.total.toFloat()

/** "done/total · +added ~updated −removed !failed". */
private fun progressLine(p: ScanProgressInfo): String {
    val changes = listOf(
        p.added to "added",
        p.updated to "updated",
        p.removed to "removed",
        p.failed to "failed",
    ).filter { (count, _) -> count > 0u }.joinToString(", ") { (count, label) -> "$count $label" }
    val files = "${p.done} of ${p.total} files"
    return if (changes.isEmpty()) "$files · no changes" else "$files · $changes"
}

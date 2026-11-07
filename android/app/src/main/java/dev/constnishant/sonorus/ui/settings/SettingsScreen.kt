package dev.constnishant.sonorus.ui.settings

import androidx.compose.foundation.clickable
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
import androidx.compose.material.icons.automirrored.rounded.KeyboardArrowRight
import androidx.compose.material.icons.rounded.Visibility
import androidx.compose.material.icons.rounded.VisibilityOff
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuAnchorType
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
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
import dev.constnishant.sonorus.core.CacheUsageInfo
import dev.constnishant.sonorus.core.OutputInfo
import dev.constnishant.sonorus.core.OutputSampleFormat
import dev.constnishant.sonorus.core.ScanProgressInfo
import dev.constnishant.sonorus.data.GIB
import dev.constnishant.sonorus.data.ScanUi
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.ConfirmDialog
import dev.constnishant.sonorus.ui.components.formatBytes
import dev.constnishant.sonorus.ui.components.formatKhz
import dev.constnishant.sonorus.ui.components.label

/**
 * Settings: bucket connection, library scan and stats, music uploads, playlist and favorite sync,
 * and about information. [onBack] returns to the previous screen; [onUploads] opens the uploads
 * screen.
 */
@Composable
fun SettingsScreen(onBack: () -> Unit, onUploads: () -> Unit, modifier: Modifier = Modifier) {
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
        UploadsSection(onUploads = onUploads)
        OutputSection(state = state, viewModel = viewModel)
        DownloadsSection(state = state, viewModel = viewModel)
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
private fun OutputSection(state: SettingsUiState, viewModel: SettingsViewModel) {
    SectionTitle("Output")
    val formats = listOf(
        OutputSampleFormat.FLOAT to "Float",
        OutputSampleFormat.INT32 to "32-bit",
        OutputSampleFormat.INT24_PACKED to "24-bit",
    )
    SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
        formats.forEachIndexed { index, (format, label) ->
            SegmentedButton(
                selected = state.output.sampleFormat == format,
                onClick = { viewModel.setSampleFormat(format) },
                shape = SegmentedButtonDefaults.itemShape(index = index, count = formats.size),
            ) {
                Text(label)
            }
        }
    }
    Text(
        text = "Float follows the volume setting. 32-bit and 24-bit send the samples unchanged " +
            "(bit-perfect) at full volume.",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Column(Modifier.weight(1f)) {
            Text(text = "Exclusive mode", style = MaterialTheme.typography.bodyLarge)
            Text(
                text = "Asks Android for a direct, low-latency stream. The device may still share it.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Switch(
            checked = state.output.exclusive,
            onCheckedChange = viewModel::setExclusive,
        )
    }
    Text(
        text = "Changing these stops playback.",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Column(Modifier.weight(1f)) {
            Text(text = "Bit-perfect to USB DAC", style = MaterialTheme.typography.bodyLarge)
            Text(
                text = "Sends each track to the DAC in its own format, untouched. Other sounds are " +
                    "muted on the DAC while it plays.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Switch(
            checked = state.bitPerfect,
            onCheckedChange = viewModel::setBitPerfect,
        )
    }
    Text(
        text = state.dacName?.let { "USB DAC: $it" } ?: "No USB DAC connected",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Text(
        text = state.outputInfo?.let(::outputLine) ?: "No active output stream",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

/** Cache size choices: the limit in bytes, labelled in whole gigabytes. */
private val CACHE_LIMIT_OPTIONS: List<Pair<ULong, String>> =
    listOf(1L, 2L, 4L, 8L, 16L).map { gb -> (gb * GIB).toULong() to "$gb GB" }

/** How long unpinned cached songs are kept; null keeps them forever. */
private val KEEP_FOR_OPTIONS: List<Pair<UInt?, String>> = listOf(
    7u to "7 days",
    30u to "30 days",
    90u to "90 days",
    null to "Forever",
)

@Composable
private fun DownloadsSection(state: SettingsUiState, viewModel: SettingsViewModel) {
    var confirmingClear by remember { mutableStateOf(false) }
    SectionTitle("Downloads & cache")
    val usage = state.cacheUsage
    if (usage != null) {
        Text(
            text = cacheUsageLine(usage),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    } else if (!state.cacheFailed) {
        Text(
            text = "Loading cache usage...",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    ChoiceDropdown(
        label = "Cache size",
        options = CACHE_LIMIT_OPTIONS,
        selected = state.cache.limitBytes,
        onSelect = viewModel::setCacheLimit,
    )
    ChoiceDropdown(
        label = "Keep songs for",
        options = KEEP_FOR_OPTIONS,
        selected = state.cache.ttlDays,
        onSelect = viewModel::setKeepDays,
    )
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Column(Modifier.weight(1f)) {
            Text(text = "Download on mobile data", style = MaterialTheme.typography.bodyLarge)
            Text(
                text = "Off: downloads wait for Wi-Fi",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Switch(
            checked = state.cache.downloadOnMetered,
            onCheckedChange = viewModel::setDownloadOnMetered,
        )
    }
    OutlinedButton(
        onClick = { confirmingClear = true },
        enabled = !state.clearing,
    ) {
        if (state.clearing) {
            CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
            Spacer(Modifier.width(8.dp))
        }
        Text("Clear cache")
    }
    state.cacheMessage?.let { message ->
        Text(
            text = message,
            style = MaterialTheme.typography.bodyMedium,
            color = if (state.cacheFailed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary,
        )
    }
    if (confirmingClear) {
        ConfirmDialog(
            title = "Clear cache?",
            message = "Removes cached songs that aren't downloaded. Downloaded albums and playlists stay.",
            confirmLabel = "Clear",
            onConfirm = {
                confirmingClear = false
                viewModel.clearCache()
            },
            onDismiss = { confirmingClear = false },
        )
    }
}

/** "Using 1.2 GB of 4 GB · 85 songs offline (412 MB downloaded)" for the Downloads section. */
private fun cacheUsageLine(usage: CacheUsageInfo): String =
    "Using ${formatBytes(usage.usedBytes)} of ${formatBytes(usage.limitBytes)} · " +
        "${usage.completeTracks} songs offline (${formatBytes(usage.pinnedBytes)} downloaded)"

/** A dropdown that shows the label of the option equal to [selected]. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun <T> ChoiceDropdown(
    label: String,
    options: List<Pair<T, String>>,
    selected: T,
    onSelect: (T) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { expanded = it }) {
        OutlinedTextField(
            value = options.firstOrNull { it.first == selected }?.second.orEmpty(),
            onValueChange = {},
            readOnly = true,
            label = { Text(label) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable, enabled = true)
                .fillMaxWidth(),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            options.forEach { (value, text) ->
                DropdownMenuItem(
                    text = { Text(text) },
                    onClick = {
                        expanded = false
                        onSelect(value)
                    },
                )
            }
        }
    }
}

/** One line describing an open output stream, for the Output section. */
private fun outputLine(info: OutputInfo): String {
    val format = info.sampleFormat.label()
    val mode = if (info.exclusive) "Exclusive" else "Shared"
    val lowLatency = if (info.lowLatency) ", low latency" else ""
    return "${backendLabel(info.backend)} · ${formatKhz(info.sampleRate)} kHz · ${info.channels} ch · " +
        "$format · $mode$lowLatency · device ${info.deviceId} · burst ${info.framesPerBurst} · " +
        "buffer ${info.bufferFrames} · xruns ${info.xruns}"
}

@Composable
private fun UploadsSection(onUploads: () -> Unit) {
    SectionTitle("Uploads")
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onUploads)
            .padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Column(Modifier.weight(1f)) {
            Text(text = "Upload music", style = MaterialTheme.typography.bodyLarge)
            Text(
                text = "Copy songs from this phone to your bucket",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Icon(
            Icons.AutoMirrored.Rounded.KeyboardArrowRight,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
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
        text = "Audio output: AAudio",
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

/** Display name for the core's output backend id ("aaudio" → "AAudio"). */
private fun backendLabel(backend: String): String = when (backend) {
    "aaudio" -> "AAudio"
    else -> backend
}

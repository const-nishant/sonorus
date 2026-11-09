package dev.constnishant.sonorus.ui.settings

import android.content.Context
import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.requiredSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CloudUpload
import androidx.compose.material.icons.outlined.DownloadForOffline
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.outlined.LibraryMusic
import androidx.compose.material.icons.outlined.Storage
import androidx.compose.material.icons.rounded.GraphicEq
import androidx.compose.material.icons.rounded.Visibility
import androidx.compose.material.icons.rounded.VisibilityOff
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModelProvider.AndroidViewModelFactory.Companion.APPLICATION_KEY
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.constnishant.sonorus.BuildConfig
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.CacheUsageInfo
import dev.constnishant.sonorus.core.OutputInfo
import dev.constnishant.sonorus.core.OutputSampleFormat
import dev.constnishant.sonorus.core.ScanProgressInfo
import dev.constnishant.sonorus.data.GIB
import dev.constnishant.sonorus.data.ScanUi
import dev.constnishant.sonorus.data.repository
import dev.constnishant.sonorus.ui.components.ConfirmDialog
import dev.constnishant.sonorus.ui.components.albumsText
import dev.constnishant.sonorus.ui.components.formatBytes
import dev.constnishant.sonorus.ui.components.formatKhz
import dev.constnishant.sonorus.ui.components.label
import dev.constnishant.sonorus.ui.components.tracksText
import dev.constnishant.sonorus.ui.theme.Dimens
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** The pages of the settings screen. [ROOT] lists the others. */
private enum class SettingsPage { ROOT, BUCKET, LIBRARY, PLAYBACK, DOWNLOADS, ABOUT }

/**
 * Settings: a short list of categories, each opening its own page. Back on a page returns to the
 * list; back on the list leaves via [onBack]. [onUploads] opens the uploads screen.
 */
@Composable
fun SettingsScreen(onBack: () -> Unit, onUploads: () -> Unit, modifier: Modifier = Modifier) {
    val repository = LocalContext.current.repository
    val viewModel: SettingsViewModel = viewModel { SettingsViewModel(checkNotNull(this[APPLICATION_KEY]), repository) }
    val state by viewModel.state.collectAsStateWithLifecycle()
    val scan by viewModel.scan.collectAsStateWithLifecycle()
    var page by rememberSaveable { mutableStateOf(SettingsPage.ROOT) }
    val toRoot = { page = SettingsPage.ROOT }

    BackHandler(enabled = page != SettingsPage.ROOT, onBack = toRoot)

    AnimatedContent(
        targetState = page,
        transitionSpec = {
            val forward = targetState.ordinal > initialState.ordinal
            val sign = if (forward) 1 else -1
            (slideInHorizontally { width -> sign * width / 8 } + fadeIn()) togetherWith
                (slideOutHorizontally { width -> -sign * width / 8 } + fadeOut())
        },
        modifier = modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing),
        label = "settings-page",
    ) { current ->
        when (current) {
            SettingsPage.ROOT -> RootPage(
                state = state,
                scan = scan,
                onBack = onBack,
                onOpen = { page = it },
                onUploads = onUploads,
            )
            SettingsPage.BUCKET -> Page(stringResource(R.string.settings_section_bucket), toRoot) {
                BucketPage(state, viewModel)
            }
            SettingsPage.LIBRARY -> Page(stringResource(R.string.settings_section_library), toRoot) {
                LibraryPage(state, scan, viewModel)
            }
            SettingsPage.PLAYBACK -> Page(stringResource(R.string.settings_section_playback), toRoot) {
                PlaybackPage(state, viewModel)
            }
            SettingsPage.DOWNLOADS -> Page(stringResource(R.string.settings_section_downloads), toRoot) {
                DownloadsPage(state, viewModel)
            }
            SettingsPage.ABOUT -> Page(stringResource(R.string.settings_section_about), toRoot) {
                AboutPage()
            }
        }
    }
}

/** A scrolling settings page with its top bar. */
@Composable
private fun Page(title: String, onBack: () -> Unit, content: @Composable ColumnScope.() -> Unit) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(bottom = Dimens.SectionSpacing),
    ) {
        SettingsTopBar(title = title, onBack = onBack)
        content()
    }
}

@Composable
private fun RootPage(
    state: SettingsUiState,
    scan: ScanUi,
    onBack: () -> Unit,
    onOpen: (SettingsPage) -> Unit,
    onUploads: () -> Unit,
) {
    val context = LocalContext.current
    Page(stringResource(R.string.settings_title), onBack) {
        SettingsRow(
            title = stringResource(R.string.settings_section_bucket),
            summary = if (state.hasSettings) bucketSummary(state.form) else stringResource(R.string.settings_not_connected),
            icon = Icons.Outlined.Storage,
            onClick = { onOpen(SettingsPage.BUCKET) },
        )
        SettingsRow(
            title = stringResource(R.string.settings_section_library),
            summary = librarySummary(state, scan),
            icon = Icons.Outlined.LibraryMusic,
            onClick = { onOpen(SettingsPage.LIBRARY) },
        )
        SettingsRow(
            title = stringResource(R.string.settings_section_playback),
            summary = listOfNotNull(formatLabel(state.output.sampleFormat), state.dacName).joinToString(" · "),
            icon = Icons.Rounded.GraphicEq,
            onClick = { onOpen(SettingsPage.PLAYBACK) },
        )
        SettingsRow(
            title = stringResource(R.string.settings_section_downloads),
            summary = state.cacheUsage?.let {
                stringResource(R.string.cache_using, formatBytes(context, it.usedBytes), formatBytes(context, it.limitBytes))
            },
            icon = Icons.Outlined.DownloadForOffline,
            onClick = { onOpen(SettingsPage.DOWNLOADS) },
        )
        SettingsRow(
            title = stringResource(R.string.upload_music),
            summary = stringResource(R.string.upload_music_hint),
            icon = Icons.Outlined.CloudUpload,
            onClick = onUploads,
        )
        SettingsRow(
            title = stringResource(R.string.settings_section_about),
            summary = stringResource(R.string.about_version, BuildConfig.VERSION_NAME),
            icon = Icons.Outlined.Info,
            onClick = { onOpen(SettingsPage.ABOUT) },
        )
    }
}

/** "bucket/prefix", or just "bucket" without a prefix. */
private fun bucketSummary(form: BucketForm): String {
    val prefix = form.prefix.trim('/')
    return if (prefix.isEmpty()) form.bucket else form.bucket + "/" + prefix
}

/** Scan progress while a scan runs, otherwise the track and album counts. */
@Composable
private fun librarySummary(state: SettingsUiState, scan: ScanUi): String {
    val progress = scan.progress
    val stats = state.stats
    return when {
        scan.running && progress != null -> progressLine(LocalContext.current, progress)
        stats != null -> tracksText(stats.tracks.toInt()) + " · " + albumsText(stats.albums.toInt())
        state.statsError != null -> state.statsError
        else -> stringResource(R.string.settings_stats_loading)
    }
}

/** The display label of a sample format. */
@Composable
private fun formatLabel(format: OutputSampleFormat): String = format.label(LocalContext.current)

@Composable
private fun BucketPage(state: SettingsUiState, viewModel: SettingsViewModel) {
    val form = state.form
    var showSecret by rememberSaveable { mutableStateOf(false) }
    var confirmingDisconnect by remember { mutableStateOf(false) }

    SettingsChoice(
        title = stringResource(R.string.settings_provider),
        options = BucketProvider.entries.map { it to stringResource(it.labelRes) },
        selected = form.provider,
        onSelect = viewModel::selectProvider,
    )
    Fields {
        SettingsField(
            value = form.endpoint,
            onValueChange = viewModel::setEndpoint,
            label = stringResource(R.string.settings_endpoint),
            keyboardType = KeyboardType.Uri,
        )
        SettingsField(
            value = form.region,
            onValueChange = { value -> viewModel.updateForm { copy(region = value) } },
            label = stringResource(R.string.settings_region),
        )
        SettingsField(
            value = form.bucket,
            onValueChange = { value -> viewModel.updateForm { copy(bucket = value) } },
            label = stringResource(R.string.settings_bucket_name),
        )
    }

    SettingsGroupLabel(stringResource(R.string.settings_group_credentials))
    Fields {
        SettingsField(
            value = form.accessKeyId,
            onValueChange = { value -> viewModel.updateForm { copy(accessKeyId = value) } },
            label = stringResource(R.string.settings_access_key),
        )
        OutlinedTextField(
            value = form.secretAccessKey,
            onValueChange = { value -> viewModel.updateForm { copy(secretAccessKey = value) } },
            label = { Text(stringResource(R.string.settings_secret_key)) },
            singleLine = true,
            visualTransformation = if (showSecret) VisualTransformation.None else PasswordVisualTransformation(),
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false),
            trailingIcon = {
                IconButton(onClick = { showSecret = !showSecret }) {
                    Icon(
                        imageVector = if (showSecret) Icons.Rounded.VisibilityOff else Icons.Rounded.Visibility,
                        contentDescription = if (showSecret) {
                            stringResource(R.string.cd_hide_secret)
                        } else {
                            stringResource(R.string.cd_show_secret)
                        },
                    )
                }
            },
            modifier = Modifier.fillMaxWidth(),
        )
    }

    SettingsGroupLabel(stringResource(R.string.settings_group_options))
    SettingsSwitch(
        title = stringResource(R.string.settings_path_style),
        summary = stringResource(R.string.settings_path_style_hint),
        checked = form.pathStyle,
        onCheckedChange = { value -> viewModel.updateForm { copy(pathStyle = value) } },
    )
    Fields {
        SettingsField(
            value = form.prefix,
            onValueChange = { value -> viewModel.updateForm { copy(prefix = value) } },
            label = stringResource(R.string.settings_prefix),
        )
    }

    Spacer(Modifier.height(Dimens.ItemSpacing))
    Button(
        onClick = viewModel::saveAndConnect,
        enabled = !state.saving,
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = Dimens.ScreenPadding)
            .heightIn(min = 48.dp),
    ) {
        if (state.saving) {
            CircularProgressIndicator(
                modifier = Modifier.size(18.dp),
                strokeWidth = 2.dp,
                color = MaterialTheme.colorScheme.onPrimary,
            )
            Spacer(Modifier.width(8.dp))
        }
        Text(stringResource(R.string.settings_save_connect))
    }
    SettingsStatus(state.bucketMessage, state.bucketFailed)
    if (state.hasSettings) {
        SettingsRow(
            title = stringResource(R.string.settings_disconnect),
            titleColor = MaterialTheme.colorScheme.error,
            onClick = { confirmingDisconnect = true },
        )
    }
    if (confirmingDisconnect) {
        ConfirmDialog(
            title = stringResource(R.string.settings_disconnect_title),
            message = stringResource(R.string.settings_disconnect_message),
            confirmLabel = stringResource(R.string.settings_disconnect),
            onConfirm = {
                confirmingDisconnect = false
                viewModel.disconnect()
            },
            onDismiss = { confirmingDisconnect = false },
        )
    }
}

/** Text fields stacked with the page padding. */
@Composable
private fun Fields(content: @Composable ColumnScope.() -> Unit) {
    Column(
        modifier = Modifier.padding(horizontal = Dimens.ScreenPadding, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(Dimens.ItemSpacing),
        content = content,
    )
}

@Composable
private fun SettingsField(
    value: String,
    onValueChange: (String) -> Unit,
    label: String,
    keyboardType: KeyboardType = KeyboardType.Text,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = { Text(label) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = keyboardType, autoCorrectEnabled = false),
        modifier = Modifier.fillMaxWidth(),
    )
}

@Composable
private fun LibraryPage(state: SettingsUiState, scan: ScanUi, viewModel: SettingsViewModel) {
    val context = LocalContext.current
    val stats = state.stats
    SettingsNote(
        text = when {
            stats != null ->
                tracksText(stats.tracks.toInt()) + " · " + albumsText(stats.albums.toInt()) + " · " +
                    stringResource(R.string.size_gb, "%.2f".format(stats.totalBytes.toDouble() / 1e9)) + " · " +
                    stringResource(R.string.duration_hours, "%.1f".format(stats.totalDurationMs.toDouble() / 3_600_000.0))
            state.statsError != null -> state.statsError
            else -> stringResource(R.string.settings_stats_loading)
        },
        color = if (state.statsError != null && stats == null) {
            MaterialTheme.colorScheme.error
        } else {
            MaterialTheme.colorScheme.onSurfaceVariant
        },
    )

    val progress = scan.progress
    SettingsRow(
        title = stringResource(R.string.settings_scan),
        summary = when {
            scan.running && progress != null -> progressLine(context, progress)
            scan.running -> null
            else -> scan.lastSummary?.let { stringResource(R.string.settings_last_scan, progressLine(context, it.progress)) }
        },
        summaryLines = 2,
        enabled = !scan.running,
        onClick = viewModel::startScan,
        trailing = if (scan.running) {
            { RowSpinner() }
        } else {
            null
        },
    )
    if (scan.running) {
        val bar = Modifier
            .fillMaxWidth()
            .padding(horizontal = Dimens.ScreenPadding)
        if (progress != null) {
            LinearProgressIndicator(progress = { progressFraction(progress) }, modifier = bar)
        } else {
            LinearProgressIndicator(modifier = bar)
        }
    }
    SettingsStatus(scan.error, failed = true)

    SettingsRow(
        title = stringResource(R.string.sync_button),
        enabled = !state.syncing,
        onClick = viewModel::syncUserData,
        trailing = if (state.syncing) {
            { RowSpinner() }
        } else {
            null
        },
    )
    SettingsStatus(state.syncMessage, state.syncFailed)
}

@Composable
private fun PlaybackPage(state: SettingsUiState, viewModel: SettingsViewModel) {
    val formats = listOf(OutputSampleFormat.FLOAT, OutputSampleFormat.INT32, OutputSampleFormat.INT24_PACKED)
        .map { it to formatLabel(it) }
    SettingsChoice(
        title = stringResource(R.string.output_format),
        options = formats,
        selected = state.output.sampleFormat,
        onSelect = viewModel::setSampleFormat,
    )
    SettingsNote(stringResource(R.string.output_follows_volume))
    SettingsSwitch(
        title = stringResource(R.string.output_exclusive_title),
        summary = stringResource(R.string.output_exclusive_message),
        checked = state.output.exclusive,
        onCheckedChange = viewModel::setExclusive,
    )
    SettingsSwitch(
        title = stringResource(R.string.output_bitperfect_title),
        summary = stringResource(R.string.output_bitperfect_message),
        checked = state.bitPerfect,
        onCheckedChange = viewModel::setBitPerfect,
    )
    SettingsNote(stringResource(R.string.output_changing_stops))

    SettingsGroupLabel(stringResource(R.string.output_current))
    SettingsRow(
        title = stringResource(R.string.output_usb_dac),
        summary = state.dacName ?: stringResource(R.string.output_no_dac),
    )
    SettingsRow(
        title = stringResource(R.string.output_stream),
        summary = state.outputInfo?.let { outputLine(LocalContext.current, it) } ?: stringResource(R.string.output_no_stream),
        summaryLines = 4,
    )
}

/** Cache size choices: the limit in bytes, labelled in whole gigabytes. */
private fun cacheLimitOptions(context: Context): List<Pair<ULong, String>> =
    listOf(1L, 2L, 4L, 8L, 16L).map { gb ->
        (gb * GIB).toULong() to context.getString(R.string.cache_size_gb, gb)
    }

/** How long unpinned cached songs are kept; null keeps them forever. */
private fun keepForOptions(context: Context): List<Pair<UInt?, String>> = listOf(
    7u to context.resources.getQuantityString(R.plurals.keep_days, 7, 7),
    30u to context.resources.getQuantityString(R.plurals.keep_days, 30, 30),
    90u to context.resources.getQuantityString(R.plurals.keep_days, 90, 90),
    null to context.getString(R.string.cache_keep_forever),
)

@Composable
private fun DownloadsPage(state: SettingsUiState, viewModel: SettingsViewModel) {
    val context = LocalContext.current
    var confirmingClear by remember { mutableStateOf(false) }
    val usage = state.cacheUsage
    if (usage != null) {
        SettingsNote(cacheUsageLine(context, usage))
    } else if (!state.cacheFailed) {
        SettingsNote(stringResource(R.string.cache_loading))
    }
    SettingsChoice(
        title = stringResource(R.string.cache_size_label),
        options = cacheLimitOptions(context),
        selected = state.cache.limitBytes,
        onSelect = viewModel::setCacheLimit,
    )
    SettingsChoice(
        title = stringResource(R.string.cache_keep_label),
        options = keepForOptions(context),
        selected = state.cache.ttlDays,
        onSelect = viewModel::setKeepDays,
    )
    SettingsSwitch(
        title = stringResource(R.string.cache_mobile_data),
        summary = stringResource(R.string.cache_mobile_data_off),
        checked = state.cache.downloadOnMetered,
        onCheckedChange = viewModel::setDownloadOnMetered,
    )
    SettingsRow(
        title = stringResource(R.string.cache_clear),
        enabled = !state.clearing,
        onClick = { confirmingClear = true },
        trailing = if (state.clearing) {
            { RowSpinner() }
        } else {
            null
        },
    )
    SettingsStatus(state.cacheMessage, state.cacheFailed)
    if (confirmingClear) {
        ConfirmDialog(
            title = stringResource(R.string.cache_clear_title),
            message = stringResource(R.string.cache_clear_message),
            confirmLabel = stringResource(R.string.action_clear),
            onConfirm = {
                confirmingClear = false
                viewModel.clearCache()
            },
            onDismiss = { confirmingClear = false },
        )
    }
}

/** "Using 1.2 GB of 4 GB · 85 songs offline (412 MB downloaded)" for the Downloads page. */
private fun cacheUsageLine(context: Context, usage: CacheUsageInfo): String =
    context.getString(
        R.string.cache_using,
        formatBytes(context, usage.usedBytes),
        formatBytes(context, usage.limitBytes),
    ) + " · " + context.resources.getQuantityString(
        R.plurals.songs_offline,
        usage.completeTracks.toInt(),
        usage.completeTracks.toInt(),
        formatBytes(context, usage.pinnedBytes),
    )

/** One line describing an open output stream, for the Playback page. */
private fun outputLine(context: Context, info: OutputInfo): String {
    val format = info.sampleFormat.label(context)
    val mode = if (info.exclusive) {
        context.getString(R.string.signal_mode_exclusive)
    } else {
        context.getString(R.string.signal_mode_shared)
    }
    val lowLatency = if (info.lowLatency) ", " + context.getString(R.string.output_low_latency) else ""
    return backendLabel(context, info.backend) +
        " · " + context.getString(R.string.khz_unit, formatKhz(info.sampleRate)) +
        " · " + context.getString(R.string.output_channels, info.channels.toInt()) +
        " · " + format +
        " · " + mode + lowLatency +
        " · " + context.getString(R.string.output_device, info.deviceId) +
        " · " + context.getString(R.string.output_burst, info.framesPerBurst.toInt()) +
        " · " + context.getString(R.string.output_buffer, info.bufferFrames.toInt()) +
        " · " + context.getString(R.string.output_xruns, info.xruns)
}

@Composable
private fun AboutPage() {
    var showLicenses by rememberSaveable { mutableStateOf(false) }
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = Dimens.SectionSpacing),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Box(
            modifier = Modifier
                .size(72.dp)
                .clip(MaterialTheme.shapes.large)
                .background(AppIconBackground),
            contentAlignment = Alignment.Center,
        ) {
            // The foreground layer is drawn larger than the box so the mark fills it; the box clips the rest.
            Image(
                painter = painterResource(R.drawable.ic_launcher_foreground),
                contentDescription = null,
                modifier = Modifier.requiredSize(104.dp),
            )
        }
        Spacer(Modifier.height(Dimens.ItemSpacing))
        Text(text = stringResource(R.string.app_name), style = MaterialTheme.typography.titleLarge)
        Text(
            text = stringResource(R.string.about_version, BuildConfig.VERSION_NAME),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            text = stringResource(R.string.about_audio_output),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    SettingsRow(
        title = stringResource(R.string.about_licenses),
        onClick = { showLicenses = true },
    )
    if (showLicenses) {
        LicensesDialog(onDismiss = { showLicenses = false })
    }
}

/** Dark launcher background, used behind the app icon in About. */
private val AppIconBackground = Color(0xFF0E1014)

/** Shows the bundled Inter license (assets/licenses/inter-ofl.txt) in a scrollable dialog. */
@Composable
private fun LicensesDialog(onDismiss: () -> Unit) {
    val context = LocalContext.current
    val unavailable = stringResource(R.string.licenses_unavailable)
    val licenseText by produceState(initialValue = stringResource(R.string.licenses_loading)) {
        value = withContext(Dispatchers.IO) {
            runCatching {
                context.assets.open("licenses/inter-ofl.txt").bufferedReader().use { it.readText() }
            }.getOrElse { unavailable }
        }
    }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.about_licenses)) },
        text = {
            Column(
                modifier = Modifier
                    .heightIn(max = 400.dp)
                    .verticalScroll(rememberScrollState()),
            ) {
                Text(text = licenseText, style = MaterialTheme.typography.bodySmall)
            }
        },
        confirmButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.action_close)) } },
    )
}

/** Fraction of a scan done, 0 when the total is not known yet. */
private fun progressFraction(p: ScanProgressInfo): Float =
    if (p.total == 0u) 0f else p.done.toFloat() / p.total.toFloat()

/** "done/total · +added ~updated −removed !failed". */
private fun progressLine(context: Context, p: ScanProgressInfo): String {
    val changes = listOf(
        p.added to R.string.scan_added,
        p.updated to R.string.scan_updated,
        p.removed to R.string.scan_removed,
        p.failed to R.string.scan_failed,
    ).filter { (count, _) -> count > 0u }
        .joinToString(", ") { (count, label) -> context.getString(label, count) }
    val files = context.resources.getQuantityString(
        R.plurals.scan_progress_files,
        p.total.toInt(),
        p.done.toInt(),
        p.total.toInt(),
    )
    val tail = if (changes.isEmpty()) context.getString(R.string.scan_no_changes) else changes
    return "$files · $tail"
}

/** Display name for the core's output backend id ("aaudio" → "AAudio"). */
private fun backendLabel(context: Context, backend: String): String = when (backend) {
    "aaudio" -> context.getString(R.string.backend_aaudio)
    else -> backend
}

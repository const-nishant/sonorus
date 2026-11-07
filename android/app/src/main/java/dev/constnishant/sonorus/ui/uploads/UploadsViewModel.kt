package dev.constnishant.sonorus.ui.uploads

import android.app.Application
import android.content.Context
import android.net.Uri
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.UploadJobInfo
import dev.constnishant.sonorus.data.PrepareResult
import dev.constnishant.sonorus.data.SonorusRepository
import dev.constnishant.sonorus.data.UploadEvent
import dev.constnishant.sonorus.data.userMessage
import dev.constnishant.sonorus.ui.screens.LoadState
import dev.constnishant.sonorus.ui.screens.refresh
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * State and actions for the uploads screen: the upload queue, the progress of running jobs, and
 * adding files. The queue reloads on start, after actions, and when a run ends.
 */
class UploadsViewModel(
    application: Application,
    private val repository: SonorusRepository,
) : AndroidViewModel(application) {
    private val context: Application = application
    private val _state = MutableStateFlow<LoadState<List<UploadJobInfo>>>(LoadState.Loading)

    /** The upload jobs, newest first, or the reason they could not be read. */
    val state: StateFlow<LoadState<List<UploadJobInfo>>> = _state.asStateFlow()

    /** Latest bytes stored and total size for each running job. */
    val progress: StateFlow<Map<Long, Pair<ULong, ULong>>> = repository.events.uploadProgress

    private val _preparing = MutableStateFlow(false)

    /** True while the picked files are being read and added. */
    val preparing: StateFlow<Boolean> = _preparing.asStateFlow()

    private val _prefix = MutableStateFlow("")

    /** The bucket's folder prefix. Object keys start with it; the screen does not show it. */
    val prefix: StateFlow<String> = _prefix.asStateFlow()

    private val _messages = MutableSharedFlow<String>(extraBufferCapacity = 4)

    /** One-shot messages for a snackbar. */
    val messages: SharedFlow<String> = _messages.asSharedFlow()

    init {
        viewModelScope.launch {
            _prefix.value = withContext(Dispatchers.IO) { repository.settings.load()?.prefix.orEmpty() }
        }
        viewModelScope.launch {
            repository.events.uploadEvents.filter { it !is UploadEvent.Progress }.collect { reload() }
        }
        reload()
    }

    /** Reloads the upload queue. */
    fun reload() {
        viewModelScope.launch {
            _state.refresh(context) { repository.uploads() }
        }
    }

    /**
     * Adds [uris] to the queue and shows a summary. [onQueued] runs when at least one file was
     * queued, so the caller can start the upload service.
     */
    fun addFiles(uris: List<Uri>, onQueued: () -> Unit) {
        viewModelScope.launch {
            _preparing.value = true
            val result = try {
                repository.prepareUploads(uris)
            } finally {
                _preparing.value = false
            }
            _messages.tryEmit(result.summary(context))
            reload()
            if (result.queued > 0) onQueued()
        }
    }

    /** Cancels job [id]. The queue reloads once the core has applied it. */
    fun cancel(id: Long) {
        repository.cancelUpload(id)
    }

    /** Deletes the finished jobs (done, duplicate or cancelled) and reloads the queue. */
    fun clearFinished() {
        viewModelScope.launch {
            try {
                repository.clearFinishedUploads()
            } catch (e: CoreException) {
                _messages.tryEmit(e.userMessage(context, R.string.error_clear_list))
            }
            reload()
        }
    }
}

/** A one-line summary such as "3 queued, 1 already uploaded, 1 skipped: <reason>". */
private fun PrepareResult.summary(context: Context): String {
    val parts = buildList {
        if (queued > 0) {
            add(context.resources.getQuantityString(R.plurals.upload_summary_queued, queued, queued))
        }
        if (duplicates > 0) {
            add(context.resources.getQuantityString(R.plurals.upload_summary_duplicates, duplicates, duplicates))
        }
        if (skipped.isNotEmpty()) {
            add(
                context.resources.getQuantityString(
                    R.plurals.upload_summary_skipped,
                    skipped.size,
                    skipped.size,
                    skipped.distinct().joinToString(" "),
                ),
            )
        }
    }
    return parts.joinToString(", ").ifEmpty { context.getString(R.string.upload_summary_none) }
}

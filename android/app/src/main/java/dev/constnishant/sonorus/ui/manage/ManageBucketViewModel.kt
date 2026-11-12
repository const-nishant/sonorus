package dev.constnishant.sonorus.ui.manage

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.BucketEntryInfo
import dev.constnishant.sonorus.core.BucketFolderInfo
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.FailureInfo
import dev.constnishant.sonorus.data.SonorusRepository
import dev.constnishant.sonorus.data.userMessage
import dev.constnishant.sonorus.ui.screens.LoadState
import dev.constnishant.sonorus.ui.screens.parentPath
import dev.constnishant.sonorus.ui.screens.refresh
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** A long operation in progress. The screen shows it in a progress dialog. */
sealed interface ManageBusy {
    /** Renaming one file. */
    data object Renaming : ManageBusy

    /** Moving [count] selected entries. */
    data class Moving(val count: Int) : ManageBusy

    /** Deleting [count] selected entries. */
    data class Deleting(val count: Int) : ManageBusy
}

/** A dialog the screen shows, with the entries it acts on. */
sealed interface ManageDialog {
    /** Rename one file. */
    data class Rename(val entry: BucketEntryInfo) : ManageDialog

    /** Pick a folder to move [entries] into. */
    data class Move(val entries: List<BucketEntryInfo>) : ManageDialog

    /** Confirm deleting [entries]. */
    data class Delete(val entries: List<BucketEntryInfo>) : ManageDialog

    /** Edit the tags of the track [trackId]. */
    data class Tags(val trackId: Long) : ManageDialog

    /** The objects of a partly failed operation, with the reason for each. */
    data class Failures(val failures: List<FailureInfo>) : ManageDialog
}

/** The outcome of an operation: a snackbar [message], and the objects that failed. */
data class ManageResult(val message: String, val failures: List<FailureInfo>)

/**
 * Backs the Manage bucket screen: browses one bucket folder, keeps the selection, the open dialog
 * and the file sheet, and runs the bucket changes. Every change reloads the folder and notifies
 * library observers, then posts its [ManageResult] to [results].
 */
class ManageBucketViewModel(
    application: Application,
    private val repository: SonorusRepository,
    initialPath: String,
) : AndroidViewModel(application) {
    private val context: Application = application
    private val _path = MutableStateFlow(normalizePath(initialPath))

    /** The folder being viewed: `""` for the root, else a path ending with `/`. */
    val path: StateFlow<String> = _path.asStateFlow()

    private val _state = MutableStateFlow<LoadState<BucketFolderInfo>>(LoadState.Loading)

    /** The folders and files of [path], or the reason they could not be loaded. */
    val state: StateFlow<LoadState<BucketFolderInfo>> = _state.asStateFlow()

    private val _refreshing = MutableStateFlow(false)

    /** True while the folder is being read, to show the pull-to-refresh indicator. */
    val refreshing: StateFlow<Boolean> = _refreshing.asStateFlow()

    private val _selection = MutableStateFlow<Set<String>>(emptySet())

    /** The keys of the selected entries. Empty when selection mode is off. */
    val selection: StateFlow<Set<String>> = _selection.asStateFlow()

    private val _dialog = MutableStateFlow<ManageDialog?>(null)

    /** The dialog to show, or null. */
    val dialog: StateFlow<ManageDialog?> = _dialog.asStateFlow()

    private val _sheet = MutableStateFlow<BucketEntryInfo?>(null)

    /** The file whose details sheet is open, or null. */
    val sheet: StateFlow<BucketEntryInfo?> = _sheet.asStateFlow()

    private val _busy = MutableStateFlow<ManageBusy?>(null)

    /** The long operation running, or null. */
    val busy: StateFlow<ManageBusy?> = _busy.asStateFlow()

    private val _results = MutableSharedFlow<ManageResult>(extraBufferCapacity = 4)

    /** One message per finished operation, for the snackbar. */
    val results: SharedFlow<ManageResult> = _results.asSharedFlow()

    private var loadJob: Job? = null

    init {
        load()
    }

    /** Opens the folder at [folder] (`""` for the root). Clears the selection. */
    fun open(folder: String) {
        _selection.value = emptySet()
        _sheet.value = null
        _state.value = LoadState.Loading
        _path.value = normalizePath(folder)
        load()
    }

    /** Goes up one folder. Does nothing at the root. */
    fun up() {
        open(parentPath(_path.value))
    }

    /** Reloads the current folder. */
    fun refresh() {
        load()
    }

    /** The entries of the loaded folder, folders first. Empty while nothing is loaded. */
    fun entries(): List<BucketEntryInfo> =
        (_state.value as? LoadState.Ready)?.value?.let { it.folders + it.files }.orEmpty()

    /** Taps an entry: toggles it in selection mode, opens a folder, or opens a file's sheet. */
    fun tap(entry: BucketEntryInfo) {
        when {
            _selection.value.isNotEmpty() -> toggle(entry)
            entry.isFolder -> open(entry.path)
            else -> _sheet.value = entry
        }
    }

    /** Long-presses an entry: starts selection mode with it, or toggles it when already on. */
    fun longPress(entry: BucketEntryInfo) {
        _sheet.value = null
        toggle(entry)
    }

    /** Adds or removes [entry] from the selection. */
    fun toggle(entry: BucketEntryInfo) {
        _selection.update { current ->
            if (entry.key in current) current - entry.key else current + entry.key
        }
    }

    /** Selects every entry in the folder. */
    fun selectAll() {
        _selection.value = entries().map { it.key }.toSet()
    }

    /** Leaves selection mode. */
    fun clearSelection() {
        _selection.value = emptySet()
    }

    /** Closes the file sheet. */
    fun dismissSheet() {
        _sheet.value = null
    }

    /** Closes the dialog. */
    fun dismissDialog() {
        _dialog.value = null
    }

    /** Shows [dialog]. */
    fun showDialog(dialog: ManageDialog) {
        _dialog.value = dialog
    }

    /** Closes the file sheet and shows [dialog] in its place. */
    fun showFromSheet(dialog: ManageDialog) {
        _sheet.value = null
        _dialog.value = dialog
    }

    /** Plays the track of [entry] from the sheet, and closes the sheet. */
    fun playFromSheet(entry: BucketEntryInfo) {
        _sheet.value = null
        val id = entry.trackId ?: return
        repository.command { core -> core.playTracks(listOf(id), 0u) }
    }

    /** Renames [entry] to [newName]. The name is a file name in the same folder. */
    fun rename(entry: BucketEntryInfo, newName: String) {
        _dialog.value = null
        operation(ManageBusy.Renaming) {
            repository.renameObject(entry.key, newName)
            ManageResult(context.getString(R.string.manage_result_renamed, newName), emptyList())
        }
    }

    /** Moves the entries into the bucket folder [folder]. */
    fun move(entries: List<BucketEntryInfo>, folder: String) {
        _dialog.value = null
        val keys = entries.map { it.key }
        operation(ManageBusy.Moving(entries.size)) {
            val report = repository.moveObjects(keys, folder)
            val moved = report.moved.toInt()
            if (report.failed.isEmpty()) {
                ManageResult(context.resources.getQuantityString(R.plurals.manage_result_moved, moved, moved), emptyList())
            } else {
                val total = moved + report.failed.size
                ManageResult(
                    context.resources.getQuantityString(R.plurals.manage_result_partial_move, total, moved, total, report.failed.size),
                    report.failed,
                )
            }
        }
    }

    /** Deletes the entries: files in one call, and each folder with everything in it. */
    fun delete(entries: List<BucketEntryInfo>) {
        _dialog.value = null
        val files = entries.filter { !it.isFolder }.map { it.key }
        val folders = entries.filter { it.isFolder }.map { it.path }
        operation(ManageBusy.Deleting(entries.size)) {
            var deleted = 0
            val failures = mutableListOf<FailureInfo>()
            if (files.isNotEmpty()) {
                val report = repository.deleteObjects(files)
                deleted += report.deleted.toInt()
                failures += report.failed
            }
            for (folder in folders) {
                val report = repository.deleteFolder(folder)
                deleted += report.deleted.toInt()
                failures += report.failed
            }
            if (failures.isEmpty()) {
                ManageResult(context.resources.getQuantityString(R.plurals.manage_result_deleted, deleted, deleted), emptyList())
            } else {
                val total = deleted + failures.size
                ManageResult(
                    context.resources.getQuantityString(R.plurals.manage_result_partial_delete, total, deleted, total, failures.size),
                    failures,
                )
            }
        }
    }

    /**
     * Runs one bucket change. Shows [busy] while it runs, then clears the selection, notifies the
     * library, reloads the folder and posts the result. A [CoreException] becomes a failure
     * message.
     */
    private fun operation(busy: ManageBusy, block: suspend () -> ManageResult) {
        viewModelScope.launch {
            _busy.value = busy
            val result = try {
                block()
            } catch (e: CoreException) {
                ManageResult(e.userMessage(context, R.string.error_manage), emptyList())
            }
            _busy.value = null
            _selection.value = emptySet()
            repository.notifyLibraryChanged()
            load()
            _results.emit(result)
        }
    }

    /** Reads the current folder. A newer load replaces an older one still running. */
    private fun load() {
        loadJob?.cancel()
        loadJob = viewModelScope.launch {
            _refreshing.value = true
            try {
                _state.refresh(context) { repository.browseBucket(_path.value) }
            } finally {
                _refreshing.value = false
            }
        }
    }
}

/** A folder path with the trailing `/` the core expects, or `""` for the root. */
internal fun normalizePath(path: String): String =
    if (path.isEmpty() || path.endsWith('/')) path else "$path/"

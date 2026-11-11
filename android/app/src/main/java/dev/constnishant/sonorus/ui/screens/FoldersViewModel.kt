package dev.constnishant.sonorus.ui.screens

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.core.CatalogueFolderInfo
import dev.constnishant.sonorus.data.SonorusRepository
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch

/**
 * Backs the Folders tab: the folder being viewed and its listing. The path is `""` for the root
 * and otherwise ends with `/`, as the core expects. It lives in the ViewModel, so it survives
 * configuration changes. The listing reloads whenever the path or the catalogue changes.
 */
class FoldersViewModel(
    application: Application,
    private val repository: SonorusRepository,
) : AndroidViewModel(application) {
    private val context: Application = application
    private val _path = MutableStateFlow("")

    /** The folder being viewed: `""` for the root, else a path ending with `/`. */
    val path: StateFlow<String> = _path.asStateFlow()

    private val _state = MutableStateFlow<LoadState<CatalogueFolderInfo>>(LoadState.Loading)

    /** The folders and tracks of [path], or the reason they could not be loaded. */
    val state: StateFlow<LoadState<CatalogueFolderInfo>> = _state.asStateFlow()

    init {
        viewModelScope.launch {
            combine(_path, repository.libraryChangeSignals()) { path, _ -> path }.collectLatest { path ->
                _state.refresh(context) { repository.call { core -> core.catalogueFolder(path) } }
            }
        }
    }

    /** Opens the folder at [path] (`""` for the root). */
    fun open(path: String) {
        _path.value = path
    }

    /** Goes up one level. Does nothing at the root. */
    fun up() {
        _path.value = parentPath(_path.value)
    }

    /** Replaces the queue with the tracks of the folder, starting at [index] (0-based). */
    fun playFrom(index: Int) {
        val ids = (_state.value as? LoadState.Ready)?.value?.tracks?.map { it.id } ?: return
        if (ids.isEmpty()) return
        repository.command { core -> core.playTracks(ids, index.toUInt()) }
    }
}

/**
 * The path one level above [path]: `"Artist A/Album One/"` gives `"Artist A/"`, `"Artist A/"` gives
 * `""`, and the root stays the root.
 */
internal fun parentPath(path: String): String {
    val trimmed = path.trimEnd('/')
    if (trimmed.isEmpty()) return ""
    val parent = trimmed.substringBeforeLast('/', "")
    return if (parent.isEmpty()) "" else "$parent/"
}

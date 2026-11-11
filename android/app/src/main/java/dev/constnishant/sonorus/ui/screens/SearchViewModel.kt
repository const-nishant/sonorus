package dev.constnishant.sonorus.ui.screens

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.QualityFilterInfo
import dev.constnishant.sonorus.core.SearchResult
import dev.constnishant.sonorus.data.SonorusRepository
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.mapLatest
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

private const val SEARCH_LIMIT = 30u
private const val QUALITY_LIST_LIMIT = 500u
private const val DEBOUNCE_MS = 250L
private const val STOP_TIMEOUT_MS = 5_000L

private val EMPTY_RESULT = SearchResult(tracks = emptyList(), albums = emptyList(), artists = emptyList())

/**
 * What the Search screen shows.
 *
 * @param query the text in the field, as typed.
 * @param term the trimmed query [results] were searched for. Lags [query] by the debounce delay.
 * @param filter the audio quality the results are limited to.
 * @param results the matches for [term] and [filter], or the failure.
 * @param recent the recent search queries, newest first.
 */
data class SearchUiState(
    val query: String = "",
    val term: String = "",
    val filter: QualityFilterInfo = QualityFilterInfo.ANY,
    val results: LoadState<SearchResult> = LoadState.Ready(EMPTY_RESULT),
    val recent: List<String> = emptyList(),
)

/**
 * Backs the Search screen. The query is debounced by 250 ms, the library is searched on each
 * settled query and quality filter (and again after catalogue changes), and a blank query yields no
 * results unless a quality filter other than All is chosen, which then lists those tracks. Recent
 * searches are saved when the user submits a query or opens a result.
 */
@OptIn(FlowPreview::class, ExperimentalCoroutinesApi::class)
class SearchViewModel(
    application: Application,
    private val repository: SonorusRepository,
) : AndroidViewModel(application) {
    private val context: Application = application
    private val query = MutableStateFlow("")
    private val filter = MutableStateFlow(QualityFilterInfo.ANY)
    private val recentStore = repository.recentSearches
    private val _recent = MutableStateFlow<List<String>>(emptyList())

    private val results: Flow<Searched> = combine(
        query.debounce(DEBOUNCE_MS).distinctUntilChanged(),
        filter,
        repository.libraryChangeSignals(),
    ) { settled, quality, _ -> settled to quality }
        .mapLatest { (settled, quality) -> search(settled, quality) }

    /** The search field, filter, results and recent searches. */
    val state: StateFlow<SearchUiState> = combine(query, filter, results, _recent) { typed, quality, searched, recent ->
        SearchUiState(
            query = typed,
            term = searched.term,
            filter = quality,
            results = searched.result,
            recent = recent,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(STOP_TIMEOUT_MS), SearchUiState())

    init {
        viewModelScope.launch {
            _recent.value = withContext(Dispatchers.IO) { recentStore.load() }
        }
    }

    /** Updates the query. Results follow after a short pause. */
    fun onQueryChange(text: String) {
        query.value = text
    }

    /** Limits the results to one audio quality. [QualityFilterInfo.ANY] shows everything. */
    fun onFilterChange(quality: QualityFilterInfo) {
        filter.value = quality
    }

    /** Saves the current query as a recent search. Called when the user presses Search on the keyboard. */
    fun onSearchSubmit() {
        saveRecent()
    }

    /** Saves the current query as a recent search. Called when the user opens a result. */
    fun onResultOpened() {
        saveRecent()
    }

    /** Removes one recent search. */
    fun removeRecent(text: String) {
        viewModelScope.launch {
            _recent.value = withContext(Dispatchers.IO) { recentStore.remove(text) }
        }
    }

    /** Removes all recent searches. */
    fun clearRecent() {
        viewModelScope.launch {
            _recent.value = withContext(Dispatchers.IO) { recentStore.clear() }
        }
    }

    /** Records the trimmed query as the newest recent search. A blank query is not saved. */
    private fun saveRecent() {
        val term = query.value.trim()
        if (term.isEmpty()) return
        viewModelScope.launch {
            _recent.value = withContext(Dispatchers.IO) { recentStore.add(term) }
        }
    }

    private suspend fun search(text: String, quality: QualityFilterInfo): Searched {
        val term = text.trim()
        if (term.isEmpty() && quality == QualityFilterInfo.ANY) return Searched(term, LoadState.Ready(EMPTY_RESULT))
        // An empty query with a quality filter lists every track of that quality, so it gets a larger limit.
        val limit = if (term.isEmpty()) QUALITY_LIST_LIMIT else SEARCH_LIMIT
        val result: LoadState<SearchResult> = try {
            LoadState.Ready(repository.call { core -> core.searchFiltered(term, limit, quality) })
        } catch (e: CoreException) {
            LoadState.Failed(e.displayMessage(context))
        }
        return Searched(term, result)
    }

    private data class Searched(val term: String, val result: LoadState<SearchResult>)
}

package dev.constnishant.sonorus.ui.screens

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.core.SearchResult
import dev.constnishant.sonorus.data.SonorusRepository
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.mapLatest
import kotlinx.coroutines.flow.stateIn

private const val SEARCH_LIMIT = 30u
private const val DEBOUNCE_MS = 250L
private const val STOP_TIMEOUT_MS = 5_000L

private val EMPTY_RESULT = SearchResult(tracks = emptyList(), albums = emptyList(), artists = emptyList())

/**
 * What the Search screen shows.
 *
 * @param query the text in the field, as typed.
 * @param term the trimmed query [results] were searched for. Lags [query] by the debounce delay.
 * @param results the matches for [term], or the failure.
 */
data class SearchUiState(
    val query: String = "",
    val term: String = "",
    val results: LoadState<SearchResult> = LoadState.Ready(EMPTY_RESULT),
)

/**
 * Backs the Search screen. The query is debounced by 250 ms, the library is searched on each
 * settled query (and again after catalogue changes), and a blank query yields no results.
 */
@OptIn(FlowPreview::class, ExperimentalCoroutinesApi::class)
class SearchViewModel(private val repository: SonorusRepository) : ViewModel() {
    private val query = MutableStateFlow("")

    private val results: Flow<Searched> = combine(
        query.debounce(DEBOUNCE_MS).distinctUntilChanged(),
        repository.libraryChangeSignals(),
    ) { settled, _ -> settled }
        .mapLatest { settled -> search(settled) }

    /** The search field and results. */
    val state: StateFlow<SearchUiState> = combine(query, results) { typed, searched ->
        SearchUiState(query = typed, term = searched.term, results = searched.result)
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(STOP_TIMEOUT_MS), SearchUiState())

    /** Updates the query. Results follow after a short pause. */
    fun onQueryChange(text: String) {
        query.value = text
    }

    private suspend fun search(text: String): Searched {
        val term = text.trim()
        if (term.isEmpty()) return Searched(term, LoadState.Ready(EMPTY_RESULT))
        val result: LoadState<SearchResult> = try {
            LoadState.Ready(repository.call { core -> core.search(term, SEARCH_LIMIT) })
        } catch (e: CoreException) {
            LoadState.Failed(e.displayMessage())
        }
        return Searched(term, result)
    }

    private data class Searched(val term: String, val result: LoadState<SearchResult>)
}

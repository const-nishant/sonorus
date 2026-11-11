package dev.constnishant.sonorus.ui.screens

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.constnishant.sonorus.core.ListeningStatsInfo
import dev.constnishant.sonorus.core.StatsRangeInfo
import dev.constnishant.sonorus.data.SonorusRepository
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch

/** How many top tracks, albums and artists the Stats screen lists. */
private const val STATS_TOP = 10u

/**
 * Backs the Stats screen: the listening totals and top lists for the selected range. The range is
 * kept here, so it survives configuration changes, and changing it reloads the stats.
 */
class StatsViewModel(
    application: Application,
    private val repository: SonorusRepository,
) : AndroidViewModel(application) {
    private val context: Application = application
    private val _range = MutableStateFlow(StatsRangeInfo.MONTH)

    /** The selected range. Starts at the month. */
    val range: StateFlow<StatsRangeInfo> = _range.asStateFlow()

    private val _state = MutableStateFlow<LoadState<ListeningStatsInfo>>(LoadState.Loading)

    /** The stats for [range], or the reason they could not be loaded. */
    val state: StateFlow<LoadState<ListeningStatsInfo>> = _state.asStateFlow()

    init {
        viewModelScope.launch {
            _range.collectLatest { range ->
                _state.refresh(context) {
                    repository.call { core -> core.listeningStats(range, STATS_TOP) }
                }
            }
        }
    }

    /** Selects a range. The stats reload for it. */
    fun setRange(range: StatsRangeInfo) {
        _range.value = range
    }

    /** Replaces the queue with the single track [trackId] and starts it. */
    fun playTrack(trackId: Long) {
        repository.command { core -> core.playTracks(listOf(trackId), 0u) }
    }
}

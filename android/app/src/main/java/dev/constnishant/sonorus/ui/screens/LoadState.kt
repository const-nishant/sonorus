package dev.constnishant.sonorus.ui.screens

import dev.constnishant.sonorus.core.CoreException
import dev.constnishant.sonorus.data.SonorusRepository
import dev.constnishant.sonorus.data.userMessage
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.onStart

/** The state of a screen's data load. */
sealed interface LoadState<out T> {
    /** Nothing has been loaded yet. */
    data object Loading : LoadState<Nothing>

    /** The latest load succeeded with [value]. */
    data class Ready<out T>(val value: T) : LoadState<T>

    /** The latest load failed; [message] is safe to show to the user. */
    data class Failed(val message: String) : LoadState<Nothing>
}

/**
 * Runs [block] (which calls the core through the repository) and stores its result as
 * [LoadState.Ready], or [LoadState.Failed] when it throws a [CoreException].
 */
internal suspend fun <T> MutableStateFlow<LoadState<T>>.refresh(block: suspend () -> T) {
    value = try {
        LoadState.Ready(block())
    } catch (e: CoreException) {
        LoadState.Failed(e.displayMessage())
    }
}

/** A message for a core failure. */
internal fun Throwable.displayMessage(): String = when (this) {
    is CoreException -> userMessage("Could not load this.")
    else -> message?.takeIf { it.isNotBlank() } ?: toString()
}

/**
 * Emits once immediately and then again whenever the catalogue changes. ViewModels collect this
 * to (re)load their data.
 */
internal fun SonorusRepository.libraryChangeSignals(): Flow<Unit> =
    events.libraryChanged.onStart { emit(Unit) }

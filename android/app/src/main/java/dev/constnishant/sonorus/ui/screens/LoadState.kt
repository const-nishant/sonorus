package dev.constnishant.sonorus.ui.screens

import android.content.Context
import dev.constnishant.sonorus.R
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
internal suspend fun <T> MutableStateFlow<LoadState<T>>.refresh(context: Context, block: suspend () -> T) {
    value = try {
        LoadState.Ready(block())
    } catch (e: CoreException) {
        LoadState.Failed(e.displayMessage(context))
    }
}

/** A message for a core failure. */
internal fun Throwable.displayMessage(context: Context): String = when (this) {
    is CoreException -> userMessage(context, R.string.error_load_generic)
    else -> message?.takeIf { it.isNotBlank() } ?: toString()
}

/**
 * Emits once immediately and then again whenever the catalogue changes. ViewModels collect this
 * to (re)load their data.
 */
internal fun SonorusRepository.libraryChangeSignals(): Flow<Unit> =
    events.libraryChanged.onStart { emit(Unit) }

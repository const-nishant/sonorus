package dev.constnishant.sonorus.data

import android.content.Context
import android.util.Log
import androidx.annotation.StringRes
import dev.constnishant.sonorus.R
import dev.constnishant.sonorus.core.CoreException

private const val TAG = "Sonorus"

/**
 * Short text for a core failure that is safe to show in the UI. The raw detail (which can be a
 * long SDK dump) goes to logcat instead; the core never puts credentials into error details.
 * [fallback] is the string resource used for the failures that have no more specific text, and
 * [args] are its format arguments.
 */
fun CoreException.userMessage(context: Context, @StringRes fallback: Int, vararg args: Any): String {
    Log.w(TAG, "core error: $this")
    return when (this) {
        is CoreException.NotConfigured -> context.getString(R.string.error_not_configured)
        is CoreException.AccessDenied -> context.getString(R.string.error_access_denied)
        is CoreException.InvalidConfig -> sentence(detail)
        is CoreException.NotFound -> context.getString(R.string.error_not_found, detail)
        is CoreException.Storage -> context.getString(R.string.error_storage)
        is CoreException.Sync -> context.getString(R.string.error_sync_refused, detail)
        is CoreException.Busy -> sentence(detail)
        is CoreException.Library, is CoreException.Internal -> context.getString(fallback, *args)
    }
}

/** [text] with a capital first letter and a final period. */
private fun sentence(text: String): String {
    val trimmed = text.trim().replaceFirstChar { it.uppercaseChar() }
    return if (trimmed.endsWith('.')) trimmed else "$trimmed."
}

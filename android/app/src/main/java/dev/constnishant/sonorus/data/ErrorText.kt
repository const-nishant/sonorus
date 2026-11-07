package dev.constnishant.sonorus.data

import android.util.Log
import dev.constnishant.sonorus.core.CoreException

private const val TAG = "Sonorus"

/**
 * Short text for a core failure that is safe to show in the UI. The raw detail (which can be a
 * long SDK dump) goes to logcat instead; the core never puts credentials into error details.
 * [fallback] is used for the failures that have no more specific text.
 */
fun CoreException.userMessage(fallback: String): String {
    Log.w(TAG, "core error: $this")
    return when (this) {
        is CoreException.NotConfigured -> "Connect a bucket in Settings first."
        is CoreException.AccessDenied ->
            "The bucket rejected the credentials. Check the key ID, secret key and bucket name."
        is CoreException.InvalidConfig -> sentence(detail)
        is CoreException.NotFound -> "Not found: $detail"
        is CoreException.Storage ->
            "Could not reach the bucket. Check the connection and try again."
        is CoreException.Sync -> "Sync refused: $detail"
        is CoreException.Busy -> sentence(detail)
        is CoreException.Library, is CoreException.Internal -> fallback
    }
}

/** [text] with a capital first letter and a final period. */
private fun sentence(text: String): String {
    val trimmed = text.trim().replaceFirstChar { it.uppercaseChar() }
    return if (trimmed.endsWith('.')) trimmed else "$trimmed."
}

package dev.constnishant.sonorus.data

import android.content.Context
import androidx.core.content.edit
import org.json.JSONArray
import org.json.JSONException

private const val PREFS_NAME = "recent_searches"
private const val K_QUERIES = "queries"

/** How many recent searches are kept. */
const val RECENT_SEARCH_LIMIT = 10

/**
 * Persists the most recent search queries, newest first, in a private SharedPreferences file. The
 * queries are not secret, so they are stored in clear text as a JSON array.
 *
 * Every change trims the query, drops earlier entries that match it ignoring case, and keeps at
 * most [RECENT_SEARCH_LIMIT] entries. Writes are applied in the background (`apply()`); reads return
 * the latest value at once. A failed write is not reported: losing a search from the history is
 * harmless.
 */
class RecentSearchesStore(context: Context) {
    private val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    /** The saved queries, newest first. An unreadable value reads as no searches. Blocking. */
    fun load(): List<String> {
        val raw = prefs.getString(K_QUERIES, null) ?: return emptyList()
        return try {
            val array = JSONArray(raw)
            List(array.length()) { array.getString(it) }.filter { it.isNotBlank() }
        } catch (e: JSONException) {
            emptyList()
        }
    }

    /**
     * Records [query] as the newest search and returns the updated list. A blank query changes
     * nothing. Blocking.
     */
    @Synchronized
    fun add(query: String): List<String> {
        val term = query.trim()
        val current = load()
        if (term.isEmpty()) return current
        val updated = (listOf(term) + current.filterNot { it.equals(term, ignoreCase = true) })
            .take(RECENT_SEARCH_LIMIT)
        save(updated)
        return updated
    }

    /** Removes the entry equal to [query] and returns the updated list. Blocking. */
    @Synchronized
    fun remove(query: String): List<String> {
        val updated = load().filterNot { it == query }
        save(updated)
        return updated
    }

    /** Removes every saved search. Blocking. */
    @Synchronized
    fun clear(): List<String> {
        prefs.edit { remove(K_QUERIES) }
        return emptyList()
    }

    private fun save(queries: List<String>) {
        prefs.edit { putString(K_QUERIES, JSONArray(queries).toString()) }
    }
}

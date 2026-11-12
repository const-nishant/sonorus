package dev.constnishant.sonorus.ui

import android.net.Uri
import dev.constnishant.sonorus.core.SmartPlaylistKind

/** Navigation routes (string routes; ids are library `Long` ids). */
object Routes {
    const val HOME = "home"
    const val LIBRARY = "library"
    const val SEARCH = "search"
    const val NOW_PLAYING = "now_playing"
    const val SETTINGS = "settings"
    const val UPLOADS = "uploads"
    const val STATS = "stats"

    const val ARG_PATH = "path"
    const val MANAGE = "manage?$ARG_PATH={$ARG_PATH}"

    const val ARG_ID = "id"
    const val ALBUM = "album/{$ARG_ID}"
    const val ARTIST = "artist/{$ARG_ID}"
    const val PLAYLIST = "playlist/{$ARG_ID}"

    const val ARG_KIND = "kind"
    const val SMART = "smart/{$ARG_KIND}"

    fun album(id: Long) = "album/$id"
    fun artist(id: Long) = "artist/$id"
    fun playlist(id: Long) = "playlist/$id"

    /** The Manage bucket screen at the bucket folder [path] (relative to the prefix, `""` for the root). */
    fun manage(path: String = "") = "manage?$ARG_PATH=${Uri.encode(path)}"

    /** The smart playlist route for [kind], with the enum name as its argument. */
    fun smartPlaylist(kind: SmartPlaylistKind) = "smart/${kind.name}"
}

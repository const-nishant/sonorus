package dev.constnishant.sonorus.ui

/** Navigation routes (string routes; ids are library `Long` ids). */
object Routes {
    const val HOME = "home"
    const val LIBRARY = "library"
    const val SEARCH = "search"
    const val NOW_PLAYING = "now_playing"
    const val SETTINGS = "settings"
    const val UPLOADS = "uploads"

    const val ARG_ID = "id"
    const val ALBUM = "album/{$ARG_ID}"
    const val ARTIST = "artist/{$ARG_ID}"
    const val PLAYLIST = "playlist/{$ARG_ID}"

    fun album(id: Long) = "album/$id"
    fun artist(id: Long) = "artist/$id"
    fun playlist(id: Long) = "playlist/$id"
}

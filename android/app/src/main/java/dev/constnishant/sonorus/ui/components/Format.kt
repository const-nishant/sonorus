package dev.constnishant.sonorus.ui.components

import dev.constnishant.sonorus.core.TrackInfo

/** `m:ss`, or `h:mm:ss` from one hour; "--:--" when unknown. */
fun formatDuration(ms: Long?): String {
    if (ms == null || ms < 0) return "--:--"
    val total = ms / 1000
    val h = total / 3600
    val m = (total % 3600) / 60
    val s = total % 60
    return if (h > 0) "%d:%02d:%02d".format(h, m, s) else "%d:%02d".format(m, s)
}

/** Long totals for album/playlist headers: "42 min" or "1 h 12 min". */
fun formatTotal(ms: Long): String {
    val minutes = ms / 60_000
    return if (minutes >= 60) "${minutes / 60} h ${minutes % 60} min" else "$minutes min"
}

/** Kilohertz without a trailing ".0": 44100 → "44.1", 96000 → "96". */
fun formatKhz(sampleRate: UInt): String {
    val khz = sampleRate.toDouble() / 1000.0
    return if (khz % 1.0 == 0.0) "%d".format(khz.toLong()) else "%.1f".format(khz)
}

/** Quality badge text such as "FLAC 24/96"; null when the codec is unknown. */
fun TrackInfo.qualityLabel(): String? {
    val codec = codec ?: return null
    val rate = sampleRate ?: return codec
    val bits = bitsPerSample
    return if (bits != null) "$codec $bits/${formatKhz(rate)}" else "$codec ${formatKhz(rate)} kHz"
}

/** True for anything above CD quality (more than 16 bits or more than 48 kHz). */
fun TrackInfo.isHiRes(): Boolean =
    (bitsPerSample?.toInt() ?: 0) > 16 || (sampleRate?.toInt() ?: 0) > 48_000

package dev.constnishant.sonorus.ui.components

import dev.constnishant.sonorus.core.OutputSampleFormat
import dev.constnishant.sonorus.core.TrackInfo
import dev.constnishant.sonorus.data.GIB
import kotlin.math.roundToLong

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

/** Short label for an output sample format, such as "24-bit". */
fun OutputSampleFormat.label(): String = when (this) {
    OutputSampleFormat.FLOAT -> "Float"
    OutputSampleFormat.INT32 -> "32-bit"
    OutputSampleFormat.INT24_PACKED -> "24-bit"
    OutputSampleFormat.INT16 -> "16-bit"
}

/** True for anything above CD quality (more than 16 bits or more than 48 kHz). */
fun TrackInfo.isHiRes(): Boolean =
    (bitsPerSample?.toInt() ?: 0) > 16 || (sampleRate?.toInt() ?: 0) > 48_000

/**
 * A byte count for people: "1.2 GB", "4 GB", "412 MB", "820 KB". Units are binary (1 GB is 1024 MB).
 * Gigabytes keep one decimal unless it is zero; megabytes and kilobytes are whole numbers.
 */
fun formatBytes(bytes: ULong): String {
    val b = bytes.toDouble()
    val mb = 1024.0 * 1024
    return when {
        b >= GIB -> gigabytes(b / GIB) + " GB"
        b >= mb -> "${(b / mb).roundToLong()} MB"
        else -> "${(b / 1024).roundToLong()} KB"
    }
}

private fun gigabytes(value: Double): String =
    if (value % 1.0 == 0.0) "%d".format(value.toLong()) else "%.1f".format(value)

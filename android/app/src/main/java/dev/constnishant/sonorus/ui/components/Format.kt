package dev.constnishant.sonorus.ui.components

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.ui.res.pluralStringResource
import dev.constnishant.sonorus.R
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
fun formatTotal(context: Context, ms: Long): String {
    val minutes = ms / 60_000
    return if (minutes >= 60) {
        context.getString(R.string.duration_hours_minutes, minutes / 60, minutes % 60)
    } else {
        context.getString(R.string.duration_minutes, minutes)
    }
}

/** Kilohertz without a trailing ".0": 44100 → "44.1", 96000 → "96". */
fun formatKhz(sampleRate: UInt): String {
    val khz = sampleRate.toDouble() / 1000.0
    return if (khz % 1.0 == 0.0) "%d".format(khz.toLong()) else "%.1f".format(khz)
}

/** Quality badge text such as "FLAC 24/96"; null when the codec is unknown. */
fun TrackInfo.qualityLabel(context: Context): String? {
    val codec = codec ?: return null
    val rate = sampleRate ?: return codec
    val bits = bitsPerSample
    return if (bits != null) {
        "$codec $bits/${formatKhz(rate)}"
    } else {
        "$codec ${context.getString(R.string.khz_unit, formatKhz(rate))}"
    }
}

/** Short label for an output sample format, such as "24-bit". */
fun OutputSampleFormat.label(context: Context): String = context.getString(
    when (this) {
        OutputSampleFormat.FLOAT -> R.string.sample_format_float
        OutputSampleFormat.INT32 -> R.string.sample_format_32bit
        OutputSampleFormat.INT24_PACKED -> R.string.sample_format_24bit
        OutputSampleFormat.INT16 -> R.string.sample_format_16bit
    },
)

/** True for anything above CD quality (more than 16 bits or more than 48 kHz). */
fun TrackInfo.isHiRes(): Boolean =
    (bitsPerSample?.toInt() ?: 0) > 16 || (sampleRate?.toInt() ?: 0) > 48_000

/**
 * A byte count for people: "1.2 GB", "4 GB", "412 MB", "820 KB". Units are binary (1 GB is 1024 MB).
 * Gigabytes keep one decimal unless it is zero; megabytes and kilobytes are whole numbers.
 */
fun formatBytes(context: Context, bytes: ULong): String {
    val b = bytes.toDouble()
    val mb = 1024.0 * 1024
    return when {
        b >= GIB -> context.getString(R.string.size_gb, gigabytes(b / GIB))
        b >= mb -> context.getString(R.string.size_mb, (b / mb).roundToLong())
        else -> context.getString(R.string.size_kb, (b / 1024).roundToLong())
    }
}

private fun gigabytes(value: Double): String =
    if (value % 1.0 == 0.0) "%d".format(value.toLong()) else "%.1f".format(value)

/** A track count such as "1 track" or "12 tracks". */
@Composable
fun tracksText(count: Int): String = pluralStringResource(R.plurals.tracks_count, count, count)

/** An album count such as "1 album" or "12 albums". */
@Composable
fun albumsText(count: Int): String = pluralStringResource(R.plurals.albums_count, count, count)

/** A song count such as "1 song" or "12 songs". */
@Composable
fun songsText(count: Int): String = pluralStringResource(R.plurals.songs_count, count, count)

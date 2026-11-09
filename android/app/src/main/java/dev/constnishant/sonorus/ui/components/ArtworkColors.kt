package dev.constnishant.sonorus.ui.components

import android.graphics.BitmapFactory
import android.graphics.Color as AndroidColor
import android.util.LruCache
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.tween
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.State
import androidx.compose.runtime.produceState
import androidx.compose.ui.graphics.Color
import androidx.core.graphics.ColorUtils
import androidx.palette.graphics.Palette
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Colors taken from a cover: [dominant] is a dark background color, [vibrant] an accent, and
 * [onDominant] the text color to use on [dominant].
 */
@Immutable
data class ArtworkColors(val dominant: Color, val vibrant: Color, val onDominant: Color)

/** Recent results keyed by cover path, so a cover shown again does not decode again. */
private object ArtworkColorCache {
    val lru = LruCache<String, ArtworkColors>(64)
}

/** Longest side, in pixels, that the cover is decoded to for palette extraction. */
private const val PaletteSidePx = 128

/** Relative luminance above which the dominant color is darkened toward black. */
private const val MaxDominantLuminance = 0.18

/**
 * The colors of the cover at [path], or null while they load, when [path] is null, or when the
 * file cannot be decoded. Results are cached in memory.
 */
@Composable
fun rememberArtworkColors(path: String?): ArtworkColors? =
    produceState(initialValue = path?.let { ArtworkColorCache.lru.get(it) }, key1 = path) {
        if (path == null) {
            value = null
            return@produceState
        }
        ArtworkColorCache.lru.get(path)?.let {
            value = it
            return@produceState
        }
        val colors = withContext(Dispatchers.IO) { computeArtworkColors(path) }
        if (colors != null) ArtworkColorCache.lru.put(path, colors)
        value = colors
    }.value

/**
 * The dominant color of these colors, animated when it changes. [fallback] is used while there
 * are no colors (no cover, or still loading).
 */
@Composable
fun ArtworkColors?.animatedDominant(fallback: Color): State<Color> =
    animateColorAsState(
        targetValue = this?.dominant ?: fallback,
        animationSpec = tween(durationMillis = 600),
        label = "artwork-dominant",
    )

/** Decodes a small copy of the cover at [path] and derives its colors. Never throws; null on failure. */
private fun computeArtworkColors(path: String): ArtworkColors? {
    return try {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(path, bounds)
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
        val longest = maxOf(bounds.outWidth, bounds.outHeight)
        var sample = 1
        while (longest / (sample * 2) >= PaletteSidePx) sample *= 2
        val bitmap = BitmapFactory.decodeFile(
            path,
            BitmapFactory.Options().apply { inSampleSize = sample },
        ) ?: return null
        val palette = try {
            Palette.from(bitmap).maximumColorCount(16).generate()
        } finally {
            bitmap.recycle()
        }

        val base = palette.darkMutedSwatch?.rgb
            ?: palette.mutedSwatch?.rgb
            ?: palette.dominantSwatch?.rgb
            ?: return null
        val dominant = darkenToLuminance(base, MaxDominantLuminance)
        val vibrant = palette.vibrantSwatch?.rgb
            ?: palette.lightVibrantSwatch?.rgb
            ?: dominant
        ArtworkColors(
            dominant = Color(dominant),
            vibrant = Color(vibrant),
            onDominant = Color.White,
        )
    } catch (t: Throwable) {
        null
    }
}

/**
 * Blends [argb] toward black just enough that its relative luminance is at most [maxLuminance].
 * Colors already dark enough are returned unchanged.
 */
private fun darkenToLuminance(argb: Int, maxLuminance: Double): Int {
    if (ColorUtils.calculateLuminance(argb) <= maxLuminance) return argb
    var low = 0f
    var high = 1f
    repeat(12) {
        val mid = (low + high) / 2f
        val blended = ColorUtils.blendARGB(argb, AndroidColor.BLACK, mid)
        if (ColorUtils.calculateLuminance(blended) > maxLuminance) low = mid else high = mid
    }
    return ColorUtils.blendARGB(argb, AndroidColor.BLACK, high)
}

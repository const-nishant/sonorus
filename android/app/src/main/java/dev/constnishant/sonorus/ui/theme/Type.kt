package dev.constnishant.sonorus.ui.theme

import androidx.compose.material3.Typography
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.sp
import dev.constnishant.sonorus.R

/** Inter, bundled in res/font. */
val Inter = FontFamily(
    Font(R.font.inter_regular, FontWeight.Normal),
    Font(R.font.inter_medium, FontWeight.Medium),
    Font(R.font.inter_semibold, FontWeight.SemiBold),
    Font(R.font.inter_bold, FontWeight.Bold),
)

/** Material 3 defaults (sizes, line heights) with Inter and the Sonorus weights. */
private val M3 = Typography()

private fun TextStyle.inter(weight: FontWeight, tracking: TextUnit? = null): TextStyle = copy(
    fontFamily = Inter,
    fontWeight = weight,
    letterSpacing = tracking ?: letterSpacing,
)

val SonorusTypography = Typography(
    displayLarge = M3.displayLarge.inter(FontWeight.SemiBold, (-0.5).sp),
    displayMedium = M3.displayMedium.inter(FontWeight.SemiBold, (-0.5).sp),
    displaySmall = M3.displaySmall.inter(FontWeight.SemiBold, (-0.25).sp),
    headlineLarge = M3.headlineLarge.inter(FontWeight.SemiBold, (-0.5).sp),
    headlineMedium = M3.headlineMedium.inter(FontWeight.SemiBold, (-0.25).sp),
    headlineSmall = M3.headlineSmall.inter(FontWeight.SemiBold, (-0.25).sp),
    titleLarge = M3.titleLarge.inter(FontWeight.SemiBold),
    titleMedium = M3.titleMedium.inter(FontWeight.Medium),
    titleSmall = M3.titleSmall.inter(FontWeight.Medium),
    bodyLarge = M3.bodyLarge.inter(FontWeight.Normal),
    bodyMedium = M3.bodyMedium.inter(FontWeight.Normal),
    bodySmall = M3.bodySmall.inter(FontWeight.Normal),
    labelLarge = M3.labelLarge.inter(FontWeight.Medium),
    labelMedium = M3.labelMedium.inter(FontWeight.Medium),
    labelSmall = M3.labelSmall.inter(FontWeight.Medium),
)

/** Tabular figures, so changing digits (times, sizes, percentages) do not shift the layout. */
fun TextStyle.tabular(): TextStyle = copy(fontFeatureSettings = "tnum")

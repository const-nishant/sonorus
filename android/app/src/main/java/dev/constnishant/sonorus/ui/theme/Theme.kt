package dev.constnishant.sonorus.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

private val DarkColors = darkColorScheme(
    primary = Color(0xFF7FD1C7),
    onPrimary = Color(0xFF00201D),
    primaryContainer = Color(0xFF1F4D48),
    onPrimaryContainer = Color(0xFFB5EFE7),
    secondary = Color(0xFFC2C8D0),
    onSecondary = Color(0xFF2B3138),
    secondaryContainer = Color(0xFF3A4149),
    onSecondaryContainer = Color(0xFFDFE3E9),
    tertiary = Color(0xFFF2C46D),
    onTertiary = Color(0xFF3F2E00),
    tertiaryContainer = Color(0xFF5B4410),
    onTertiaryContainer = Color(0xFFFFDEA0),
    background = Color(0xFF0E1014),
    onBackground = Color(0xFFE3E6EA),
    surface = Color(0xFF0E1014),
    onSurface = Color(0xFFE3E6EA),
    surfaceVariant = Color(0xFF1E232A),
    onSurfaceVariant = Color(0xFFA9B1BB),
    surfaceTint = Color(0xFF7FD1C7),
    surfaceContainerLowest = Color(0xFF0B0D10),
    surfaceContainerLow = Color(0xFF15191D),
    surfaceContainer = Color(0xFF181C21),
    surfaceContainerHigh = Color(0xFF1E2328),
    surfaceContainerHighest = Color(0xFF262C33),
    inverseSurface = Color(0xFFE3E6EA),
    inverseOnSurface = Color(0xFF2A2F36),
    outline = Color(0xFF747D88),
    outlineVariant = Color(0xFF3A424C),
    error = Color(0xFFFFB4AB),
    onError = Color(0xFF690005),
    errorContainer = Color(0xFF93000A),
    onErrorContainer = Color(0xFFFFDAD6),
)

private val LightColors = lightColorScheme(
    primary = Color(0xFF006A62),
    onPrimary = Color(0xFFFFFFFF),
    primaryContainer = Color(0xFF8EF3E7),
    onPrimaryContainer = Color(0xFF00201D),
    secondary = Color(0xFF4C5661),
    onSecondary = Color(0xFFFFFFFF),
    secondaryContainer = Color(0xFFD8DEE6),
    onSecondaryContainer = Color(0xFF131C24),
    tertiary = Color(0xFF7A5800),
    onTertiary = Color(0xFFFFFFFF),
    tertiaryContainer = Color(0xFFFFDEA0),
    onTertiaryContainer = Color(0xFF261900),
    background = Color(0xFFF6F8FA),
    onBackground = Color(0xFF1A1C1F),
    surface = Color(0xFFF6F8FA),
    onSurface = Color(0xFF1A1C1F),
    surfaceVariant = Color(0xFFDCE2E9),
    onSurfaceVariant = Color(0xFF40484F),
    surfaceTint = Color(0xFF006A62),
    surfaceContainerLowest = Color(0xFFFFFFFF),
    surfaceContainerLow = Color(0xFFF0F3F6),
    surfaceContainer = Color(0xFFEBEFF2),
    surfaceContainerHigh = Color(0xFFE5EAEE),
    surfaceContainerHighest = Color(0xFFDFE4E9),
    inverseSurface = Color(0xFF2F3134),
    inverseOnSurface = Color(0xFFF0F1F4),
    outline = Color(0xFF70787F),
    outlineVariant = Color(0xFFC0C7CF),
    error = Color(0xFFBA1A1A),
    onError = Color(0xFFFFFFFF),
    errorContainer = Color(0xFFFFDAD6),
    onErrorContainer = Color(0xFF410002),
)

/**
 * Sonorus Material 3 theme. Dark-first: the dark scheme is used when the system is in dark mode,
 * the light scheme otherwise. Dynamic color is never used.
 */
@Composable
fun SonorusTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = if (isSystemInDarkTheme()) DarkColors else LightColors,
        typography = SonorusTypography,
        shapes = SonorusShapes,
        content = content,
    )
}

package com.teamofsilicons.extend.ui

import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Typography
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.ExperimentalTextApi
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontVariation
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp
import com.teamofsilicons.extend.R

/**
 * Silicon Interface's tokens (its styles.css `:root`), as Compose colours.
 * Two are tuned for Android's contrast rules and say so.
 */
object Tokens {
    val Paper = Color(0xFFFFFDF9)
    val Surface = Color(0xFFF8F7F3)
    val Rail = Color(0xFFF4F4EF)
    val Line = Color(0xFFE9E8E1)
    val LineStrong = Color(0xFFDEDFD6)
    val Ink = Color(0xFF262A29)

    /** Interface's `--muted` (#777a74) is 4.3:1 on paper; this is the nearest shade that passes 4.5:1 on paper and surface. */
    val Muted = Color(0xFF6B6E68)

    /** Interface's `--muted` itself, for rules, dots and other non-text marks. */
    val MutedMark = Color(0xFF777A74)

    val Cobalt = Color(0xFF1736B8)
    val CobaltDeep = Color(0xFF10288F)
    val CobaltPressed = Color(0xFF102CAA)
    val CobaltPale = Color(0xFFEFF1FB)
    val CobaltEdge = Color(0xFFCCD4EF)
    val CobaltWash = Color(0xFFF0F3FD)

    /** The SILICON tag's ink. Interface's #7a86af is 3.2:1 on the pale pill; this passes 4.5:1. */
    val TagInk = Color(0xFF4A5A9C)

    /** Sky, from Interface's shader (.64, .82, .85). */
    val Sky = Color(0xFFA3D1D9)

    /** Risograph orange-red: Stop and danger only. White text on it is 4.6:1. */
    val Stop = Color(0xFFD63D22)
    val StopDeep = Color(0xFFB3361F)
    val StopPale = Color(0xFFFDF0EC)

    /** The one edge colour for Stop-pale notes and the Danger button. */
    val StopEdge = Color(0xFFF1CFC5)

    /**
     * The soft peach horizon of Interface's shader (.96, .72, .49), for the odd printed speck in
     * the dither fields. Decoration only: orange-red stays reserved for Stop and danger.
     */
    val Horizon = Color(0xFFF5B87D)
}

/** One variable font file (unmodified, from Google Fonts), three weights. */
@OptIn(ExperimentalTextApi::class)
val PlexSans = FontFamily(
    Font(R.font.ibm_plex_sans, FontWeight.Normal, variationSettings = FontVariation.Settings(FontVariation.weight(400))),
    Font(R.font.ibm_plex_sans, FontWeight.Medium, variationSettings = FontVariation.Settings(FontVariation.weight(500))),
    Font(R.font.ibm_plex_sans, FontWeight.SemiBold, variationSettings = FontVariation.Settings(FontVariation.weight(600))),
)

val PlexMono = FontFamily(
    Font(R.font.ibm_plex_mono_regular, FontWeight.Normal),
    Font(R.font.ibm_plex_mono_medium, FontWeight.Medium),
)

val SourceSerif = FontFamily(Font(R.font.source_serif_4_regular, FontWeight.Normal))

/**
 * Type sizes and page geometry for the two form factors: TV text is read from across the room.
 * [gutter] is the page's side margin (48 dp on TV: its overscan-safe area), [column] the widest the
 * content (and the top bar above it) gets, and [overscanTop] keeps the top bar out of a TV's
 * overscan.
 */
@Immutable
data class Scale(
    val tv: Boolean,
    val title: TextUnit,
    val cardTitle: TextUnit,
    val body: TextUnit,
    val small: TextUnit,
    val eyebrow: TextUnit,
    val button: TextUnit,
    val gutter: Dp,
    val column: Dp,
    val overscanTop: Dp,
) {
    companion object {
        val Phone = Scale(false, title = 32.sp, cardTitle = 21.sp, body = 15.sp, small = 13.sp, eyebrow = 11.sp, button = 15.sp, gutter = 20.dp, column = 640.dp, overscanTop = 0.dp)
        // A TV is 960 dp wide: the column fills it between the 48 dp overscan gutters, the same
        // edges the corner badge keeps, so the badge, top bar and cards share one right edge.
        val Tv = Scale(true, title = 44.sp, cardTitle = 28.sp, body = 20.sp, small = 17.sp, eyebrow = 14.sp, button = 19.sp, gutter = 48.dp, column = 864.dp, overscanTop = 27.dp)
    }
}

val LocalScale = staticCompositionLocalOf { Scale.Phone }

object Type {
    fun title(s: Scale) = TextStyle(fontFamily = SourceSerif, fontSize = s.title, lineHeight = s.title * 1.15, letterSpacing = (-0.01).em, color = Tokens.Ink)
    fun cardTitle(s: Scale) = TextStyle(fontFamily = SourceSerif, fontSize = s.cardTitle, lineHeight = s.cardTitle * 1.3, color = Tokens.Ink)
    fun body(s: Scale) = TextStyle(fontFamily = PlexSans, fontSize = s.body, lineHeight = s.body * 1.6, color = Tokens.Ink)
    fun muted(s: Scale) = TextStyle(fontFamily = PlexSans, fontSize = s.small, lineHeight = s.small * 1.7, color = Tokens.Muted)
    fun eyebrow(s: Scale) = TextStyle(fontFamily = PlexMono, fontSize = s.eyebrow, lineHeight = s.eyebrow * 1.5, letterSpacing = 0.12.em, color = Tokens.Muted)
    fun mono(s: Scale) = TextStyle(fontFamily = PlexMono, fontSize = s.small, lineHeight = s.small * 1.6, color = Tokens.Muted)
    fun label(s: Scale) = TextStyle(fontFamily = PlexSans, fontSize = s.button, fontWeight = FontWeight.Medium, lineHeight = s.button * 1.4)
}

private fun typography(): Typography {
    val base = Typography()
    fun TextStyle.sans() = copy(fontFamily = PlexSans)
    return Typography(
        displayLarge = base.displayLarge.copy(fontFamily = SourceSerif),
        displayMedium = base.displayMedium.copy(fontFamily = SourceSerif),
        displaySmall = base.displaySmall.copy(fontFamily = SourceSerif),
        headlineLarge = base.headlineLarge.copy(fontFamily = SourceSerif),
        headlineMedium = base.headlineMedium.copy(fontFamily = SourceSerif),
        headlineSmall = base.headlineSmall.copy(fontFamily = SourceSerif),
        titleLarge = base.titleLarge.copy(fontFamily = SourceSerif),
        titleMedium = base.titleMedium.sans(),
        titleSmall = base.titleSmall.sans(),
        bodyLarge = base.bodyLarge.sans(),
        bodyMedium = base.bodyMedium.sans(),
        bodySmall = base.bodySmall.sans(),
        labelLarge = base.labelLarge.sans(),
        labelMedium = base.labelMedium.sans(),
        labelSmall = base.labelSmall.sans(),
    )
}

private val ExtendColors = lightColorScheme(
    primary = Tokens.Cobalt,
    onPrimary = Color.White,
    primaryContainer = Tokens.CobaltPale,
    onPrimaryContainer = Tokens.CobaltDeep,
    secondary = Tokens.Cobalt,
    onSecondary = Color.White,
    secondaryContainer = Tokens.CobaltPale,
    onSecondaryContainer = Tokens.CobaltDeep,
    background = Tokens.Paper,
    onBackground = Tokens.Ink,
    surface = Tokens.Paper,
    onSurface = Tokens.Ink,
    surfaceVariant = Tokens.Surface,
    onSurfaceVariant = Tokens.Muted,
    surfaceContainerLowest = Tokens.Paper,
    surfaceContainerLow = Tokens.Paper,
    surfaceContainer = Tokens.Paper,
    surfaceContainerHigh = Tokens.Paper,
    surfaceContainerHighest = Tokens.Surface,
    outline = Tokens.LineStrong,
    outlineVariant = Tokens.Line,
    error = Tokens.Stop,
    onError = Color.White,
    errorContainer = Tokens.StopPale,
    onErrorContainer = Tokens.StopDeep,
)

/** Interface's light paper system on phones, tablets and TVs alike. */
@Composable
fun ExtendTheme(tv: Boolean, content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = ExtendColors, typography = typography()) {
        CompositionLocalProvider(LocalScale provides if (tv) Scale.Tv else Scale.Phone) {
            Surface(modifier = Modifier.fillMaxSize(), color = Tokens.Paper, contentColor = Tokens.Ink) { content() }
        }
    }
}

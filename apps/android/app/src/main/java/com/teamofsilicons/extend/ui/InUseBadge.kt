package com.teamofsilicons.extend.ui

import android.content.Context
import android.graphics.Canvas
import android.graphics.ColorFilter
import android.graphics.Paint
import android.graphics.PixelFormat
import android.graphics.Typeface
import android.graphics.drawable.Drawable
import android.graphics.drawable.GradientDrawable
import android.graphics.fonts.Font
import android.graphics.fonts.FontFamily
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.TextUtils
import android.os.Build
import android.text.TextPaint
import android.text.style.MetricAffectingSpan
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.widget.LinearLayout
import android.widget.TextView
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import com.teamofsilicons.extend.R
import com.teamofsilicons.extend.core.UiState
import com.teamofsilicons.extend.core.InUseIndicator
import java.io.File

/**
 * What the TV's bottom-centre badge says while a Silicon works: short, and always which Silicon.
 * [line] follows the Silicon's name; [detail] is the takeover reason, on a second line.
 */
data class InUseBadge(
    val silicon: String,
    val line: String,
    val detail: String?,
    val waiting: Boolean,
) {
    /** The whole badge as one sentence, for TalkBack. */
    val spoken: String get() = "$silicon $line" + (detail?.let { ": $it" } ?: "")

    companion object {
        /** The badge for this state, or null when a TV has nothing to show (or this isn't a TV). */
        fun from(state: UiState): InUseBadge? {
            if (!state.isTv || InUseIndicator.show(state) == InUseIndicator.Show.NONE) return null
            val session = state.session
            val takeover = state.takeover
            return when {
                takeover != null -> InUseBadge(session?.siliconId ?: "A Silicon", "is waiting for you", takeover.reason, waiting = true)
                session != null -> InUseBadge(session.siliconId, if (session.stopping) "is stopping…" else "is using this TV", null, waiting = false)
                else -> null
            }
        }
    }
}

/**
 * The bottom-centre badge a TV shows over other apps, in Interface's system: a cobalt pill with a pale
 * cobalt edge, the 3 × 3 pixel indicator, a mono SILICON tag and the Silicon's name in IBM Plex
 * Sans. When a Silicon is waiting for the Carbon it turns ink with an orange-red edge and
 * indicator, and the reason goes on a second line. A plain View: it lives in an accessibility
 * overlay window, outside any Compose host.
 */
class InUseBadgeView(context: Context) : LinearLayout(context) {
    private val density = resources.displayMetrics.density
    private fun dp(v: Float): Int = (v * density).toInt()

    private val sans = plex(context, R.font.ibm_plex_sans, 400, variable = true)
    private val sansSemiBold = plex(context, R.font.ibm_plex_sans, 600, variable = true)
    private val mono = plex(context, R.font.ibm_plex_mono_medium, 500, variable = false)

    private val pixel = PixelDrawable()
    private val indicator = View(context).apply { background = pixel }
    private val tag = TextView(context).apply {
        text = "SILICON"
        typeface = mono
        includeFontPadding = false
        letterSpacing = 0.08f
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
        setTextColor(Tokens.TagInk.toArgb())
        background = GradientDrawable().apply {
            cornerRadius = dp(999f).toFloat()
            setColor(Tokens.CobaltPale.toArgb())
            setStroke(dp(1f), Tokens.CobaltEdge.toArgb())
        }
        setPadding(dp(8f), dp(3f), dp(8f), dp(3f))
    }
    private val title = TextView(context).apply {
        typeface = sans
        includeFontPadding = false
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 17f)
        setTextColor(android.graphics.Color.WHITE)
        maxLines = 1
        ellipsize = TextUtils.TruncateAt.END
        maxWidth = dp(460f)
    }
    private val detail = TextView(context).apply {
        typeface = sans
        includeFontPadding = false
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
        setTextColor(Color.White.copy(alpha = 0.84f).toArgb())
        maxLines = 1
        ellipsize = TextUtils.TruncateAt.END
        maxWidth = dp(460f)
    }

    init {
        orientation = HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        setPadding(dp(16f), dp(11f), dp(20f), dp(11f))
        addView(indicator, LayoutParams(dp(12f), dp(12f)))
        addView(tag, LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.WRAP_CONTENT).apply { marginStart = dp(12f) })
        val lines = LinearLayout(context).apply {
            orientation = VERTICAL
            addView(title)
            addView(detail, LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.WRAP_CONTENT).apply { topMargin = dp(4f) })
        }
        addView(lines, LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.WRAP_CONTENT).apply { marginStart = dp(10f) })
        // TalkBack reads the badge as one sentence, not as three fragments.
        importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_YES
        lines.importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO_HIDE_DESCENDANTS
        tag.importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO
        indicator.importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO
    }

    fun bind(badge: InUseBadge) {
        title.text = SpannableStringBuilder()
            .append(badge.silicon, TypefaceCompatSpan(sansSemiBold), Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            .append(' ')
            .append(badge.line)
        detail.text = badge.detail
        detail.visibility = if (badge.detail == null) GONE else VISIBLE
        pixel.color = if (badge.waiting) Tokens.Stop.toArgb() else android.graphics.Color.WHITE
        background = GradientDrawable().apply {
            // A stadium on one line; a softer card corner once the reason adds a second.
            cornerRadius = dp(if (badge.detail == null) 999f else 14f).toFloat()
            setColor(if (badge.waiting) Tokens.Ink.copy(alpha = 0.94f).toArgb() else Tokens.Cobalt.toArgb())
            setStroke(dp(1f), if (badge.waiting) Tokens.Stop.toArgb() else Tokens.CobaltEdge.toArgb())
        }
        contentDescription = badge.spoken
    }

    private companion object {
        /**
         * A bundled font at a fixed weight (the Plex Sans file is variable, so its axis is set too).
         * Font.Builder is Android 10+; before that Typeface.Builder (Android 8+) reads a copy of the
         * font in the app's no-backup files, since it can't read a resource directly.
         */
        fun plex(context: Context, id: Int, weight: Int, variable: Boolean): Typeface = runCatching {
            if (Build.VERSION.SDK_INT >= 29) {
                val font = Font.Builder(context.resources, id).setWeight(weight).apply {
                    if (variable) setFontVariationSettings("'wght' $weight")
                }.build()
                Typeface.CustomFallbackBuilder(FontFamily.Builder(font).build()).setSystemFallback("sans-serif").build()
            } else {
                val file = File(context.noBackupFilesDir, "fonts/${context.resources.getResourceEntryName(id)}.ttf")
                if (!file.exists()) {
                    file.parentFile?.mkdirs()
                    val part = File(file.path + ".part")
                    context.resources.openRawResource(id).use { input -> part.outputStream().use { input.copyTo(it) } }
                    part.renameTo(file)
                }
                Typeface.Builder(file).setWeight(weight).apply {
                    if (variable) setFontVariationSettings("'wght' $weight")
                }.setFallback("sans-serif").build() ?: error("The font didn't load")
            }
        }.getOrElse { Typeface.create(Typeface.SANS_SERIF, if (weight >= 600) Typeface.BOLD else Typeface.NORMAL) }
    }
}

/** Compose's [PixelIndicator] as a Drawable: the 3 × 3 block of printed dots, lit. */
private class PixelDrawable : Drawable() {
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG)
    var color: Int = android.graphics.Color.WHITE
        set(value) {
            field = value
            invalidateSelf()
        }

    override fun draw(canvas: Canvas) {
        paint.color = color
        val c = bounds.width() / 3f
        for (y in 0 until 3) for (x in 0 until 3) {
            if ((x + y) % 2 == 0) {
                val l = bounds.left + x * c
                val t = bounds.top + y * c
                canvas.drawRect(l, t, l + c * 0.86f, t + c * 0.86f, paint)
            }
        }
    }

    override fun setAlpha(alpha: Int) {
        paint.alpha = alpha
    }

    override fun setColorFilter(colorFilter: ColorFilter?) {
        paint.colorFilter = colorFilter
    }

    @Deprecated("Deprecated in Java")
    override fun getOpacity(): Int = PixelFormat.TRANSLUCENT
}

/** `TypefaceSpan(Typeface)` is Android 9+; this sets the typeface the same way on every version. */
private class TypefaceCompatSpan(private val typeface: Typeface) : MetricAffectingSpan() {
    override fun updateDrawState(paint: TextPaint) {
        paint.typeface = typeface
    }

    override fun updateMeasureState(paint: TextPaint) {
        paint.typeface = typeface
    }
}

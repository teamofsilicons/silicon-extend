package com.teamofsilicons.bridge.a11y

import android.graphics.Color
import android.graphics.PixelFormat
import android.graphics.drawable.GradientDrawable
import android.util.TypedValue
import android.view.Gravity
import android.view.WindowManager
import android.widget.TextView
import com.teamofsilicons.bridge.core.UiState

/**
 * The TV's in-use indicator: a small badge in the top-right corner, drawn as an accessibility
 * overlay so it needs no "display over other apps" permission. It can't take focus or touches, so
 * it never gets in the way of the remote.
 */
class TvBadgeOverlay(private val service: BridgeAccessibilityService) {
    private val wm = service.getSystemService(WindowManager::class.java)
    private var view: TextView? = null

    fun render(state: UiState) {
        val text = when {
            !state.isTv -> null
            state.takeover != null -> "${state.session?.siliconId ?: "A Silicon"} is waiting for you: ${state.takeover.reason}"
            state.session != null -> "${state.session.siliconId} is using this TV" + if (state.session.stopping) " (stopping…)" else ""
            else -> null
        }
        if (text == null) remove() else show(text)
    }

    private fun show(text: String) {
        val v = view ?: TextView(service).also { tv ->
            tv.setTextColor(Color.WHITE)
            tv.setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
            val pad = dp(10)
            tv.setPadding(pad * 2, pad, pad * 2, pad)
            tv.background = GradientDrawable().apply {
                cornerRadius = dp(18).toFloat()
                setColor(Color.argb(220, 20, 24, 32))
                setStroke(dp(2), Color.rgb(91, 140, 255))
            }
            val lp = WindowManager.LayoutParams(
                WindowManager.LayoutParams.WRAP_CONTENT,
                WindowManager.LayoutParams.WRAP_CONTENT,
                WindowManager.LayoutParams.TYPE_ACCESSIBILITY_OVERLAY,
                WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or
                    WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE or
                    WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN,
                PixelFormat.TRANSLUCENT,
            ).apply {
                gravity = Gravity.TOP or Gravity.END
                x = dp(24)
                y = dp(24)
                title = "Silicon Bridge in-use badge"
            }
            runCatching { wm.addView(tv, lp) }.onFailure { return }
            view = tv
        }
        v.text = "● $text"
    }

    fun remove() {
        view?.let { runCatching { wm.removeView(it) } }
        view = null
    }

    private fun dp(v: Int): Int = (v * service.resources.displayMetrics.density).toInt()
}

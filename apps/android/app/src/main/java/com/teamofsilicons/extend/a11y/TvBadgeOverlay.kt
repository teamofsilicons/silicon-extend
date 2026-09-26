package com.teamofsilicons.extend.a11y

import android.graphics.PixelFormat
import android.view.Gravity
import android.view.WindowManager
import com.teamofsilicons.extend.core.UiState
import com.teamofsilicons.extend.ui.InUseBadge
import com.teamofsilicons.extend.ui.InUseBadgeView

/**
 * The TV's in-use indicator: a small badge in the top-right corner, drawn as an accessibility
 * overlay so it needs no "display over other apps" permission. It can't take focus or touches, so
 * it never gets in the way of the remote.
 */
class TvBadgeOverlay(private val service: ExtendAccessibilityService) {
    private val wm = service.getSystemService(WindowManager::class.java)
    private var view: InUseBadgeView? = null

    fun render(state: UiState) {
        val badge = InUseBadge.from(state)
        if (badge == null) remove() else show(badge)
    }

    private fun show(badge: InUseBadge) {
        // The view and its look live in ui/InUseBadge.kt, with the rest of the app's styling.
        val v = view ?: InUseBadgeView(service).also { tv ->
            val lp = WindowManager.LayoutParams(
                WindowManager.LayoutParams.WRAP_CONTENT,
                WindowManager.LayoutParams.WRAP_CONTENT,
                WindowManager.LayoutParams.TYPE_ACCESSIBILITY_OVERLAY,
                WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or
                    WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE or
                    WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN,
                PixelFormat.TRANSLUCENT,
            ).apply {
                // Inside a TV's overscan-safe area (48 dp from the side, at least 27 dp from the
                // top), centred on the Extend app's top bar row so it sits in line with it there.
                gravity = Gravity.TOP or Gravity.END
                x = dp(48)
                y = dp(40)
                title = "Silicon Extend in-use badge"
            }
            runCatching { wm.addView(tv, lp) }.onFailure { return }
            view = tv
        }
        v.bind(badge)
    }

    fun remove() {
        view?.let { runCatching { wm.removeView(it) } }
        view = null
    }

    private fun dp(v: Int): Int = (v * service.resources.displayMetrics.density).toInt()
}

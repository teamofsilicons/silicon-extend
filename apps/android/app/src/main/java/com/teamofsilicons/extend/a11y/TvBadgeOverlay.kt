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
 *
 * While a Silicon works on a TV that is awake, the badge's window also keeps the screen on
 * ([KeepScreenOn]): Android's screen timeout would otherwise send the TV to standby mid-task. It
 * never turns a screen on, and the Carbon can still put the TV in standby.
 */
class TvBadgeOverlay(private val service: ExtendAccessibilityService) {
    private val wm = service.getSystemService(WindowManager::class.java)
    private var view: InUseBadgeView? = null
    private var params: WindowManager.LayoutParams? = null

    fun render(state: UiState) {
        val badge = InUseBadge.from(state)
        if (badge == null) remove() else show(badge, KeepScreenOn.wanted(state))
    }

    private fun show(badge: InUseBadge, keepOn: Boolean) {
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
                if (keepOn) flags = flags or WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON
            }
            runCatching { wm.addView(tv, lp) }.onFailure { return }
            view = tv
            params = lp
        }
        v.bind(badge)
        val lp = params ?: return
        val on = lp.flags and WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON != 0
        if (on != keepOn) {
            lp.flags = if (keepOn) lp.flags or WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON
            else lp.flags and WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON.inv()
            runCatching { wm.updateViewLayout(v, lp) }
        }
    }

    fun remove() {
        view?.let { runCatching { wm.removeView(it) } }
        view = null
        params = null
    }

    private fun dp(v: Int): Int = (v * service.resources.displayMetrics.density).toInt()
}

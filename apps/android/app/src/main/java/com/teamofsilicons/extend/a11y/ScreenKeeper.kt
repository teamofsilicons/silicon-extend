package com.teamofsilicons.extend.a11y

import android.graphics.PixelFormat
import android.view.Gravity
import android.view.View
import android.view.WindowManager
import com.teamofsilicons.extend.core.UiState

/**
 * When the screen stays on (UNDERSTANDING.md, Waking a device): while a Silicon uses the device and
 * the device is awake, so the screen timeout doesn't lock it mid-task. Not during a takeover (the
 * Carbon is at the device), not while stopping, not while the device isn't awake: Extend never
 * turns a screen on, and the Carbon can still lock the device at any time.
 */
object KeepScreenOn {
    fun wanted(state: UiState): Boolean {
        val session = state.session ?: return false
        return !session.stopping && state.takeover == null && state.awake?.awake == true
    }
}

/**
 * Keeps a phone's or tablet's screen on while [KeepScreenOn] wants it: one transparent pixel in an
 * accessibility overlay (no "display over other apps" permission), which can't take touches or
 * focus, with FLAG_KEEP_SCREEN_ON. Android honours that flag only for a window it has drawn, which
 * a zero-sized window may never be, hence one pixel. No wake lock: those can turn a screen on.
 */
class ScreenKeeper(private val service: ExtendAccessibilityService) {
    private val wm = service.getSystemService(WindowManager::class.java)
    private var view: View? = null

    fun render(keepOn: Boolean) {
        if (keepOn) show() else remove()
    }

    private fun show() {
        if (view != null) return
        val v = View(service)
        val lp = WindowManager.LayoutParams(
            1,
            1,
            WindowManager.LayoutParams.TYPE_ACCESSIBILITY_OVERLAY,
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or
                WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE or
                WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN or
                WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON,
            PixelFormat.TRANSLUCENT,
        ).apply {
            gravity = Gravity.TOP or Gravity.START
            title = TITLE
        }
        runCatching { wm.addView(v, lp) }.onFailure { return }
        view = v
    }

    private fun remove() {
        view?.let { runCatching { wm.removeView(it) } }
        view = null
    }

    companion object {
        /** The window's title, as `dumpsys window` lists it (the emulator tests look for it). */
        const val TITLE = "Silicon Extend keeps the screen on"
    }
}

package com.teamofsilicons.extend.a11y

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.content.ComponentName
import android.content.Context
import android.graphics.Bitmap
import android.graphics.Path
import android.graphics.Rect
import android.os.Build
import android.provider.Settings
import android.view.Display
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityWindowInfo
import com.teamofsilicons.extend.Extend
import com.teamofsilicons.extend.driver.Bounds
import com.teamofsilicons.extend.driver.Capture
import com.teamofsilicons.extend.driver.CommandFailure
import com.teamofsilicons.extend.driver.UiNode
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlin.coroutines.resume

/**
 * How the Extend app sees and acts on the screen. This is the Android app's deliberate
 * replacement for agent-device's ADB-driven Android helpers: an AccessibilityService reads every
 * window's element tree, dispatches touch gestures, presses system buttons and takes screenshots
 * without Android debugging, so it works on any network and survives reboots.
 */
class ExtendAccessibilityService : AccessibilityService() {
    private var scope: CoroutineScope? = null
    private var badge: TvBadgeOverlay? = null

    @Volatile var foregroundPackage: String? = null
        private set
    @Volatile var foregroundActivity: String? = null
        private set

    override fun onServiceConnected() {
        super.onServiceConnected()
        instance = this
        _connected.value = true
        val s = CoroutineScope(SupervisorJob() + Dispatchers.Main)
        scope = s
        badge = TvBadgeOverlay(this)
        s.launch {
            Extend.get(this@ExtendAccessibilityService).state.collect { st ->
                badge?.render(st)
            }
        }
        Extend.get(this).onCapabilitiesMayHaveChanged()
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {
        if (event == null) return
        if (event.eventType == AccessibilityEvent.TYPE_WINDOW_STATE_CHANGED) {
            val pkg = event.packageName?.toString() ?: return
            val cls = event.className?.toString()
            if (cls != null && isActivity(pkg, cls)) {
                foregroundPackage = pkg
                foregroundActivity = cls
            } else if (foregroundPackage == null) {
                foregroundPackage = pkg
            }
        }
    }

    private fun isActivity(pkg: String, cls: String): Boolean = try {
        packageManager.getActivityInfo(ComponentName(pkg, cls), 0)
        true
    } catch (_: Exception) {
        false
    }

    override fun onInterrupt() {}

    override fun onDestroy() {
        disconnect()
        super.onDestroy()
    }

    override fun onUnbind(intent: android.content.Intent?): Boolean {
        disconnect()
        return super.onUnbind(intent)
    }

    private fun disconnect() {
        if (instance === this) instance = null
        _connected.value = false
        badge?.remove()
        badge = null
        scope?.cancel()
        scope = null
        runCatching { Extend.get(this).onCapabilitiesMayHaveChanged() }
    }

    // ───────────── Reading the screen ─────────────

    fun screenBounds(): Bounds {
        val wm = getSystemService(android.view.WindowManager::class.java)
        val b = wm.currentWindowMetrics.bounds
        return Bounds(b.left, b.top, b.right, b.bottom)
    }

    /** Every window's tree, bottom window first. Our own overlay badge is left out. */
    fun capture(): Capture {
        // Compose can update bounds without invalidating every cached virtual descendant.
        if (android.os.Build.VERSION.SDK_INT >= 33) clearCache()
        val screen = screenBounds()
        val roots = ArrayList<UiNode>()
        val windows = runCatching { windows }.getOrDefault(emptyList())
        for (w in windows.reversed()) {
            if (w.type == AccessibilityWindowInfo.TYPE_ACCESSIBILITY_OVERLAY) continue
            val root = w.root ?: continue
            roots += convert(root, windowType(w.type), w.title?.toString(), IntArray(1))
        }
        if (roots.isEmpty()) {
            rootInActiveWindow?.let { roots += convert(it, "application", null, IntArray(1)) }
        }
        if (roots.isEmpty()) {
            throw CommandFailure(
                CommandFailure.NOT_READY,
                "No window on screen could be read. The screen may be off or locked, or Android is showing a screen that hides its content from accessibility.",
            )
        }
        val fg = foregroundPackage ?: roots.lastOrNull { it.windowType == "application" }?.packageName
        return Capture(roots, screen, fg)
    }

    /** The package of the app in front: the focused (else top) application window, else the last window event. */
    fun currentForeground(): String? = runCatching {
        val apps = windows.filter { it.type == AccessibilityWindowInfo.TYPE_APPLICATION }
        (apps.firstOrNull { it.isFocused } ?: apps.firstOrNull { it.isActive } ?: apps.firstOrNull())?.root?.packageName?.toString()
    }.getOrNull() ?: foregroundPackage

    fun imeVisible(): Boolean =
        runCatching { windows.any { it.type == AccessibilityWindowInfo.TYPE_INPUT_METHOD } }.getOrDefault(false)

    fun imeBounds(): Bounds? = runCatching {
        windows.firstOrNull { it.type == AccessibilityWindowInfo.TYPE_INPUT_METHOD }?.let {
            val r = Rect()
            it.getBoundsInScreen(r)
            Bounds(r.left, r.top, r.right, r.bottom)
        }
    }.getOrNull()

    /** The node holding input focus, in any window. */
    fun focusedInput(): AccessibilityNodeInfo? {
        rootInActiveWindow?.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)?.let { return it }
        for (w in runCatching { windows }.getOrDefault(emptyList())) {
            w.root?.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)?.let { return it }
        }
        return null
    }

    private fun windowType(type: Int): String = when (type) {
        AccessibilityWindowInfo.TYPE_APPLICATION -> "application"
        AccessibilityWindowInfo.TYPE_INPUT_METHOD -> "input_method"
        AccessibilityWindowInfo.TYPE_SYSTEM -> "system"
        AccessibilityWindowInfo.TYPE_ACCESSIBILITY_OVERLAY -> "accessibility_overlay"
        AccessibilityWindowInfo.TYPE_SPLIT_SCREEN_DIVIDER -> "split_screen_divider"
        AccessibilityWindowInfo.TYPE_MAGNIFICATION_OVERLAY -> "magnification_overlay"
        else -> "other"
    }

    private fun convert(info: AccessibilityNodeInfo, windowType: String, windowTitle: String?, count: IntArray): UiNode {
        count[0]++
        val children = ArrayList<UiNode>(info.childCount)
        if (count[0] < MAX_CAPTURE_NODES) {
            for (i in 0 until info.childCount) {
                val child = runCatching { info.getChild(i) }.getOrNull() ?: continue
                children += convert(child, windowType, windowTitle, count)
                if (count[0] >= MAX_CAPTURE_NODES) break
            }
        }
        val r = Rect()
        info.getBoundsInScreen(r)
        return UiNode(
            className = info.className?.toString(),
            text = info.text?.toString(),
            contentDescription = info.contentDescription?.toString(),
            resourceId = info.viewIdResourceName,
            packageName = info.packageName?.toString(),
            hint = info.hintText?.toString(),
            bounds = Bounds(r.left, r.top, r.right, r.bottom),
            clickable = info.isClickable,
            longClickable = info.isLongClickable,
            focusable = info.isFocusable,
            focused = info.isFocused,
            selected = info.isSelected,
            checked = if (info.isCheckable) info.isChecked else null,
            enabled = info.isEnabled,
            editable = info.isEditable,
            password = info.isPassword,
            scrollable = info.isScrollable,
            visibleToUser = info.isVisibleToUser,
            hintShowing = info.isShowingHintText,
            heading = info.isHeading,
            inputType = info.inputType,
            windowType = windowType,
            windowTitle = windowTitle,
            handle = info,
            children = children,
        )
    }

    // ───────────── Acting ─────────────

    data class Stroke(val points: List<Pair<Float, Float>>, val startMs: Long, val durationMs: Long)

    /** Dispatches one gesture made of [strokes] (up to 10 fingers) and waits for it to finish. */
    suspend fun gesture(strokes: List<Stroke>): Boolean {
        val builder = GestureDescription.Builder()
        for (s in strokes) {
            val path = Path()
            val (x0, y0) = s.points.first()
            path.moveTo(x0.coerceAtLeast(0f), y0.coerceAtLeast(0f))
            for ((x, y) in s.points.drop(1)) path.lineTo(x.coerceAtLeast(0f), y.coerceAtLeast(0f))
            builder.addStroke(GestureDescription.StrokeDescription(path, s.startMs, s.durationMs.coerceAtLeast(1)))
        }
        return dispatch(builder.build())
    }

    /** A continuous one-finger gesture made of phases (hold, move, hold) with the finger kept down. */
    suspend fun continuousStroke(phases: List<Stroke>): Boolean {
        var previous: GestureDescription.StrokeDescription? = null
        for ((i, phase) in phases.withIndex()) {
            val path = Path()
            val (x0, y0) = phase.points.first()
            path.moveTo(x0, y0)
            for ((x, y) in phase.points.drop(1)) path.lineTo(x, y)
            val willContinue = i < phases.size - 1
            val stroke = previous?.continueStroke(path, 0, phase.durationMs.coerceAtLeast(1), willContinue)
                ?: GestureDescription.StrokeDescription(path, 0, phase.durationMs.coerceAtLeast(1), willContinue)
            if (!dispatch(GestureDescription.Builder().addStroke(stroke).build())) return false
            previous = stroke
        }
        return true
    }

    private suspend fun dispatch(gesture: GestureDescription): Boolean = suspendCancellableCoroutine { cont ->
        val ok = dispatchGesture(gesture, object : GestureResultCallback() {
            override fun onCompleted(gestureDescription: GestureDescription?) {
                if (cont.isActive) cont.resume(true)
            }

            override fun onCancelled(gestureDescription: GestureDescription?) {
                if (cont.isActive) cont.resume(false)
            }
        }, null)
        if (!ok && cont.isActive) cont.resume(false)
    }

    suspend fun tap(x: Float, y: Float, holdMs: Long = 50): Boolean =
        gesture(listOf(Stroke(listOf(x to y), 0, holdMs)))

    fun global(action: Int): Boolean = performGlobalAction(action)

    /** A screenshot of the default display, or a precise failure. */
    suspend fun screenshot(): Bitmap {
        var lastError = 0
        repeat(4) {
            val (bmp, err) = takeOnce()
            if (bmp != null) return bmp
            lastError = err
            if (err != ERROR_TAKE_SCREENSHOT_INTERVAL_TIME_SHORT) {
                throw CommandFailure(CommandFailure.ACTION_FAILED, screenshotError(err))
            }
            delay(1_050L)
        }
        throw CommandFailure(CommandFailure.ACTION_FAILED, screenshotError(lastError))
    }

    private fun screenshotError(code: Int): String = when (code) {
        ERROR_TAKE_SCREENSHOT_INTERNAL_ERROR -> "Android failed to take the screenshot (internal error)."
        ERROR_TAKE_SCREENSHOT_NO_ACCESSIBILITY_ACCESS -> "Silicon Extend's accessibility access doesn't allow screenshots."
        ERROR_TAKE_SCREENSHOT_INTERVAL_TIME_SHORT -> "Android allows one screenshot per second; try again."
        ERROR_TAKE_SCREENSHOT_INVALID_DISPLAY -> "The display can't be captured."
        ERROR_TAKE_SCREENSHOT_SECURE_WINDOW -> "The app on screen blocks screenshots (a secure window, e.g. a banking or password screen)."
        else -> "Android refused the screenshot (error $code)."
    }

    private suspend fun takeOnce(): Pair<Bitmap?, Int> = suspendCancellableCoroutine { cont ->
        takeScreenshot(Display.DEFAULT_DISPLAY, mainExecutor, object : TakeScreenshotCallback {
            override fun onSuccess(screenshot: ScreenshotResult) {
                val buffer = screenshot.hardwareBuffer
                val bmp = try {
                    Bitmap.wrapHardwareBuffer(buffer, screenshot.colorSpace)?.copy(Bitmap.Config.ARGB_8888, false)
                } finally {
                    buffer.close()
                }
                if (cont.isActive) cont.resume(bmp to (if (bmp == null) ERROR_TAKE_SCREENSHOT_INTERNAL_ERROR else 0))
            }

            override fun onFailure(errorCode: Int) {
                if (cont.isActive) cont.resume(null to errorCode)
            }
        })
    }

    companion object {
        private const val MAX_CAPTURE_NODES = 6000
        @Volatile var instance: ExtendAccessibilityService? = null
            private set
        private val _connected = MutableStateFlow(false)
        val connected: StateFlow<Boolean> = _connected

        fun component(context: Context) = ComponentName(context, ExtendAccessibilityService::class.java)

        /** Whether the Carbon turned the service on in Settings (it may still be starting). */
        fun isEnabledInSettings(context: Context): Boolean {
            val enabled = Settings.Secure.getString(context.contentResolver, Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES) ?: return false
            val me = component(context)
            return enabled.split(':').any { ComponentName.unflattenFromString(it) == me }
        }

        val dpadSupported: Boolean get() = Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU
    }
}

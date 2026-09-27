package com.teamofsilicons.extend.a11y

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.content.ComponentName
import android.content.Context
import android.graphics.Bitmap
import android.graphics.Path
import android.graphics.Point
import android.graphics.Rect
import android.os.Build
import android.provider.Settings
import android.view.Display
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityWindowInfo
import androidx.annotation.ChecksSdkIntAtLeast
import androidx.annotation.RequiresApi
import androidx.core.view.accessibility.AccessibilityNodeInfoCompat
import com.teamofsilicons.extend.Extend
import com.teamofsilicons.extend.driver.Bounds
import com.teamofsilicons.extend.driver.Capture
import com.teamofsilicons.extend.driver.CommandFailure
import com.teamofsilicons.extend.driver.ScreenshotPath
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
 * replacement for the device engine's Android debugging helpers: an AccessibilityService reads
 * every window's element tree, dispatches touch gestures, presses system buttons and takes
 * screenshots without Android debugging, so it works on any network and survives reboots.
 *
 * It also draws what must show over other apps without the "display over other apps" permission:
 * the TV's in-use badge, and on phones the invisible window that keeps an awake screen on while a
 * Silicon works ([ScreenKeeper]).
 */
class ExtendAccessibilityService : AccessibilityService() {
    private var scope: CoroutineScope? = null
    private var badge: TvBadgeOverlay? = null
    private var keeper: ScreenKeeper? = null

    /** Counts what changes on screen ([CHANGE_EVENTS]), so a click can tell whether it did anything. */
    private val changeCount = java.util.concurrent.atomic.AtomicLong()
    val changes: Long get() = changeCount.get()

    /** A list scrolled since the last capture (Android 8.0/8.1 re-read the window roots then). */
    @Volatile private var scrolled = false

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
        keeper = ScreenKeeper(this)
        s.launch {
            Extend.get(this@ExtendAccessibilityService).state.collect { st ->
                badge?.render(st)
                // A TV's badge window keeps its screen on; a phone needs a window of its own.
                keeper?.render(!st.isTv && KeepScreenOn.wanted(st))
            }
        }
        Extend.get(this).onCapabilitiesMayHaveChanged()
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {
        if (event == null) return
        if (event.eventType and CHANGE_EVENTS != 0) changeCount.incrementAndGet()
        // Android 8.0/8.1 clear a scrolled list's cached nodes only for services that receive
        // TYPE_VIEW_SCROLLED (Android 9+ always deliver it to the cache), which is why the service
        // asks for it (accessibility_service_config.xml); without it a snapshot after a scroll
        // showed the rows from before it.
        if (event.eventType == AccessibilityEvent.TYPE_VIEW_SCROLLED) scrolled = true
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
        keeper?.render(false)
        keeper = null
        scope?.cancel()
        scope = null
        runCatching { Extend.get(this).onCapabilitiesMayHaveChanged() }
    }

    // ───────────── Reading the screen ─────────────

    fun screenBounds(): Bounds {
        val wm = getSystemService(android.view.WindowManager::class.java)
        if (Build.VERSION.SDK_INT >= 30) {
            val b = wm.currentWindowMetrics.bounds
            return Bounds(b.left, b.top, b.right, b.bottom)
        }
        // Before Android 11: the default display's full size, system bars included (as WindowMetrics gives).
        val size = Point()
        @Suppress("DEPRECATION")
        wm.defaultDisplay.getRealSize(size)
        return Bounds(0, 0, size.x, size.y)
    }

    /** Every window's tree, bottom window first. Our own overlay badge is left out. */
    fun capture(): Capture {
        // Compose can update bounds without invalidating every cached virtual descendant.
        if (android.os.Build.VERSION.SDK_INT >= 33) clearCache()
        // Before Android 9, a scroll's cache clearing could miss a window root; read them afresh.
        val refreshRoots = Build.VERSION.SDK_INT < 28 && scrolled
        scrolled = false
        val screen = screenBounds()
        val roots = ArrayList<UiNode>()
        val windows = runCatching { windows }.getOrDefault(emptyList())
        for (w in windows.reversed()) {
            if (w.type == AccessibilityWindowInfo.TYPE_ACCESSIBILITY_OVERLAY) continue
            val root = w.root ?: continue
            if (refreshRoots) runCatching { root.refresh() }
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
            // AccessibilityNodeInfo.isHeading is Android 9+; the compat wrapper reads the same flag before that.
            heading = AccessibilityNodeInfoCompat.wrap(info).isHeading,
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

    /** Waits up to [timeoutMs] for anything on screen to change after [since] ([changes]); whether it did. */
    suspend fun awaitChange(since: Long, timeoutMs: Long): Boolean {
        val until = android.os.SystemClock.elapsedRealtime() + timeoutMs
        while (changeCount.get() == since) {
            if (android.os.SystemClock.elapsedRealtime() >= until) return false
            delay(40)
        }
        return true
    }

    fun global(action: Int): Boolean = performGlobalAction(action)

    /**
     * A screenshot of the default display, or a precise failure. Accessibility screenshots exist
     * from Android 11; before that the command executor uses Android debugging's screencap
     * ([com.teamofsilicons.extend.driver.ScreenshotPath]) and never calls this.
     */
    suspend fun screenshot(): Bitmap {
        if (Build.VERSION.SDK_INT < ScreenshotPath.ACCESSIBILITY_SDK) throw CommandFailure.unsupported(
            "Accessibility screenshots need Android 11; this device runs Android ${Build.VERSION.RELEASE}. Connect Android debugging in the Extend app's setup to take screenshots here.",
        )
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

    @RequiresApi(30)
    private fun screenshotError(code: Int): String = when (code) {
        ERROR_TAKE_SCREENSHOT_INTERNAL_ERROR -> "Android failed to take the screenshot (internal error)."
        ERROR_TAKE_SCREENSHOT_NO_ACCESSIBILITY_ACCESS -> "Silicon Extend's accessibility access doesn't allow screenshots."
        ERROR_TAKE_SCREENSHOT_INTERVAL_TIME_SHORT -> "Android allows one screenshot per second; try again."
        ERROR_TAKE_SCREENSHOT_INVALID_DISPLAY -> "The display can't be captured."
        ERROR_TAKE_SCREENSHOT_SECURE_WINDOW -> "The app on screen blocks screenshots (a secure window, e.g. a banking or password screen)."
        else -> "Android refused the screenshot (error $code)."
    }

    /**
     * The Android 11 screenshot call lives in its own class, so this service's class never refers
     * to `TakeScreenshotCallback`: Android 8–10 then verify it without falling back to the
     * interpreter for it.
     */
    @RequiresApi(30)
    private suspend fun takeOnce(): Pair<Bitmap?, Int> = Api30Screenshot.take(this)

    companion object {
        private const val MAX_CAPTURE_NODES = 6000

        /** Events that mean the screen changed: a click was handled, a window or its content changed. */
        val CHANGE_EVENTS = AccessibilityEvent.TYPE_VIEW_CLICKED or AccessibilityEvent.TYPE_VIEW_SELECTED or
            AccessibilityEvent.TYPE_VIEW_SCROLLED or AccessibilityEvent.TYPE_WINDOW_STATE_CHANGED or
            AccessibilityEvent.TYPE_WINDOWS_CHANGED or AccessibilityEvent.TYPE_WINDOW_CONTENT_CHANGED
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

        /** GLOBAL_ACTION_DPAD_* exist from Android 13. */
        @get:ChecksSdkIntAtLeast(api = Build.VERSION_CODES.TIRAMISU)
        val dpadSupported: Boolean get() = Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU
    }
}

/** `AccessibilityService.takeScreenshot` (Android 11+), kept out of the service's own class. */
@RequiresApi(30)
private object Api30Screenshot {
    suspend fun take(service: AccessibilityService): Pair<Bitmap?, Int> = suspendCancellableCoroutine { cont ->
        service.takeScreenshot(Display.DEFAULT_DISPLAY, service.mainExecutor, object : AccessibilityService.TakeScreenshotCallback {
            override fun onSuccess(screenshot: AccessibilityService.ScreenshotResult) {
                val buffer = screenshot.hardwareBuffer
                val bmp = try {
                    Bitmap.wrapHardwareBuffer(buffer, screenshot.colorSpace)?.copy(Bitmap.Config.ARGB_8888, false)
                } finally {
                    buffer.close()
                }
                if (cont.isActive) cont.resume(bmp to (if (bmp == null) AccessibilityService.ERROR_TAKE_SCREENSHOT_INTERNAL_ERROR else 0))
            }

            override fun onFailure(errorCode: Int) {
                if (cont.isActive) cont.resume(null to errorCode)
            }
        })
    }
}

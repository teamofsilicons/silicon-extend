package com.teamofsilicons.extend.display

import android.app.Activity
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Color
import android.net.Uri
import android.os.Bundle
import android.os.SystemClock
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.TextView
import android.widget.VideoView
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.teamofsilicons.extend.Extend
import kotlinx.coroutines.delay
import okhttp3.Request
import java.io.File
import kotlin.concurrent.thread

/**
 * The TV's full-screen display: a link, an image, a video or text a Silicon put up with
 * `display show`. It stays until `display clear` or Back on the remote.
 */
class DisplayActivity : Activity() {
    private lateinit var root: FrameLayout

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        root = FrameLayout(this).apply { setBackgroundColor(Color.BLACK) }
        setContentView(root)
        // The compat controller uses system UI flags before Android 11.
        WindowCompat.getInsetsController(window, window.decorView).let {
            it.hide(WindowInsetsCompat.Type.systemBars())
            it.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
        current = this
        render(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        render(intent)
    }

    override fun onResume() {
        super.onResume()
        current = this
        lastShownAt = SystemClock.elapsedRealtime()
    }

    override fun onDestroy() {
        if (current === this) current = null
        super.onDestroy()
    }

    private fun render(intent: Intent) {
        root.removeAllViews()
        val kind = intent.getStringExtra(EXTRA_KIND) ?: "text"
        val value = intent.getStringExtra(EXTRA_VALUE)
        val file = intent.getStringExtra(EXTRA_FILE)
        val match = FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)
        // Named for accessibility, so a Silicon's snapshot can confirm what the display shows.
        root.contentDescription = "Silicon Extend display: $kind"
        when (kind) {
            "url" -> {
                // Creating a WebView throws when the device has no working WebView provider (common
                // on Android 8–10 TV boxes); the command checks first, this covers a provider that
                // is installed but fails to load. The Carbon sees why, and the command reports it.
                val web = try {
                    WebView(this).apply {
                        @Suppress("SetJavaScriptEnabled")
                        settings.javaScriptEnabled = true
                        settings.domStorageEnabled = true
                        settings.mediaPlaybackRequiresUserGesture = false
                        webViewClient = WebViewClient()
                        loadUrl(value ?: "about:blank")
                    }
                } catch (e: Throwable) {
                    Extend.log("display: no WebView", e)
                    failure = NO_WEBVIEW + " (Android said: ${e.message ?: e.javaClass.simpleName})"
                    failedAt = SystemClock.elapsedRealtime()
                    null
                }
                if (web != null) root.addView(web, match)
                else showText("This screen can't show web pages: no web view is installed.\n${value.orEmpty()}", 28f)
            }
            "image" -> {
                val image = ImageView(this).apply {
                    scaleType = ImageView.ScaleType.FIT_CENTER
                    contentDescription = "Silicon Extend display: image"
                }
                root.addView(image, match)
                when {
                    file != null -> image.setImageBitmap(decode(File(file)))
                    value != null -> thread(name = "display-image") {
                        val bmp = runCatching {
                            Extend.get(this).api.client.newCall(Request.Builder().url(value).build()).execute().use { r ->
                                r.body?.bytes()?.let { BitmapFactory.decodeByteArray(it, 0, it.size) }
                            }
                        }.getOrNull()
                        runOnUiThread { if (bmp != null) image.setImageBitmap(bmp) else showText("Couldn't load the image:\n$value", 28f) }
                    }
                }
            }
            "video" -> {
                val video = VideoView(this)
                val lp = FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT, Gravity.CENTER)
                root.addView(video, lp)
                if (file != null) video.setVideoPath(file) else video.setVideoURI(Uri.parse(value))
                video.setOnPreparedListener { it.isLooping = true; video.start() }
                video.setOnErrorListener { _, what, extra ->
                    showText("Couldn't play the video ($what/$extra)", 28f)
                    true
                }
            }
            else -> showText(value.orEmpty(), 56f)
        }
        if (current === this) lastShownAt = SystemClock.elapsedRealtime()
    }

    private fun showText(text: String, sizeSp: Float) {
        root.removeAllViews()
        val tv = TextView(this).apply {
            this.text = text
            setTextColor(Color.WHITE)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, sizeSp)
            gravity = Gravity.CENTER
            val pad = (48 * resources.displayMetrics.density).toInt()
            setPadding(pad, pad, pad, pad)
            textAlignment = View.TEXT_ALIGNMENT_CENTER
        }
        root.addView(tv, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
    }

    private fun decode(file: File): Bitmap? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(file.absolutePath, bounds)
        var sample = 1
        while (bounds.outWidth / sample > 3840 || bounds.outHeight / sample > 2160) sample *= 2
        return BitmapFactory.decodeFile(file.absolutePath, BitmapFactory.Options().apply { inSampleSize = sample })
    }

    companion object {
        const val EXTRA_KIND = "kind"
        const val EXTRA_VALUE = "value"
        const val EXTRA_FILE = "file"

        @Volatile private var current: DisplayActivity? = null
        @Volatile private var lastShownAt = 0L
        @Volatile private var failure: String? = null
        @Volatile private var failedAt = 0L

        /** Why `display show --url` can't work on this device. */
        const val NO_WEBVIEW = "This device has no web view (Android System WebView isn't installed or is turned off), so display show --url " +
            "can't show a page here. Use display show --image, --video or --text, or open <url> to open the link in a browser if the device has one."

        /**
         * Android has a WebView provider to load (`WebView.getCurrentWebViewPackage`, Android 8+;
         * it doesn't load WebView). False on devices that ship without Android System WebView. If
         * Android can't answer, the page is tried: the activity shows the reason if it fails.
         */
        fun webViewAvailable(): Boolean = runCatching { WebView.getCurrentWebViewPackage() != null }.getOrDefault(true)

        /** Why the display couldn't show what it was asked to after [sinceElapsed], or null. */
        fun failureSince(sinceElapsed: Long): String? = failure.takeIf { failedAt >= sinceElapsed }

        /** Closes the display; false when nothing was showing. */
        fun clear(): Boolean {
            val c = current ?: return false
            c.runOnUiThread { c.finish() }
            return true
        }

        /** Waits for the display to come to the front after [sinceElapsed]. */
        suspend fun awaitShown(timeoutMs: Long, sinceElapsed: Long): Boolean {
            val until = SystemClock.elapsedRealtime() + timeoutMs
            while (SystemClock.elapsedRealtime() < until) {
                if (lastShownAt >= sinceElapsed && current != null) return true
                delay(100)
            }
            return false
        }
    }
}

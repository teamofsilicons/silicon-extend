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
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import okhttp3.Call
import okhttp3.Request
import java.io.File
import java.io.IOException
import java.util.UUID

/**
 * The TV's full-screen display: a link, an image, a video or text a Silicon put up with
 * `display show`. It stays until `display clear` or Back on the remote.
 */
class DisplayActivity : Activity() {
    private lateinit var root: FrameLayout
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private var imageJob: Job? = null
    private var imageCall: Call? = null
    private var webView: WebView? = null
    private var videoView: VideoView? = null
    @Volatile private var requestId: String? = null
    @Volatile private var resumed = false

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
        if (savedInstanceState != null && loads.result == null) {
            intent.getStringExtra(EXTRA_REQUEST_ID)?.let { loads.begin(it) }
        }
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
        resumed = true
    }

    override fun onPause() {
        resumed = false
        super.onPause()
    }

    override fun onDestroy() {
        releaseContent()
        scope.cancel()
        requestId?.let { loads.clear(it) }
        if (current === this) current = null
        super.onDestroy()
    }

    private fun render(intent: Intent) {
        val id = intent.getStringExtra(EXTRA_REQUEST_ID) ?: UUID.randomUUID().toString().also { loads.begin(it) }
        if (loads.result?.requestId != id) return
        releaseContent()
        requestId = id
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
                    fail(id, NO_WEBVIEW + " (Android said: ${e.message ?: e.javaClass.simpleName})")
                    null
                }
                if (web != null) {
                    webView = web
                    root.addView(web, match)
                    loads.complete(id)
                }
            }
            "image" -> {
                val image = ImageView(this).apply {
                    scaleType = ImageView.ScaleType.FIT_CENTER
                    contentDescription = "Silicon Extend display: image"
                }
                root.addView(image, match)
                val call = if (file == null && value != null) {
                    try {
                        Extend.get(this).api.client.newCall(Request.Builder().url(value).build())
                    } catch (e: Exception) { fail(id, "Couldn't load the image: ${e.message}"); return }
                } else null
                imageCall = call
                imageJob = scope.launch {
                    try {
                        val bmp = withContext(Dispatchers.IO) {
                            var downloaded: File? = null
                            try {
                                val source = file?.let(::File) ?: call?.let {
                                    DisplayImage.download(it, cacheDir).also { downloaded = it }
                                } ?: throw IOException("No image was provided")
                                decode(source)
                            } finally { downloaded?.delete() }
                        }
                        if (isCurrent(id)) {
                            image.setImageBitmap(bmp)
                            loads.complete(id)
                        }
                    } catch (e: CancellationException) {
                        throw e
                    } catch (e: Exception) {
                        fail(id, "Couldn't load the image: ${e.message ?: e.javaClass.simpleName}")
                    } catch (_: OutOfMemoryError) {
                        fail(id, "The device doesn't have enough free memory to display this image")
                    }
                }
            }
            "video" -> {
                val video = VideoView(this)
                videoView = video
                val lp = FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT, Gravity.CENTER)
                root.addView(video, lp)
                if (file != null) video.setVideoPath(file) else video.setVideoURI(Uri.parse(value))
                video.setOnPreparedListener {
                    if (isCurrent(id)) { it.isLooping = true; video.start(); loads.complete(id) }
                }
                video.setOnErrorListener { _, what, extra ->
                    fail(id, "Couldn't play the video ($what/$extra)")
                    true
                }
            }
            else -> { showText(value.orEmpty(), 56f); loads.complete(id) }
        }
    }

    private fun isCurrent(id: String) = requestId == id && loads.result?.requestId == id && !isDestroyed

    private fun fail(id: String, message: String) {
        if (!isCurrent(id)) return
        showText(message, 28f)
        loads.complete(id, message)
    }

    private fun releaseContent() {
        imageCall?.cancel()
        imageCall = null
        imageJob?.cancel()
        imageJob = null
        videoView?.stopPlayback()
        videoView = null
        webView?.let { root.removeView(it); it.stopLoading(); it.destroy() }
        webView = null
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

    private fun decode(file: File): Bitmap {
        if (file.length() > DisplayImage.MAX_BYTES) throw IOException("Image exceeds the download limit (${DisplayImage.MAX_BYTES} bytes)")
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(file.absolutePath, bounds)
        val screen = resources.displayMetrics
        val sample = DisplayImage.sampleSize(bounds.outWidth, bounds.outHeight, screen.widthPixels, screen.heightPixels)
        return BitmapFactory.decodeFile(file.absolutePath, BitmapFactory.Options().apply { inSampleSize = sample })
            ?: throw IOException("The file is not a supported image or is damaged")
    }

    companion object {
        const val EXTRA_KIND = "kind"
        const val EXTRA_VALUE = "value"
        const val EXTRA_FILE = "file"
        const val EXTRA_REQUEST_ID = "request_id"

        @Volatile private var current: DisplayActivity? = null
        private val loads = DisplayLoadState()

        /** Why `display show --url` can't work on this device. */
        const val NO_WEBVIEW = "This device has no web view (Android System WebView isn't installed or is turned off), so display show --url " +
            "can't show a page here. Use display show --image, --video or --text, or open <url> to open the link in a browser if the device has one."

        /**
         * Android has a WebView provider to load (`WebView.getCurrentWebViewPackage`, Android 8+;
         * it doesn't load WebView). False on devices that ship without Android System WebView. If
         * Android can't answer, the page is tried: the activity shows the reason if it fails.
         */
        fun webViewAvailable(): Boolean = runCatching { WebView.getCurrentWebViewPackage() != null }.getOrDefault(true)

        fun beginRequest(requestId: String) { loads.begin(requestId) }
        fun failureFor(requestId: String): String? = loads.result?.takeIf { it.requestId == requestId }?.error

        fun cancelRequest(requestId: String) {
            loads.clear(requestId)
            val c = current ?: return
            c.runOnUiThread { if (c.requestId == requestId) { c.releaseContent(); c.finish() } }
        }

        /** Closes the display; false when nothing was showing. */
        fun clear(): Boolean {
            val c = current ?: return false
            c.runOnUiThread { c.requestId?.let { loads.clear(it) }; c.releaseContent(); c.finish() }
            return true
        }

        /** Waits for this request's media to load and reach the foreground, or to fail. */
        suspend fun awaitShown(timeoutMs: Long, requestId: String): Boolean {
            val until = SystemClock.elapsedRealtime() + timeoutMs
            while (SystemClock.elapsedRealtime() < until) {
                val result = loads.result?.takeIf { it.requestId == requestId } ?: return false
                if (result.error != null) return false
                val activity = current
                if (result.ready && activity?.requestId == requestId && activity.resumed) return true
                delay(100)
            }
            return false
        }
    }
}

package com.teamofsilicons.extend.driver

import android.app.Activity
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.os.Bundle
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

/**
 * Android 10+ lets only the app with window focus (or the keyboard) read the clipboard. This
 * invisible activity takes focus for a moment, reads it, and closes. The app underneath loses
 * focus briefly, which can close its keyboard or a menu.
 */
class ClipboardActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        @Suppress("DEPRECATION")
        overridePendingTransition(0, 0)
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (!hasFocus) return
        val cm = getSystemService(ClipboardManager::class.java)
        val text = runCatching { cm.primaryClip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(this)?.toString() }
        pending?.complete(Read(text.getOrNull(), text.exceptionOrNull()?.toString()))
        pending = null
        finish()
        @Suppress("DEPRECATION")
        overridePendingTransition(0, 0)
    }

    private data class Read(val text: String?, val error: String?)

    companion object {
        @Volatile private var pending: CompletableDeferred<Read>? = null

        /** The clipboard's text, or null when it's empty. Started from [context] (the accessibility service). */
        suspend fun read(context: Context): String? {
            val waiter = CompletableDeferred<Read>()
            pending = waiter
            withContext(Dispatchers.Main) {
                context.startActivity(
                    Intent(context, ClipboardActivity::class.java).addFlags(
                        Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_NO_ANIMATION or Intent.FLAG_ACTIVITY_EXCLUDE_FROM_RECENTS,
                    ),
                )
            }
            val read = withTimeoutOrNull(4_000) { waiter.await() } ?: throw CommandFailure(
                CommandFailure.ACTION_FAILED,
                "Couldn't read the clipboard: Android lets only the app in front read it, and Silicon Extend couldn't take focus within 4 s " +
                    "(the screen may be locked).",
            )
            read.error?.let { throw CommandFailure(CommandFailure.ACTION_FAILED, "Android refused the clipboard read: $it") }
            return read.text
        }
    }
}

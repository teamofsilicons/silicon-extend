package com.teamofsilicons.extend

import android.content.Intent
import android.app.UiAutomation
import android.graphics.Bitmap
import android.graphics.drawable.BitmapDrawable
import android.os.Build
import android.os.ParcelFileDescriptor
import android.view.View
import android.view.ViewGroup
import android.widget.ImageView
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.teamofsilicons.extend.display.DisplayActivity
import com.teamofsilicons.extend.display.DisplayImage
import com.teamofsilicons.extend.a11y.ExtendAccessibilityService
import com.teamofsilicons.extend.driver.CommandExecutor
import com.teamofsilicons.extend.driver.CommandFailure
import com.teamofsilicons.extend.protocol.ServiceFrame
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.After
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.net.ServerSocket
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread

/** Native decode and asynchronous readiness on a dedicated test emulator. */
@RunWith(AndroidJUnit4::class)
class DisplayTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = instrumentation.targetContext
    private lateinit var dir: File

    @Before fun setUp() {
        assumeTrue("Dedicated emulator only", Build.HARDWARE in setOf("ranchu", "goldfish"))
        dir = File(context.cacheDir, "display-test-${UUID.randomUUID()}").apply { mkdirs() }
        // Instrumentation restarts the app process; rebind its accessibility service in this
        // process and tell UiAutomation to leave real services enabled for command-path tests.
        val automation = instrumentation.getUiAutomation(UiAutomation.FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES)
        for (command in listOf(
            "settings put secure enabled_accessibility_services null",
            "settings put secure enabled_accessibility_services ${context.packageName}/${context.packageName}.a11y.ExtendAccessibilityService",
            "settings put secure accessibility_enabled 1",
        )) {
            ParcelFileDescriptor.AutoCloseInputStream(automation.executeShellCommand(command)).use { it.readBytes() }
        }
    }

    @After fun tearDown() {
        DisplayActivity.clear()
        instrumentation.waitForIdleSync()
        if (::dir.isInitialized) dir.deleteRecursively()
    }

    private fun show(id: String, kind: String, value: String, file: Boolean = false): DisplayActivity {
        DisplayActivity.beginRequest(id)
        val intent = Intent(context, DisplayActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            .putExtra(DisplayActivity.EXTRA_REQUEST_ID, id)
            .putExtra(DisplayActivity.EXTRA_KIND, kind)
            .putExtra(if (file) DisplayActivity.EXTRA_FILE else DisplayActivity.EXTRA_VALUE, value)
        return instrumentation.startActivitySync(intent) as DisplayActivity
    }

    @Test fun damagedImageFailsInsteadOfReportingShown() = runBlocking {
        val file = File(dir, "broken.png").apply { writeText("not an image") }
        show("damaged", "image", file.absolutePath, true)
        assertFalse(DisplayActivity.awaitShown(3_000, "damaged"))
        assertTrue(DisplayActivity.failureFor("damaged")!!.contains("damaged"))
    }

    @Test fun validImageIsDecodedAndShown() = runBlocking {
        val file = File(dir, "valid.png")
        val bitmap = Bitmap.createBitmap(3840, 2160, Bitmap.Config.ARGB_8888)
        bitmap.eraseColor(android.graphics.Color.BLUE)
        file.outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
        bitmap.recycle()
        val activity = show("valid", "image", file.absolutePath, true)
        assertTrue(DisplayActivity.awaitShown(3_000, "valid"))
        assertNull(DisplayActivity.failureFor("valid"))
        fun image(view: View): ImageView? = when (view) {
            is ImageView -> view
            is ViewGroup -> (0 until view.childCount).firstNotNullOfOrNull { image(view.getChildAt(it)) }
            else -> null
        }
        instrumentation.runOnMainSync {
            val decoded = (image(activity.window.decorView)!!.drawable as BitmapDrawable).bitmap
            assertTrue("decoded bitmap exceeds memory budget", decoded.allocationByteCount <= DisplayImage.MAX_PIXELS * 4)
            assertEquals(android.graphics.Color.BLUE, decoded.getPixel(0, 0))
        }
    }

    @Test fun corruptImageReturnsACommandFailure() = runBlocking {
        // Enable accessibility on the dedicated emulator before running this instrumentation lane.
        withTimeout(5_000) { while (ExtendAccessibilityService.instance == null) delay(50) }
        val file = File(dir, "broken-command.png").apply { writeText("not an image") }
        val commands = CommandExecutor(Extend.get(context))
        val result = commands.run(ServiceFrame.Command("display-broken", "display-test",
            command = "display", args = listOf("show", "--image", file.absolutePath)))
        assertFalse(result.toString(), result.ok)
        assertEquals(CommandFailure.ACTION_FAILED, result.error?.code)
        assertTrue(result.error!!.message, result.error!!.message.contains("damaged"))
        commands.endSession("display-test")
    }

    @Test fun aSlowHttpFailureCannotReportSuccessBeforeLoading() = runBlocking {
        val server = ServerSocket(0)
        val requested = CountDownLatch(1)
        val respond = CountDownLatch(1)
        val serving = thread {
            server.accept().use { socket ->
                val reader = socket.getInputStream().bufferedReader()
                while (!reader.readLine().isNullOrEmpty()) { /* request headers */ }
                requested.countDown()
                respond.await(5, TimeUnit.SECONDS)
                socket.getOutputStream().write("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".toByteArray())
            }
        }
        try {
            show("remote", "image", "http://127.0.0.1:${server.localPort}/missing.png")
            assertTrue(requested.await(3, TimeUnit.SECONDS))
            assertFalse("foreground alone is not success", DisplayActivity.awaitShown(200, "remote"))
            assertNull(DisplayActivity.failureFor("remote"))
            respond.countDown()
            assertFalse(DisplayActivity.awaitShown(3_000, "remote"))
            assertTrue(DisplayActivity.failureFor("remote")!!.contains("HTTP 404"))
        } finally { respond.countDown(); serving.join(6_000); server.close() }
    }
}

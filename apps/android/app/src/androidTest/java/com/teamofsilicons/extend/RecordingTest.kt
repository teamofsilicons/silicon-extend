package com.teamofsilicons.extend

import android.content.Intent
import android.media.MediaMetadataRetriever
import android.os.Build
import android.os.SystemClock
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.teamofsilicons.extend.adb.AdbCommand
import com.teamofsilicons.extend.adb.AdbExecutor
import com.teamofsilicons.extend.adb.LocalAdb
import com.teamofsilicons.extend.driver.CommandExecutor
import com.teamofsilicons.extend.driver.CommandFailure
import com.teamofsilicons.extend.protocol.ServiceFrame
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.After
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.UUID

/**
 * Recording through [AdbExecutor] on the dedicated test emulator, after `adb tcpip 5555`. Timing is
 * checked against the test's own clock, not against the muxer's own duration model. Short native
 * segments (3 s) exercise rollover, still screens and bursts without waiting minutes.
 */
@RunWith(AndroidJUnit4::class)
class RecordingTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = instrumentation.targetContext
    private lateinit var adb: LocalAdb
    private lateinit var dir: File

    @Before fun setUp() = runBlocking {
        assumeTrue("Runs only on a dedicated emulator", Build.HARDWARE in setOf("ranchu", "goldfish"))
        adb = Extend.get(context).adb
        assertTrue(adb.lastError, adb.connect(5555))
        dir = File(context.cacheDir, "record-test-${UUID.randomUUID()}").apply { mkdirs() }
    }

    @After fun tearDown() {
        instrumentation.runOnMainSync { RecordingFixtureActivity.current?.finish() }
        if (::dir.isInitialized) dir.deleteRecursively()
    }

    private fun executor(segmentSeconds: Int = 3, durationSeconds: Int = 1800, free: () -> Long = { Long.MAX_VALUE }) =
        AdbExecutor(adb, File(dir, "cache"), File(dir, "state"), context.packageName, { free() }, segmentSeconds, durationSeconds)

    private suspend fun fixture(intervalMs: Int, animateMs: Long = Long.MAX_VALUE) {
        instrumentation.context.startActivity(
            Intent(instrumentation.context, RecordingFixtureActivity::class.java)
                .putExtra("interval_ms", intervalMs).putExtra("animate_ms", animateMs)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TASK),
        )
        delay(1500)
    }

    private fun durationMs(file: File): Long {
        val media = MediaMetadataRetriever()
        try {
            media.setDataSource(file.absolutePath)
            return media.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)!!.toLong()
        } finally { media.release() }
    }

    private fun keep(file: File, name: String) = file.copyTo(File(context.getExternalFilesDir(null), name), overwrite = true)

    /** Records for [ms] and returns the result with the wall-clock time between start and stop. */
    private suspend fun record(executor: AdbExecutor, session: String, ms: Long): Pair<AdbExecutor.Result, Long> {
        val started = SystemClock.elapsedRealtime()
        executor.execute(session, AdbCommand.Record("start", "proof"), emptyList())
        delay(ms)
        val stopAsked = SystemClock.elapsedRealtime()
        val result = withTimeout(60_000) { executor.execute(session, AdbCommand.Record("stop"), emptyList()) }
        return result to (stopAsked - started)
    }

    @Test fun nativeDurationLimit(): Unit = runBlocking { withTimeout(60_000) {
        fixture(intervalMs = 2000)
        val executor = executor(segmentSeconds = 3, durationSeconds = 7)
        try {
            val started = SystemClock.elapsedRealtime()
            executor.execute("limit", AdbCommand.Record("start", "proof"), emptyList())
            // Independent evidence: when the supervisor reports it stopped at its limit.
            val remote = adb.shell("ls -td /data/local/tmp/silicon-extend-* | head -n 1").text.trim()
            while (adb.shell("cat $remote/completed 2>/dev/null", check = false).text.isBlank()) delay(100)
            val ran = SystemClock.elapsedRealtime() - started
            val result = executor.execute("limit", AdbCommand.Record("stop"), emptyList())
            assertTrue(result.text, result.text.contains("duration-limit"))
            assertTrue(result.text, result.text.contains("Combined"))
            val video = result.artifacts.single().file
            val actual = durationMs(video)
            keep(video, "duration-recording-proof.mp4")
            // Sparse frames (one every 2 s) must neither shorten nor stretch the capture.
            assertTrue("Sparse capture that ran $ran ms lasted $actual ms", actual in (ran - 1_500)..(ran + 500))
            assertTrue("The 7 s limit was reached: $actual ms", actual >= 6_500)
        } finally { withContext(NonCancellable) { executor.endAll() } }
    } }

    /** fa92507 regression: a still screen gives one-frame segments whose container duration is 0. */
    @Test fun stillScreenSegmentsKeepTheirWallClockLength(): Unit = runBlocking { withTimeout(60_000) {
        fixture(intervalMs = 80, animateMs = 300)
        val executor = executor(segmentSeconds = 3)
        try {
            val (result, wallMs) = record(executor, "still", 7_500)
            val video = result.artifacts.single().file
            val actual = durationMs(video)
            keep(video, "still-recording-proof.mp4")
            assertTrue("Still capture of $wallMs ms lasted $actual ms (${result.text})", actual in (wallMs - 1_500)..(wallMs + 1_500))
        } finally { withContext(NonCancellable) { executor.endAll() } }
    } }

    /** A burst of frames then a still tail must not pull later segments earlier. */
    @Test fun burstThenStillKeepsLaterSegmentsInPlace(): Unit = runBlocking { withTimeout(60_000) {
        // The fixture animates for its first 4 s: about 2 s of frames 16 ms apart at the start of
        // the first 3 s segment, then a still screen until the recording stops.
        fixture(intervalMs = 16, animateMs = 4_000)
        val executor = executor(segmentSeconds = 3)
        try {
            val (result, wallMs) = record(executor, "burst", 8_000)
            val video = result.artifacts.single().file
            val actual = durationMs(video)
            keep(video, "burst-recording-proof.mp4")
            assertTrue("Burst-then-still capture of $wallMs ms lasted $actual ms", actual in (wallMs - 1_500)..(wallMs + 1_500))
        } finally { withContext(NonCancellable) { executor.endAll() } }
    } }

    /** An empty or unreadable segment is left out; the rest of the recording is saved. */
    @Test fun badSegmentsAreLeftOutInsteadOfFailingEveryRetry(): Unit = runBlocking { withTimeout(60_000) {
        fixture(intervalMs = 80)
        val executor = executor(segmentSeconds = 3)
        try {
            executor.execute("bad", AdbCommand.Record("start", "bad"), emptyList())
            delay(4_000)
            val remote = adb.shell("ls -td /data/local/tmp/silicon-extend-* | head -n 1").text.trim()
            adb.shell("printf 'not an mp4' >$remote/chunk-50.mp4; echo '1.00 2.00 3' >$remote/chunk-50.mp4.timing; : >$remote/chunk-51.mp4")
            delay(1_000)
            val result = executor.execute("bad", AdbCommand.Record("stop"), emptyList())
            assertTrue(result.text, result.text.contains("Left out segment 51"))
            assertTrue(result.text, result.text.contains("Left out segment 52: it was empty"))
            assertTrue(durationMs(result.artifacts.single().file) > 3_000)
            assertEquals("", adb.shell("ls -d $remote 2>/dev/null", check = false).text.trim())
        } finally { withContext(NonCancellable) { executor.endAll() } }
    } }

    /** A rotation changes the picture size of the next segment: it continues in a second file. */
    @Test fun rotationContinuesInASecondFile(): Unit = runBlocking { withTimeout(60_000) {
        fixture(intervalMs = 80)
        val executor = executor(segmentSeconds = 3)
        val before = adb.shell("settings get system accelerometer_rotation; settings get system user_rotation").text.lines()
        try {
            executor.execute("rotate", AdbCommand.Record("start", "turned"), emptyList())
            delay(1_000)
            adb.shell("settings put system accelerometer_rotation 0; settings put system user_rotation 1")
            delay(6_000)
            val result = withTimeout(30_000) { executor.execute("rotate", AdbCommand.Record("stop"), emptyList()) }
            assertTrue(result.text, result.artifacts.size >= 2)
            assertEquals("turned.mp4", result.artifacts[0].file.name)
            assertEquals("turned-2.mp4", result.artifacts[1].file.name)
            assertTrue(result.text, result.text.contains("The screen size changed"))
            result.artifacts.forEach { assertTrue(durationMs(it.file) > 500) }
        } finally {
            withContext(NonCancellable) {
                adb.shell("settings put system user_rotation ${before.getOrNull(1)?.trim()?.takeIf { it != "null" } ?: 0}; " +
                    "settings put system accelerometer_rotation ${before.getOrNull(0)?.trim()?.takeIf { it != "null" } ?: 1}", check = false)
                executor.endAll()
            }
        }
    } }

    /** A finished recording stays until it is delivered, so a failed upload can be retried. */
    @Test fun anUndeliveredRecordingIsSentAgain(): Unit = runBlocking { withTimeout(60_000) {
        fixture(intervalMs = 80)
        val executor = executor()
        try {
            val (first, _) = record(executor, "retry", 2_500)
            val video = first.artifacts.single()
            assertTrue(video.retained)
            // The upload failed or ran out of time: nothing was delivered.
            val again = executor.execute("retry", AdbCommand.Record("stop"), emptyList())
            assertEquals(video, again.artifacts.single())
            assertTrue(again.text, again.text.contains("earlier record stop"))
            assertTrue(video.file.isFile)
            val refused = runCatching { executor.execute("retry", AdbCommand.Record("start"), emptyList()) }.exceptionOrNull()
            assertTrue(refused?.message, refused is CommandFailure && refused.message!!.contains("hasn't reached Briefcase"))
            executor.delivered("retry", video)
            assertFalse(video.file.exists())
            val none = runCatching { executor.execute("retry", AdbCommand.Record("stop"), emptyList()) }.exceptionOrNull()
            assertTrue(none?.message, none?.message?.contains("No recording in this session") == true)
        } finally { withContext(NonCancellable) { executor.endAll() } }
    } }

    /** Too little space keeps the recording for a later retry instead of failing half way. */
    @Test fun lowSpaceKeepsTheRecording(): Unit = runBlocking { withTimeout(60_000) {
        fixture(intervalMs = 80)
        var free = 1L
        val executor = executor(free = { free })
        try {
            executor.execute("space", AdbCommand.Record("start"), emptyList())
            delay(2_000)
            val failure = runCatching { executor.execute("space", AdbCommand.Record("stop"), emptyList()) }.exceptionOrNull()
            assertTrue(failure?.message, failure?.message?.contains("MiB free") == true && failure.message!!.contains("kept"))
            free = Long.MAX_VALUE
            val saved = executor.execute("space", AdbCommand.Record("stop"), emptyList())
            assertTrue(saved.artifacts.single().file.length() > 1000)
        } finally { withContext(NonCancellable) { executor.endAll() } }
    } }

    /**
     * The device gave up on a session while offline (longer than Extend keeps one), and Extend
     * announced the session again anyway: stop must say the capture was discarded and why, not
     * that nothing was started.
     */
    @Test fun aCaptureDiscardedWhileOfflineIsExplained(): Unit = runBlocking { withTimeout(60_000) {
        fixture(intervalMs = 80)
        val executor = executor()
        try {
            executor.execute("away", AdbCommand.Record("start", "away"), emptyList())
            executor.execute("away", AdbCommand.Logs("start"), emptyList())
            executor.endSession("away", lostReason = "the device was offline too long.")
            assertEquals("", adb.shell("ls -d /data/local/tmp/silicon-extend-* 2>/dev/null", check = false).text.trim())

            val record = runCatching { executor.execute("away", AdbCommand.Record("stop"), emptyList()) }.exceptionOrNull()
            assertTrue(record?.message, record is CommandFailure && record.code == CommandFailure.ACTION_FAILED)
            assertTrue(record?.message, record!!.message!!.startsWith("The recording in this session was discarded on the device: the device was offline too long."))
            assertTrue(record.message, record.message!!.contains("Run record start to record again"))
            val logs = runCatching { executor.execute("away", AdbCommand.Logs("stop"), emptyList()) }.exceptionOrNull()
            assertTrue(logs?.message, logs is CommandFailure && logs.message!!.startsWith("The log capture in this session was discarded on the device"))

            // A new recording in the session works as usual; once it is saved, nothing is left to explain.
            executor.execute("away", AdbCommand.Record("start", "again"), emptyList())
            delay(1_500)
            val saved = withTimeout(30_000) { executor.execute("away", AdbCommand.Record("stop"), emptyList()) }
            assertTrue(saved.artifacts.single().file.length() > 1000)
            executor.delivered("away", saved.artifacts.single())
            val none = runCatching { executor.execute("away", AdbCommand.Record("stop"), emptyList()) }.exceptionOrNull()
            assertTrue(none?.message, none is CommandFailure && none.code == CommandFailure.INVALID_ARGS && none.message!!.contains("No recording in this session"))
            // Logs were not started again, so their loss is still explained.
            val stillLost = runCatching { executor.execute("away", AdbCommand.Logs("stop"), emptyList()) }.exceptionOrNull()
            assertTrue(stillLost?.message, stillLost!!.message!!.contains("discarded on the device"))

            // An ordinary end (Carbon Stop, Extend ended it) keeps no explanation.
            executor.execute("plain", AdbCommand.Logs("start"), emptyList())
            executor.endSession("plain")
            val plain = runCatching { executor.execute("plain", AdbCommand.Logs("stop"), emptyList()) }.exceptionOrNull()
            assertTrue(plain?.message, plain!!.message!!.contains("No logs are being collected"))
        } finally { withContext(NonCancellable) { executor.endAll() } }
    } }

    /**
     * The whole path: the device socket drops during a recording, the device's grace for the
     * session runs out (shortened here from 4 minutes), and Extend then announces the session
     * again. record stop answers with why the recording is gone, not "No recording".
     */
    @Test fun aSessionTheDeviceGaveUpOnExplainsItsLostRecording(): Unit = runBlocking { withTimeout(60_000) {
        fixture(intervalMs = 80)
        val extend = Extend.get(context)
        val commands = CommandExecutor(extend, retentionGraceMs = 1_500)
        val session = "lost-${UUID.randomUUID()}"
        try {
            commands.beginSession(session)
            val started = commands.run(ServiceFrame.Command("c1", session, command = "record", args = listOf("start", "lost")))
            assertTrue(started.toString(), started.ok)
            assertTrue(extend.adbExecutor.sessionIds().contains(session))
            commands.connectionLost("")
            // The grace passes while the socket is down; the capture ends on the device.
            val gone = withTimeout(20_000) { while (extend.adbExecutor.sessionIds().contains(session)) delay(100); true }
            assertTrue(gone)
            commands.beginSession(session) // Extend announces the session again after the reconnect
            val stop = commands.run(ServiceFrame.Command("c2", session, command = "record", args = listOf("stop")))
            assertFalse(stop.toString(), stop.ok)
            assertEquals(CommandFailure.ACTION_FAILED, stop.error?.code)
            assertTrue(stop.error!!.message, stop.error!!.message.startsWith("The recording in this session was discarded on the device: this device lost contact with Extend for more than 4 minutes"))
            assertTrue(stop.error!!.message, stop.error!!.message.endsWith("Run record start to record again."))
        } finally {
            withContext(NonCancellable) { extend.adbExecutor.endSession(session) }
        }
    } }

    /**
     * Regression: the ownership check read `/proc/<pid>/cmdline` through toybox `tr`, which spins
     * forever when the process exits between the open and the read; `record stop` then hung until
     * its timeout while holding the executor's lock. Checks a process that exits during the check,
     * many times over; the tr version hung in about 5 to 12 of 100 runs on the emulator.
     */
    @Test fun ownershipChecksFinishWhenTheProcessExitsDuringThem(): Unit = runBlocking { withTimeout(120_000) {
        val remote = "/data/local/tmp/extend-owned-test-${UUID.randomUUID()}"
        val loop = File(dir, "loop.sh").apply { writeText(AdbExecutor.OWNED + "\nwhile owned \"\$1/pid\" \"\$1\"; do :; done\n") }
        try {
            adb.shell("mkdir -p $remote")
            adb.push(loop, "$remote/loop.sh")
            // `sleep` rejects its second argument and exits at once: a short-lived process whose
            // command line names the directory, so it often exits while a check reads it.
            val result = adb.shell(
                "n=0; for i in $(seq 1 150); do " +
                    "sleep 0.0$((RANDOM % 9 + 1)) $remote/x 2>/dev/null & echo $! >$remote/pid; " +
                    "timeout 3 sh $remote/loop.sh $remote 2>/dev/null; [ $? -eq 124 ] && n=$((n+1)); wait 2>/dev/null; " +
                    "done; echo \"hung=${'$'}n\"",
                check = false,
            )
            assertEquals("Ownership checks that never finished", "hung=0", result.text.lines().last { it.isNotBlank() }.trim())
            // The check still recognises a live owner, and refuses a live process that isn't one.
            val check = File(dir, "check.sh").apply { writeText(AdbExecutor.OWNED + "\nowned \"\$1\" \"\$2\"; echo \"owned=\$?\"\n") }
            adb.push(check, "$remote/check.sh")
            val live = adb.shell("sh -c 'sleep 5; :' $remote/live >/dev/null 2>&1 & echo $! >$remote/pid; sh $remote/check.sh $remote/pid $remote; " +
                "sh $remote/check.sh $remote/pid /data/local/tmp/another-directory; kill ${'$'}(cat $remote/pid)", check = false)
            assertEquals(live.text, listOf("owned=0", "owned=1"), live.text.lines().filter { it.startsWith("owned=") })
        } finally {
            withContext(NonCancellable) { adb.shell("for p in $(pidof tr); do kill -9 ${'$'}p; done; rm -rf $remote", check = false) }
        }
    } }

    @Test fun recoveryStateLivesOutsideTheCache(): Unit = runBlocking { withTimeout(30_000) {
        val executor = executor()
        try {
            executor.execute("state", AdbCommand.Record("start"), emptyList())
            assertTrue(File(dir, "state/pending-recordings.txt").readText().contains("/data/local/tmp/silicon-extend-"))
            assertFalse(File(dir, "cache/pending-recordings.txt").exists())
        } finally { withContext(NonCancellable) { executor.endAll() } }
    } }

    @Test fun beyondNativeLimit(): Unit = runBlocking { withTimeout(230_000) {
        assumeTrue(InstrumentationRegistry.getArguments().getString("long_recording") == "true")
        fixture(intervalMs = 80)
        val executor = executor(segmentSeconds = 180)
        try {
            delay(1000)
            executor.execute("long", AdbCommand.Record("start", "long-proof"), emptyList())
            delay(187_000)
            val result = executor.execute("long", AdbCommand.Record("stop"), emptyList())
            val video = result.artifacts.single().file
            keep(video, "long-recording-proof.mp4")
            val media = MediaMetadataRetriever()
            try {
                media.setDataSource(video.absolutePath)
                val duration = media.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)!!.toLong()
                assertTrue("Recording stopped at the native cap: $duration ms", duration > 181_000)
                assertNotNull("Frames beyond 180 seconds must exist", media.getFrameAtTime(182_000_000))
                android.util.Log.i("ExtendRecordingTest", "PASS duration=$duration bytes=${video.length()} ${result.text}")
            } finally { media.release() }
        } finally { withContext(NonCancellable) { executor.endAll() } }
    } }
}

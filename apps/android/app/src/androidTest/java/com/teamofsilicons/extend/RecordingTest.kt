package com.teamofsilicons.extend

import android.content.Intent
import android.media.MediaMetadataRetriever
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.teamofsilicons.extend.adb.AdbCommand
import com.teamofsilicons.extend.adb.AdbExecutor
import com.teamofsilicons.extend.adb.AdbWire
import com.teamofsilicons.extend.adb.RecordingScript
import com.teamofsilicons.extend.adb.RecordingMuxer
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.UUID

/** Dedicated emulator lane: opt in because this keeps recording for over three minutes. */
@RunWith(AndroidJUnit4::class)
class RecordingTest {
    @Test fun nativeDurationLimit(): Unit = runBlocking { withTimeout(25_000) {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        val adb = Extend.get(context).adb
        assertTrue(adb.lastError, adb.connect(5555))
        val directory = "/data/local/tmp/silicon-extend-${UUID.randomUUID()}"
        val local = File(context.cacheDir, "record-limit-${UUID.randomUUID()}").apply { mkdirs() }
        instrumentation.context.startActivity(Intent(instrumentation.context, RecordingFixtureActivity::class.java).putExtra("interval_ms", 2000).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        try {
            val script = File(local, "capture.sh").apply { writeText(RecordingScript.create(directory, false, durationSeconds = 7, segmentSeconds = 3)) }
            adb.shell("mkdir -m 700 ${AdbWire.quote(directory)}")
            adb.push(script, "$directory/capture.sh")
            adb.shell("exec sh ${AdbWire.quote("$directory/capture.sh")}", pty = true)
            assertEquals("duration-limit", adb.shell("cat $directory/completed").text.trim())
            val names = adb.shell("ls $directory/chunk-*.mp4").text.lineSequence().filter { it.isNotBlank() }.toList()
            assertTrue("The supervisor must roll over", names.size >= 2)
            val parts = names.mapIndexed { index, name ->
                val path = name.trim()
                val file = File(local, "part-$index.mp4").also { adb.pull(path, it) }
                RecordingMuxer.segment(file, adb.shell("cat $path.timing").text)
            }
            val output = File(context.getExternalFilesDir(null), "duration-recording-proof.mp4")
            RecordingMuxer.combine(parts, output)
            val media = MediaMetadataRetriever()
            try {
                media.setDataSource(output.absolutePath)
                val actual = media.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)!!.toLong()
                val expected = parts.sumOf { part ->
                    val source = MediaMetadataRetriever()
                    try {
                        source.setDataSource(part.file.absolutePath)
                        minOf(source.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)!!.toLong(), part.maximumDurationUs / 1000)
                    } finally { source.release() }
                }
                assertTrue("Combined duration $actual must match source duration $expected", kotlin.math.abs(actual - expected) < 100)
                assertTrue(actual in 4000..7100)
            } finally { media.release() }
        } finally {
            try {
                File(local, "pending-recordings.txt").writeText(directory)
                AdbExecutor(adb, local).recover()
            } finally { adb.disconnect(); local.deleteRecursively() }
        }
    } }
    @Test fun beyondNativeLimit(): Unit = runBlocking { withTimeout(230_000) {
        org.junit.Assume.assumeTrue(InstrumentationRegistry.getArguments().getString("long_recording") == "true")
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        val adb = Extend.get(context).adb
        assertTrue(adb.lastError, adb.connect(5555))
        val cache = File(context.cacheDir, "record-test-${UUID.randomUUID()}")
        val executor = AdbExecutor(adb, cache)
        instrumentation.context.startActivity(Intent(instrumentation.context, RecordingFixtureActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        try {
            delay(1000)
            executor.execute("long", AdbCommand.Record("start", "long-proof"), emptyList())
            delay(187_000)
            val result = executor.execute("long", AdbCommand.Record("stop"), emptyList())
            val video = result.artifact!!.file
            val saved = File(context.getExternalFilesDir(null), "long-recording-proof.mp4")
            video.copyTo(saved, overwrite = true)
            val media = MediaMetadataRetriever()
            try {
                media.setDataSource(video.absolutePath)
                val duration = media.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)!!.toLong()
                assertTrue("Recording stopped at the native cap: $duration ms", duration > 181_000)
                assertNotNull("Frames beyond 180 seconds must exist", media.getFrameAtTime(182_000_000))
                android.util.Log.i("ExtendRecordingTest", "PASS duration=$duration bytes=${video.length()} ${result.text}")
            } finally { media.release() }
        } finally {
            executor.endAll()
            adb.disconnect()
            cache.deleteRecursively()
            instrumentation.runOnMainSync { RecordingFixtureActivity.current?.finish() }
        }
    } }
}

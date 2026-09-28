package com.teamofsilicons.extend.display

import okhttp3.Call
import java.io.File
import java.io.IOException

/** Download to disk with a bounded buffer; Content-Length is not trusted for the limit. */
object DisplayImage {
    const val MAX_BYTES = 32L * 1024 * 1024
    const val MAX_PIXELS = 1920L * 1080

    fun download(call: Call, directory: File, limit: Long = MAX_BYTES): File {
        directory.mkdirs()
        val file = File.createTempFile("display-image-", ".download", directory)
        try {
            call.execute().use { response ->
                if (!response.isSuccessful) throw IOException("Image server returned HTTP ${response.code}")
                val body = response.body ?: throw IOException("Image server returned no data")
                if (body.contentLength() > limit) throw IOException("Image exceeds the download limit ($limit bytes)")
                body.byteStream().use { input ->
                    file.outputStream().use { output ->
                        val buffer = ByteArray(16 * 1024)
                        var total = 0L
                        while (true) {
                            val count = input.read(buffer)
                            if (count < 0) break
                            total += count
                            if (total > limit) throw IOException("Image exceeds the download limit ($limit bytes)")
                            output.write(buffer, 0, count)
                        }
                    }
                }
            }
            return file
        } catch (e: Throwable) {
            file.delete()
            throw e
        }
    }

    /** Power-of-two downsampling keeps the decoded bitmap within the display and memory budget. */
    fun sampleSize(width: Int, height: Int, screenWidth: Int, screenHeight: Int): Int {
        if (width <= 0 || height <= 0) throw IOException("The file is not a supported image or is damaged")
        var sample = 1
        while (true) {
            val w = (width.toLong() + sample - 1) / sample
            val h = (height.toLong() + sample - 1) / sample
            if (w <= screenWidth.coerceAtLeast(1) && h <= screenHeight.coerceAtLeast(1) && w * h <= MAX_PIXELS) return sample
            if (sample == 1 shl 30) throw IOException("Image dimensions are too large")
            sample *= 2
        }
    }
}

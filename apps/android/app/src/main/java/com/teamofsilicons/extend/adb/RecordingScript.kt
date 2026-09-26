package com.teamofsilicons.extend.adb

/** Native screenrecord has a per-process limit; the owning shell sequences bounded segments. */
internal object RecordingScript {
    const val MAX_BYTES = 1024L * 1024 * 1024
    fun create(directory: String, highQuality: Boolean, durationSeconds: Int = 1800, segmentSeconds: Int = 180): String {
        require(durationSeconds in 1..1800 && segmentSeconds in 1..180)
        return """
        #!/system/bin/sh
        dir=${AdbWire.quote(directory)}
        echo ${'$'}${'$'} >"${'$'}dir/supervisor.pid"
        child=''
        stopping=0
        child_running() {
          [ -n "${'$'}child" ] && ${AdbExecutor.NAMES_IN_CMDLINE} "${'$'}dir/" /proc/"${'$'}child"/cmdline 2>/dev/null
        }
        stop_capture() {
          stopping=1
          touch "${'$'}dir/stop"
          if child_running; then kill -2 "${'$'}child" 2>/dev/null || true; fi
        }
        trap stop_capture HUP INT TERM
        clock_seconds() { read up rest </proc/uptime; echo "${'$'}{up%%.*}"; }
        started=${'$'}(clock_seconds)
        index=0
        reason=stopped
        while [ "${'$'}stopping" -eq 0 ] && [ ! -f "${'$'}dir/stop" ]; do
          remaining=${'$'}(($durationSeconds - ${'$'}(clock_seconds) + started))
          if [ "${'$'}remaining" -le 0 ]; then reason=duration-limit; break; fi
          seconds=$segmentSeconds
          [ "${'$'}remaining" -ge $segmentSeconds ] || seconds=${'$'}remaining
          file="${'$'}dir/chunk-${'$'}index.mp4"
          read segment_start rest </proc/uptime
          screenrecord --time-limit "${'$'}seconds" --bit-rate ${if (highQuality) 10000000 else 8000000} "${'$'}file" &
          child=${'$'}!
          echo "${'$'}child" >"${'$'}dir/pid"
          if [ "${'$'}stopping" -ne 0 ] || [ -f "${'$'}dir/stop" ]; then
            if child_running; then kill -2 "${'$'}child" 2>/dev/null || true; fi
          fi
          while child_running; do
            bytes=0
            for part in "${'$'}dir"/chunk-*.mp4; do
              [ -f "${'$'}part" ] || continue
              size=${'$'}(stat -c %s "${'$'}part") || { stop_capture; reason=source-error; break; }
              bytes=${'$'}((bytes + size))
            done
            if [ "${'$'}stopping" -ne 0 ]; then
              :
            elif [ "${'$'}bytes" -ge ${MAX_BYTES - 16 * 1024 * 1024} ]; then
              reason=size-limit
              stop_capture
            elif [ ${'$'}((${ '$'}(clock_seconds) - started)) -ge $durationSeconds ]; then
              reason=duration-limit
              stop_capture
            fi
            sleep 1 &
            wait ${'$'}! || true
          done
          wait "${'$'}child"
          status=${'$'}?
          child=''
          read segment_end rest </proc/uptime
          echo "${'$'}segment_start ${'$'}segment_end ${'$'}seconds" >"${'$'}file.timing"
          if [ "${'$'}status" -ne 0 ] && [ "${'$'}stopping" -eq 0 ] && [ ! -f "${'$'}dir/stop" ]; then
            reason=source-error
            break
          fi
          index=${'$'}((index + 1))
        done
        echo "${'$'}reason" >"${'$'}dir/completed"
    """.trimIndent() + "\n"
    }
}

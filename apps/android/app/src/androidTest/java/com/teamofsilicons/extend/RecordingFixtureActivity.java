package com.teamofsilicons.extend;

import android.app.Activity;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.os.Bundle;
import android.os.SystemClock;
import android.view.View;
import android.view.WindowManager;

/**
 * Standalone test-APK process: use only Android/Java classes, not target-APK dependencies.
 * Extras: interval_ms between frames (80), animate_ms to stop changing after that long (never).
 */
public class RecordingFixtureActivity extends Activity {
    public static RecordingFixtureActivity current;

    @Override public void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        current = this;
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        final int interval = getIntent().getIntExtra("interval_ms", 80);
        final long animateMs = getIntent().getLongExtra("animate_ms", Long.MAX_VALUE);
        final long started = SystemClock.elapsedRealtime();
        setContentView(new View(this) {
            private int frame = 0;
            private final Paint paint = new Paint();

            @Override public void onDraw(Canvas canvas) {
                canvas.drawRGB((frame++ * 7) % 256, 80, 160);
                paint.setColor(Color.WHITE);
                paint.setTextSize(40);
                canvas.drawText("Extend recording fixture " + frame, 24, 120, paint);
                if (SystemClock.elapsedRealtime() - started < animateMs) postInvalidateDelayed(interval);
            }
        });
    }

    @Override public void onDestroy() {
        current = null;
        super.onDestroy();
    }
}

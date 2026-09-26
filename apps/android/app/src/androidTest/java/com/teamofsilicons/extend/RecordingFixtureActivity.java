package com.teamofsilicons.extend;

import android.app.Activity;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.os.Bundle;
import android.view.View;
import android.view.WindowManager;

/** Standalone test-APK process: use only Android/Java classes, not target-APK dependencies. */
public class RecordingFixtureActivity extends Activity {
    public static RecordingFixtureActivity current;

    @Override public void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        current = this;
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        final int interval = getIntent().getIntExtra("interval_ms", 80);
        setContentView(new View(this) {
            private int frame = 0;
            private final Paint paint = new Paint();

            @Override public void onDraw(Canvas canvas) {
                canvas.drawRGB((frame++ * 7) % 256, 80, 160);
                paint.setColor(Color.WHITE);
                paint.setTextSize(40);
                canvas.drawText("Extend recording fixture " + frame, 24, 120, paint);
                postInvalidateDelayed(interval);
            }
        });
    }

    @Override public void onDestroy() {
        current = null;
        super.onDestroy();
    }
}

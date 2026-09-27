package com.teamofsilicons.extend.ui

import android.Manifest
import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.toArgb
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.teamofsilicons.extend.Extend
import com.teamofsilicons.extend.BuildConfig
import com.teamofsilicons.extend.service.ExtendForegroundService

class MainActivity : ComponentActivity() {
    private val extend by lazy { Extend.get(this) }

    private val notificationPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) {
        extend.onCapabilitiesMayHaveChanged()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        // Interface's paper behind light system bars, with dark icons. Pages scroll on under the
        // navigation bar, so where Android still paints one (3-button navigation before Android 15)
        // it is a translucent paper scrim, not a solid strip.
        val paper = Tokens.Paper.toArgb()
        val scrim = Tokens.Paper.copy(alpha = 0.9f).toArgb()
        enableEdgeToEdge(SystemBarStyle.light(paper, paper), SystemBarStyle.light(scrim, scrim))
        super.onCreate(savedInstanceState)
        // Only for a fresh launch: when the activity is recreated (rotation, a density or font
        // change) its launch intent would otherwise re-apply `forget_pair` and unpair the device.
        if (savedInstanceState == null) applyTestOverrides(intent)
        ExtendForegroundService.createChannels(this)
        ExtendForegroundService.start(this)
        setContent {
            val state by extend.state.collectAsStateWithLifecycle()
            var showDev by remember { mutableStateOf(false) }
            var showLicences by remember { mutableStateOf(false) }
            ExtendTheme(tv = state.isTv) {
                if (showLicences) {
                    LicencesScreen(onClose = { showLicences = false })
                } else if (showDev) {
                    DeveloperSettingsScreen(extend, state, onClose = { showDev = false })
                } else {
                    AppScreen(
                        extend = extend,
                        state = state,
                        onOpenDeveloperSettings = { showDev = true },
                        onRequestNotifications = { notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS) },
                        onOpen = { intent -> runCatching { startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)) } },
                        onOpenLicences = { showLicences = true },
                    )
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        applyTestOverrides(intent)
        // Consumed: a later recreation must not apply it again.
        setIntent(Intent(intent).apply { replaceExtras(Bundle()) })
    }

    override fun onResume() {
        super.onResume()
        // Coming back from a settings screen: show the new setup state right away.
        extend.onCapabilitiesMayHaveChanged()
        // The Carbon may have just turned Wireless debugging back on.
        extend.wakeAdbReconnect()
    }

    /**
     * Debug builds only: `--es service_url http://10.0.2.2:8480`, `--ez force_tv true`,
     * `--ez forget_pair true`, `--ez static_grain true` (draw the pre-Android 13 grain fallback).
     * Release builds ignore every extra (see build.gradle.kts).
     */
    private fun applyTestOverrides(intent: Intent?) {
        if (!BuildConfig.ALLOW_TEST_OVERRIDES || intent == null) return
        if (intent.hasExtra("static_grain")) GrainSettings.forceStatic = intent.getBooleanExtra("static_grain", false)
        var changed = false
        intent.getStringExtra("service_url")?.let { url ->
            if (url.trimEnd('/') != extend.config.serviceUrl) {
                extend.config.serviceUrl = url
                extend.secrets.clearCredential()
                extend.config.clearPair()
                changed = true
            }
        }
        if (intent.hasExtra("force_tv")) {
            val tv = intent.getBooleanExtra("force_tv", false)
            if (tv != extend.config.forceTv) {
                extend.config.forceTv = tv
                changed = true
            }
        }
        if (intent.getBooleanExtra("forget_pair", false)) {
            extend.secrets.clearCredential()
            extend.config.clearPair()
            changed = true
        }
        if (changed) {
            extend.update { it.copy(serviceUrl = extend.config.serviceUrl, isTv = extend.isTv) }
            extend.connection.reconnect()
            extend.onCapabilitiesMayHaveChanged()
        }
    }
}

package com.teamofsilicons.extend.ui

import android.Manifest
import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
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
        super.onCreate(savedInstanceState)
        applyTestOverrides(intent)
        ExtendForegroundService.createChannels(this)
        ExtendForegroundService.start(this)
        setContent {
            val state by extend.state.collectAsStateWithLifecycle()
            var showDev by remember { mutableStateOf(false) }
            ExtendTheme(tv = state.isTv) {
                if (showDev) {
                    DeveloperSettingsScreen(extend, state, onClose = { showDev = false })
                } else {
                    AppScreen(
                        extend = extend,
                        state = state,
                        onOpenDeveloperSettings = { showDev = true },
                        onRequestNotifications = { notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS) },
                        onOpen = { intent -> runCatching { startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)) } },
                    )
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        applyTestOverrides(intent)
    }

    override fun onResume() {
        super.onResume()
        // Coming back from a settings screen: show the new setup state right away.
        extend.onCapabilitiesMayHaveChanged()
    }

    /**
     * Debug builds only: `--es service_url http://10.0.2.2:8480`, `--ez force_tv true`,
     * `--ez forget_pair true`. Release builds ignore every extra (see build.gradle.kts).
     */
    private fun applyTestOverrides(intent: Intent?) {
        if (!BuildConfig.ALLOW_TEST_OVERRIDES || intent == null) return
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

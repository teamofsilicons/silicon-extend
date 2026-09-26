package com.teamofsilicons.bridge.ui

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
import com.teamofsilicons.bridge.Bridge
import com.teamofsilicons.bridge.BuildConfig
import com.teamofsilicons.bridge.service.BridgeForegroundService

class MainActivity : ComponentActivity() {
    private val bridge by lazy { Bridge.get(this) }

    private val notificationPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) {
        bridge.onCapabilitiesMayHaveChanged()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        applyTestOverrides(intent)
        BridgeForegroundService.createChannels(this)
        BridgeForegroundService.start(this)
        setContent {
            val state by bridge.state.collectAsStateWithLifecycle()
            var showDev by remember { mutableStateOf(false) }
            BridgeTheme(tv = state.isTv) {
                if (showDev) {
                    DeveloperSettingsScreen(bridge, state, onClose = { showDev = false })
                } else {
                    AppScreen(
                        bridge = bridge,
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
        bridge.onCapabilitiesMayHaveChanged()
    }

    /**
     * Debug builds only: `--es service_url http://10.0.2.2:8480`, `--ez force_tv true`,
     * `--ez forget_pair true`. Release builds ignore every extra (see build.gradle.kts).
     */
    private fun applyTestOverrides(intent: Intent?) {
        if (!BuildConfig.ALLOW_TEST_OVERRIDES || intent == null) return
        var changed = false
        intent.getStringExtra("service_url")?.let { url ->
            if (url.trimEnd('/') != bridge.config.serviceUrl) {
                bridge.config.serviceUrl = url
                bridge.secrets.clearCredential()
                bridge.config.clearPair()
                changed = true
            }
        }
        if (intent.hasExtra("force_tv")) {
            val tv = intent.getBooleanExtra("force_tv", false)
            if (tv != bridge.config.forceTv) {
                bridge.config.forceTv = tv
                changed = true
            }
        }
        if (intent.getBooleanExtra("forget_pair", false)) {
            bridge.secrets.clearCredential()
            bridge.config.clearPair()
            changed = true
        }
        if (changed) {
            bridge.update { it.copy(serviceUrl = bridge.config.serviceUrl, isTv = bridge.isTv) }
            bridge.connection.reconnect()
            bridge.onCapabilitiesMayHaveChanged()
        }
    }
}

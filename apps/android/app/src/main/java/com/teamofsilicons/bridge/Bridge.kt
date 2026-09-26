package com.teamofsilicons.bridge

import android.app.Application
import android.content.Context
import android.util.Log
import com.teamofsilicons.bridge.config.Config
import com.teamofsilicons.bridge.config.DeviceInfo
import com.teamofsilicons.bridge.core.ConnectionManager
import com.teamofsilicons.bridge.core.UiState
import com.teamofsilicons.bridge.driver.CommandExecutor
import com.teamofsilicons.bridge.net.BridgeApi
import com.teamofsilicons.bridge.security.SecretStore
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update

/** The app's one instance of everything: settings, secrets, the connection and the driver. */
class Bridge private constructor(val context: Context) {
    val config = Config(context)
    val secrets = SecretStore(context)
    val api = BridgeApi({ config.serviceUrl })
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    private val _state = MutableStateFlow(
        UiState(
            isTv = DeviceInfo.isTv(context, config),
            isFireTv = DeviceInfo.isFireTv(context),
            serviceUrl = config.serviceUrl,
            deviceId = config.deviceId,
            environment = config.environment,
        ),
    )
    val state: StateFlow<UiState> = _state

    val executor = CommandExecutor(this)
    val connection = ConnectionManager(this)

    fun update(f: (UiState) -> UiState) = _state.update(f)

    /** Accessibility, notification access or a permission may have changed. */
    fun onCapabilitiesMayHaveChanged() = connection.recomputeSetup()

    val isTv: Boolean get() = DeviceInfo.isTv(context, config)
    val os: String get() = DeviceInfo.os(context, config)

    companion object {
        const val TAG = "SiliconBridge"

        @Volatile private var instance: Bridge? = null

        fun get(context: Context): Bridge =
            instance ?: synchronized(this) {
                instance ?: Bridge(context.applicationContext).also { instance = it }
            }

        fun log(message: String, error: Throwable? = null) {
            if (error != null) Log.w(TAG, message, error) else Log.i(TAG, message)
        }
    }
}

class BridgeApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        Bridge.get(this)
    }
}

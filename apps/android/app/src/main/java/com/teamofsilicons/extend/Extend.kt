package com.teamofsilicons.extend

import android.app.Application
import android.content.Context
import android.util.Log
import com.teamofsilicons.extend.config.Config
import com.teamofsilicons.extend.config.DeviceInfo
import com.teamofsilicons.extend.core.ConnectionManager
import com.teamofsilicons.extend.core.UiState
import com.teamofsilicons.extend.driver.CommandExecutor
import com.teamofsilicons.extend.net.ExtendApi
import com.teamofsilicons.extend.security.SecretStore
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update

/** The app's one instance of everything: settings, secrets, the connection and the driver. */
class Extend private constructor(val context: Context) {
    val config = Config(context)
    val secrets = SecretStore(context)
    val api = ExtendApi({ config.serviceUrl })
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

    val adb = com.teamofsilicons.extend.adb.LocalAdb(context)
    val adbExecutor = com.teamofsilicons.extend.adb.AdbExecutor(adb, java.io.File(context.cacheDir, "adb-output"))
    val executor = CommandExecutor(this)
    val connection = ConnectionManager(this)

    init {
        scope.launch {
            while (true) {
                if (config.deviceId != null && adb.enabled && !adb.connected) {
                    runCatching { if (adb.reconnect()) adbExecutor.recover() }
                    onCapabilitiesMayHaveChanged()
                }
                delay(15_000)
            }
        }
    }

    fun update(f: (UiState) -> UiState) = _state.update(f)

    /** Accessibility, notification access or a permission may have changed. */
    fun onCapabilitiesMayHaveChanged() = connection.recomputeSetup()

    val isTv: Boolean get() = DeviceInfo.isTv(context, config)
    val os: String get() = DeviceInfo.os(context, config)

    companion object {
        const val TAG = "SiliconExtend"

        @Volatile private var instance: Extend? = null

        fun get(context: Context): Extend =
            instance ?: synchronized(this) {
                instance ?: Extend(context.applicationContext).also { instance = it }
            }

        fun log(message: String, error: Throwable? = null) {
            if (error != null) Log.w(TAG, message, error) else Log.i(TAG, message)
        }
    }
}

class ExtendApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        Extend.get(this)
    }
}

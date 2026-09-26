package com.teamofsilicons.extend.core

import com.teamofsilicons.extend.protocol.DeviceSelf
import com.teamofsilicons.extend.protocol.TestingEnvironment

enum class Phase { STARTING, UNPAIRED, PAIRED }

enum class Link { CONNECTING, CONNECTED, OFFLINE, SUPERSEDED, UPGRADE_REQUIRED }

data class PairingUi(
    val code: String? = null,
    val expiresAt: String? = null,
    val live: Boolean = false,
    val error: String? = null,
)

data class SessionUi(val siliconId: String, val sessionId: String, val since: String, val stopping: Boolean = false)

data class TakeoverUi(val sessionId: String, val reason: String, val expiresAt: String?)

/** Everything the screens, the notification and the TV badge show. */
data class UiState(
    val phase: Phase = Phase.STARTING,
    val pairing: PairingUi = PairingUi(),
    val deviceId: String? = null,
    val device: DeviceSelf? = null,
    val link: Link = Link.CONNECTING,
    val linkDetail: String? = null,
    val session: SessionUi? = null,
    val takeover: TakeoverUi? = null,
    val environment: TestingEnvironment? = null,
    val isTv: Boolean = false,
    val isFireTv: Boolean = false,
    val report: SetupReport? = null,
    val serviceUrl: String = "",
    val lastCommand: String? = null,
)

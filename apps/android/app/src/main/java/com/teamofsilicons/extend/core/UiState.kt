package com.teamofsilicons.extend.core

import com.teamofsilicons.extend.protocol.Member
import com.teamofsilicons.extend.protocol.TestingEnvironment

enum class Phase { STARTING, UNPAIRED, PAIRED }

enum class Link { CONNECTING, CONNECTED, OFFLINE, SUPERSEDED, UPGRADE_REQUIRED }

data class PairingUi(
    val code: String? = null,
    val expiresAt: String? = null,
    val live: Boolean = false,
    val error: String? = null,
)

/**
 * One Carbon's pair of this device. Each Carbon who paired it has their own: their own device id,
 * their own name for the device, and their own connection to Extend.
 */
data class PairUi(
    val deviceId: String,
    /** That Carbon's name for this device; null until Extend said. */
    val name: String? = null,
    val owner: Member? = null,
    val link: Link = Link.CONNECTING,
    val linkDetail: String? = null,
    /** The pair made by the app's first enrollment (the Carbon who installed the app); null: unknown. */
    val firstPair: Boolean? = null,
    val instanceId: String? = null,
) {
    /** "c:alice", or "a Carbon" before Extend said whose pair this is. */
    val carbon: String get() = owner?.id ?: "a Carbon"

    /** "Alice (c:alice)" when Extend gave a display name. */
    val carbonLabel: String get() = owner?.let { o -> o.displayName?.takeIf { it.isNotBlank() }?.let { "$it (${o.id})" } ?: o.id } ?: "a Carbon"
}

/**
 * The Silicon using this device. [pairId] is the pair its session runs through, [carbon] the
 * Carbon who gave it access (that pair's owner), and [side] the session's side tag, which decides
 * which wake requests are shown in full while it runs ([WakeRequests.redacted]).
 */
data class SessionUi(
    val siliconId: String,
    val sessionId: String,
    val since: String,
    val stopping: Boolean = false,
    val pairId: String? = null,
    val carbon: String? = null,
    val side: String? = null,
)

data class TakeoverUi(val sessionId: String, val reason: String, val expiresAt: String?)

/** Whether this device is awake, as the app last reported it; [sleepState] says why not. */
data class AwakeUi(val awake: Boolean, val sleepState: String? = null)

/**
 * A Silicon's request that the Carbon wake this device, through the pair [pairId]. Without
 * [siliconId] and [reason] the service already left out who asked (another side holds the device).
 */
data class WakeUi(
    val wakeId: String,
    val pairId: String,
    val siliconId: String?,
    val reason: String?,
    val side: String?,
    val createdAt: String,
    val expiresAt: String,
)

/** "Pair with another Carbon" in progress: first the shared-device note, then the code. */
data class AddPairUi(
    val showingCode: Boolean = false,
    val pairing: PairingUi = PairingUi(),
    /** Why no code can be shown (an old service, the pair limit); the flow stops there. */
    val error: String? = null,
)

/** Everything the screens, the notifications and the TV badge show. */
data class UiState(
    val phase: Phase = Phase.STARTING,
    val pairing: PairingUi = PairingUi(),
    /** Every Carbon's pair of this device, the app's first enrollment first. */
    val pairs: List<PairUi> = emptyList(),
    /** The connection as a whole ([Links.overall]); each pair's own is in [pairs]. */
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
    val awake: AwakeUi? = null,
    val wakeRequests: List<WakeUi> = emptyList(),
    val addingPair: AddPairUi? = null,
    /** The device's `in_use_indicator` ([InUseIndicator]): the badge or notification names the Silicon for 10 s. */
    val indicatorShown: Boolean = true,
    /** What happened to the Carbon's last change of [indicatorShown], when it needs saying. */
    val indicatorNote: String? = null,
    /** The session the device last announced, and whether its 10 s are running ([InUseAnnouncer]). */
    val announce: AnnounceUi? = null,
) {
    /** The first pair's device id (the app's first enrollment), for the developer settings. */
    val deviceId: String? get() = pairs.firstOrNull()?.deviceId

    fun pair(deviceId: String?): PairUi? = pairs.firstOrNull { it.deviceId == deviceId }
}

/** How the app sums up its pairs' connections for the status pill and the connection notification. */
object Links {
    /**
     * Something that needs the Carbon (an update, a connection that was taken over) first, then
     * connected when any pair is (each pair shows its own state), then connecting, then offline.
     */
    fun overall(pairs: List<PairUi>): Pair<Link, String?> {
        if (pairs.isEmpty()) return Link.CONNECTING to null
        pairs.firstOrNull { it.link == Link.UPGRADE_REQUIRED }?.let { return it.link to it.linkDetail }
        pairs.firstOrNull { it.link == Link.SUPERSEDED }?.let { return it.link to supersededText(it, pairs.size > 1) }
        if (pairs.any { it.link == Link.CONNECTED }) return Link.CONNECTED to null
        pairs.firstOrNull { it.link == Link.CONNECTING }?.let { return it.link to it.linkDetail }
        return Link.OFFLINE to pairs.first().linkDetail
    }

    /** The words for a pair whose connection another one took over. */
    fun supersededText(pair: PairUi, several: Boolean): String =
        if (several) "Another connection took over ${pair.carbon}'s pair on this device. Tap Reconnect to use this one."
        else "Another connection for this device took over. Tap Reconnect to use this one."
}

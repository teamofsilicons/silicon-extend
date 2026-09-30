package com.teamofsilicons.extend.ui

import android.os.Build
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.windowInsetsBottomHeight
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.LineHeightStyle
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.teamofsilicons.extend.BuildConfig
import com.teamofsilicons.extend.Extend
import com.teamofsilicons.extend.config.DeviceInfo
import com.teamofsilicons.extend.core.FindHelp
import com.teamofsilicons.extend.core.FinderResults
import com.teamofsilicons.extend.core.Link
import com.teamofsilicons.extend.core.Links
import com.teamofsilicons.extend.core.OpenResult
import com.teamofsilicons.extend.core.PairUi
import com.teamofsilicons.extend.core.WakeRequests
import com.teamofsilicons.extend.core.OpenedScreen
import com.teamofsilicons.extend.core.Phase
import com.teamofsilicons.extend.core.SettingsCandidate
import com.teamofsilicons.extend.core.SettingsFinder
import com.teamofsilicons.extend.core.SettingsTarget
import com.teamofsilicons.extend.core.SetupItem
import com.teamofsilicons.extend.adb.DebuggingPath
import com.teamofsilicons.extend.core.UiState
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

/** What the setup steps' buttons do; MainActivity wires them to this device. */
class SetupActions(
    val requestNotifications: () -> Unit,
    /** Runs a failed step again (its Retry button). */
    val retry: (String) -> Unit = {},
    /** Opens a step's settings page: the page itself, only the main settings screen, or nothing (with a message). */
    val open: (SettingsTarget) -> OpenResult,
    /** Opens a screen from a step's "Can't find it?" list; the message to show when it didn't open. */
    val openCandidate: (SettingsCandidate) -> String?,
    /** Every system screen's candidates on this device, scanned off the main thread ([refresh]: scan again); null when the scan failed. */
    val findScreens: suspend (refresh: Boolean) -> FinderResults?,
)

@Composable
fun AppScreen(
    extend: Extend,
    state: UiState,
    onOpenDeveloperSettings: () -> Unit,
    actions: SetupActions,
    onOpenLicences: () -> Unit = {},
) {
    val footer: @Composable () -> Unit = { Footer(state, onOpenDeveloperSettings, onOpenLicences) }
    if (state.phase == Phase.UNPAIRED && state.isTv) {
        TvPairingScreen(state, footer)
        return
    }
    val adding = state.addingPair
    if (state.phase == Phase.PAIRED && adding != null) {
        AddPairScreen(extend, state, adding)
        return
    }
    if (state.phase == Phase.PAIRED && state.isTv) {
        TvDeviceScreen(extend, state, actions, footer)
        return
    }
    // Each phase opens at the top, with the Silicon using the device first.
    val scroll = rememberScrollState()
    LaunchedEffect(state.phase) { scroll.scrollTo(0) }
    ScrollingPage(
        topBar = { TopBar(context = breadcrumb(state), trailing = { StatusPill(state) }) },
        scroll = scroll,
    ) {
        state.environment?.let { EnvironmentBanner(it.name) }
        when (state.phase) {
            Phase.STARTING -> Starting()
            Phase.UNPAIRED -> PairingScreen(state)
            Phase.PAIRED -> PairedScreen(extend, state, actions)
        }
        Gap(32.dp)
        Hairline()
        Gap(4.dp)
        // A TV remote's first focus lands in the footer while the app starts; a new footer
        // per phase drops that focus so the paired page doesn't open scrolled to its end.
        key(state.phase) { footer() }
    }
}

/**
 * A page: the top bar, then content in the page column that scrolls. The status bar and side
 * insets pad the whole page; the bottom inset (gesture bar, keyboard) is padding at the end of the
 * scroll, so cards scroll on under a transparent gesture bar instead of stopping at a hard edge.
 */
@Composable
fun ScrollingPage(
    topBar: @Composable () -> Unit,
    scroll: androidx.compose.foundation.ScrollState = rememberScrollState(),
    content: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit,
) {
    val s = LocalScale.current
    Column(
        Modifier
            .fillMaxSize()
            .paperGrain()
            .windowInsetsPadding(WindowInsets.safeDrawing.only(WindowInsetsSides.Top + WindowInsetsSides.Horizontal)),
    ) {
        topBar()
        if (s.tv) {
            TvScrollPane(scroll, Modifier.weight(1f).padding(horizontal = s.gutter)) {
                Column(Modifier.fillMaxWidth().padding(vertical = 16.dp), content = content)
            }
        } else {
            Column(Modifier.fillMaxWidth().weight(1f).verticalScroll(scroll)) {
                PageColumn(Modifier.padding(vertical = 22.dp), content = content)
                Spacer(Modifier.windowInsetsBottomHeight(WindowInsets.safeDrawing))
            }
        }
    }
}

/**
 * The breadcrumb gives context, never the page title. A device belongs to the Carbons who paired
 * it, not to a Team, so it is "Devices" once paired too.
 */
private fun breadcrumb(state: UiState): String? = when (state.phase) {
    Phase.STARTING -> null
    Phase.UNPAIRED, Phase.PAIRED -> "Devices"
}

/**
 * On a TV the accessibility service draws the in-use badge in the top-right corner over every app,
 * this one included, saying which Silicon it is. While it is up, the top bar leaves that corner to
 * it rather than printing a second, less specific "In use" underneath.
 */
private fun tvBadgeShowing(state: UiState): Boolean =
    InUseBadge.from(state) != null &&
        state.report?.items?.any { it.step.key == "accessibility" && it.step.status == "done" } == true

@Composable
private fun StatusPill(state: UiState) {
    when {
        state.phase == Phase.STARTING -> Unit
        tvBadgeShowing(state) -> Unit
        state.phase == Phase.UNPAIRED -> Pill("Not paired", Tokens.Muted, Tokens.Paper, border = Tokens.LineStrong)
        state.session != null -> Pill("In use", Color.White, Tokens.Cobalt, leading = { PixelIndicator(Color.White, size = 8.dp) })
        state.link == Link.CONNECTED -> Pill("Online", Tokens.TagInk, Tokens.CobaltPale, leading = { PixelIndicator(Tokens.Cobalt, size = 8.dp) })
        state.link == Link.CONNECTING -> Pill("Connecting", Tokens.Muted, Tokens.Paper, border = Tokens.LineStrong)
        state.link == Link.OFFLINE -> Pill("Offline", Tokens.Muted, Tokens.Paper, border = Tokens.LineStrong, leading = { PixelIndicator(Tokens.MutedMark, on = false, size = 8.dp) })
        else -> Pill("Action needed", Tokens.StopDeep, Tokens.StopPale)
    }
}

@Composable
private fun EnvironmentBanner(name: String) {
    Row(
        Modifier
            .fillMaxWidth()
            .padding(bottom = 18.dp)
            .background(Tokens.StopPale, Radius)
            .border(1.dp, Tokens.StopEdge, Radius)
            .padding(horizontal = 14.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        PixelIndicator(Tokens.Stop, size = 10.dp)
        Spacer(Modifier.width(10.dp))
        Text(
            "Test environment: $name",
            style = Type.mono(LocalScale.current).copy(color = Tokens.StopDeep, fontWeight = FontWeight.Medium),
        )
    }
}

@Composable
private fun Footer(state: UiState, onOpenDeveloperSettings: () -> Unit, onOpenLicences: () -> Unit) {
    // Seven taps on the version open the hidden developer settings.
    var taps by remember { mutableIntStateOf(0) }
    val versionFocus = remember { MutableInteractionSource() }
    val versionFocused by versionFocus.collectIsFocusedAsState()
    val s = LocalScale.current
    // On a narrow screen with large text the licences link goes under the version instead of
    // being cut short; its text then lines up with the column's start edge.
    FooterRow(nudge = if (s.tv) 0.dp else 18.dp) {
        Box(
            Modifier
                .heightIn(min = 48.dp)
                .focusRing(versionFocused, radius = 4.dp)
                .clickable(interactionSource = versionFocus, indication = null) {
                    taps++
                    if (taps >= 7) {
                        taps = 0
                        onOpenDeveloperSettings()
                    }
                }
                .padding(vertical = 12.dp),
            contentAlignment = Alignment.CenterStart,
        ) {
            Mono("${DeviceInfo.appName(state.isTv)} ${BuildConfig.VERSION_NAME}")
        }
        ExtendButton("Open-source licences", onOpenLicences, tone = Tone.Quiet)
    }
}

/**
 * The footer's two parts: side by side when they fit, the second under the first when not.
 * [nudge] moves a quiet button by its own padding so its text meets the column edge: out to the
 * end beside the version, or out to the start below it.
 */
@Composable
private fun FooterRow(nudge: androidx.compose.ui.unit.Dp, content: @Composable () -> Unit) {
    androidx.compose.ui.layout.Layout(content, Modifier.fillMaxWidth()) { measurables, constraints ->
        val loose = constraints.copy(minWidth = 0, minHeight = 0)
        val first = measurables[0].measure(loose)
        val second = measurables[1].measure(loose)
        val width = constraints.maxWidth
        val n = nudge.roundToPx()
        if (first.width + second.width - n <= width) {
            val height = maxOf(first.height, second.height)
            layout(width, height) {
                first.placeRelative(0, (height - first.height) / 2)
                second.placeRelative(width - second.width + n, (height - second.height) / 2)
            }
        } else {
            layout(width, first.height + second.height) {
                first.placeRelative(0, 0)
                second.placeRelative(-n, first.height)
            }
        }
    }
}

@Composable
private fun Starting() {
    Column(Modifier.fillMaxWidth().padding(vertical = 64.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        GrainField(Modifier.size(72.dp), Dissolve.OUT, cell = 3.dp, shape = CircleShape)
        Gap(18.dp)
        Muted("Starting…")
    }
}

// ───────────── Unpaired ─────────────

@Composable
private fun PairingScreen(state: UiState) {
    val noun = if (state.isTv) "TV" else "device"
    Eyebrow("${DeviceInfo.appName(state.isTv)} · Pairing")
    Gap(8.dp)
    Title("Pair this $noun")
    Gap(8.dp)
    Muted("On extend.teamofsilicons.com, choose Add a device and enter this code.")
    Gap(22.dp)
    Column(
        Modifier
            .fillMaxWidth()
            .background(Tokens.Paper, PanelShape)
            .border(1.dp, Tokens.Line, PanelShape),
    ) {
        Box(Modifier.fillMaxWidth().height(136.dp)) {
            GrainField(Modifier.fillMaxSize(), Dissolve.DOWN, cell = 4.dp, shape = RoundedCornerShape(topStart = 10.dp, topEnd = 10.dp))
            Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 14.dp), verticalAlignment = Alignment.CenterVertically) {
                Mark(18.dp, tint = Color.White)
                Spacer(Modifier.width(8.dp))
                Eyebrow("Pairing code", color = Color.White)
                Spacer(Modifier.weight(1f))
                Eyebrow("Works once", color = Color.White)
            }
        }
        Column(Modifier.padding(start = 16.dp, end = 16.dp, top = 6.dp, bottom = 14.dp)) {
            PosterCode(state.pairing.code, maxSp = 120f)
            Gap(10.dp)
            Hairline()
            Gap(10.dp)
            CodeExpiry(state)
        }
    }
    state.pairing.error?.let {
        Gap(14.dp)
        ErrorNote(it)
    }
    Gap(18.dp)
    Muted("No sign-in needed on this $noun. The code changes every 5 minutes and works once.")
}

/**
 * The pairing code at poster scale: IBM Plex Mono sized to fill the width it has, tight, in ink
 * on paper. The text stays "ABC 123" (tests and TalkBack read it); TalkBack hears it spelled out.
 */
@Composable
private fun PosterCode(code: String?, maxSp: Float) {
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        val density = LocalDensity.current
        // Plex Mono advances are 0.6 em: six characters plus a half-width gap, with tight tracking.
        val fit = with(density) { (maxWidth / 3.72f).toSp() }
        val size = if (fit.value > maxSp) maxSp.sp else fit
        val text = code?.let { it.take(3) + " " + it.drop(3) } ?: "··· ···"
        val spoken = code?.let { "Pairing code: " + it.toList().joinToString(" ") } ?: "Getting a pairing code"
        Text(
            buildAnnotatedString {
                val parts = text.split(" ", limit = 2)
                append(parts[0])
                withStyle(SpanStyle(fontSize = size * 0.5f)) { append(" ") }
                append(parts.getOrElse(1) { "" })
            },
            style = TextStyle(
                fontFamily = PlexMono,
                fontWeight = FontWeight.Medium,
                fontSize = size,
                lineHeight = size * 1.02f,
                letterSpacing = (-0.035).em,
                color = if (code != null) Tokens.Ink else Tokens.MutedMark,
                lineHeightStyle = LineHeightStyle(LineHeightStyle.Alignment.Center, LineHeightStyle.Trim.Both),
            ),
            maxLines = 1,
            softWrap = false,
            modifier = Modifier.semantics {
                contentDescription = spoken
                liveRegion = LiveRegionMode.Polite
            },
        )
    }
}

@Composable
private fun CodeExpiry(state: UiState) = CodeExpiry(state.pairing, state.isTv)

@Composable
private fun CodeExpiry(pairing: com.teamofsilicons.extend.core.PairingUi, tv: Boolean) {
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(1_000)
            now = System.currentTimeMillis()
        }
    }
    val expires = pairing.expiresAt?.let { runCatching { Instant.parse(it).toEpochMilli() }.getOrNull() }
    val left = expires?.let { ((it - now) / 1000).coerceAtLeast(0) }
    val status = when {
        pairing.code == null -> "Getting a code…"
        left == null -> ""
        left == 0L -> "Getting a new code…"
        else -> "New code in ${left / 60}:${"%02d".format(left % 60)}"
    }
    val reconnecting = !pairing.live && pairing.code != null
    Row(verticalAlignment = Alignment.CenterVertically) {
        PixelIndicator(if (pairing.live) Tokens.Cobalt else Tokens.MutedMark, on = pairing.live, size = if (tv) 12.dp else 10.dp)
        Spacer(Modifier.width(10.dp))
        Mono("$status${if (reconnecting) " · reconnecting" else ""}")
        Spacer(Modifier.weight(1f))
        if (pairing.live) Mono("Live", color = Tokens.Cobalt)
    }
}

@Composable
private fun ErrorNote(text: String) {
    Row(
        Modifier
            .fillMaxWidth()
            .background(Tokens.StopPale, Radius)
            .border(1.dp, Tokens.StopEdge, Radius)
            .padding(horizontal = 14.dp, vertical = 12.dp)
            .semantics { liveRegion = LiveRegionMode.Polite },
    ) {
        Muted(text, color = Tokens.StopDeep)
    }
}

/** TV: a poster. A dithered cobalt field on the left, the code across the paper on the right. */
@Composable
internal fun TvPairingScreen(state: UiState, footer: @Composable () -> Unit) {
    val first = remember { FocusRequester() }
    LaunchedEffect(Unit) { first.requestFocus() }
    Row(Modifier.fillMaxSize().paperGrain()) {
        Box(Modifier.fillMaxHeight().weight(0.22f)) {
            GrainField(Modifier.fillMaxSize(), Dissolve.RIGHT, cell = 6.dp, seed = 2.1f)
            Column(Modifier.fillMaxHeight().padding(start = 40.dp, top = 40.dp, bottom = 36.dp)) {
                Mark(40.dp, tint = Color.White)
                Spacer(Modifier.weight(1f))
                Eyebrow("Silicon", color = Color.White)
                Eyebrow("Extend", color = Color.White)
                Eyebrow("TV", color = Color.White)
            }
        }
        TvScrollPane(
            rememberScrollState(),
            Modifier.weight(0.78f).fillMaxHeight().padding(start = 24.dp, end = 48.dp, top = 27.dp),
        ) {
            state.environment?.let { EnvironmentBanner(it.name) }
            Eyebrow("${DeviceInfo.TV_APP_NAME} · Pairing")
            Gap(8.dp)
            Title("Pair this TV", Modifier.focusRequester(first))
            Gap(8.dp)
            Muted("On extend.teamofsilicons.com, choose Add a device and enter this code.")
            Gap(10.dp)
            PosterCode(state.pairing.code, maxSp = 96f)
            Gap(6.dp)
            CodeExpiry(state)
            state.pairing.error?.let {
                Gap(12.dp)
                ErrorNote(it)
            }
            Gap(10.dp)
            Muted("No sign-in needed on this TV. The code changes every 5 minutes and works once.")
            Gap(8.dp)
            footer()
        }
    }
}

// ───────────── Paired ─────────────

private fun formatTime(ts: String?): String = ts?.let {
    runCatching { DateTimeFormatter.ofPattern("HH:mm").withZone(ZoneId.systemDefault()).format(Instant.parse(it)) }.getOrNull()
} ?: ""

/** "Paired to Alice (c:alice)", or "Paired to c:alice (Living room TV) · c:bob (Family TV)" when several Carbons paired it. */
internal fun pairedToLine(pairs: List<PairUi>): String = when {
    pairs.isEmpty() -> "Paired"
    pairs.size == 1 -> pairs[0].let { p -> if (p.owner != null) "Paired to ${p.carbonLabel}" else "Paired · device ${p.deviceId}" }
    else -> "Paired to " + pairs.joinToString(" · ") { p -> p.carbon + (p.name?.let { " ($it)" } ?: "") }
}

@Composable
private fun PairedScreen(extend: Extend, state: UiState, actions: SetupActions) {
    val tv = state.isTv
    val noun = if (tv) "TV" else "device"
    val first = state.pairs.firstOrNull()

    Eyebrow(if (tv) "This TV" else "This device")
    Gap(8.dp)
    Title(first?.name ?: "Paired $noun")
    Gap(6.dp)
    Muted(pairedToLine(state.pairs))
    Gap(8.dp)
    LinkStatus(state)
    // One pair: its Reconnect sits here. With several, each Carbon's row has its own.
    if (state.pairs.size <= 1 && (state.link == Link.SUPERSEDED || state.link == Link.UPGRADE_REQUIRED)) {
        Gap(10.dp)
        ExtendButton("Reconnect", { first?.let { extend.connection.reconnect(it.deviceId) } ?: extend.connection.reconnect() }, tone = Tone.Secondary)
    }

    Gap(22.dp)
    InUseCard(extend, state)
    WakeRequestsCard(state)
    IndicatorSettings(extend, state)

    DeviceSetup(state, actions)

    // The Carbons before Android debugging: on a TV the remote reaches Pair with another Carbon and
    // Revoke pair without passing the debugging card's text fields (which open the keyboard).
    CarbonsCard(extend, state)
    AndroidDebuggingCard(extend, state)
}

@Composable
private fun IndicatorSettings(extend: Extend, state: UiState) {
    val tv = state.isTv
    val noun = if (tv) "TV" else "device"
    Gap(22.dp)
    CardTitle("In-use banner")
    Gap(8.dp)
    ExtendSwitch("Show a banner while a Silicon uses this $noun", state.indicatorShown, extend.connection::setInUseIndicator)
    Muted("Shows for 10 seconds when a session starts, then hides. This choice applies to every Carbon paired to this $noun. You can always stop the Silicon here or on the website.")
    if (!tv) Muted("Android's quiet running notification stays. Requests that need your help still appear.")
    else Muted("Requests that need your help still appear.")
    state.indicatorNote?.let { Muted(it) }

}

@Composable
private fun DeviceSetup(state: UiState, actions: SetupActions) {
    val tv = state.isTv
    val noun = if (tv) "TV" else "device"
    val report = state.report
    if (report != null) {
        val setup = report.setup
        val (required, optional) = report.items.partition { it.required }
        val done = required.count { it.step.status == "done" }
        Gap(if (tv) 14.dp else 34.dp)
        Eyebrow(if (setup.state == "complete") "Setup" else "Setup · $done of ${required.size} allowed")
        Gap(6.dp)
        CardTitle(if (setup.state == "complete") "Setup is done" else "Finish setting up")
        Gap(4.dp)
        Muted(if (setup.state != "complete") "Do these on this $noun, one at a time." else report.doneSummary)
        Gap(14.dp)
        // The finder's notes say more when Developer options are still off.
        val devOptions = report.items.any { it.step.key == "developer_options" && it.step.status == "done" }
        StepList(required, 1, tv, devOptions, actions)
        if (optional.isNotEmpty()) {
            Gap(22.dp)
            Eyebrow("Optional · Android debugging")
            Gap(6.dp)
            Muted(report.debuggingBenefits)
            Gap(8.dp)
            StepList(optional, required.size + 1, tv, devOptions, actions)
        }
        if (report.capabilities.isNotEmpty()) {
            Gap(22.dp)
            Capabilities(report.capabilities)
        }
    }

}

/** Short TV pages keep settings reachable without traversing every setup step. */
@Composable
private fun TvDeviceScreen(extend: Extend, state: UiState, actions: SetupActions, footer: @Composable () -> Unit) {
    var selected by rememberSaveable { mutableIntStateOf(0) }
    val labels = listOf("Overview", "Setup", "Sharing", "Debugging", "Settings")
    val first = remember { FocusRequester() }
    LaunchedEffect(Unit) { first.requestFocus() }
    BackHandler(enabled = selected != 0) { selected = 0; first.requestFocus() }
    val scroll = rememberScrollState()
    LaunchedEffect(selected) { scroll.scrollTo(0) }
    ScrollingPage(
        topBar = {
            TopBar("TV", trailing = { StatusPill(state) })
            PageColumn(Modifier.padding(top = 8.dp, bottom = 8.dp)) {
                Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    labels.forEachIndexed { index, label ->
                        ExtendButton(label, { selected = index },
                            tone = if (selected == index) Tone.Primary else Tone.Quiet,
                            modifier = Modifier.weight(1f).then(if (index == 0) Modifier.focusRequester(first) else Modifier))
                    }
                }
            }
        },
        scroll = scroll,
    ) {
        key(selected) {
            state.environment?.let { EnvironmentBanner(it.name) }
            when (selected) {
                0 -> {
                    Title(state.pairs.firstOrNull()?.name ?: "This TV")
                    Gap(8.dp)
                    Muted(pairedToLine(state.pairs))
                    Gap(6.dp)
                    LinkStatus(state)
                    if (state.pairs.size <= 1 && (state.link == Link.SUPERSEDED || state.link == Link.UPGRADE_REQUIRED)) {
                        Gap(10.dp)
                        ExtendButton("Reconnect", { extend.connection.reconnect() }, tone = Tone.Secondary)
                    }
                    Gap(14.dp)
                    InUseCard(extend, state)
                    WakeRequestsCard(state)
                    if (state.report?.setup?.state != "complete") {
                        Gap(14.dp)
                        ExtendButton("Finish setting up", { selected = 1 }, tone = Tone.Secondary)
                    }
                }
                1 -> { Title("TV setup"); DeviceSetup(state, actions) }
                2 -> { Title("Sharing this TV"); CarbonsCard(extend, state) }
                3 -> { Title("Android debugging"); AndroidDebuggingCard(extend, state) }
                4 -> { Title("TV settings"); IndicatorSettings(extend, state); Gap(24.dp); Hairline(); Gap(8.dp); footer() }
            }
        }
    }
}

/**
 * The Carbons this device is paired to, each with their name for it, their pair's connection and
 * Revoke pair (UNDERSTANDING.md, The Extend app 3, 6 and 7), and "Pair with another Carbon".
 */
@Composable
private fun CarbonsCard(extend: Extend, state: UiState) {
    val s = LocalScale.current
    val noun = if (state.isTv) "TV" else "device"
    val scope = rememberCoroutineScope()
    var confirm by remember { mutableStateOf<PairUi?>(null) }
    var revoking by remember { mutableStateOf<String?>(null) }
    var revokeError by remember { mutableStateOf<String?>(null) }
    val several = state.pairs.size > 1

    Gap(22.dp)
    Panel {
        Eyebrow(if (several) "Paired to ${state.pairs.size} Carbons" else "Paired to")
        Gap(6.dp)
        CardTitle(if (several) "Carbons who paired this $noun" else "The Carbon who paired this $noun")
        Gap(4.dp)
        Muted(
            if (several) "Each Carbon names this $noun, gives access to their own Silicons and can end their own pair. Only one Silicon uses it at a time, and any of them can stop it."
            else "Another Carbon, like someone else in your home, can pair this $noun to their own account too, with the button below.",
        )
        state.pairs.forEachIndexed { i, pair ->
            Gap(14.dp)
            if (i > 0) {
                Hairline()
                Gap(14.dp)
            }
            Body(pair.carbonLabel, weight = FontWeight.Medium)
            Mono(listOfNotNull(pair.name, "device ${pair.deviceId}", "installed Extend here".takeIf { pair.firstPair == true && several }).joinToString(" · "))
            if (several && pair.link != Link.CONNECTED) {
                Gap(4.dp)
                Muted(pairLinkText(pair, several), color = if (pair.link == Link.SUPERSEDED || pair.link == Link.UPGRADE_REQUIRED) Tokens.StopDeep else Tokens.Muted)
            }
            Gap(10.dp)
            if (several && (pair.link == Link.SUPERSEDED || pair.link == Link.UPGRADE_REQUIRED)) {
                ExtendButton("Reconnect", { extend.connection.reconnect(pair.deviceId) }, tone = Tone.Secondary)
                Gap(8.dp)
            }
            ExtendButton(
                if (revoking == pair.deviceId) "Revoking…" else "Revoke pair",
                { confirm = pair },
                enabled = revoking == null,
                tone = Tone.Danger,
                modifier = Modifier.fillMaxWidth(),
            )
        }
        revokeError?.let {
            Gap(10.dp)
            ErrorNote(it)
        }
        Gap(18.dp)
        Hairline()
        Gap(14.dp)
        ExtendButton("Pair with another Carbon", { extend.connection.startAddPair() }, tone = Tone.Secondary, modifier = Modifier.fillMaxWidth())
    }

    confirm?.let { pair ->
        val others = state.pairs.size > 1
        AlertDialog(
            onDismissRequest = { confirm = null },
            containerColor = Tokens.Paper,
            shape = PanelShape,
            title = { Text("Revoke pair?", style = Type.cardTitle(s)) },
            text = {
                Muted(
                    "This removes ${pair.name ?: "this $noun"} from ${pair.carbon}'s account and ends access for the Silicons ${pair.carbon} gave access to, " +
                        "including a session of theirs running now. " +
                        (if (others) "The other Carbons' pairs stay. " else "") +
                        "To pair it to ${pair.carbon} again, you'll need a new code.",
                )
            },
            confirmButton = {
                ExtendButton(
                    "Revoke pair",
                    {
                        confirm = null
                        revoking = pair.deviceId
                        revokeError = null
                        scope.launch {
                            try {
                                extend.connection.revokePair(pair.deviceId)
                            } catch (e: Exception) {
                                revokeError = "Couldn't revoke ${pair.carbon}'s pair: ${e.message?.trimEnd('.') ?: "the service didn't answer"}. Check this $noun's connection and try again."
                            } finally {
                                revoking = null
                            }
                        }
                    },
                    tone = Tone.Stop,
                )
            },
            dismissButton = { ExtendButton("Cancel", { confirm = null }, tone = Tone.Secondary) },
        )
    }
}

/** One pair's connection, for its row when the device has several. */
private fun pairLinkText(pair: PairUi, several: Boolean): String = when (pair.link) {
    Link.CONNECTED -> "Connected"
    Link.CONNECTING -> "Connecting…"
    Link.OFFLINE -> "Offline${pair.linkDetail?.let { " · $it" } ?: ""}"
    Link.SUPERSEDED -> Links.supersededText(pair, several)
    Link.UPGRADE_REQUIRED -> pair.linkDetail ?: "This version of the app is too old for ${pair.carbon}'s pair. Install the latest from extend.teamofsilicons.com."
}

/**
 * "Pair with another Carbon": first what sharing the device means, then a new pairing code the
 * other Carbon enters on the website (large on a TV), until it pairs or the Carbon cancels.
 */
@Composable
private fun AddPairScreen(extend: Extend, state: UiState, adding: com.teamofsilicons.extend.core.AddPairUi) {
    val tv = state.isTv
    val noun = if (tv) "TV" else "device"
    ScrollingPage(topBar = { TopBar(context = "Devices", trailing = { ExtendButton("Cancel", { extend.connection.cancelAddPair() }, tone = Tone.Quiet, flushEnd = true) }) }) {
        state.environment?.let { EnvironmentBanner(it.name) }
        Eyebrow("${DeviceInfo.appName(tv)} · Pair with another Carbon")
        Gap(8.dp)
        Title("Pair with another Carbon")
        Gap(8.dp)
        if (!adding.showingCode) {
            Panel(highlight = true) {
                Eyebrow("Before you share this $noun", color = Tokens.Cobalt)
                Gap(6.dp)
                CardTitle(SHARED_DEVICE_NOTE, heading = false)
                Gap(8.dp)
                Muted(
                    "The other Carbon pairs this $noun to their own account and gives access to their own Silicons. " +
                        "You each see only your own Silicons and their activity. Only one Silicon uses the $noun at a time, " +
                        "and any Carbon who paired it can stop it. Each of you can revoke your own pair here.",
                )
            }
            Gap(18.dp)
            ExtendButton("Show a pairing code", { extend.connection.showAddPairCode() }, modifier = Modifier.fillMaxWidth())
            Gap(10.dp)
            ExtendButton("Cancel", { extend.connection.cancelAddPair() }, tone = Tone.Secondary, modifier = Modifier.fillMaxWidth())
            return@ScrollingPage
        }
        Muted("The other Carbon chooses Add a device on extend.teamofsilicons.com and enters this code. It pairs this $noun to their account.")
        Gap(18.dp)
        adding.error?.let {
            ErrorNote(it)
            Gap(14.dp)
            ExtendButton("Back", { extend.connection.cancelAddPair() }, tone = Tone.Secondary, modifier = Modifier.fillMaxWidth())
            return@ScrollingPage
        }
        Column(
            Modifier
                .fillMaxWidth()
                .background(Tokens.Paper, PanelShape)
                .border(1.dp, Tokens.Line, PanelShape)
                .padding(horizontal = 16.dp, vertical = 14.dp),
        ) {
            Eyebrow("Pairing code for another Carbon")
            Gap(6.dp)
            PosterCode(adding.pairing.code, maxSp = if (tv) 96f else 120f)
            Gap(10.dp)
            Hairline()
            Gap(10.dp)
            CodeExpiry(adding.pairing, tv)
        }
        adding.pairing.error?.let {
            Gap(14.dp)
            ErrorNote(it)
        }
        Gap(18.dp)
        Muted("The code changes every 5 minutes and works once. Your own pair stays as it is.")
        Gap(14.dp)
        ExtendButton("Cancel", { extend.connection.cancelAddPair() }, tone = Tone.Secondary, modifier = Modifier.fillMaxWidth())
    }
}

/** What sharing a device means, shown before another Carbon's code (UNDERSTANDING.md, Pairing and Access). */
internal const val SHARED_DEVICE_NOTE = "Silicons any Carbon gives access to can use this whole device, including what others leave on it."

/** The device's open wake requests, with the same hiding as the notification ([WakeRequests.lines]). */
@Composable
private fun WakeRequestsCard(state: UiState) {
    val lines = WakeRequests.lines(state.wakeRequests, state.session)
    if (lines.isEmpty()) return
    val noun = if (state.isTv) "TV" else "device"
    Gap(22.dp)
    Panel {
        Eyebrow("Asked to use this $noun")
        Gap(6.dp)
        val named = lines.firstOrNull { it.silicon != null }?.silicon
        CardTitle(
            when {
                lines.size == 1 && named != null -> "$named asks to use this $noun"
                lines.size == 1 -> "A Silicon asked to use this $noun"
                else -> "${lines.size} Silicons asked to use this $noun"
            },
            heading = false,
        )
        lines.forEach { l ->
            Gap(10.dp)
            Body(l.silicon ?: "A Silicon", weight = FontWeight.Medium)
            Muted(l.reason ?: com.teamofsilicons.extend.core.WakeNotice.TING)
            Mono("Until ${formatTime(l.expiresAt)}")
        }
        Gap(10.dp)
        Muted(if (state.isTv) "This TV is on, so they hear it's awake." else "You're using this $noun, so they hear it's awake.")
    }
}

@Composable
private fun LinkStatus(state: UiState) {
    val (color, on, text) = when (state.link) {
        Link.CONNECTED -> Triple(Tokens.Cobalt, true, "Connected")
        Link.CONNECTING -> Triple(Tokens.MutedMark, true, "Connecting…")
        Link.OFFLINE -> Triple(Tokens.MutedMark, false, "Offline${state.linkDetail?.let { " · $it" } ?: ""}")
        Link.SUPERSEDED -> Triple(Tokens.Stop, true, state.linkDetail ?: "Connected elsewhere")
        Link.UPGRADE_REQUIRED -> Triple(Tokens.Stop, true, state.linkDetail ?: "Update required")
    }
    Row(verticalAlignment = Alignment.CenterVertically) {
        PixelIndicator(color, on = on, size = if (state.isTv) 12.dp else 10.dp)
        Spacer(Modifier.width(10.dp))
        Muted(text, color = if (color == Tokens.Stop) Tokens.StopDeep else Tokens.Muted)
    }
}

@Composable
private fun InUseCard(extend: Extend, state: UiState) {
    val s = LocalScale.current
    val session = state.session
    val noun = if (state.isTv) "TV" else "device"
    if (session == null) {
        Panel {
            Row(verticalAlignment = Alignment.Top) {
                GrainField(Modifier.size(if (s.tv) 64.dp else 52.dp), Dissolve.OUT, cell = 3.dp, seed = 4.2f, shape = CircleShape)
                Spacer(Modifier.width(16.dp))
                Column(Modifier.weight(1f)) {
                    // Not "In use": that is the status pill's word for a running session.
                    Eyebrow("Right now")
                    Gap(4.dp)
                    CardTitle(
                        "No Silicon is using this $noun",
                        heading = false,
                        modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                    )
                    Gap(4.dp)
                    Muted("When one starts, you'll see which Silicon here${if (state.isTv) " and in a corner of the screen" else " and in a notification"}, with Stop.")
                }
            }
        }
        return
    }
    Panel(highlight = true) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            SiliconAvatar(session.siliconId, if (s.tv) 52.dp else 44.dp)
            Spacer(Modifier.width(14.dp))
            Column(Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Body(session.siliconId, weight = FontWeight.SemiBold)
                    Spacer(Modifier.width(8.dp))
                    SiliconBadge()
                }
                Mono("Since ${formatTime(session.since)} · session ${session.sessionId}${session.carbon?.let { " · through $it" } ?: ""}")
            }
        }
        Gap(14.dp)
        CardTitle(
            "${session.siliconId} is using this $noun",
            heading = false,
            modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
        )
        val takeover = state.takeover
        if (takeover != null) {
            Gap(14.dp)
            Column(
                Modifier
                    .fillMaxWidth()
                    .background(Tokens.Paper, Radius)
                    .border(1.dp, Tokens.CobaltEdge, Radius)
                    .padding(16.dp),
            ) {
                Eyebrow("${session.siliconId} needs you", color = Tokens.Cobalt)
                Gap(6.dp)
                Text(takeover.reason, style = Type.cardTitle(s), modifier = Modifier.semantics { liveRegion = LiveRegionMode.Assertive })
                Gap(6.dp)
                Muted("Finish this on this $noun yourself, then tell ${session.siliconId} it can carry on.")
                Gap(12.dp)
                ExtendButton("Done", { extend.connection.takeoverDone() }, modifier = Modifier.fillMaxWidth())
            }
        }
        Gap(14.dp)
        ExtendButton(
            if (session.stopping) "Stopping…" else "Stop",
            { extend.connection.stopSession() },
            enabled = !session.stopping,
            tone = Tone.Stop,
            leading = { StopGlyph() },
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

@Composable
private fun StepList(items: List<SetupItem>, first: Int, tv: Boolean, devOptions: Boolean, actions: SetupActions) {
    Panel(padding = 0.dp) {
        items.forEachIndexed { i, item ->
            if (i > 0) Hairline()
            key(item.step.key) { StepRow(first + i, item, tv, devOptions, actions) }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun StepRow(index: Int, item: SetupItem, tv: Boolean, devOptions: Boolean, actions: SetupActions) {
    val s = LocalScale.current
    val step = item.step
    Row(Modifier.fillMaxWidth().tvReadingFocus().padding(horizontal = 16.dp, vertical = if (tv) 12.dp else 16.dp)) {
        Mono("%02d".format(index), modifier = Modifier.width(if (s.tv) 40.dp else 30.dp).padding(top = 2.dp))
        Column(Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.Top) {
                Body(step.title, weight = FontWeight.Medium, modifier = Modifier.weight(1f))
                Spacer(Modifier.width(10.dp))
                StepStatus(step.status, item.required)
            }
            if (step.status != "done") {
                step.help?.let {
                    Gap(6.dp)
                    Muted(it)
                }
                step.error?.let {
                    Gap(6.dp)
                    Muted(it, color = Tokens.StopDeep, modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite })
                }
                if (step.status == com.teamofsilicons.extend.core.SetupRetry.FAILED) {
                    Gap(12.dp)
                    ExtendButton("Retry", { actions.retry(step.key) }, tone = Tone.Primary)
                }
                val label = item.actionLabel
                val open = item.open
                if (step.key == "notifications") {
                    Gap(12.dp)
                    ExtendButton(label ?: "Allow notifications", actions.requestNotifications, tone = if (item.required) Tone.Primary else Tone.Secondary)
                } else if (open != null) {
                    Gap(12.dp)
                    val noun = if (tv) "TV" else "device"
                    // An optional step that needs the Carbon (Wireless debugging after a restart) gets the primary button too.
                    val primary = item.required || step.status == "needs_carbon"
                    // Set when no settings screen opened (a maker's menu can hide Android's pages): which setting, and where it usually is.
                    var openError by remember(step.key) { mutableStateOf<String?>(null) }
                    // Set when the button only reached a main settings screen (by what opened: a maker's menu that
                    // claims the Accessibility action counts), which can leave the entry out.
                    var mainOnly by remember(step.key) { mutableStateOf(false) }
                    // What the button opened: the list puts it last and says so, since the Carbon has looked there.
                    var opened by remember(step.key) { mutableStateOf<OpenedScreen?>(null) }
                    // "Can't find it?": the other screens on this device that may be the one this step needs.
                    var showOthers by remember(step.key) { mutableStateOf(false) }
                    // Whether the remote is on one of the two buttons: the list takes the focus only then.
                    var openFocused by remember(step.key) { mutableStateOf(false) }
                    var othersFocused by remember(step.key) { mutableStateOf(false) }
                    val find = item.find
                    val buttons: @Composable () -> Unit = {
                        ExtendButton(
                            label ?: "Open settings",
                            {
                                val r = actions.open(open)
                                openError = r.message
                                mainOnly = r.outcome == OpenResult.Outcome.MAIN_SCREEN
                                opened = r.opened
                                if (r.offerOthers && find != null) showOthers = true
                            },
                            tone = if (primary) Tone.Primary else Tone.Secondary,
                            modifier = Modifier.onFocusChanged { openFocused = it.hasFocus },
                        )
                        if (find != null) {
                            ExtendButton(
                                if (showOthers) "Hide other screens" else FindHelp.buttonLabel(tv),
                                { showOthers = !showOthers },
                                tone = Tone.Secondary,
                                modifier = Modifier.onFocusChanged { othersFocused = it.hasFocus },
                            )
                        }
                    }
                    // On a TV one under the other: side by side, the remote's Down skipped "Can't find it?" (only Right reached it).
                    if (s.tv) {
                        Column(verticalArrangement = Arrangement.spacedBy(10.dp)) { buttons() }
                    } else {
                        FlowRow(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) { buttons() }
                    }
                    openError?.let {
                        Gap(8.dp)
                        Muted(it, color = Tokens.StopDeep, modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite })
                    }
                    if (mainOnly && openError == null) {
                        Gap(8.dp)
                        Muted(
                            "That opened this $noun's main settings menu, which may not have ${open.name}. If it doesn't, try the other screens below.",
                            modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                        )
                    }
                    if (find != null && showOthers) OtherScreens(find, tv, devOptions, opened, { openFocused || othersFocused }, actions)
                }
            }
        }
    }
}

/**
 * A step's "Can't find it?" list: the screens on this device that may be the one the step needs
 * ([SettingsFinder]), each a button that opens it, with what to do there; the one the step's
 * button opened ([opened]) last. When the device hides the setting altogether, it says so and
 * what works instead. On a TV the remote moves to the first screen once the list has screens, if
 * it is still on the step's buttons ([onStepButtons]).
 */
@Composable
private fun OtherScreens(
    help: FindHelp,
    tv: Boolean,
    devOptions: Boolean,
    opened: OpenedScreen?,
    onStepButtons: () -> Boolean,
    actions: SetupActions,
) {
    val s = LocalScale.current
    val noun = if (tv) "TV" else "device"
    val sdk = Build.VERSION.SDK_INT
    val scope = rememberCoroutineScope()
    var results by remember { mutableStateOf<FinderResults?>(null) }
    // The scan failed and none worked before: say so, never that the device hides the setting.
    var scanFailed by remember { mutableStateOf(false) }
    suspend fun scan(refresh: Boolean) {
        val r = actions.findScreens(refresh)
        if (r != null) results = r
        scanFailed = r == null && results == null
    }
    // Scanned when the list opens (a scan under a minute old is reused, unless Developer options
    // changed since), again once Developer options turn on or off (Android 9 enables its real page
    // only then)...
    var scans by remember { mutableIntStateOf(0) }
    LaunchedEffect(devOptions) {
        scan(refresh = scans > 0)
        scans++
    }
    // ...and each time the Carbon comes back to Extend: a settings screen can enable or disable its own pages.
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    DisposableEffect(lifecycle) {
        var away = false
        val observer = LifecycleEventObserver { _, event ->
            when (event) {
                Lifecycle.Event.ON_PAUSE -> away = true
                Lifecycle.Event.ON_RESUME -> if (away) {
                    away = false
                    scope.launch { scan(refresh = true) }
                }
                else -> Unit
            }
        }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }
    // The candidate that didn't open (its component) and what to tell the Carbon.
    var failed by remember { mutableStateOf<Pair<String, String>?>(null) }
    val first = remember { FocusRequester() }
    // The remote is moved to the list at most once: a rescan never pulls it back.
    var focusOffered by remember { mutableStateOf(false) }
    Gap(12.dp)
    Column(
        Modifier
            .fillMaxWidth()
            .background(Tokens.Surface, Radius)
            .border(1.dp, Tokens.Line, Radius)
            .padding(horizontal = 14.dp, vertical = 14.dp),
    ) {
        Eyebrow("Other screens on this $noun")
        Gap(6.dp)
        val r = results
        if (r == null) {
            if (scanFailed) {
                Muted(
                    "Extend couldn't read this $noun's screens. Look in the $noun's own settings menu, or try again.",
                    color = Tokens.StopDeep,
                    modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                )
                Gap(8.dp)
                ExtendButton("Look again", { scope.launch { scan(refresh = true) } }, tone = Tone.Secondary)
            } else {
                Muted("Looking for screens on this $noun…", modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite })
            }
            return@Column
        }
        val list = SettingsFinder.forStep(help, r, opened = opened)
        val notes = SettingsFinder.notes(help, r, tv, sdk, devOptions)
        if (list.isNotEmpty()) {
            Muted(help.lookFor, color = Tokens.Ink)
            list.forEachIndexed { i, c ->
                Gap(8.dp)
                ChoiceButton(
                    c.title,
                    c.reason(tv, sdk),
                    { failed = actions.openCandidate(c)?.let { c.component to it } },
                    modifier = if (i == 0) Modifier.focusRequester(first) else Modifier,
                )
                failed?.takeIf { it.first == c.component }?.let { (_, message) ->
                    Gap(6.dp)
                    Muted(message, color = Tokens.StopDeep, modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite })
                }
            }
            if (s.tv) {
                LaunchedEffect(Unit) {
                    if (!focusOffered) {
                        focusOffered = true
                        // Not if the Carbon moved on while Extend was looking.
                        if (onStepButtons()) runCatching { first.requestFocus() }
                    }
                }
            }
        } else {
            Muted("Extend found no other settings screens on this $noun.")
        }
        notes.forEach {
            Gap(10.dp)
            Row(
                Modifier
                    .fillMaxWidth()
                    .background(Tokens.Paper, Radius)
                    .border(1.dp, Tokens.CobaltEdge, Radius)
                    .padding(horizontal = 14.dp, vertical = 12.dp)
                    .semantics { liveRegion = LiveRegionMode.Polite },
            ) {
                Muted(it, color = Tokens.Ink)
            }
        }
    }
}

@Composable
private fun StepStatus(status: String, required: Boolean) {
    when (status) {
        "done" -> Pill(if (required) "Allowed" else "On", Tokens.TagInk, Tokens.CobaltPale, leading = { PixelIndicator(Tokens.Cobalt, size = 7.dp) })
        "in_progress" -> Pill("Starting", Tokens.Muted, Tokens.Paper, border = Tokens.LineStrong)
        "todo" -> Pill("Not on", Tokens.Muted, Tokens.Paper, border = Tokens.LineStrong)
        "failed" -> Pill("Failed", Tokens.StopDeep, Tokens.StopPale)
        else -> Pill("Needs you", Tokens.Ink, Tokens.Paper, border = Tokens.Ink)
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Capabilities(capabilities: List<String>) {
    Eyebrow("A Silicon can use")
    Gap(10.dp)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        capabilities.forEach { Pill(it, Tokens.Ink, Tokens.Surface, border = Tokens.Line, caps = false) }
    }
}

// ───────────── Developer settings ─────────────

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun DeveloperSettingsScreen(extend: Extend, state: UiState, onClose: () -> Unit) {
    var url by remember { mutableStateOf(extend.config.serviceUrl) }
    var forceTv by remember { mutableStateOf(extend.config.forceTv) }
    var message by remember { mutableStateOf<String?>(null) }
    val paired = state.phase == Phase.PAIRED
    // Close sits in the top bar, as on the licences screen, so the form's buttons fit a narrow screen.
    ScrollingPage(topBar = { TopBar("Settings", trailing = { ExtendButton("Close", onClose, tone = Tone.Quiet, flushEnd = true) }) }) {
        Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Eyebrow("For testing")
            Title("Developer settings")
            Muted("For testing ${DeviceInfo.appName(state.isTv)}. Changing the service moves this device to another Extend.")
            Gap(4.dp)
            ExtendTextField(url, { url = it }, "Extend service URL", keyboardType = KeyboardType.Uri, mono = true)
            Column {
                Mono("Production")
                Mono("https://backend.extend.teamofsilicons.com", color = Tokens.Ink)
                Gap(8.dp)
                Mono("Emulator → this computer")
                Mono("http://10.0.2.2:8480", color = Tokens.Ink)
            }
            if (paired) Muted("Saving a different URL forgets this device's pairs here (they aren't revoked on the old service).", color = Tokens.StopDeep)
            ExtendSwitch("Behave as a TV (android_tv, corner badge, remote)", forceTv, { forceTv = it })
            FlowRow(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                ExtendButton("Save", {
                    val clean = url.trim().trimEnd('/')
                    if (!clean.startsWith("http://") && !clean.startsWith("https://")) {
                        message = "The URL must start with https:// (or http:// for 10.0.2.2 / localhost)."
                        return@ExtendButton
                    }
                    var changed = false
                    if (clean != extend.config.serviceUrl) {
                        extend.config.serviceUrl = clean
                        extend.connection.forgetPairsLocally()
                        changed = true
                    }
                    if (forceTv != extend.config.forceTv) {
                        extend.config.forceTv = forceTv
                        changed = true
                    }
                    if (changed) {
                        extend.update { it.copy(serviceUrl = extend.config.serviceUrl, isTv = extend.isTv) }
                        extend.connection.reconnect()
                        extend.onCapabilitiesMayHaveChanged()
                    }
                    message = if (changed) "Saved." else "Nothing changed."
                })
                ExtendButton("Default URL", {
                    url = BuildConfig.DEFAULT_SERVICE_URL
                    message = "Press Save to switch to ${BuildConfig.DEFAULT_SERVICE_URL}."
                }, tone = Tone.Secondary)
            }
            message?.let { Muted(it) }
            Mono("Device ids: ${state.pairs.joinToString { it.deviceId }.ifEmpty { "—" }} · os: ${extend.os} · build ${BuildConfig.BUILD_TYPE}")
        }
    }
}

/**
 * The Android debugging card's words. Once the Carbon connected debugging, it stays theirs to
 * disconnect even while it isn't connected (after a restart turned Wireless debugging off, the
 * after-restart step tells them they can), so Disconnect is offered then too.
 */
internal object DebuggingCardCopy {
    const val DISCONNECT = "Disconnect Android debugging"
    const val TURN_ON_ACCESSIBILITY = "Turn on accessibility through debugging"
    const val ACCESSIBILITY_THROUGH_DEBUGGING =
        "Accessibility isn't on yet. If this device's settings don't show Android's Accessibility page, Extend can turn it on through Android debugging."

    /** Android 8–10 (no Wireless debugging): network debugging on [port], approved on the screen. */
    fun text(connected: Boolean, enabled: Boolean, wirelessDebuggingOff: Boolean, sdk: Int, tv: Boolean, fire: Boolean, release: String, port: Int): String =
        if (DebuggingPath.mode(sdk) == DebuggingPath.Mode.NETWORK) DebuggingPath.legacyCardText(connected, enabled, tv, fire, release, port)
        else text(connected, enabled, wirelessDebuggingOff)

    /** Android 11+: Wireless debugging, paired with Android's pairing code. */
    fun text(connected: Boolean, enabled: Boolean, wirelessDebuggingOff: Boolean): String = when {
        connected -> "Connected · app installation, device logs and recording are available."
        enabled && wirelessDebuggingOff ->
            "Paired, but not connected: Wireless debugging is off (Android turns it off when this device restarts). " +
                "Turn it back on in Developer options and Extend reconnects by itself. To stop using Android debugging, tap $DISCONNECT below."
        enabled ->
            "Paired, but not connected right now; Extend keeps reconnecting. If it doesn't connect within a minute, " +
                "connect or pair again below, or tap $DISCONNECT to stop using it."
        else -> "Enable Wireless debugging in Developer options. Open ‘Pair device with pairing code’ in split screen beside Extend, " +
            "then enter its port and code here. These are Android's values, separate from your Extend pairing code."
    }

    fun offersDisconnect(connected: Boolean, enabled: Boolean) = connected || enabled
}

@Composable
private fun AndroidDebuggingCard(extend: Extend, state: UiState) {
    val sdk = Build.VERSION.SDK_INT
    // Android 8–10: no Wireless debugging, so no pairing; network debugging on port 5555.
    val legacy = DebuggingPath.mode(sdk) == DebuggingPath.Mode.NETWORK
    val scope = rememberCoroutineScope()
    var pairingPort by remember { mutableStateOf("") }
    var pairingCode by remember { mutableStateOf("") }
    var connectionPort by remember { mutableStateOf("") }
    var otherPort by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    var connected by remember { mutableStateOf(extend.adb.connected) }
    var enabled by remember { mutableStateOf(extend.adb.enabled) }
    var wirelessOff by remember { mutableStateOf(extend.adb.wirelessDebuggingOff) }
    LaunchedEffect(Unit) {
        while (true) {
            connected = extend.adb.connected
            enabled = extend.adb.enabled
            wirelessOff = extend.adb.wirelessDebuggingOff
            delay(1000)
        }
    }
    fun act(action: suspend () -> String) {
        busy = true
        scope.launch {
            try { message = action() }
            catch (e: Exception) { message = e.message ?: "Android debugging failed with no reason given. Check that debugging is on in Developer options, then try again." }
            finally { busy = false; connected = extend.adb.connected; enabled = extend.adb.enabled; extend.onCapabilitiesMayHaveChanged() }
        }
    }
    Gap(22.dp)
    Panel {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Eyebrow("Optional", modifier = Modifier.weight(1f))
            if (connected) Pill("Connected", Tokens.TagInk, Tokens.CobaltPale, leading = { PixelIndicator(Tokens.Cobalt, size = 7.dp) })
            else Pill("Not connected", Tokens.Muted, Tokens.Paper, border = Tokens.LineStrong)
        }
        Gap(6.dp)
        CardTitle("Android debugging")
        Gap(4.dp)
        state.report?.debuggingBenefits?.let { benefits ->
            Muted(benefits)
            Gap(8.dp)
        }
        Muted(
            DebuggingCardCopy.text(
                connected, enabled, wirelessOff, sdk, state.isTv, state.isFireTv, Build.VERSION.RELEASE ?: "$sdk",
                extend.adb.port.takeIf { it != 0 } ?: DebuggingPath.LEGACY_PORT,
            ),
        )
        if (!connected && legacy) {
            // No text field unless asked for: Android 8.0/8.1 give a window's first focusable
            // element focus once a key (Home, Back) has left touch mode, and a port field there
            // would open the keyboard and scroll the page away from who is using the device.
            if (otherPort) {
                Gap(12.dp)
                ExtendTextField(connectionPort, { connectionPort = it }, "Android debugging port (blank: ${DebuggingPath.LEGACY_PORT})", enabled = !busy, keyboardType = KeyboardType.Number)
            }
            Gap(12.dp)
            ExtendButton("Connect Android debugging", {
                act {
                    val port = if (connectionPort.isBlank()) DebuggingPath.LEGACY_PORT else connectionPort.toIntOrNull() ?: error("Enter a valid port.")
                    message = "Connecting to port $port… If Android asks \"Allow USB debugging?\", select Allow (tick \"Always allow from this computer\")."
                    if (extend.adb.connect(port, startedByCarbon = true)) "Connected." else extend.adb.lastError ?: "Could not connect."
                }
            }, enabled = !busy, tone = Tone.Secondary)
            if (!otherPort) {
                Gap(8.dp)
                ExtendButton("Use another port", { otherPort = true }, enabled = !busy, tone = Tone.Quiet)
            }
        } else if (!connected) {
            Gap(12.dp)
            ExtendTextField(pairingPort, { pairingPort = it }, "Android pairing port", enabled = !busy, keyboardType = KeyboardType.Number)
            Gap(8.dp)
            ExtendTextField(pairingCode, { pairingCode = it }, "Android six-digit pairing code", enabled = !busy, keyboardType = KeyboardType.Number)
            Gap(12.dp)
            ExtendButton("Pair Android debugging", {
                act {
                    extend.adb.pair(pairingPort.toIntOrNull() ?: error("Enter the pairing port."), pairingCode)
                    pairingCode = ""
                    if (extend.adb.connect()) "Paired and connected." else "Paired. Enter the connection port from the main Wireless debugging page, then tap Connect."
                }
            }, enabled = !busy, tone = Tone.Secondary)
            Gap(18.dp)
            Hairline()
            Gap(14.dp)
            Muted("Already paired, or using a TV with network debugging? Enter the connection port (usually 5555 on TVs), then approve Android's debugging prompt. Leave blank to discover this device's port.")
            Gap(10.dp)
            ExtendTextField(connectionPort, { connectionPort = it }, "Android connection port", enabled = !busy, keyboardType = KeyboardType.Number)
            Gap(12.dp)
            ExtendButton("Connect Android debugging", {
                act {
                    val port = if (connectionPort.isBlank()) 0 else connectionPort.toIntOrNull() ?: error("Enter a valid connection port.")
                    if (extend.adb.connect(port, startedByCarbon = true)) "Connected." else extend.adb.lastError ?: "Could not connect."
                }
            }, enabled = !busy, tone = Tone.Secondary)
        }
        // A maker's menu can hide Android's Accessibility page; with debugging connected the
        // Carbon can turn on Extend's own service from here instead (the shell may do that).
        val a11yOff = state.report?.items?.firstOrNull { it.step.key == "accessibility" }?.step?.status == "needs_carbon"
        if (connected && a11yOff) {
            Gap(12.dp)
            Muted(DebuggingCardCopy.ACCESSIBILITY_THROUGH_DEBUGGING)
            Gap(10.dp)
            ExtendButton(DebuggingCardCopy.TURN_ON_ACCESSIBILITY, {
                act {
                    val component = com.teamofsilicons.extend.a11y.ExtendAccessibilityService.component(extend.context).flattenToString()
                    val r = extend.adb.shell(DebuggingPath.enableAccessibilityCommand(component), check = false)
                    if (r.exitCode == 0) "Asked Android to turn on accessibility for ${DeviceInfo.appName(state.isTv)}. It shows as allowed above within a few seconds."
                    else {
                        Extend.log("turning on accessibility through debugging exited ${r.exitCode}: ${r.text.trim().take(300)}")
                        "Android didn't turn on accessibility this way. Try the Accessibility settings button above."
                    }
                }
            }, enabled = !busy, tone = Tone.Primary)
        }
        if (DebuggingCardCopy.offersDisconnect(connected, enabled)) {
            if (!connected) {
                Gap(18.dp)
                Hairline()
            }
            Gap(12.dp)
            ExtendButton(DebuggingCardCopy.DISCONNECT, {
                act {
                    extend.executor.cancelAdbCommands()
                    extend.adbExecutor.endAll()
                    extend.adb.disconnect()
                    DebuggingPath.disconnected(sdk)
                }
            }, enabled = !busy, tone = Tone.Secondary)
        }
        message?.let {
            Gap(10.dp)
            Muted(it, color = if (connected) Tokens.Cobalt else Tokens.StopDeep)
        }
    }
}

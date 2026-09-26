package com.teamofsilicons.extend.ui

import android.content.Intent
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
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
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
import com.teamofsilicons.extend.BuildConfig
import com.teamofsilicons.extend.Extend
import com.teamofsilicons.extend.core.Link
import com.teamofsilicons.extend.core.Phase
import com.teamofsilicons.extend.core.SetupItem
import com.teamofsilicons.extend.core.UiState
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

@Composable
fun AppScreen(
    extend: Extend,
    state: UiState,
    onOpenDeveloperSettings: () -> Unit,
    onRequestNotifications: () -> Unit,
    onOpen: (Intent) -> Unit,
    onOpenLicences: () -> Unit = {},
) {
    val footer: @Composable () -> Unit = { Footer(state, onOpenDeveloperSettings, onOpenLicences) }
    if (state.phase == Phase.UNPAIRED && state.isTv) {
        TvPairingScreen(state, footer)
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
            Phase.PAIRED -> PairedScreen(extend, state, onRequestNotifications, onOpen)
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
        Column(Modifier.fillMaxWidth().weight(1f).verticalScroll(scroll)) {
            PageColumn(Modifier.padding(vertical = if (s.tv) 28.dp else 22.dp), content = content)
            Spacer(Modifier.windowInsetsBottomHeight(WindowInsets.safeDrawing))
        }
    }
}

/** The breadcrumb gives context, never the page title: the Team once paired, like Interface's. */
private fun breadcrumb(state: UiState): String? = when (state.phase) {
    Phase.STARTING -> null
    Phase.UNPAIRED -> "Devices"
    Phase.PAIRED -> state.device?.team?.takeIf { it.isNotBlank() } ?: "Devices"
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
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
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
            Mono("Silicon Extend ${BuildConfig.VERSION_NAME}${if (state.isTv) " · TV" else ""}")
        }
        Spacer(Modifier.weight(1f))
        // On the TV pairing screen (nothing else to press) the remote starts on the licences link,
        // not on the hidden developer-settings trigger. Paired screens keep focus at the top.
        val licences = remember { FocusRequester() }
        if (state.isTv && state.phase == Phase.UNPAIRED) LaunchedEffect(Unit) { runCatching { licences.requestFocus() } }
        ExtendButton("Open-source licences", onOpenLicences, tone = Tone.Quiet, flushEnd = true, modifier = Modifier.focusRequester(licences))
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
    Eyebrow("Silicon Extend · Pairing")
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
private fun CodeExpiry(state: UiState) {
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(1_000)
            now = System.currentTimeMillis()
        }
    }
    val expires = state.pairing.expiresAt?.let { runCatching { Instant.parse(it).toEpochMilli() }.getOrNull() }
    val left = expires?.let { ((it - now) / 1000).coerceAtLeast(0) }
    val status = when {
        state.pairing.code == null -> "Getting a code…"
        left == null -> ""
        left == 0L -> "Getting a new code…"
        else -> "New code in ${left / 60}:${"%02d".format(left % 60)}"
    }
    val reconnecting = !state.pairing.live && state.pairing.code != null
    Row(verticalAlignment = Alignment.CenterVertically) {
        PixelIndicator(if (state.pairing.live) Tokens.Cobalt else Tokens.MutedMark, on = state.pairing.live, size = if (state.isTv) 12.dp else 10.dp)
        Spacer(Modifier.width(10.dp))
        Mono("$status${if (reconnecting) " · reconnecting" else ""}")
        Spacer(Modifier.weight(1f))
        if (state.pairing.live) Mono("Live", color = Tokens.Cobalt)
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
private fun TvPairingScreen(state: UiState, footer: @Composable () -> Unit) {
    Row(Modifier.fillMaxSize().paperGrain()) {
        Box(Modifier.fillMaxHeight().weight(0.3f)) {
            GrainField(Modifier.fillMaxSize(), Dissolve.RIGHT, cell = 6.dp, seed = 2.1f)
            Column(Modifier.fillMaxHeight().padding(start = 40.dp, top = 40.dp, bottom = 36.dp)) {
                Mark(40.dp, tint = Color.White)
                Spacer(Modifier.weight(1f))
                Eyebrow("Silicon", color = Color.White)
                Eyebrow("Extend", color = Color.White)
            }
        }
        Column(
            Modifier
                .weight(0.7f)
                .fillMaxHeight()
                .verticalScroll(rememberScrollState())
                // Android TV's overscan-safe margins: 48 dp at the sides, 27 dp top and bottom.
                .padding(start = 32.dp, end = 56.dp, top = 36.dp, bottom = 27.dp),
        ) {
            state.environment?.let { EnvironmentBanner(it.name) }
            Eyebrow("Silicon Extend · Pairing")
            Gap(8.dp)
            Title("Pair this TV")
            Gap(8.dp)
            Muted("On extend.teamofsilicons.com, choose Add a device and enter this code.")
            Gap(10.dp)
            PosterCode(state.pairing.code, maxSp = 168f)
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

@Composable
private fun PairedScreen(extend: Extend, state: UiState, onRequestNotifications: () -> Unit, onOpen: (Intent) -> Unit) {
    val s = LocalScale.current
    val tv = state.isTv
    val noun = if (tv) "TV" else "device"
    val scope = rememberCoroutineScope()
    var confirmRevoke by remember { mutableStateOf(false) }
    var revokeError by remember { mutableStateOf<String?>(null) }
    var revoking by remember { mutableStateOf(false) }

    Eyebrow(if (tv) "This TV" else "This device")
    Gap(8.dp)
    Title(state.device?.name ?: "Paired $noun")
    Gap(6.dp)
    val owner = state.device?.owner
    Muted(
        if (owner != null) "Paired to ${owner.displayName?.let { "$it (${owner.id})" } ?: owner.id}${state.device.team.takeIf { it.isNotEmpty() }?.let { " · team $it" } ?: ""}"
        else "Paired${state.deviceId?.let { " · device $it" } ?: ""}",
    )
    Gap(8.dp)
    LinkStatus(state)
    if (state.link == Link.SUPERSEDED || state.link == Link.UPGRADE_REQUIRED) {
        Gap(10.dp)
        ExtendButton("Reconnect", { extend.connection.reconnect() }, tone = Tone.Secondary)
    }

    Gap(22.dp)
    InUseCard(extend, state)

    val report = state.report
    if (report != null) {
        val setup = report.setup
        val (required, optional) = report.items.partition { it.required }
        val done = required.count { it.step.status == "done" }
        Gap(34.dp)
        Eyebrow(if (setup.state == "complete") "Setup" else "Setup · $done of ${required.size} allowed")
        Gap(6.dp)
        CardTitle(if (setup.state == "complete") "Setup is done" else "Finish setting up")
        Gap(4.dp)
        Muted(
            if (setup.state == "complete") "Core device control is ready. Android debugging below adds app installation, logs and recording."
            else "Do these on this $noun, one at a time.",
        )
        Gap(14.dp)
        StepList(required, 1, tv, onRequestNotifications, onOpen)
        if (optional.isNotEmpty()) {
            Gap(22.dp)
            Eyebrow("Optional")
            Gap(8.dp)
            StepList(optional, required.size + 1, tv, onRequestNotifications, onOpen)
        }
        if (report.capabilities.isNotEmpty()) {
            Gap(22.dp)
            Capabilities(report.capabilities)
        }
    }

    AndroidDebuggingCard(extend)
    Gap(28.dp)
    ExtendButton(
        if (revoking) "Revoking…" else "Revoke pair",
        { confirmRevoke = true },
        enabled = !revoking,
        tone = Tone.Danger,
        modifier = Modifier.fillMaxWidth(),
    )
    revokeError?.let {
        Gap(10.dp)
        ErrorNote(it)
    }

    if (confirmRevoke) {
        AlertDialog(
            onDismissRequest = { confirmRevoke = false },
            containerColor = Tokens.Paper,
            shape = PanelShape,
            title = { Text("Revoke pair?", style = Type.cardTitle(s)) },
            text = {
                Muted(
                    "This removes ${state.device?.name ?: "this device"} from ${state.device?.owner?.id ?: "your"} account and ends every Silicon's access to it, " +
                        "including any session running now. To use it with Extend again you'll pair it with a new code.",
                )
            },
            confirmButton = {
                ExtendButton(
                    "Revoke pair",
                    {
                        confirmRevoke = false
                        revoking = true
                        revokeError = null
                        scope.launch {
                            try {
                                extend.connection.revokePair()
                            } catch (e: Exception) {
                                revokeError = "Couldn't revoke the pair: ${e.message ?: "the service didn't answer"}. Check this $noun's connection and try again."
                            } finally {
                                revoking = false
                            }
                        }
                    },
                    tone = Tone.Stop,
                )
            },
            dismissButton = { ExtendButton("Cancel", { confirmRevoke = false }, tone = Tone.Secondary) },
        )
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
                Mono("Since ${formatTime(session.since)} · session ${session.sessionId}")
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
private fun StepList(items: List<SetupItem>, first: Int, tv: Boolean, onRequestNotifications: () -> Unit, onOpen: (Intent) -> Unit) {
    Panel(padding = 0.dp) {
        items.forEachIndexed { i, item ->
            if (i > 0) Hairline()
            StepRow(first + i, item, onRequestNotifications, onOpen)
        }
    }
}

@Composable
private fun StepRow(index: Int, item: SetupItem, onRequestNotifications: () -> Unit, onOpen: (Intent) -> Unit) {
    val s = LocalScale.current
    val step = item.step
    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 16.dp)) {
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
                    Muted(it, color = Tokens.StopDeep)
                }
                val label = item.actionLabel
                if (step.key == "notifications") {
                    Gap(12.dp)
                    ExtendButton(label ?: "Allow notifications", onRequestNotifications, tone = if (item.required) Tone.Primary else Tone.Secondary)
                } else if (item.open != null) {
                    Gap(12.dp)
                    ExtendButton(label ?: "Open settings", { onOpen(item.open.invoke()) }, tone = if (item.required) Tone.Primary else Tone.Secondary)
                }
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

@Composable
fun DeveloperSettingsScreen(extend: Extend, state: UiState, onClose: () -> Unit) {
    var url by remember { mutableStateOf(extend.config.serviceUrl) }
    var forceTv by remember { mutableStateOf(extend.config.forceTv) }
    var message by remember { mutableStateOf<String?>(null) }
    val paired = state.phase == Phase.PAIRED
    ScrollingPage(topBar = { TopBar("Settings") }) {
        Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Eyebrow("For testing")
            Title("Developer settings")
            Muted("For testing Silicon Extend. Changing the service moves this device to another Extend.")
            Gap(4.dp)
            ExtendTextField(url, { url = it }, "Extend service URL", keyboardType = KeyboardType.Uri)
            Column {
                Mono("Production")
                Mono("https://backend.extend.teamofsilicons.com", color = Tokens.Ink)
                Gap(8.dp)
                Mono("Emulator → this computer")
                Mono("http://10.0.2.2:8480", color = Tokens.Ink)
            }
            if (paired) Muted("Saving a different URL forgets this device's pair here (it isn't revoked on the old service).", color = Tokens.StopDeep)
            ExtendSwitch("Behave as a TV (android_tv, corner badge, remote)", forceTv, { forceTv = it })
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                ExtendButton("Save", {
                    val clean = url.trim().trimEnd('/')
                    if (!clean.startsWith("http://") && !clean.startsWith("https://")) {
                        message = "The URL must start with https:// (or http:// for 10.0.2.2 / localhost)."
                        return@ExtendButton
                    }
                    var changed = false
                    if (clean != extend.config.serviceUrl) {
                        extend.config.serviceUrl = clean
                        extend.secrets.clearCredential()
                        extend.config.clearPair()
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
                Spacer(Modifier.weight(1f))
                ExtendButton("Close", onClose, tone = Tone.Quiet, flushEnd = true)
            }
            message?.let { Muted(it) }
            Mono("Device id: ${state.deviceId ?: "—"} · os: ${extend.os} · build ${BuildConfig.BUILD_TYPE}")
        }
    }
}

@Composable
private fun AndroidDebuggingCard(extend: Extend) {
    val scope = rememberCoroutineScope()
    var pairingPort by remember { mutableStateOf("") }
    var pairingCode by remember { mutableStateOf("") }
    var connectionPort by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    var connected by remember { mutableStateOf(extend.adb.connected) }
    LaunchedEffect(Unit) {
        while (true) { connected = extend.adb.connected; delay(1000) }
    }
    fun act(action: suspend () -> String) {
        busy = true
        scope.launch {
            try { message = action() }
            catch (e: Exception) { message = e.message ?: "Android debugging failed with no reason given. Check Wireless debugging is on, then try again." }
            finally { busy = false; connected = extend.adb.connected; extend.onCapabilitiesMayHaveChanged() }
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
        Muted(
            if (connected) "Connected · app installation, device logs and recording are available."
            else "Enable Wireless debugging in Developer options. Open ‘Pair device with pairing code’ in split screen beside Extend, then enter its port and code here. These are Android's values, separate from your Extend pairing code.",
        )
        if (!connected) {
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
                    if (extend.adb.connect(port)) "Connected." else extend.adb.lastError ?: "Could not connect."
                }
            }, enabled = !busy, tone = Tone.Secondary)
        } else {
            Gap(12.dp)
            ExtendButton("Disconnect Android debugging", {
                act {
                    extend.executor.cancelAdbCommands()
                    extend.adbExecutor.endAll()
                    extend.adb.disconnect()
                    "Disconnected. You can also forget Silicon Extend in Android's Wireless debugging settings."
                }
            }, enabled = !busy, tone = Tone.Secondary)
        }
        message?.let {
            Gap(10.dp)
            Muted(it, color = if (connected) Tokens.Cobalt else Tokens.StopDeep)
        }
    }
}

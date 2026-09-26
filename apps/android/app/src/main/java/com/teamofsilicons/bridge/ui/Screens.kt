package com.teamofsilicons.bridge.ui

import android.content.Intent
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.teamofsilicons.bridge.Bridge
import com.teamofsilicons.bridge.BuildConfig
import com.teamofsilicons.bridge.core.Link
import com.teamofsilicons.bridge.core.Phase
import com.teamofsilicons.bridge.core.SetupItem
import com.teamofsilicons.bridge.core.UiState
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

private val Accent = Color(0xFF5B8CFF)
private val Danger = Color(0xFFFF5B6E)
private val Warn = Color(0xFFFFB547)
private val Ok = Color(0xFF3DDC97)
private val Card = Color(0xFF181C24)
private val Muted = Color(0xFF9AA3B2)

@Composable
fun BridgeTheme(tv: Boolean, content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = darkColorScheme(
            primary = Accent,
            background = Color(0xFF0E1116),
            surface = Color(0xFF0E1116),
            error = Danger,
        ),
    ) {
        Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) { content() }
    }
}

@Composable
fun AppScreen(
    bridge: Bridge,
    state: UiState,
    onOpenDeveloperSettings: () -> Unit,
    onRequestNotifications: () -> Unit,
    onOpen: (Intent) -> Unit,
) {
    val tv = state.isTv
    Column(
        modifier = Modifier
            .fillMaxSize()
            .systemBarsPadding()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = if (tv) 64.dp else 20.dp, vertical = if (tv) 32.dp else 16.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        state.environment?.let { EnvironmentBanner(it.name) }
        when (state.phase) {
            Phase.STARTING -> Text("Starting…", color = Muted, modifier = Modifier.padding(32.dp))
            Phase.UNPAIRED -> PairingScreen(state)
            Phase.PAIRED -> PairedScreen(bridge, state, onRequestNotifications, onOpen)
        }
        Spacer(Modifier.height(24.dp))
        VersionFooter(state, onOpenDeveloperSettings)
    }
}

@Composable
private fun EnvironmentBanner(name: String) {
    Box(
        Modifier
            .fillMaxWidth()
            .padding(bottom = 16.dp)
            .background(Warn, RoundedCornerShape(10.dp))
            .padding(12.dp),
    ) {
        Text(
            "Test environment: $name",
            color = Color.Black,
            fontWeight = FontWeight.Bold,
            modifier = Modifier.align(Alignment.Center),
        )
    }
}

@Composable
private fun VersionFooter(state: UiState, onOpenDeveloperSettings: () -> Unit) {
    // Seven taps on the version open the hidden developer settings.
    var taps by remember { mutableIntStateOf(0) }
    Text(
        "Silicon Bridge ${BuildConfig.VERSION_NAME}${if (state.isTv) " · TV" else ""}",
        color = Muted,
        fontSize = 12.sp,
        modifier = Modifier
            .clickable {
                taps++
                if (taps >= 7) {
                    taps = 0
                    onOpenDeveloperSettings()
                }
            }
            .padding(8.dp),
    )
}

// ───────────── Unpaired ─────────────

@Composable
private fun PairingScreen(state: UiState) {
    val tv = state.isTv
    val code = state.pairing.code
    Spacer(Modifier.height(if (tv) 24.dp else 32.dp))
    Text("Pair this ${if (tv) "TV" else "device"} with Silicon Bridge", fontSize = if (tv) 34.sp else 24.sp, fontWeight = FontWeight.SemiBold, textAlign = TextAlign.Center)
    Spacer(Modifier.height(12.dp))
    Text(
        "On bridge.teamofsilicons.com, choose Add a device and enter this code:",
        color = Muted,
        fontSize = if (tv) 22.sp else 16.sp,
        textAlign = TextAlign.Center,
    )
    Spacer(Modifier.height(if (tv) 32.dp else 28.dp))
    Box(
        Modifier
            .background(Card, RoundedCornerShape(20.dp))
            .border(2.dp, Accent.copy(alpha = 0.5f), RoundedCornerShape(20.dp))
            .padding(horizontal = if (tv) 64.dp else 28.dp, vertical = if (tv) 36.dp else 24.dp),
    ) {
        Text(
            text = code?.chunked(3)?.joinToString(" ") ?: "······",
            fontSize = if (tv) 140.sp else 64.sp,
            fontFamily = FontFamily.Monospace,
            fontWeight = FontWeight.Bold,
            letterSpacing = if (tv) 12.sp else 6.sp,
            color = if (code != null) Color.White else Muted,
        )
    }
    Spacer(Modifier.height(16.dp))
    CodeExpiry(state)
    state.pairing.error?.let {
        Spacer(Modifier.height(12.dp))
        Text(it, color = Danger, textAlign = TextAlign.Center, fontSize = if (tv) 20.sp else 14.sp)
    }
    Spacer(Modifier.height(24.dp))
    Text(
        "No sign-in needed on this ${if (tv) "TV" else "device"}. The code changes every 5 minutes and works once.",
        color = Muted,
        fontSize = if (tv) 18.sp else 13.sp,
        textAlign = TextAlign.Center,
    )
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
    Row(verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.size(10.dp).background(if (state.pairing.live) Ok else Warn, CircleShape))
        Spacer(Modifier.size(8.dp))
        Text(
            "$status${if (!state.pairing.live && state.pairing.code != null) " · reconnecting" else ""}",
            color = Muted,
            fontSize = if (state.isTv) 20.sp else 14.sp,
        )
    }
}

// ───────────── Paired ─────────────

private fun formatTime(ts: String?): String = ts?.let {
    runCatching { DateTimeFormatter.ofPattern("HH:mm").withZone(ZoneId.systemDefault()).format(Instant.parse(it)) }.getOrNull()
} ?: ""

@Composable
private fun PairedScreen(bridge: Bridge, state: UiState, onRequestNotifications: () -> Unit, onOpen: (Intent) -> Unit) {
    val tv = state.isTv
    val scope = rememberCoroutineScope()
    var confirmRevoke by remember { mutableStateOf(false) }
    var revokeError by remember { mutableStateOf<String?>(null) }
    var revoking by remember { mutableStateOf(false) }
    val big = if (tv) 30.sp else 22.sp
    val body = if (tv) 20.sp else 15.sp

    Column(Modifier.widthIn(max = 760.dp).fillMaxWidth()) {
        Text(state.device?.name ?: "Paired device", fontSize = if (tv) 38.sp else 28.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(6.dp))
        val owner = state.device?.owner
        Text(
            if (owner != null) "Paired to ${owner.displayName?.let { "$it (${owner.id})" } ?: owner.id}${state.device.team.takeIf { it.isNotEmpty() }?.let { " · team $it" } ?: ""}"
            else "Paired${state.deviceId?.let { " · device $it" } ?: ""}",
            color = Muted,
            fontSize = body,
        )
        Spacer(Modifier.height(8.dp))
        LinkStatus(state, body)
        if (state.link == Link.SUPERSEDED || state.link == Link.UPGRADE_REQUIRED) {
            Spacer(Modifier.height(8.dp))
            OutlinedButton(onClick = { bridge.connection.reconnect() }) { Text("Reconnect") }
        }

        Spacer(Modifier.height(20.dp))
        InUseCard(bridge, state, big, body)

        val report = state.report
        if (report != null) {
            val setup = report.setup
            Spacer(Modifier.height(24.dp))
            Text(
                if (setup.state == "complete") "Setup" else "Finish setting up",
                fontSize = big,
                fontWeight = FontWeight.SemiBold,
            )
            Spacer(Modifier.height(4.dp))
            Text(
                if (setup.state == "complete") "Everything a Silicon needs is allowed." else "Do these on this ${if (tv) "TV" else "device"}, one at a time.",
                color = Muted,
                fontSize = body,
            )
            Spacer(Modifier.height(12.dp))
            val (required, optional) = report.items.partition { it.required }
            required.forEachIndexed { i, item -> SetupCard(i + 1, item, tv, onRequestNotifications, onOpen) }
            if (optional.isNotEmpty()) {
                Spacer(Modifier.height(12.dp))
                Text("Optional", color = Muted, fontSize = body, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.height(8.dp))
                optional.forEachIndexed { i, item -> SetupCard(required.size + i + 1, item, tv, onRequestNotifications, onOpen) }
            }
            if (report.capabilities.isNotEmpty()) {
                Spacer(Modifier.height(8.dp))
                Text("A Silicon can use: ${report.capabilities.joinToString(", ")}", color = Muted, fontSize = if (tv) 16.sp else 12.sp)
            }
        }

        Spacer(Modifier.height(32.dp))
        OutlinedButton(
            onClick = { confirmRevoke = true },
            enabled = !revoking,
            colors = ButtonDefaults.outlinedButtonColors(contentColor = Danger),
            modifier = Modifier.fillMaxWidth(),
        ) { Text(if (revoking) "Revoking…" else "Revoke pair", fontSize = body) }
        revokeError?.let { Text(it, color = Danger, fontSize = body, modifier = Modifier.padding(top = 8.dp)) }
    }

    if (confirmRevoke) {
        AlertDialog(
            onDismissRequest = { confirmRevoke = false },
            title = { Text("Revoke pair?") },
            text = {
                Text(
                    "This removes ${state.device?.name ?: "this device"} from ${state.device?.owner?.id ?: "your"} account and ends every Silicon's access to it, " +
                        "including any session running now. To use it with Bridge again you'll pair it with a new code.",
                )
            },
            confirmButton = {
                Button(
                    onClick = {
                        confirmRevoke = false
                        revoking = true
                        revokeError = null
                        scope.launch {
                            try {
                                bridge.connection.revokePair()
                            } catch (e: Exception) {
                                revokeError = "Couldn't revoke: ${e.message}. Check the connection and try again."
                            } finally {
                                revoking = false
                            }
                        }
                    },
                    colors = ButtonDefaults.buttonColors(containerColor = Danger),
                ) { Text("Revoke pair") }
            },
            dismissButton = { TextButton(onClick = { confirmRevoke = false }) { Text("Cancel") } },
        )
    }
}

@Composable
private fun LinkStatus(state: UiState, size: androidx.compose.ui.unit.TextUnit) {
    val (color, text) = when (state.link) {
        Link.CONNECTED -> Ok to "Connected"
        Link.CONNECTING -> Warn to "Connecting…"
        Link.OFFLINE -> Warn to "Offline${state.linkDetail?.let { " · $it" } ?: ""}"
        Link.SUPERSEDED -> Danger to (state.linkDetail ?: "Connected elsewhere")
        Link.UPGRADE_REQUIRED -> Danger to (state.linkDetail ?: "Update required")
    }
    Row(verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.size(10.dp).background(color, CircleShape))
        Spacer(Modifier.size(8.dp))
        Text(text, fontSize = size, color = Muted)
    }
}

@Composable
private fun InUseCard(bridge: Bridge, state: UiState, big: androidx.compose.ui.unit.TextUnit, body: androidx.compose.ui.unit.TextUnit) {
    val session = state.session
    Column(
        Modifier
            .fillMaxWidth()
            .background(Card, RoundedCornerShape(16.dp))
            .border(1.dp, if (session != null) Accent else Color.Transparent, RoundedCornerShape(16.dp))
            .padding(18.dp),
    ) {
        if (session == null) {
            Text("No Silicon is using this ${if (state.isTv) "TV" else "device"}", fontSize = big)
            Spacer(Modifier.height(4.dp))
            Text("When one starts, you'll see who it is here${if (state.isTv) " and in a corner of the screen" else " and in a notification"}, with Stop.", color = Muted, fontSize = body)
            return@Column
        }
        Text("${session.siliconId} is using this ${if (state.isTv) "TV" else "device"}", fontSize = big, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(4.dp))
        Text("Since ${formatTime(session.since)} · session ${session.sessionId}", color = Muted, fontSize = body)
        val takeover = state.takeover
        if (takeover != null) {
            Spacer(Modifier.height(14.dp))
            Text("${session.siliconId} needs you:", fontSize = body, color = Warn, fontWeight = FontWeight.SemiBold)
            Text(takeover.reason, fontSize = big)
            Spacer(Modifier.height(10.dp))
            Button(onClick = { bridge.connection.takeoverDone() }, modifier = Modifier.fillMaxWidth()) { Text("Done", fontSize = body) }
        }
        Spacer(Modifier.height(14.dp))
        Button(
            onClick = { bridge.connection.stopSession() },
            enabled = !session.stopping,
            colors = ButtonDefaults.buttonColors(containerColor = Danger),
            modifier = Modifier.fillMaxWidth(),
        ) { Text(if (session.stopping) "Stopping…" else "Stop", fontSize = body) }
    }
}

@Composable
private fun SetupCard(index: Int, item: SetupItem, tv: Boolean, onRequestNotifications: () -> Unit, onOpen: (Intent) -> Unit) {
    val s = item.step
    val (color, label) = when (s.status) {
        "done" -> Ok to (if (item.required) "Allowed" else "On")
        "in_progress" -> Warn to "Starting…"
        "todo" -> Muted to "Not on"
        "failed" -> Danger to "Failed"
        else -> Warn to "Needs you"
    }
    Column(
        Modifier
            .fillMaxWidth()
            .padding(bottom = 10.dp)
            .background(Card, RoundedCornerShape(14.dp))
            .padding(16.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("$index. ${s.title}", fontSize = if (tv) 22.sp else 16.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
            Text(label, color = color, fontSize = if (tv) 18.sp else 13.sp)
        }
        if (s.status != "done") {
            s.help?.let {
                Spacer(Modifier.height(6.dp))
                Text(it, color = Muted, fontSize = if (tv) 18.sp else 13.sp)
            }
            Spacer(Modifier.height(10.dp))
            if (s.key == "notifications") {
                Button(onClick = onRequestNotifications) { Text(item.actionLabel ?: "Allow") }
            } else if (item.open != null) {
                Button(onClick = { onOpen(item.open.invoke()) }) { Text(item.actionLabel ?: "Open settings") }
            }
        }
    }
}

// ───────────── Developer settings ─────────────

@Composable
fun DeveloperSettingsScreen(bridge: Bridge, state: UiState, onClose: () -> Unit) {
    var url by remember { mutableStateOf(bridge.config.serviceUrl) }
    var forceTv by remember { mutableStateOf(bridge.config.forceTv) }
    var message by remember { mutableStateOf<String?>(null) }
    val paired = state.phase == Phase.PAIRED
    Column(
        Modifier
            .fillMaxSize()
            .systemBarsPadding()
            .verticalScroll(rememberScrollState())
            .padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("Developer settings", fontSize = 24.sp, fontWeight = FontWeight.SemiBold)
        Text("For testing Silicon Bridge. Changing the service moves this device to another Bridge.", color = Muted)
        OutlinedTextField(
            value = url,
            onValueChange = { url = it },
            label = { Text("Bridge service URL") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        Text("Production: https://backend.bridge.teamofsilicons.com · Emulator → this computer: http://10.0.2.2:8480", color = Muted, fontSize = 12.sp)
        if (paired) Text("Saving a different URL forgets this device's pair here (it isn't revoked on the old service).", color = Warn, fontSize = 13.sp)
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Behave as a TV (android_tv, corner badge, remote)", modifier = Modifier.weight(1f))
            Switch(checked = forceTv, onCheckedChange = { forceTv = it })
        }
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Button(onClick = {
                val clean = url.trim().trimEnd('/')
                if (!clean.startsWith("http://") && !clean.startsWith("https://")) {
                    message = "The URL must start with https:// (or http:// for 10.0.2.2 / localhost)."
                    return@Button
                }
                var changed = false
                if (clean != bridge.config.serviceUrl) {
                    bridge.config.serviceUrl = clean
                    bridge.secrets.clearCredential()
                    bridge.config.clearPair()
                    changed = true
                }
                if (forceTv != bridge.config.forceTv) {
                    bridge.config.forceTv = forceTv
                    changed = true
                }
                if (changed) {
                    bridge.update { it.copy(serviceUrl = bridge.config.serviceUrl, isTv = bridge.isTv) }
                    bridge.connection.reconnect()
                    bridge.onCapabilitiesMayHaveChanged()
                }
                message = if (changed) "Saved." else "Nothing changed."
            }) { Text("Save") }
            OutlinedButton(onClick = {
                url = BuildConfig.DEFAULT_SERVICE_URL
                message = "Press Save to switch to ${BuildConfig.DEFAULT_SERVICE_URL}."
            }) { Text("Default URL") }
            TextButton(onClick = onClose) { Text("Close") }
        }
        message?.let { Text(it, color = Muted) }
        Text("Device id: ${state.deviceId ?: "—"} · os: ${bridge.os} · build ${BuildConfig.BUILD_TYPE}", color = Muted, fontSize = 12.sp)
    }
}

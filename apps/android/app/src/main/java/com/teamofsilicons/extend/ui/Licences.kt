package com.teamofsilicons.extend.ui

import android.content.Context
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.focusable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** The asset listing every open-source component in the app and its licence text. */
const val LICENCES_ASSET = "open_source_licences.txt"

fun readLicences(context: Context): String =
    runCatching { context.assets.open(LICENCES_ASSET).bufferedReader().use { it.readText() } }
        .getOrElse { "The licence list could not be read from this app (${it.message}). Reinstall Silicon Extend to restore it." }

/** One paragraph of the notices as the screen shows it; [heading] paragraphs were set between rules. */
data class LicenceBlock(val text: String, val heading: Boolean)

private val RULE = Regex("^\\s*[=#]{8,}\\s*$")
private val LIST_ITEM = Regex("^(?:[-*]|\\(?[0-9a-z]{1,3}[.)])\\s")
private val COORDINATE = Regex("^[\\w.\\-]+:[\\w.\\-]+:[\\w.\\-]+$")

/**
 * The notices file is hard-wrapped at 72–96 columns, which re-wraps raggedly on a phone. For
 * display only, this drops the ==== / #### rules (a heading takes their place), removes the
 * paragraph's common indent and joins wrapped lines back into sentences. List items, artifact
 * coordinates, lines that step back out and indented lines after a colon, semicolon or full stop
 * keep their own line. Only whitespace changes.
 */
fun licenceBlocks(raw: String): List<LicenceBlock> {
    val blocks = ArrayList<LicenceBlock>()
    for (paragraph in raw.replace("\r", "").split(Regex("\n\\s*\n"))) {
        // Text between a pair of rules is a heading; anything after the closing rule is body.
        var heading = false
        val buffer = ArrayList<String>()
        fun flush(asHeading: Boolean) {
            reflow(buffer)?.let { blocks += LicenceBlock(it, asHeading) }
            buffer.clear()
        }
        for (line in paragraph.lines()) {
            if (RULE.matches(line)) {
                flush(heading)
                heading = !heading
            } else {
                buffer += line
            }
        }
        flush(false)
    }
    return blocks
}

private fun reflow(lines: List<String>): String? {
    val kept = lines.filterNot { it.isBlank() }
    if (kept.isEmpty()) return null
    val base = kept.minOf { it.length - it.trimStart().length }
    val out = StringBuilder()
    var previousIndent = 0
    for (raw in kept) {
        val line = raw.drop(base).trimEnd()
        val t = line.trimStart()
        val indent = line.length - t.length
        val ownLine = LIST_ITEM.containsMatchIn(t) ||
            COORDINATE.matches(t) ||
            indent < previousIndent ||
            (indent > 0 && out.isNotEmpty() && out.last() in ":;.")
        when {
            out.isEmpty() -> out.append(" ".repeat(minOf(indent, 2))).append(t)
            // At most two spaces of indent: deeper alignment wraps badly on a phone.
            ownLine -> out.append('\n').append(" ".repeat(minOf(indent, 2))).append(t)
            else -> out.append(' ').append(t)
        }
        previousIndent = indent
    }
    return out.toString()
}

/**
 * Open-source licences: the notices the app's components require it to show. The text sits on
 * the page gutter like the title above it; on TV each paragraph takes focus in turn, marked by a
 * cobalt wash that reaches out into the gutter, so the remote can page through it.
 */
@Composable
fun LicencesScreen(onClose: () -> Unit) {
    val context = LocalContext.current
    val s = LocalScale.current
    // Paragraphs, so a long licence scrolls smoothly and a TV remote can move through it.
    val blocks = remember { licenceBlocks(readLicences(context)) }
    BackHandler(onBack = onClose)
    val bottom = WindowInsets.safeDrawing.only(WindowInsetsSides.Bottom).asPaddingValues()
    Column(
        Modifier
            .fillMaxSize()
            .paperGrain()
            .windowInsetsPadding(WindowInsets.safeDrawing.only(WindowInsetsSides.Top + WindowInsetsSides.Horizontal)),
    ) {
        TopBar("About", trailing = { ExtendButton("Close", onClose, tone = Tone.Quiet, flushEnd = true) })
        LazyColumn(
            Modifier.fillMaxWidth().weight(1f),
            contentPadding = PaddingValues(start = s.gutter, end = s.gutter, top = 22.dp, bottom = 22.dp + bottom.calculateBottomPadding()),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            item {
                Column(Modifier.widthIn(max = s.column).fillMaxWidth().padding(bottom = 10.dp)) {
                    Eyebrow("${com.teamofsilicons.extend.config.DeviceInfo.appName(s.tv)} ${com.teamofsilicons.extend.BuildConfig.VERSION_NAME}")
                    Gap(8.dp)
                    Title("Open-source licences")
                    Gap(6.dp)
                    Muted("The software and fonts this app is built with, and the licences they are used under.")
                }
            }
            itemsIndexed(blocks) { i, block ->
                val interaction = remember { MutableInteractionSource() }
                val focused by interaction.collectIsFocusedAsState()
                Column(Modifier.widthIn(max = s.column).fillMaxWidth()) {
                    if (block.heading) {
                        // The rule stays outside the part that takes focus.
                        Gap(if (i > 0) 18.dp else 8.dp)
                        Hairline()
                        Gap(8.dp)
                    }
                    Column(
                        Modifier
                            .fillMaxWidth()
                            .drawBehind {
                                if (focused) {
                                    // Out into the gutter, so the text itself never moves.
                                    val o = 12.dp.toPx()
                                    val r = CornerRadius(6.dp.toPx())
                                    drawRoundRect(Tokens.CobaltWash, Offset(-o, 0f), Size(size.width + 2 * o, size.height), r)
                                    drawRoundRect(Tokens.CobaltEdge, Offset(-o, 0f), Size(size.width + 2 * o, size.height), r, style = Stroke(1.dp.toPx()))
                                }
                            }
                            .focusable(interactionSource = interaction)
                            .padding(vertical = 6.dp),
                    ) {
                        if (block.heading) {
                            Text(block.text, style = Type.body(s).copy(fontWeight = FontWeight.Medium, fontSize = s.small * 1.1f, lineHeight = s.small * 1.6f))
                        } else {
                            Text(
                                block.text,
                                style = TextStyle(
                                    fontFamily = PlexMono,
                                    fontSize = if (s.tv) 15.sp else 12.5.sp,
                                    lineHeight = if (s.tv) 23.sp else 19.sp,
                                    color = Tokens.Muted,
                                ),
                            )
                        }
                    }
                }
            }
        }
    }
}

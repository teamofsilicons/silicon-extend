package com.teamofsilicons.extend.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.background
import androidx.compose.foundation.focusable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.composed
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.gestures.scrollBy
import kotlinx.coroutines.launch

/** Read-only headings and setup rows are valid remote destinations, including above buttons. */
fun Modifier.tvReadingFocus(): Modifier = composed {
    if (!LocalScale.current.tv) return@composed this
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    this.focusRing(focused).focusable(interactionSource = interaction)
}

/**
 * A remote can follow buttons normally, or move right to the scroll rail and use Up/Down.
 * The rail stays outside the scrolling content: even a page of text or a paragraph taller
 * than the viewport can be read in either direction without focus jumping to a distant button.
 */
@Composable
internal fun TvScrollPane(
    scroll: ScrollState,
    modifier: Modifier = Modifier,
    content: @Composable ColumnScope.() -> Unit,
) {
    val scope = rememberCoroutineScope()
    val step = with(LocalDensity.current) { 96.dp.toPx() }
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Column(modifier.fillMaxWidth()) {
        Row(Modifier.weight(1f), horizontalArrangement = Arrangement.spacedBy(16.dp)) {
            Column(
                Modifier.weight(1f).verticalScroll(scroll).padding(8.dp),
                content = content,
            )
            // Keep a stable width when no scrolling is needed; code and text never reflow
            // when an error appears or setup changes. Android's safe TV margins wrap this pane.
            if (scroll.maxValue == 0) {
                Spacer(Modifier.width(32.dp))
            } else Column(
                Modifier.width(32.dp).fillMaxHeight().padding(vertical = 10.dp)
                    .focusRing(focused)
                    .background(if (focused) Tokens.CobaltWash else Tokens.Surface, RoundedCornerShape(6.dp))
                    .semantics { contentDescription = "Scroll page" }
                    .onPreviewKeyEvent { event ->
                        val delta = when (event.key) {
                            Key.DirectionUp -> -step
                            Key.DirectionDown -> step
                            Key.PageUp -> -scroll.viewportSize * 0.8f
                            Key.PageDown -> scroll.viewportSize * 0.8f
                            else -> return@onPreviewKeyEvent false
                        }
                        if (event.type == KeyEventType.KeyDown) scope.launch { scroll.scrollBy(delta) }
                        true
                    }
                    .focusable(interactionSource = interaction)
                    .padding(vertical = 8.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text("↑", color = if (scroll.canScrollBackward) Tokens.Cobalt else Tokens.MutedMark)
                Canvas(Modifier.weight(1f).width(4.dp).padding(vertical = 8.dp)) {
                    drawRoundRect(Tokens.Line, cornerRadius = CornerRadius(size.width))
                    val total = scroll.maxValue.toFloat() + scroll.viewportSize
                    val fraction = if (total > 0) scroll.viewportSize / total else 1f
                    val thumb = (size.height * fraction).coerceIn(16.dp.toPx().coerceAtMost(size.height), size.height)
                    val top = if (scroll.maxValue > 0) (size.height - thumb) * scroll.value / scroll.maxValue else 0f
                    drawRoundRect(Tokens.Cobalt, Offset(0f, top), Size(size.width, thumb), CornerRadius(size.width))
                }
                Text("↓", color = if (scroll.canScrollForward) Tokens.Cobalt else Tokens.MutedMark)
            }
        }
        Text(
            when {
                focused -> "↑ ↓  Scroll    ·    ←  Back to controls"
                scroll.maxValue > 0 -> "Use the arrows to move    ·    Select the scroll bar to scroll"
                else -> "Use the arrows to move    ·    OK to select"
            },
            style = Type.eyebrow(LocalScale.current).copy(letterSpacing = androidx.compose.ui.unit.TextUnit.Unspecified),
            modifier = Modifier.padding(top = 6.dp, bottom = 27.dp),
        )
    }
}

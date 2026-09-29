package com.teamofsilicons.extend.ui

import androidx.compose.animation.core.animateDpAsState
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsFocusedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import com.teamofsilicons.extend.R

val Radius = RoundedCornerShape(6.dp)
val PanelShape = RoundedCornerShape(10.dp)

/** A small uppercase mono label above a title, like Interface's "YOUR SHARED SPACE". */
@Composable
fun Eyebrow(text: String, modifier: Modifier = Modifier, color: Color = Tokens.Muted) {
    Text(text.uppercase(), style = Type.eyebrow(LocalScale.current).copy(color = color), modifier = modifier)
}

/** A page title: Source Serif, written as a sentence that ends in a period. */
@Composable
fun Title(text: String, modifier: Modifier = Modifier) {
    Text(sentence(text), style = Type.title(LocalScale.current), modifier = modifier.tvReadingFocus().semantics { heading() })
}

@Composable
fun CardTitle(text: String, modifier: Modifier = Modifier, heading: Boolean = true) {
    Text(
        sentence(text),
        style = Type.cardTitle(LocalScale.current),
        modifier = if (heading) modifier.semantics { heading() } else modifier,
    )
}

@Composable
fun Body(text: String, modifier: Modifier = Modifier, color: Color = Tokens.Ink, weight: FontWeight? = null) {
    Text(text, style = Type.body(LocalScale.current).copy(color = color, fontWeight = weight), modifier = modifier)
}

@Composable
fun Muted(text: String, modifier: Modifier = Modifier, color: Color = Tokens.Muted) {
    Text(text, style = Type.muted(LocalScale.current).copy(color = color), modifier = modifier)
}

@Composable
fun Mono(text: String, modifier: Modifier = Modifier, color: Color = Tokens.Muted) {
    Text(text, style = Type.mono(LocalScale.current).copy(color = color), modifier = modifier)
}

/** Ends a title with a period unless it already ends in punctuation. */
fun sentence(text: String): String {
    val t = text.trimEnd()
    return if (t.isEmpty() || t.last() in ".?!…") t else "$t."
}

/** A pill badge in mono caps: SILICON tags, statuses, capabilities. */
@Composable
fun Pill(
    text: String,
    fg: Color,
    bg: Color,
    modifier: Modifier = Modifier,
    border: Color? = null,
    caps: Boolean = true,
    leading: (@Composable () -> Unit)? = null,
) {
    val s = LocalScale.current
    val shape = RoundedCornerShape(50)
    Row(
        modifier
            .background(bg, shape)
            .then(if (border != null) Modifier.border(1.dp, border, shape) else Modifier)
            .padding(horizontal = if (s.tv) 12.dp else 9.dp, vertical = if (s.tv) 5.dp else 3.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        leading?.invoke()
        Text(
            if (caps) text.uppercase() else text,
            style = TextStyle(
                fontFamily = PlexMono,
                fontSize = if (s.tv) s.eyebrow * 0.95f else s.eyebrow * 0.92f,
                letterSpacing = 0.08.em,
                color = fg,
            ),
            maxLines = 1,
        )
    }
}

@Composable
fun SiliconBadge() = Pill("Silicon", Tokens.TagInk, Tokens.CobaltPale, border = Tokens.CobaltEdge)

/**
 * Interface's `:focus-visible` outline (`2px solid var(--blue)`, `outline-offset: 4px`): a 2 dp
 * cobalt ring 4 dp outside the element while it has focus, so a TV remote always shows where it is.
 */
fun Modifier.focusRing(focused: Boolean, radius: Dp = 6.dp, color: Color = FocusRing.color): Modifier = drawWithContent {
    drawContent()
    if (focused) {
        val stroke = FocusRing.width.toPx()
        // The stroke is centred on its path: put the path half a stroke beyond the offset.
        val o = FocusRing.offset.toPx() + stroke / 2
        drawRoundRect(
            color = color,
            topLeft = Offset(-o, -o),
            size = Size(size.width + 2 * o, size.height + 2 * o),
            cornerRadius = CornerRadius(radius.toPx() + o),
            style = Stroke(width = stroke),
        )
    }
}

/** Interface's focus-visible outline, as the app draws it ([focusRing]). */
object FocusRing {
    val color = Tokens.Cobalt
    val width = 2.dp
    val offset = 4.dp
}

enum class Tone { Primary, Secondary, Stop, Danger, Quiet }

/**
 * Interface's button: 6 dp corners, Plex Sans medium, at least 48 dp tall for touch and remotes.
 * [flushEnd] (quiet buttons at the end of a row) shifts the button by its own padding so its text
 * lines up with the column's end edge; the touch target keeps its full size, out into the gutter.
 * TVs keep the container inside the column, since focus paints it.
 */
@Composable
fun ExtendButton(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    tone: Tone = Tone.Primary,
    enabled: Boolean = true,
    flushEnd: Boolean = false,
    leading: (@Composable () -> Unit)? = null,
) {
    val s = LocalScale.current
    val hPad = 18.dp
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val (container, content) = when (tone) {
        Tone.Primary -> Tokens.Cobalt to Color.White
        Tone.Stop -> Tokens.Stop to Color.White
        Tone.Secondary -> Tokens.Paper to Tokens.Ink
        Tone.Danger -> Tokens.Paper to Tokens.StopDeep
        Tone.Quiet -> Color.Transparent to Tokens.Cobalt
    }
    val border = when (tone) {
        Tone.Secondary -> BorderStroke(1.dp, Tokens.LineStrong)
        Tone.Danger -> BorderStroke(1.dp, Tokens.StopEdge)
        else -> null
    }
    Button(
        onClick = onClick,
        enabled = enabled,
        shape = Radius,
        border = border,
        interactionSource = interaction,
        // Disabled buttons stay solid and readable ("Stopping…" is a status people read), one shade deeper.
        colors = ButtonDefaults.buttonColors(
            containerColor = container,
            contentColor = content,
            disabledContainerColor = when (tone) {
                Tone.Primary -> Tokens.CobaltDeep
                Tone.Stop -> Tokens.StopDeep
                else -> container
            },
            disabledContentColor = when (tone) {
                Tone.Primary, Tone.Stop -> Color.White
                else -> Tokens.Muted
            },
        ),
        elevation = null,
        contentPadding = PaddingValues(horizontal = hPad, vertical = 10.dp),
        modifier = modifier
            // Not on TV: there a focused quiet button shows its container, which must stay inside the column.
            .then(if (flushEnd && !s.tv) Modifier.offset(x = hPad) else Modifier)
            .heightIn(min = 48.dp)
            .widthIn(min = 48.dp)
            .focusRing(focused),
    ) {
        if (leading != null) {
            leading()
            Spacer(Modifier.width(10.dp))
        }
        // A long label on a narrow screen with large text wraps to a second line rather than
        // losing its end ("Disconnect Android debu…").
        Text(text, style = Type.label(s), maxLines = 2, overflow = TextOverflow.Ellipsis, textAlign = androidx.compose.ui.text.style.TextAlign.Center)
    }
}

/**
 * One choice in a list: a cobalt title with a muted line under it, the whole row one button (at
 * least 56 dp, 64 dp on TV) with Interface's focus ring and a cobalt wash while focused, so a TV
 * remote can walk the list and a phone can tap it.
 */
@Composable
fun ChoiceButton(title: String, detail: String?, onClick: () -> Unit, modifier: Modifier = Modifier) {
    val s = LocalScale.current
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    Button(
        onClick = onClick,
        shape = Radius,
        border = BorderStroke(if (focused) 1.5.dp else 1.dp, if (focused) Tokens.Cobalt else Tokens.LineStrong),
        interactionSource = interaction,
        colors = ButtonDefaults.buttonColors(containerColor = if (focused) Tokens.CobaltWash else Tokens.Paper, contentColor = Tokens.Ink),
        elevation = null,
        contentPadding = PaddingValues(horizontal = if (s.tv) 20.dp else 16.dp, vertical = 10.dp),
        modifier = modifier
            .fillMaxWidth()
            .heightIn(min = if (s.tv) 64.dp else 56.dp)
            .focusRing(focused),
    ) {
        Column(Modifier.fillMaxWidth()) {
            Text(title, style = Type.label(s).copy(color = Tokens.Cobalt), maxLines = 2, overflow = TextOverflow.Ellipsis)
            if (detail != null) Text(detail, style = Type.muted(s), maxLines = 3, overflow = TextOverflow.Ellipsis)
        }
    }
}

/** A calm card on paper with Interface's hairline. [highlight] marks the one thing happening now. */
@Composable
fun Panel(
    modifier: Modifier = Modifier,
    highlight: Boolean = false,
    padding: Dp = 18.dp,
    content: @Composable ColumnScope.() -> Unit,
) {
    Column(
        modifier
            .fillMaxWidth()
            .background(if (highlight) Tokens.CobaltWash else Tokens.Paper, PanelShape)
            .border(1.dp, if (highlight) Tokens.CobaltEdge else Tokens.Line, PanelShape)
            .padding(padding),
        content = content,
    )
}

@Composable
fun Hairline(modifier: Modifier = Modifier) {
    Box(modifier.fillMaxWidth().height(1.dp).background(Tokens.Line))
}

/** The Extend mark, tinted. */
@Composable
fun Mark(size: Dp, tint: Color = Tokens.Cobalt) {
    Icon(painterResource(R.drawable.ic_extend_mark), contentDescription = null, tint = tint, modifier = Modifier.size(size))
}

/**
 * The page's column: [Scale.gutter] in from the sides, at most [Scale.column] wide, centred. The
 * top bar and the content both sit in it, so the mark, titles, cards and status pill share edges.
 */
@Composable
fun PageColumn(modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) {
    val s = LocalScale.current
    Box(modifier.fillMaxWidth().padding(horizontal = s.gutter), contentAlignment = Alignment.TopCenter) {
        Column(Modifier.widthIn(max = s.column).fillMaxWidth(), content = content)
    }
}

/**
 * Interface's top bar: the mark, a breadcrumb that gives context (`extend / acme`, the Team, like
 * Interface's `interface / Bricks`), and a status or action on the right. On TV it starts below
 * the overscan area.
 */
@Composable
fun TopBar(context: String?, trailing: @Composable () -> Unit = {}) {
    val s = LocalScale.current
    Column(Modifier.fillMaxWidth()) {
        PageColumn(Modifier.padding(top = s.overscanTop)) {
            Row(
                Modifier.fillMaxWidth().heightIn(min = if (s.tv) 48.dp else 56.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Mark(26.dp)
                Spacer(Modifier.width(10.dp))
                Text("extend", style = Type.body(s).copy(fontWeight = FontWeight.Medium))
                if (context != null) {
                    Text("  /  ", style = Type.body(s).copy(color = Tokens.MutedMark))
                    Text(
                        context,
                        style = Type.body(s).copy(color = Tokens.Muted),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f),
                    )
                } else {
                    Spacer(Modifier.weight(1f))
                }
                Spacer(Modifier.width(12.dp))
                trailing()
            }
        }
        Hairline()
    }
}

/**
 * Interface's form field: a mono uppercase label above a 6 dp paper field with a hairline edge
 * that turns cobalt while you type. The label is part of the field, so TalkBack reads it with it.
 */
@Composable
fun ExtendTextField(
    value: String,
    onValueChange: (String) -> Unit,
    label: String,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    keyboardType: KeyboardType = KeyboardType.Text,
    /** Set in IBM Plex Mono, like Interface's URLs, codes and ids. */
    mono: Boolean = false,
) {
    val s = LocalScale.current
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    BasicTextField(
        value = value,
        onValueChange = onValueChange,
        enabled = enabled,
        singleLine = true,
        textStyle = (if (mono) Type.monoField(s) else Type.body(s)).copy(color = if (enabled) Tokens.Ink else Tokens.Muted),
        cursorBrush = SolidColor(Tokens.Cobalt),
        keyboardOptions = KeyboardOptions(keyboardType = keyboardType),
        interactionSource = interaction,
        modifier = modifier.fillMaxWidth(),
        decorationBox = { field ->
            Column {
                Eyebrow(label, color = if (focused) Tokens.Cobalt else Tokens.Muted)
                Gap(8.dp)
                Box(
                    Modifier
                        .fillMaxWidth()
                        .heightIn(min = 48.dp)
                        .background(if (enabled) Tokens.Paper else Tokens.Surface, Radius)
                        .border(if (focused) 1.5.dp else 1.dp, if (focused) Tokens.Cobalt else Tokens.LineStrong, Radius)
                        .padding(horizontal = 14.dp, vertical = 10.dp),
                    contentAlignment = Alignment.CenterStart,
                ) { field() }
            }
        },
    )
}

/**
 * A setting that turns on and off. The whole row is the switch (one 48 dp target that TalkBack
 * reads as "label, switch, on/off"); the track is Interface's pill with a square thumb from the
 * mark's grid.
 */
@Composable
fun ExtendSwitch(label: String, checked: Boolean, onCheckedChange: (Boolean) -> Unit, modifier: Modifier = Modifier) {
    val interaction = remember { MutableInteractionSource() }
    val focused by interaction.collectIsFocusedAsState()
    val track = 46.dp
    val thumb = 18.dp
    val x by animateDpAsState(if (checked) track - thumb - 8.dp else 0.dp, label = "switch")
    val pill = RoundedCornerShape(50)
    Row(
        modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .focusRing(focused)
            .toggleable(value = checked, onValueChange = onCheckedChange, role = Role.Switch, interactionSource = interaction, indication = null),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Body(label, modifier = Modifier.weight(1f))
        Spacer(Modifier.width(14.dp))
        Box(
            Modifier
                .size(track, 26.dp)
                .background(if (checked) Tokens.Cobalt else Tokens.Surface, pill)
                .border(1.dp, if (checked) Tokens.Cobalt else Tokens.LineStrong, pill)
                .padding(4.dp),
            contentAlignment = Alignment.CenterStart,
        ) {
            Box(Modifier.offset(x = x).size(thumb).background(if (checked) Color.White else Tokens.MutedMark, RoundedCornerShape(4.dp)))
        }
    }
}

/** A white "stop" square for the Stop button. */
@Composable
fun StopGlyph() {
    Box(Modifier.size(10.dp).background(Color.White, RoundedCornerShape(1.dp)))
}

@Composable
fun Gap(h: Dp) = Spacer(Modifier.height(h))

package com.teamofsilicons.extend.ui

import android.graphics.Bitmap
import android.graphics.RuntimeShader
import android.os.Build
import androidx.annotation.RequiresApi
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.ImageShader
import androidx.compose.ui.graphics.RectangleShape
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.ShaderBrush
import androidx.compose.ui.graphics.TileMode
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.atan2
import kotlin.math.exp
import kotlin.math.floor
import kotlin.math.hypot
import kotlin.math.max
import kotlin.math.sin

/**
 * The printed texture from the Pinterest board and Interface's shader: a cobalt field that
 * dissolves into paper through ordered (Bayer) dithering, with grain on top. Decorative only;
 * it never carries meaning, so it has no accessibility semantics.
 */
/** Which way the field dissolves into paper: downward, rightward, or outward from the centre (an orb). */
enum class Dissolve(val mode: Float) { DOWN(0f), RIGHT(1f), OUT(2f) }

/** Debug builds can force the static fallback (`--ez static_grain true`) to check it on a new device. */
object GrainSettings {
    var forceStatic by mutableStateOf(false)
}

/**
 * How fast the orb (Dissolve.OUT) falls off: at 1.9 its tone reaches paper before the tile's edge,
 * so it reads as an orb dissolving into paper, not a clipped square.
 */
private const val ORB_FALLOFF = 1.9

/** How often a peach speck is printed along the cobalt/sky horizon (a quiet accent, not a pattern). */
private const val HORIZON_DENSITY = 0.045

/** The field's five inks, straight from Tokens, for the shader uniforms and the bitmap fallback. */
object Inks {
    val paper = Tokens.Paper
    val sky = Tokens.Sky
    val cobalt = Tokens.Cobalt
    val deep = Tokens.CobaltDeep
    val horizon = Tokens.Horizon
}

private val AGSL = """
uniform float2 size;
uniform float cell;
uniform float fieldMode;
uniform float seed;
// The inks come from Tokens (via Inks), so the shader and the bitmap fallback can't drift apart.
uniform float3 inkPaper;
uniform float3 inkSky;
uniform float3 inkCobalt;
uniform float3 inkDeep;
uniform float3 inkHorizon;

// "Hash without sine" (Dave Hoskins): stable on GPUs, where sin() of large arguments isn't.
float hash(float2 p) {
    float3 p3 = fract(float3(p.x, p.y, p.x) * 0.1031 + seed * 0.37);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}
float bayer2(float2 a) { a = floor(a); return fract(a.x / 2.0 + a.y * a.y * 0.75); }
float bayer4(float2 a) { return bayer2(0.5 * a) * 0.25 + bayer2(a); }
float bayer8(float2 a) { return bayer4(0.5 * a) * 0.25 + bayer2(a); }

half4 main(float2 p) {
    float2 c = floor(p / cell);
    float2 uv = (c + 0.5) * cell / size;
    float s = uv.y;
    float q = uv.x;
    if (fieldMode > 1.5) {
        float2 d = uv - 0.5;
        s = length(d) * $ORB_FALLOFF + 0.02;
        q = atan(d.y, d.x) / 6.2831853 + 0.5;
    } else if (fieldMode > 0.5) {
        s = uv.x;
        q = uv.y;
    }
    float wave = fieldMode > 1.5
        ? sin(q * 18.849556 + seed) * 0.035
        : sin(q * 3.8 + seed) * 0.05 + sin(q * 9.7 + seed * 1.7) * 0.018;
    float tone = 1.0 - smoothstep(0.08, 0.95, s + wave);
    tone = clamp(tone + (hash(c) - 1.0) * 0.06, 0.0, 1.0);
    float level = tone * 3.0;
    float band = floor(level);
    float ink = band + ((level - band) > bayer8(c) ? 1.0 : 0.0);
    half3 col = half3(ink < 0.5 ? inkPaper : (ink < 1.5 ? inkSky : (ink < 2.5 ? inkCobalt : inkDeep)));
    float z = (s - 0.5 + wave * 0.6 - q * 0.04) * 20.0;
    float horizon = exp(-z * z);
    if (fieldMode < 1.5 && horizon > 0.05 && hash(c + 17.0) < horizon * $HORIZON_DENSITY && ink > 0.5) { col = half3(inkHorizon); }
    float grain = hash(floor(p) + 3.0) - 0.5;
    col = col + half3(grain * (ink < 0.5 ? 0.025 : 0.07));
    return half4(col, 1.0);
}
"""

/** The same field as the shader, one pixel per dither cell, for Android 11 and 12. */
object DitherField {
    private val PAPER = Inks.paper.toArgb()
    private val SKY = Inks.sky.toArgb()
    private val COBALT = Inks.cobalt.toArgb()
    private val DEEP = Inks.deep.toArgb()
    private val HORIZON = Inks.horizon.toArgb()
    val INKS = setOf(PAPER, SKY, COBALT, DEEP, HORIZON)

    private fun fract(x: Double) = x - floor(x)
    private fun hash(x: Double, y: Double, seed: Double): Double {
        var a = fract(x * 0.1031 + seed * 0.37)
        var b = fract(y * 0.1031 + seed * 0.37)
        var c = fract(x * 0.1031 + seed * 0.37)
        val d = a * (b + 33.33) + b * (c + 33.33) + c * (a + 33.33)
        a += d
        b += d
        c += d
        return fract((a + b) * c)
    }
    private fun bayer2(x: Double, y: Double): Double {
        val a = floor(x)
        val b = floor(y)
        return fract(a / 2.0 + b * b * 0.75)
    }
    private fun bayer4(x: Double, y: Double) = bayer2(0.5 * x, 0.5 * y) * 0.25 + bayer2(x, y)
    private fun bayer8(x: Double, y: Double) = bayer4(0.5 * x, 0.5 * y) * 0.25 + bayer2(x, y)
    private fun smoothstep(e0: Double, e1: Double, x: Double): Double {
        val t = ((x - e0) / (e1 - e0)).coerceIn(0.0, 1.0)
        return t * t * (3 - 2 * t)
    }

    /** ARGB pixels, [w] × [h] cells, in the four inks plus the sparse peach horizon. */
    fun pixels(w: Int, h: Int, dissolve: Dissolve, seed: Double): IntArray {
        val out = IntArray(w * h)
        for (y in 0 until h) for (x in 0 until w) {
            val u = (x + 0.5) / w
            val v = (y + 0.5) / h
            val (s, q) = when (dissolve) {
                Dissolve.DOWN -> v to u
                Dissolve.RIGHT -> u to v
                Dissolve.OUT -> (hypot(u - 0.5, v - 0.5) * ORB_FALLOFF + 0.02) to (atan2(v - 0.5, u - 0.5) / (2 * PI) + 0.5)
            }
            val wave = if (dissolve == Dissolve.OUT) sin(q * 18.849556 + seed) * 0.035
            else sin(q * 3.8 + seed) * 0.05 + sin(q * 9.7 + seed * 1.7) * 0.018
            val tone = (1.0 - smoothstep(0.08, 0.95, s + wave) + (hash(x.toDouble(), y.toDouble(), seed) - 1.0) * 0.06).coerceIn(0.0, 1.0)
            val level = tone * 3.0
            val band = floor(level)
            val ink = band + if (level - band > bayer8(x.toDouble(), y.toDouble())) 1 else 0
            var color = when {
                ink < 0.5 -> PAPER
                ink < 1.5 -> SKY
                ink < 2.5 -> COBALT
                else -> DEEP
            }
            val z = (s - 0.5 + wave * 0.6 - q * 0.04) * 20.0
            if (dissolve != Dissolve.OUT && ink > 0.5 && exp(-z * z) > 0.05 && hash(x + 17.0, y + 17.0, seed) < exp(-z * z) * HORIZON_DENSITY) color = HORIZON
            out[y * w + x] = color
        }
        return out
    }

    fun bitmap(w: Int, h: Int, dissolve: Dissolve, seed: Double): ImageBitmap =
        Bitmap.createBitmap(pixels(w, h, dissolve, seed), w, h, Bitmap.Config.ARGB_8888).asImageBitmap()
}

@RequiresApi(33)
private class AgslField {
    val shader = RuntimeShader(AGSL).apply {
        fun ink(name: String, c: Color) = setFloatUniform(name, c.red, c.green, c.blue)
        ink("inkPaper", Inks.paper)
        ink("inkSky", Inks.sky)
        ink("inkCobalt", Inks.cobalt)
        ink("inkDeep", Inks.deep)
        ink("inkHorizon", Inks.horizon)
    }
    val brush = ShaderBrush(shader)
}

/**
 * A grainy, dithered cobalt block. [cell] is the size of one dither "pixel": the bitmap look of
 * the board's pixel transitions.
 */
@Composable
fun GrainField(
    modifier: Modifier,
    dissolve: Dissolve = Dissolve.DOWN,
    cell: Dp = 4.dp,
    seed: Float = 1.3f,
    shape: Shape = RectangleShape,
) {
    val cellPx = with(LocalDensity.current) { cell.toPx() }.coerceAtLeast(1f)
    val useShader = Build.VERSION.SDK_INT >= 33 && !GrainSettings.forceStatic
    // A driver that can't compile the shader gets the bitmap instead of a crash.
    val field = if (useShader) remember { runCatching { AgslField() }.getOrNull() } else null
    if (field != null && Build.VERSION.SDK_INT >= 33) {
        Canvas(modifier.clip(shape)) {
            field.shader.setFloatUniform("size", size.width, size.height)
            field.shader.setFloatUniform("cell", cellPx)
            field.shader.setFloatUniform("fieldMode", dissolve.mode)
            field.shader.setFloatUniform("seed", seed)
            drawRect(field.brush)
        }
    } else {
        var cached by remember { mutableStateOf<Pair<IntSize, ImageBitmap>?>(null) }
        Canvas(modifier.clip(shape)) {
            val cells = IntSize(max(1, (size.width / cellPx).toInt()), max(1, (size.height / cellPx).toInt()))
            val image = cached?.takeIf { it.first == cells }?.second
                ?: DitherField.bitmap(cells.width, cells.height, dissolve, seed.toDouble()).also { cached = cells to it }
            drawImage(
                image,
                dstSize = IntSize(size.width.toInt() + 1, size.height.toInt() + 1),
                filterQuality = FilterQuality.None,
            )
        }
    }
}

/** Interface's paper grain (feTurbulence noise, alpha .055) as a repeating tile. */
object PaperGrain {
    private var tile: ImageBitmap? = null

    fun tile(): ImageBitmap = tile ?: run {
        val n = 128
        val px = IntArray(n * n)
        var r = 17L
        for (i in px.indices) {
            r = (r * 6364136223846793005L + 1442695040888963407L)
            val v = ((r ushr 33) and 0xFF).toInt()
            val alpha = (v * 0.055).toInt().coerceIn(0, 255)
            px[i] = (alpha shl 24) or (0x45 shl 16) or (0x4A shl 8) or 0x57
        }
        Bitmap.createBitmap(px, n, n, Bitmap.Config.ARGB_8888).asImageBitmap().also { tile = it }
    }
}

/** Paints Interface's paper grain over whatever is behind it. */
fun Modifier.paperGrain(): Modifier = drawBehind {
    drawRect(ShaderBrush(ImageShader(PaperGrain.tile(), TileMode.Repeated, TileMode.Repeated)))
}

/**
 * The small dithered pixel indicator for "online", "in use" and "offline": a 3 × 3 block of
 * printed dots. [on] prints the checker (corners and centre); off leaves only the four corners,
 * hollow in the middle.
 */
@Composable
fun PixelIndicator(color: Color, on: Boolean = true, size: Dp = 10.dp) {
    Canvas(Modifier.size(size)) {
        val c = this.size.width / 3f
        for (y in 0 until 3) for (x in 0 until 3) {
            val corner = x != 1 && y != 1
            val lit = corner || (on && x == 1 && y == 1)
            if (lit) drawRect(color, topLeft = Offset(x * c, y * c), size = Size(c * 0.86f, c * 0.86f))
        }
    }
}

/**
 * A Silicon's pixel avatar: a mirrored 5 × 5 bitmap from its id, in cobalt inks on pale cobalt,
 * so the same Silicon always looks the same. Decorative: its name is always next to it.
 */
@Composable
fun SiliconAvatar(id: String, size: Dp = 40.dp) {
    val bits = remember(id) {
        var h = 2166136261L.toInt()
        for (ch in id) h = (h xor ch.code) * 16777619
        h
    }
    Box(
        Modifier
            .size(size)
            .clip(androidx.compose.foundation.shape.RoundedCornerShape(size * 0.22f))
            .drawBehind {
                drawRect(Tokens.CobaltPale)
                val grid = 5
                val inset = this.size.width * 0.14f
                val c = (this.size.width - inset * 2) / grid
                for (y in 0 until grid) for (x in 0 until 3) {
                    val bit = (bits ushr ((y * 3 + x) % 31)) and 1
                    if (bit == 1 || (x == 2 && y == 2)) {
                        val ink = if (((abs(bits) ushr (y + x)) and 1) == 1) Tokens.Cobalt else Tokens.CobaltDeep
                        drawRect(ink, Offset(inset + x * c, inset + y * c), Size(c, c))
                        drawRect(ink, Offset(inset + (grid - 1 - x) * c, inset + y * c), Size(c, c))
                    }
                }
            },
    )
}

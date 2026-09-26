package com.teamofsilicons.extend.driver

/**
 * agent-device selector expressions (`packages/selectors/src/internal/parse.ts` and `match.ts`):
 * `role="button" label="Continue"`, `id=com.example:id/login`, `text='Sign in' || label=Login`.
 * Terms in one segment must all match; `||` separates fallbacks tried in order. Text comparison
 * trims, lowercases and collapses whitespace.
 */
data class SelectorTerm(val key: String, val value: String?, val flag: Boolean?)

data class Selector(val raw: String, val terms: List<SelectorTerm>)

data class SelectorChain(val raw: String, val selectors: List<Selector>)

class SelectorException(message: String) : Exception(message)

object Selectors {
    val TEXT_KEYS = setOf("id", "role", "text", "label", "value", "appname", "windowtitle")
    val BOOLEAN_KEYS = setOf("visible", "hidden", "editable", "selected", "focused", "enabled", "hittable")
    private val ALL_KEYS = TEXT_KEYS + BOOLEAN_KEYS

    fun parse(expression: String): SelectorChain {
        val raw = expression.trim()
        if (raw.isEmpty()) throw SelectorException("Selector expression cannot be empty")
        val segments = splitByFallback(raw)
        return SelectorChain(raw, segments.map { parseSegment(it) })
    }

    fun tryParse(expression: String): SelectorChain? = try {
        parse(expression)
    } catch (_: SelectorException) {
        null
    }

    fun isSelectorToken(token: String): Boolean {
        val t = token.trim()
        if (t.isEmpty()) return false
        if (t == "||") return true
        val eq = t.indexOf('=')
        if (eq != -1) return t.substring(0, eq).trim().lowercase() in ALL_KEYS
        return t.lowercase() in ALL_KEYS
    }

    /**
     * Takes the longest run of leading tokens that parses as a selector and returns it with the
     * remaining tokens. With [preferTrailingValue], leaves at least one token behind when it can
     * (for `is text <selector> <value>` and `fill <selector> <text>`).
     */
    fun splitFromArgs(args: List<String>, preferTrailingValue: Boolean = false): Pair<SelectorChain, List<String>>? {
        val boundaries = mutableListOf<Int>()
        var i = 0
        while (i < args.size) {
            if (!isSelectorToken(args[i])) break
            i++
            val candidate = args.subList(0, i).joinToString(" ").trim()
            if (candidate.isNotEmpty() && tryParse(candidate) != null) boundaries += i
        }
        if (boundaries.isEmpty()) return null
        var boundary = boundaries.last()
        if (preferTrailingValue) {
            boundary = boundaries.lastOrNull { it < args.size } ?: boundary
        }
        val expr = args.subList(0, boundary).joinToString(" ").trim()
        return parse(expr) to args.subList(boundary, args.size)
    }

    private fun parseSegment(segment: String): Selector {
        val raw = segment.trim()
        val tokens = tokenize(raw)
        if (tokens.isEmpty()) throw SelectorException("Invalid selector segment: $segment")
        return Selector(raw, tokens.map { parseTerm(it) })
    }

    private fun parseTerm(token: String): SelectorTerm {
        val t = token.trim()
        val eq = t.indexOf('=')
        if (eq == -1) {
            val key = t.lowercase()
            if (key !in BOOLEAN_KEYS) throw SelectorException("Invalid selector term \"$token\", expected key=value")
            return SelectorTerm(key, null, true)
        }
        val key = t.substring(0, eq).trim().lowercase()
        val valueRaw = t.substring(eq + 1).trim()
        if (key !in ALL_KEYS) {
            val hint = if (key.isNotEmpty()) " Use role=\"$key\" or label=\"…\"; keys are ${ALL_KEYS.sorted().joinToString(", ")}." else ""
            throw SelectorException("Unknown selector key: $key.$hint")
        }
        if (valueRaw.isEmpty()) throw SelectorException("Missing selector value for key: $key")
        if (key in BOOLEAN_KEYS) {
            val b = when (unquote(valueRaw).lowercase()) {
                "true" -> true
                "false" -> false
                else -> throw SelectorException("Invalid boolean value for $key: $valueRaw")
            }
            return SelectorTerm(key, null, b)
        }
        return SelectorTerm(key, unquote(valueRaw), null)
    }

    private fun splitByFallback(expression: String): List<String> {
        val segments = mutableListOf<String>()
        val current = StringBuilder()
        var quote: Char? = null
        var i = 0
        while (i < expression.length) {
            val ch = expression[i]
            if ((ch == '"' || ch == '\'') && !isEscaped(expression, i)) {
                quote = if (quote == null) ch else if (quote == ch) null else quote
                current.append(ch)
                i++
                continue
            }
            if (quote == null && ch == '|' && i + 1 < expression.length && expression[i + 1] == '|') {
                val seg = current.toString().trim()
                if (seg.isEmpty()) throw SelectorException("Invalid selector fallback expression: $expression")
                segments += seg
                current.clear()
                i += 2
                continue
            }
            current.append(ch)
            i++
        }
        val last = current.toString().trim()
        if (last.isEmpty()) throw SelectorException("Invalid selector fallback expression: $expression")
        segments += last
        return segments
    }

    private fun tokenize(segment: String): List<String> {
        val tokens = mutableListOf<String>()
        val current = StringBuilder()
        var quote: Char? = null
        for (i in segment.indices) {
            val ch = segment[i]
            if ((ch == '"' || ch == '\'') && !isEscaped(segment, i)) {
                quote = if (quote == null) ch else if (quote == ch) null else quote
                current.append(ch)
                continue
            }
            if (quote == null && ch.isWhitespace()) {
                if (current.isNotBlank()) tokens += current.toString().trim()
                current.clear()
                continue
            }
            current.append(ch)
        }
        if (quote != null) throw SelectorException("Unclosed quote in selector: $segment")
        if (current.isNotBlank()) tokens += current.toString().trim()
        return tokens
    }

    private fun isEscaped(s: String, index: Int): Boolean {
        var count = 0
        var i = index - 1
        while (i >= 0 && s[i] == '\\') {
            count++
            i--
        }
        return count % 2 == 1
    }

    private fun unquote(value: String): String {
        val t = value.trim()
        if (t.length >= 2 && ((t.startsWith('"') && t.endsWith('"')) || (t.startsWith('\'') && t.endsWith('\'')))) {
            return decodeEscapes(t.substring(1, t.length - 1))
        }
        return t
    }

    private fun decodeEscapes(value: String): String {
        val out = StringBuilder()
        var i = 0
        while (i < value.length) {
            val c = value[i]
            if (c != '\\' || i + 1 >= value.length) {
                out.append(c)
                i++
                continue
            }
            val e = value[i + 1]
            val simple = when (e) {
                '"' -> '"'; '\'' -> '\''; '\\' -> '\\'; '/' -> '/'
                'b' -> '\b'; 'f' -> '\u000C'; 'n' -> '\n'; 'r' -> '\r'; 't' -> '\t'
                else -> null
            }
            if (simple != null) {
                out.append(simple)
                i += 2
                continue
            }
            if (e == 'u' && i + 6 <= value.length) {
                val hex = value.substring(i + 2, i + 6)
                if (hex.all { it.isDigit() || it.lowercaseChar() in 'a'..'f' }) {
                    out.append(hex.toInt(16).toChar())
                    i += 6
                    continue
                }
            }
            out.append(c)
            i++
        }
        return out.toString()
    }

    // ───────────── matching ─────────────

    fun normalizeText(value: String?): String = (value ?: "").trim().lowercase().replace(Regex("\\s+"), " ")

    private fun textEquals(value: String?, query: String): Boolean = normalizeText(value) == normalizeText(query)

    /** agent-device's `extractNodeText`: the first non-blank of label, value, identifier. */
    fun nodeText(node: UiNode): String =
        listOf(node.label, node.value, node.identifier).map { it?.trim().orEmpty() }.firstOrNull { it.isNotEmpty() } ?: ""

    fun isVisible(node: UiNode, screen: Bounds?): Boolean {
        if (!node.visibleToUser) return false
        if (node.bounds.isEmpty) return false
        return screen == null || node.bounds.intersects(screen)
    }

    fun matches(node: UiNode, selector: Selector, screen: Bounds?): Boolean =
        selector.terms.all { matchesTerm(node, it, screen) }

    private fun matchesTerm(node: UiNode, term: SelectorTerm, screen: Bounds?): Boolean {
        val v = term.value.orEmpty()
        return when (term.key) {
            "id" -> textEquals(node.identifier, v) ||
                // Accept the bare entry name too: id=login matches com.example:id/login.
                (!v.contains(":id/") && node.identifier?.substringAfter(":id/", "")?.let { textEquals(it, v) } == true)
            "role" -> textEquals(Roles.normalizeType(node.className), v) || textEquals(node.role, v)
            "label" -> textEquals(node.label, v)
            "value" -> textEquals(node.value, v)
            "text" -> textEquals(nodeText(node), v)
            "appname" -> textEquals(node.packageName, v)
            "windowtitle" -> textEquals(node.windowTitle, v)
            "visible" -> isVisible(node, screen) == term.flag
            "hidden" -> !isVisible(node, screen) == term.flag
            "editable" -> node.isEditable == term.flag
            "selected" -> node.selected == term.flag
            "focused" -> node.focused == term.flag
            "enabled" -> node.enabled == term.flag
            "hittable" -> node.hittable == term.flag
            else -> false
        }
    }

    /**
     * All nodes matching the first selector of [chain] that matches anything, visible ones first,
     * then in document order.
     */
    fun resolveAll(chain: SelectorChain, nodes: List<UiNode>, screen: Bounds?): List<UiNode> {
        for (selector in chain.selectors) {
            val matches = nodes.filter { matches(it, selector, screen) }
            if (matches.isNotEmpty()) {
                return matches.sortedBy { if (isVisible(it, screen)) 0 else 1 }
            }
        }
        return emptyList()
    }
}

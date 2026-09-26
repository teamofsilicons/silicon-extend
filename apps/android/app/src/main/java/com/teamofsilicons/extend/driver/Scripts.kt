package com.teamofsilicons.extend.driver

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull

/** One step of a batch or a replay script: a command name and its CLI tokens. */
data class Step(val command: String, val args: List<String>, val line: Int? = null)

/** Parses `.ad` replay scripts and `batch --steps` JSON into [Step]s. */
object Scripts {
    /**
     * Splits one line the way a POSIX shell would for plain words: whitespace separates tokens,
     * single quotes are literal, double quotes allow `\"` and `\\`.
     */
    fun shellWords(line: String): List<String> {
        val out = ArrayList<String>()
        val cur = StringBuilder()
        var inToken = false
        var i = 0
        while (i < line.length) {
            val c = line[i]
            when {
                c == '\'' -> {
                    inToken = true
                    val end = line.indexOf('\'', i + 1)
                    if (end == -1) throw CommandFailure.invalid("Unclosed ' in: $line")
                    cur.append(line, i + 1, end)
                    i = end
                }
                c == '"' -> {
                    inToken = true
                    i++
                    while (i < line.length && line[i] != '"') {
                        if (line[i] == '\\' && i + 1 < line.length && line[i + 1] in "\"\\$`") {
                            cur.append(line[i + 1])
                            i += 2
                            continue
                        }
                        cur.append(line[i])
                        i++
                    }
                    if (i >= line.length) throw CommandFailure.invalid("Unclosed \" in: $line")
                }
                c == '\\' && i + 1 < line.length -> {
                    inToken = true
                    cur.append(line[i + 1])
                    i++
                }
                c.isWhitespace() -> {
                    if (inToken) {
                        out += cur.toString()
                        cur.clear()
                        inToken = false
                    }
                }
                else -> {
                    inToken = true
                    cur.append(c)
                }
            }
            i++
        }
        if (inToken) out += cur.toString()
        return out
    }

    /** An `.ad` script: one command per line; `#` comments and `context …` header lines skipped. */
    fun parseAd(script: String): List<Step> {
        val steps = ArrayList<Step>()
        for ((i, raw) in script.lines().withIndex()) {
            val line = raw.trim()
            if (line.isEmpty() || line.startsWith("#") || line.startsWith("//")) continue
            if (line.startsWith("context ") || line == "context") continue
            val words = shellWords(line)
            if (words.isEmpty()) continue
            steps += Step(words[0], words.drop(1), i + 1)
        }
        return steps
    }

    /**
     * `batch --steps` JSON: an array of `{"command": "click", "args": ["@e2"]}`. agent-device's
     * legacy `positionals`/`flags` form is accepted too; its structured `input` form isn't.
     */
    fun parseBatch(json: String): List<Step> {
        val arr = try {
            kotlinx.serialization.json.Json.parseToJsonElement(json) as? JsonArray
        } catch (e: Exception) {
            null
        } ?: throw CommandFailure.invalid("batch --steps must be a JSON array of {\"command\": …, \"args\": […]}")
        if (arr.isEmpty()) throw CommandFailure.invalid("batch --steps is empty")
        if (arr.size > 100) throw CommandFailure.invalid("batch runs at most 100 steps, got ${arr.size}")
        return arr.mapIndexed { i, el ->
            val obj = el as? JsonObject ?: throw CommandFailure.invalid("batch step ${i + 1} isn't an object")
            val command = (obj["command"] as? JsonPrimitive)?.contentOrNull
                ?: throw CommandFailure.invalid("batch step ${i + 1} has no \"command\"")
            if ("input" in obj) {
                throw CommandFailure.invalid(
                    "batch step ${i + 1} uses agent-device's structured \"input\"; the Android app takes CLI tokens: {\"command\":\"$command\",\"args\":[…]}",
                )
            }
            val tokens = (obj["args"] ?: obj["positionals"]) as? JsonArray ?: JsonArray(emptyList())
            val args = tokens.map { t ->
                (t as? JsonPrimitive)?.takeIf { it.isString }?.content ?: throw CommandFailure.invalid("batch step ${i + 1}: args must be strings")
            }
            val flags = (obj["flags"] as? JsonObject)?.flatMap { (k, v) ->
                val name = if (k.startsWith("-")) k else "--$k"
                when {
                    v is JsonPrimitive && v.contentOrNull == "true" -> listOf(name)
                    v is JsonPrimitive && v.contentOrNull == "false" -> emptyList()
                    v is JsonPrimitive -> listOf(name, v.content)
                    else -> emptyList()
                }
            } ?: emptyList()
            Step(command, args + flags)
        }
    }
}

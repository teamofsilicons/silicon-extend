package com.teamofsilicons.extend.driver

/** Post-fill observation only: polling never focuses, types, or repeats an input action. */
object FillReadback {
    const val MAX_WAIT_MS = 1_000L
    private const val POLL_MS = 75L

    data class Identity(
        val handle: Any,
        val windowId: Int,
        val packageName: String?,
        val className: String?,
        val resourceId: String?,
    ) {
        fun sameResource(other: Identity): Boolean =
            !resourceId.isNullOrBlank() && !packageName.isNullOrBlank() && !className.isNullOrBlank() &&
                windowId == other.windowId && packageName == other.packageName &&
                className == other.className && resourceId == other.resourceId

        fun sameNode(other: Identity): Boolean =
            handle == other.handle && windowId == other.windowId && packageName == other.packageName &&
                className == other.className
    }

    data class Field(
        val identity: Identity,
        val text: String?,
        val hintShowing: Boolean = false,
        val password: Boolean = false,
        val editable: Boolean = true,
        val visible: Boolean = true,
        val applicationWindow: Boolean = true,
    )

    enum class State { VERIFIED, UNAVAILABLE, MISMATCH, UNCONFIRMED }
    data class Result(val state: State, val reason: String? = null, val actualLength: Int? = null)

    /** Prompt evidence belongs to one session and never contains entered field values. */
    class Prompts {
        private val entries = LinkedHashMap<Identity, String>()

        fun remember(identity: Identity, prompt: String) {
            if (prompt.isEmpty()) return
            entries.remove(identity)
            entries[identity] = prompt
            while (entries.size > 64) entries.remove(entries.keys.first())
        }

        fun find(before: Field, candidates: List<Field>): String? {
            entries.entries.firstOrNull { it.key.sameNode(before.identity) }?.let { return it.value }
            // A replacement node may inherit provenance only with one live field for that id.
            if (candidates.count { before.identity.sameResource(it.identity) } != 1) return null
            return entries.filterKeys { before.identity.sameResource(it) }.values.distinct().singleOrNull()
        }

        fun forget(identity: Identity) {
            entries.keys.removeAll { it.sameNode(identity) || it.sameResource(identity) }
        }
    }

    private fun eligible(field: Field, before: Field): Boolean =
        field.editable && field.visible && field.applicationWindow &&
            field.identity.windowId == before.identity.windowId &&
            field.identity.packageName == before.identity.packageName &&
            field.identity.className == before.identity.className

    /** Fresh candidates are matched by native identity, then by an unambiguous stable id. */
    fun locate(before: Field, candidates: List<Field>): Field? {
        val eligible = candidates.filter { eligible(it, before) }
        val exact = eligible.filter { before.identity.sameNode(it.identity) }
        if (exact.size == 1) return exact.single()
        if (exact.isNotEmpty()) return null
        return eligible.filter { before.identity.sameResource(it.identity) }.singleOrNull()
    }

    suspend fun verify(
        before: Field,
        expected: String,
        prompts: Prompts,
        budgetMs: Long,
        now: () -> Long,
        capture: () -> List<Field>?,
        refresh: (Field) -> Field?,
        pause: suspend (Long) -> Unit,
    ): Result {
        if (before.password) return Result(State.UNAVAILABLE, "password")
        val initialPrompt = before.text?.takeIf { before.hintShowing && it.isNotEmpty() }
        initialPrompt?.let { prompts.remember(before.identity, it) }
        val budget = budgetMs.coerceIn(0, MAX_WAIT_MS)
        if (budget == 0L) return Result(State.UNCONFIRMED, "readback_budget_exhausted")
        val deadline = now() + budget
        var last = Result(State.UNCONFIRMED, "field_unavailable")
        while (now() < deadline) {
            val candidates = capture()
            val live = candidates?.let { locate(before, it) }
            val fresh = live?.let(refresh)?.takeIf { eligible(it, before) &&
                (before.identity.sameNode(it.identity) || before.identity.sameResource(it.identity)) }
            last = when {
                candidates == null -> Result(State.UNCONFIRMED, "capture_unavailable")
                live == null -> Result(State.UNCONFIRMED, "field_missing_or_ambiguous")
                fresh == null -> Result(State.UNCONFIRMED, "field_refresh_failed")
                fresh.password -> Result(State.UNAVAILABLE, "password")
                else -> {
                    val knownPrompt = initialPrompt ?: prompts.find(before, candidates)
                    val raw = fresh.text.orEmpty()
                    // Some apps keep announcing the initial prompt after accepting text while
                    // flipping hintShowing to false. That string is not evidence of field value.
                    if (knownPrompt != null && !fresh.hintShowing && raw == knownPrompt) {
                        prompts.remember(fresh.identity, knownPrompt)
                        Result(State.UNAVAILABLE, "accessibility_prompt")
                    } else {
                        val actual = if (fresh.hintShowing) "" else raw
                        if (!fresh.hintShowing && raw != knownPrompt) {
                            prompts.forget(before.identity)
                            prompts.forget(fresh.identity)
                        }
                        if (actual == expected) Result(State.VERIFIED)
                        else Result(State.MISMATCH, "text_mismatch", actual.length)
                    }
                }
            }
            if (last.state == State.VERIFIED || last.reason == "password") return last
            val remaining = deadline - now()
            if (remaining <= 0) break
            pause(minOf(POLL_MS, remaining))
        }
        return last
    }
}

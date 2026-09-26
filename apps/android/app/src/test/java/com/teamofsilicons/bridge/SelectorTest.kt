package com.teamofsilicons.bridge

import com.teamofsilicons.bridge.driver.Bounds
import com.teamofsilicons.bridge.driver.SelectorException
import com.teamofsilicons.bridge.driver.Selectors
import com.teamofsilicons.bridge.driver.UiNode
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SelectorTest {
    private val screen = Bounds(0, 0, 1080, 2400)

    private val button = UiNode(
        className = "android.widget.Button", text = "Continue", resourceId = "com.example:id/next",
        bounds = Bounds(100, 2000, 980, 2150), clickable = true, focusable = true,
    )
    private val email = UiNode(
        className = "android.widget.EditText", text = "user@example.com", hint = "Email", resourceId = "com.example:id/email",
        bounds = Bounds(100, 800, 980, 950), clickable = true, focusable = true, focused = true, editable = true,
    )
    private val icon = UiNode(className = "android.widget.ImageButton", contentDescription = "Navigate up", bounds = Bounds(0, 100, 150, 250), clickable = true)
    private val offscreen = UiNode(className = "android.widget.TextView", text = "Continue", bounds = Bounds(0, 3000, 500, 3100), visibleToUser = false)
    private val selected = UiNode(className = "android.widget.TextView", text = "Wi-Fi", bounds = Bounds(0, 400, 500, 500), selected = true)
    private val nodes = listOf(button, email, icon, offscreen, selected)

    private fun matches(expr: String) = Selectors.resolveAll(Selectors.parse(expr), nodes, screen)

    @Test
    fun parsesTermsQuotesAndFallbacks() {
        val chain = Selectors.parse("role=\"button\" label='Continue' || text=Next")
        assertEquals(2, chain.selectors.size)
        assertEquals(listOf("role", "label"), chain.selectors[0].terms.map { it.key })
        assertEquals("button", chain.selectors[0].terms[0].value)
        assertEquals("Continue", chain.selectors[0].terms[1].value)
        assertEquals("Next", chain.selectors[1].terms[0].value)
        val quoted = Selectors.parse("label=\"Sign in || up\"")
        assertEquals(1, quoted.selectors.size)
        assertEquals("Sign in || up", quoted.selectors[0].terms[0].value)
        assertEquals("say \"hi\"", Selectors.parse("text=\"say \\\"hi\\\"\"").selectors[0].terms[0].value)
        val flag = Selectors.parse("editable visible=false").selectors[0].terms
        assertEquals(true, flag[0].flag)
        assertEquals(false, flag[1].flag)
    }

    @Test
    fun rejectsBadSelectors() {
        for (bad in listOf("", "button=\"Go\"", "label=", "visible=maybe", "label=\"open", "|| label=x", "Continue")) {
            try {
                Selectors.parse(bad)
                throw AssertionError("should reject: $bad")
            } catch (_: SelectorException) {
            }
        }
        assertNull(Selectors.tryParse("nope"))
    }

    @Test
    fun splitsSelectorFromTrailingText() {
        val (chain, rest) = Selectors.splitFromArgs(listOf("role=button", "label=\"Email\"", "hello"))!!
        assertEquals("role=button label=\"Email\"", chain.raw)
        assertEquals(listOf("hello"), rest)
        val (c2, r2) = Selectors.splitFromArgs(listOf("id=\"greeting\"", "Welcome back"), preferTrailingValue = true)!!
        assertEquals("id=\"greeting\"", c2.raw)
        assertEquals(listOf("Welcome back"), r2)
        // A trailing token that is itself a selector term stays a value when asked to.
        val (c3, r3) = Selectors.splitFromArgs(listOf("label=a", "text=b"), preferTrailingValue = true)!!
        assertEquals("label=a", c3.raw)
        assertEquals(listOf("text=b"), r3)
        assertNull(Selectors.splitFromArgs(listOf("Continue")))
        assertTrue(Selectors.isSelectorToken("||"))
        assertFalse(Selectors.isSelectorToken("hello"))
    }

    @Test
    fun matchesLikeAgentDevice() {
        // label = text or content description; case, whitespace-insensitive equality.
        assertEquals(listOf(button, offscreen), matches("label=\"  continue \""))
        assertEquals(listOf(icon), matches("label=\"Navigate up\""))
        // Visible matches come first.
        assertEquals(button, matches("text=Continue").first())
        assertEquals(listOf(button), matches("text=Continue visible"))
        assertEquals(listOf(offscreen), matches("text=Continue hidden"))
        // role: the Android class or agent-device's display role.
        assertEquals(listOf(button, icon), matches("role=button"))
        assertEquals(listOf(email), matches("role=text-field"))
        assertEquals(listOf(email), matches("role=edittext"))
        // id: the full resource id or its entry name.
        assertEquals(listOf(button), matches("id=com.example:id/next"))
        assertEquals(listOf(email), matches("id=email"))
        assertEquals(listOf(email), matches("value=user@example.com"))
        assertEquals(listOf(email), matches("editable"))
        assertEquals(listOf(email), matches("focused=true"))
        assertEquals(listOf(selected), matches("selected label=Wi-Fi"))
        assertTrue(matches("hittable=true").containsAll(listOf(button, email)))
        // Fallback: the first alternative that matches anything wins.
        assertEquals(listOf(icon), matches("label=Nope || label=\"Navigate up\" || role=button"))
        assertTrue(matches("label=Nope").isEmpty())
    }

    @Test
    fun nodeTextPrefersLabelThenValueThenId() {
        assertEquals("Continue", Selectors.nodeText(button))
        assertEquals("Navigate up", Selectors.nodeText(icon))
        assertEquals("com.x:id/y", Selectors.nodeText(UiNode(resourceId = "com.x:id/y")))
    }
}

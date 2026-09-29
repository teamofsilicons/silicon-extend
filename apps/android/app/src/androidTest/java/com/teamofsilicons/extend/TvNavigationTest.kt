package com.teamofsilicons.extend

import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.unit.Density
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.teamofsilicons.extend.core.PairingUi
import com.teamofsilicons.extend.core.Phase
import com.teamofsilicons.extend.core.UiState
import com.teamofsilicons.extend.ui.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Actual remote events, including text taller than a screen and low logical TV resolution. */
@OptIn(ExperimentalTestApi::class)
@RunWith(AndroidJUnit4::class)
class TvNavigationTest {
    @get:Rule val ui = createComposeRule()

    @Test fun remoteScrollsLongTextBothWaysAndCanLeaveTheRail() {
        val scroll = ScrollState(0)
        var pressed = false
        ui.setContent {
            ExtendTheme(tv = true) {
                TvScrollPane(scroll, Modifier.fillMaxSize()) {
                    Title("Read from the top")
                    Muted("A long explanation that must remain readable with only a remote. ".repeat(250))
                    ExtendButton("Last action", { pressed = true })
                }
            }
        }
        ui.onNodeWithContentDescription("Scroll page")
            .performSemanticsAction(SemanticsActions.RequestFocus)
        ui.onNodeWithContentDescription("Scroll page").performKeyInput {
            repeat(12) { pressKey(Key.DirectionDown) }
        }
        ui.runOnIdle { assertTrue("Down must scroll without a nearby button", scroll.value > 0) }
        ui.onNodeWithContentDescription("Scroll page").performKeyInput {
            repeat(100) { pressKey(Key.DirectionDown) }
        }
        ui.runOnIdle { assertEquals(scroll.maxValue, scroll.value) }
        ui.onNodeWithContentDescription("Scroll page").performKeyInput {
            repeat(120) { pressKey(Key.DirectionUp) }
        }
        ui.runOnIdle { assertEquals("Up must return to the start", 0, scroll.value) }
        ui.onNodeWithContentDescription("Scroll page").performKeyInput { pressKey(Key.DirectionLeft) }
        ui.onNodeWithContentDescription("Scroll page").assertIsNotFocused()
        ui.runOnIdle { assertFalse("Scrolling must not activate a control", pressed) }
    }

    @Test fun compactPairingStartsAtTheTopAndRemoteReturnsFromFooter() {
        ui.setContent {
            // 1920px TV at density 3 -> 640dp: reproduces the cramped maker-TV viewport.
            CompositionLocalProvider(LocalDensity provides Density(3f, 1.3f)) {
                ExtendTheme(tv = true) {
                    TvPairingScreen(UiState(phase = Phase.UNPAIRED, isTv = true, pairing = PairingUi(code = "ABC123"))) {
                        ExtendButton("Open-source licences", {})
                    }
                }
            }
        }
        ui.onNodeWithText("Pair this TV.").assertIsFocused().assertIsDisplayed()
        ui.onNodeWithContentDescription("Pairing code: A B C 1 2 3").assertIsDisplayed()
        ui.onNodeWithText("Pair this TV.").performKeyInput { pressKey(Key.DirectionDown) }
        ui.onNodeWithText("Open-source licences").assertIsFocused().assertIsDisplayed()
        ui.onNodeWithText("Open-source licences").performKeyInput { pressKey(Key.DirectionUp) }
        ui.onNodeWithText("Pair this TV.").assertIsFocused().assertIsDisplayed()
    }
    @Test fun everyTvSectionIsReachableWithTheRemote() {
        val context = androidx.test.platform.app.InstrumentationRegistry.getInstrumentation().targetContext
        val extend = Extend.get(context)
        ui.setContent {
            CompositionLocalProvider(LocalDensity provides Density(3f)) {
                ExtendTheme(tv = true) {
                    AppScreen(extend, UiState(phase = Phase.PAIRED, isTv = true), {},
                        SetupActions(requestNotifications = {}, open = { error("No system settings in this test") },
                            openCandidate = { null }, findScreens = { null }))
                }
            }
        }
        val labels = listOf("Overview", "Setup", "Sharing", "Debugging", "Settings")
        val headings = listOf("This TV.", "TV setup.", "Sharing this TV.", "Android debugging.", "TV settings.")
        labels.forEachIndexed { index, label ->
            ui.onNodeWithText(label).assertIsFocused().assertIsDisplayed()
                .performKeyInput { pressKey(Key.DirectionCenter) }
            ui.onAllNodesWithText(headings[index]).onFirst().assertIsDisplayed()
            if (index < labels.lastIndex) ui.onNodeWithText(label).performKeyInput { pressKey(Key.DirectionRight) }
        }
        // Back is dispatched by the Activity, outside Compose's key injection target.
        androidx.test.platform.app.InstrumentationRegistry.getInstrumentation()
            .sendKeyDownUpSync(android.view.KeyEvent.KEYCODE_BACK)
        ui.waitForIdle()
        ui.onNodeWithText("Overview").assertIsFocused()
        ui.onNodeWithText("This TV.").assertIsDisplayed()
    }

}

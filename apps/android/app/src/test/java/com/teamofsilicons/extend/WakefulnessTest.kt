package com.teamofsilicons.extend

import com.teamofsilicons.extend.core.Wakefulness
import com.teamofsilicons.extend.core.Wakefulness.Event
import com.teamofsilicons.extend.core.Wakefulness.Reading
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** When a phone or a TV counts as awake (UNDERSTANDING.md, Waking a device). */
class WakefulnessTest {
    private fun phone(current: Reading?, event: Event, interactive: Boolean = true, locked: Boolean = true) =
        Wakefulness.next(tv = false, current, event, interactive, locked)

    private fun tv(current: Reading?, event: Event, interactive: Boolean = true) =
        Wakefulness.next(tv = true, current, event, interactive, keyguardLocked = false)

    @Test fun aPhoneIsAwakeOnlyPastTheLockScreen() {
        val off = phone(null, Event.SCREEN_OFF, interactive = false)
        assertEquals(Reading(false, "screen_off"), off)
        val lit = phone(off, Event.SCREEN_ON)
        assertEquals("a screen lit by a notification is still locked", Reading(false, "locked"), lit)
        val unlocked = phone(lit, Event.USER_PRESENT, locked = false)
        assertEquals("an unlock is a person: input_seen", Reading(true, inputSeen = true), unlocked)
        assertEquals(Reading(false, "screen_off"), phone(unlocked, Event.SCREEN_OFF, interactive = false))
        // A charging screensaver changes nothing.
        assertEquals(unlocked, phone(unlocked, Event.DREAMING_STARTED))
    }

    @Test fun aPhoneReadDirectlyAtConnect() {
        assertEquals(Reading(true), Wakefulness.snapshot(tv = false, interactive = true, keyguardLocked = false))
        assertEquals(Reading(false, "locked"), Wakefulness.snapshot(tv = false, interactive = true, keyguardLocked = true))
        assertEquals(Reading(false, "screen_off"), Wakefulness.snapshot(tv = false, interactive = false, keyguardLocked = true))
        assertEquals("the app can't tell a person unlocked it", null, Wakefulness.snapshot(tv = false, interactive = true, keyguardLocked = false).inputSeen)
    }

    @Test fun aTvIsAwakeWhileOnAndInStandbyOtherwise() {
        assertEquals(Reading(true), Wakefulness.snapshot(tv = true, interactive = true, keyguardLocked = false))
        assertEquals(Reading(false, "standby"), Wakefulness.snapshot(tv = true, interactive = false, keyguardLocked = false))
        val standby = tv(null, Event.SCREEN_OFF, interactive = false)
        assertEquals(Reading(false, "standby"), standby)
        val on = tv(standby, Event.SCREEN_ON)
        assertEquals("a TV can't tell whether a person turned it on", Reading(true), on)
        assertEquals("a dream (screensaver) is a TV that is on", Reading(true), tv(on, Event.DREAMING_STARTED))
        assertEquals(Reading(true), tv(on, Event.DREAMING_STOPPED))
    }

    @Test fun onlyChangesAndUnlocksAreSent() {
        assertTrue(Wakefulness.changed(null, Reading(true)))
        assertFalse(Wakefulness.changed(Reading(false, "locked"), Reading(false, "locked")))
        assertTrue(Wakefulness.changed(Reading(false, "screen_off"), Reading(false, "locked")))
        assertTrue("an unlock is always worth sending", Wakefulness.changed(Reading(true), Reading(true, inputSeen = true)))
    }

    @Test fun theUiShowsAwakeAndWhyNot() {
        assertEquals(com.teamofsilicons.extend.core.AwakeUi(false, "locked"), Reading(false, "locked").ui)
    }
}

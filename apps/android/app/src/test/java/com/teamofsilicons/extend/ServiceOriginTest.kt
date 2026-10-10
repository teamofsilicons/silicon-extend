package com.teamofsilicons.extend

import com.teamofsilicons.extend.config.initialServiceOrigin
import org.junit.Assert.assertEquals
import org.junit.Test

class ServiceOriginTest {
    @Test fun existingStateKeepsItsLegacyService() {
        assertEquals("https://backend.extend.teamofsilicons.com",
            initialServiceOrigin(true, "https://api.extend.teamofsilicons.com"))
    }
    @Test fun freshInstallUsesTheNewService() {
        assertEquals("https://api.extend.teamofsilicons.com",
            initialServiceOrigin(false, "https://api.extend.teamofsilicons.com"))
    }
}

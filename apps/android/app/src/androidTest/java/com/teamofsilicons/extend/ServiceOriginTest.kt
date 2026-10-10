package com.teamofsilicons.extend

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.teamofsilicons.extend.config.Config
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.util.UUID

@RunWith(AndroidJUnit4::class)
class ServiceOriginTest {
    private fun isolated(): Context {
        val prefix = "origin-test-${UUID.randomUUID()}-"
        return object : ContextWrapper(InstrumentationRegistry.getInstrumentation().targetContext) {
            override fun getSharedPreferences(name: String, mode: Int): SharedPreferences =
                super.getSharedPreferences(prefix + name, mode)
        }
    }

    @Test fun pairedUpgradeKeepsLegacyOriginAndState() {
        val ctx = isolated()
        val settings = ctx.getSharedPreferences("extend_config", Context.MODE_PRIVATE)
        settings.edit().putString("pair_ids", "legacy-pair").commit()
        val vault = ctx.getSharedPreferences("extend_secrets", Context.MODE_PRIVATE)
        vault.edit().putString("credential", "opaque-encrypted-fixture").commit()
        val before = vault.all
        assertEquals("https://backend.extend.teamofsilicons.com", Config(ctx).serviceUrl)
        assertEquals(listOf("legacy-pair"), Config(ctx).pairIds)
        assertEquals(before, vault.all)
        assertEquals("https://backend.extend.teamofsilicons.com", settings.getString("service_url", null))
    }

    @Test fun credentialOnlyUpgradeAndExplicitOriginArePreserved() {
        val ctx = isolated()
        ctx.getSharedPreferences("extend_secrets", Context.MODE_PRIVATE).edit()
            .putString("credential", "opaque-encrypted-fixture").commit()
        assertEquals("https://backend.extend.teamofsilicons.com", Config(ctx).serviceUrl)
        Config(ctx).serviceUrl = "https://chosen.example"
        assertEquals("https://chosen.example", Config(ctx).serviceUrl)
    }

    @Test fun freshStatePinsAccountsAcrossRestart() {
        val ctx = isolated()
        assertEquals(BuildConfig.DEFAULT_SERVICE_URL, Config(ctx).serviceUrl)
        Config(ctx).pairIds = listOf("new-pair")
        assertEquals(BuildConfig.DEFAULT_SERVICE_URL, Config(ctx).serviceUrl)
    }
}

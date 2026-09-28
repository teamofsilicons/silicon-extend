plugins { id("com.android.library") }
android {
    namespace = "io.github.muntashirakon.adb"
    compileSdk = 36
    defaultConfig { minSdk = 26 }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    // android.util.Log and android.os.Build return defaults in JVM tests of the transport.
    testOptions { unitTests.isReturnDefaultValues = true }
    // The app runs on Android 8+ (minSdk 26): newer APIs must be guarded here too.
    lint { fatal += setOf("NewApi", "InlinedApi") }
}
dependencies {
    implementation("androidx.annotation:annotation:1.9.1")
    implementation("org.bouncycastle:bcprov-jdk15to18:1.81")
    // SPAKE2 for Wireless debugging pairing. LGPL-3.0 with a prebuilt libspake2.so per ABI; see
    // README.extend.md for how the licence is met. Its checksum is pinned in
    // gradle/verification-metadata.xml because JitPack builds it from a mutable Git tag.
    implementation("com.github.MuntashirAkon.spake2-java:spake2-android:2.2.1")
    testImplementation("junit:junit:4.13.2")
}

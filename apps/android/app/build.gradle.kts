import java.io.FileInputStream
import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
}

// The service URL a fresh install talks to. Override for local work with
// `./gradlew assembleDebug -PextendServiceUrl=http://10.0.2.2:8480`.
val productionServiceUrl = "https://backend.extend.teamofsilicons.com"
val debugServiceUrl = (project.findProperty("extendServiceUrl") as String?) ?: productionServiceUrl

// The release key lives outside the repository. EXTEND_ANDROID_SIGNING_PROPERTIES names a
// properties file with storeFile, storePassword, keyAlias and keyPassword; without it the release
// APK is left unsigned (never debug-signed, which would install but could never be updated by the
// published build).
val releaseSigning: Properties? = System.getenv("EXTEND_ANDROID_SIGNING_PROPERTIES")?.takeIf { it.isNotBlank() }?.let { path ->
    Properties().also { props -> FileInputStream(path).use { stream -> props.load(stream) } }
}

android {
    namespace = "com.teamofsilicons.extend"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.teamofsilicons.extend"
        minSdk = 26
        targetSdk = 36
        versionCode = 6
        versionName = "1.1.2"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    signingConfigs {
        if (releaseSigning != null) {
            create("release") {
                storeFile = file(releaseSigning.getProperty("storeFile"))
                storePassword = releaseSigning.getProperty("storePassword")
                keyAlias = releaseSigning.getProperty("keyAlias")
                keyPassword = releaseSigning.getProperty("keyPassword")
                // minSdk 26: Android 8.0/8.1 read only v2 (v3 is Android 9+), so v2 must stay on
                // beside v3. v1 is left to AGP (not needed from Android 7).
                enableV2Signing = true
                enableV3Signing = true
            }
        }
    }

    buildTypes {
        debug {
            buildConfigField("String", "DEFAULT_SERVICE_URL", "\"$debugServiceUrl\"")
            // Debug builds accept `service_url` / `force_tv` intent extras so tests can point the
            // app at a local service without touching the UI. Release builds ignore them: an
            // exported intent that could move a paired device to another service would let any
            // installed app take the device over.
            buildConfigField("boolean", "ALLOW_TEST_OVERRIDES", "true")
        }
        release {
            isMinifyEnabled = false
            buildConfigField("String", "DEFAULT_SERVICE_URL", "\"$productionServiceUrl\"")
            buildConfigField("boolean", "ALLOW_TEST_OVERRIDES", "false")
            signingConfig = if (releaseSigning != null) signingConfigs.getByName("release") else null
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    testOptions {
        unitTests.isReturnDefaultValues = true
    }

    // minSdk 26 reaches TVs, TV boxes and Fire TV sticks on Android 8–10: a call or constant from
    // a newer API must be guarded (or have a fallback). Fatal, so assembleRelease's lintVitalRelease
    // refuses to build a release APK with one, not only lintDebug; checkDependencies makes it look
    // at libadb's code too (checked 2026-09-27 with an unguarded call planted in each module).
    lint {
        fatal += setOf("NewApi", "InlinedApi")
        checkDependencies = true
        abortOnError = true
    }

    packaging {
        resources.excludes += setOf("/META-INF/{AL2.0,LGPL2.1}", "/META-INF/versions/9/previous-compilation-data.bin")
    }
}

kotlin {
    jvmToolchain(17)
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2025.09.00")
    implementation(composeBom)
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.ui:ui-tooling-preview")
    debugImplementation("androidx.compose.ui:ui-tooling")

    implementation("androidx.core:core-ktx:1.16.0")
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.9.2")
    implementation("androidx.lifecycle:lifecycle-service:2.9.2")
    implementation("androidx.lifecycle:lifecycle-process:2.9.2")

    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.9.0")
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    implementation(project(":libadb"))
    implementation("org.conscrypt:conscrypt-android:2.5.3")
    implementation("org.bouncycastle:bcpkix-jdk15to18:1.81")

    androidTestImplementation(composeBom)
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
    androidTestImplementation("androidx.test:runner:1.6.2")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.10.2")
    testImplementation("com.squareup.okhttp3:mockwebserver:4.12.0")
}

// The installation fixture is generated only for instrumentation tests.
android.sourceSets.getByName("androidTest").assets.srcDir(layout.buildDirectory.dir("generated/adb-test-assets"))
val generateAdbFixture by tasks.registering(Exec::class) {
    val output = layout.buildDirectory.dir("generated/adb-test-assets")
    inputs.files(rootProject.file("tools/adb-test-fixture/AndroidManifest.xml"), rootProject.file("tools/adb-test-fixture/build.sh"))
    outputs.file(output.map { it.file("fixture.apk") })
    environment("JAVA_HOME", System.getProperty("java.home"))
    commandLine("bash", rootProject.file("tools/adb-test-fixture/build.sh"), android.sdkDirectory, output.get().asFile)
}
tasks.matching { it.name == "mergeDebugAndroidTestAssets" }.configureEach { dependsOn(generateAdbFixture) }
// Lint's androidTest model reads the same folder: order it after the fixture when both run
// (`lintDebug assembleDebugAndroidTest`), without making lint build the fixture.
tasks.matching { it.name.contains("AndroidTest") && it.name.contains("lint", ignoreCase = true) }
    .configureEach { mustRunAfter(generateAdbFixture) }

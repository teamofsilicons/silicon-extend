plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
    id("org.jetbrains.kotlin.plugin.serialization")
}

// The service URL a fresh install talks to. Override for local work with
// `./gradlew assembleDebug -PbridgeServiceUrl=http://10.0.2.2:8480`.
val productionServiceUrl = "https://backend.bridge.teamofsilicons.com"
val debugServiceUrl = (project.findProperty("bridgeServiceUrl") as String?) ?: productionServiceUrl

android {
    namespace = "com.teamofsilicons.bridge"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.teamofsilicons.bridge"
        minSdk = 30
        targetSdk = 36
        versionCode = 1
        versionName = "1.0.0"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
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
            // Signed with the debug key so the APK installs as-is; real distribution signs it
            // with the release key outside this build.
            signingConfig = signingConfigs.getByName("debug")
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

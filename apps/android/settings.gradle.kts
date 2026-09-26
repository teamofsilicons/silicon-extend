pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
        maven { url = uri("https://jitpack.io"); content { includeGroupByRegex("com\\.github\\.MuntashirAkon.*") } }
    }
}

rootProject.name = "silicon-extend-android"
include(":app")
include(":libadb")
project(":libadb").projectDir = file("vendor/libadb")

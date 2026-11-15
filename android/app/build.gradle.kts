// The Rust cdylib is built by cargo-ndk into src/main/jniLibs/<abi>/ before this runs (see README).
plugins {
    id("com.android.application")
}

android {
    namespace = "dev.gpui.beeper"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.gpui.beeper"
        minSdk = 26
        // shortcut: targetSdk 35 enforces edge-to-edge, so adjustResize stops shrinking the content
        // rect and the keyboard covers the composer; raise it once IME insets are read via WindowInsets.
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
        manifestPlaceholders["nativeLibraryName"] = "beeper_gpui"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }

    packaging {
        jniLibs {
            useLegacyPackaging = false
        }
    }

    lint {
        checkReleaseBuilds = false
    }
}

dependencies {
    implementation("androidx.core:core:1.12.0")
    implementation("androidx.core:core-splashscreen:1.0.1")
    implementation("androidx.media:media:1.7.1")
    testImplementation("junit:junit:4.12")
}

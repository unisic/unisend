// The Rust cdylib is built by cargo-ndk into src/main/jniLibs/<abi>/ before this runs (see README).
plugins {
    id("com.android.application")
}

android {
    namespace = "dev.gpui.beeper"
    compileSdk = 35

    buildFeatures {
        resValues = true
    }

    defaultConfig {
        applicationId = if (System.getenv("UNISEND_DEV") == "1") "dev.gpui.beeper.dev" else "dev.gpui.beeper"
        resValue("string", "app_name", if (System.getenv("UNISEND_DEV") == "1") "Unisend Dev" else "Unisend")
        minSdk = 26
        // shortcut: targetSdk 35 enforces edge-to-edge, so adjustResize stops shrinking the content
        // rect and the keyboard covers the composer; raise it once IME insets are read via WindowInsets.
        targetSdk = 34
        // Each release must raise the code (scripts/release.sh passes the commit count); a lower one is refused.
        versionCode = System.getenv("UNISEND_VERSION_CODE")?.toInt() ?: 1
        versionName = System.getenv("UNISEND_VERSION_NAME") ?: "0.1.1"
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
        manifestPlaceholders["nativeLibraryName"] = "beeper_gpui"
    }

    // The release key is read from the environment only; nothing about it is kept in the repository.
    val keystore = System.getenv("UNISEND_KEYSTORE")
    signingConfigs {
        if (keystore != null) {
            create("release") {
                storeFile = file(keystore)
                storePassword = System.getenv("UNISEND_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("UNISEND_KEY_ALIAS")
                keyPassword = System.getenv("UNISEND_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            if (keystore != null) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
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

    // The dependency block is encrypted for Google Play alone; this app is not shipped there.
    dependenciesInfo {
        includeInApk = false
        includeInBundle = false
    }
}

dependencies {
    implementation("androidx.core:core:1.12.0")
    implementation("androidx.core:core-splashscreen:1.0.1")
    implementation("androidx.media:media:1.7.1")
    testImplementation("junit:junit:4.12")
}

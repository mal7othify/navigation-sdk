import org.jetbrains.kotlin.gradle.dsl.JvmTarget

// Approach B (push): BoltFFI-generated Kotlin + JNI library over navcore.
// Generated sources live in `generated/` and the native libraries in
// `src/main/jniLibs/` (and `boltffi/desktopJniLibs/` for JVM tests), all written by `boltffi pack android` (see
// crates/navcore-boltffi/boltffi.toml) and gitignored. Never edit them.
plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.android)
}

android {
    namespace = "com.navsdk.push"
    compileSdk = 36

    defaultConfig {
        minSdk = 24
        consumerProguardFiles("consumer-rules.pro")
        ndk {
            // Must match `architectures` in crates/navcore-boltffi/boltffi.toml.
            abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    sourceSets {
        getByName("main") {
            kotlin.srcDir("generated")
        }
        // The generated desktop loader extracts `<os>-<arch>/libnavcore_boltffi_jni.dylib`
        // from classpath resources, which `boltffi pack android` writes here.
        getByName("test") {
            resources.srcDir("boltffi/desktopJniLibs")
        }
    }

    packaging {
        jniLibs {
            // `boltffi pack` ships an unstripped library (~6 MB per ABI with
            // debug info); let AGP strip it at packaging time (~0.6 MB).
            useLegacyPackaging = false
        }
    }

    testOptions {
        unitTests.isReturnDefaultValues = true
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
    }
}

dependencies {
    // No JNA: the generated bindings are plain JNI. Coroutines back the
    // suspend functions and the async callback bridge.
    implementation(libs.coroutines.core)
    implementation(libs.coroutines.android)

    testImplementation(libs.junit)
    testImplementation(libs.coroutines.test)
    testImplementation(libs.org.json)
}

tasks.withType<Test>().configureEach {
    systemProperty("navsdk.fixtures", rootProject.projectDir.resolve("../fixtures").absolutePath)
}

import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.android)
}

android {
    namespace = "com.navsdk"
    compileSdk = 36

    defaultConfig {
        minSdk = 24
        consumerProguardFiles("consumer-rules.pro")
        ndk {
            // Must match the targets built by scripts/build-android.sh.
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

    packaging {
        jniLibs {
            // The Rust library is already stripped by cargo-ndk; ship it as-is
            // and never let a consumer's packaging rules drop it.
            useLegacyPackaging = false
            keepDebugSymbols += "**/libnavcore_ffi.so"
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
    // UniFFI's Kotlin bindings load the native library through JNA.
    api("net.java.dev.jna:jna:${libs.versions.jna.get()}@aar")
    implementation(libs.coroutines.core)
    implementation(libs.coroutines.android)

    // JVM unit tests run the generated bindings against the host dylib.
    testImplementation(libs.junit)
    testImplementation(libs.jna.jar)
    testImplementation(libs.coroutines.test)
}

// Point JNA at the host-built Rust library so unit tests exercise the real core.
tasks.withType<Test>().configureEach {
    systemProperty("jna.library.path", rootProject.projectDir.resolve("../target/release").absolutePath)
    systemProperty("navsdk.fixtures", rootProject.projectDir.resolve("../fixtures").absolutePath)
}

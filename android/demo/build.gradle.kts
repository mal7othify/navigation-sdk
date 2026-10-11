import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "com.navsdk.demo"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.navsdk.demo"
        minSdk = 24
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
    }

    sourceSets {
        // Ship the repo fixtures as assets so the demo can replay any of them.
        getByName("main").assets.srcDirs(rootProject.projectDir.resolve("../fixtures"))
        // The boundary benchmark (src/test) loads both host libraries: the
        // BoltFFI desktop JNI library comes from classpath resources.
        getByName("test").resources.srcDir(rootProject.projectDir.resolve("navsdk-push/boltffi/desktopJniLibs"))
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
    implementation(project(":navsdk"))
    implementation(project(":navsdk-push"))

    val composeBom = platform(libs.compose.bom)
    implementation(composeBom)
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.tooling.preview)
    implementation(libs.compose.material3)
    implementation(libs.activity.compose)
    implementation(libs.lifecycle.runtime.compose)
    implementation(libs.lifecycle.viewmodel.compose)
    implementation(libs.coroutines.android)
    implementation(libs.play.services.location)

    // Boundary benchmark on the JVM (scripts/bench-boundary.sh).
    testImplementation(libs.junit)
    testImplementation(libs.jna.jar)
    testImplementation(libs.org.json)

    androidTestImplementation(composeBom)
    androidTestImplementation(libs.androidx.junit)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.coroutines.test)
}

// Point JNA at the host-built UniFFI library and the benchmark at the fixtures.
tasks.withType<Test>().configureEach {
    systemProperty("jna.library.path", rootProject.projectDir.resolve("../target/release").absolutePath)
    systemProperty("navsdk.fixtures", rootProject.projectDir.resolve("../fixtures").absolutePath)
    systemProperty("navsdk.bench.out", layout.buildDirectory.file("boundary-bench.json").get().asFile.absolutePath)
    System.getProperty("navsdk.bench.calls")?.let { systemProperty("navsdk.bench.calls", it) }
    testLogging.showStandardStreams = true
}

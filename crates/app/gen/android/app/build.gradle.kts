import java.util.Base64
import java.util.Properties
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

android {
    compileSdk = 36
    buildToolsVersion = "36.0.0"
    namespace = "dev.fleetglass.Fleetglass"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "dev.fleetglass.Fleetglass"
        minSdk = 29
        targetSdk = 35
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
    }
    val releaseKeystoreBase64 = System.getenv("KEYSTORE_BASE64")
        ?: System.getenv("ANDROID_KEYSTORE_BASE64")
        ?: (findProperty("keystoreBase64") as? String)
        ?: (findProperty("KEYSTORE_BASE64") as? String)

    val releaseKeystorePath = System.getenv("KEYSTORE_PATH")
        ?: System.getenv("ANDROID_KEYSTORE_PATH")
        ?: System.getenv("ANDROID_RELEASE_STORE_FILE")
        ?: (findProperty("keystorePath") as? String)
        ?: (findProperty("KEYSTORE_PATH") as? String)

    val releaseKeystorePassword = System.getenv("KEYSTORE_PASSWORD")
        ?: System.getenv("ANDROID_KEYSTORE_PASSWORD")
        ?: System.getenv("ANDROID_RELEASE_STORE_PASSWORD")
        ?: (findProperty("keystorePassword") as? String)
        ?: (findProperty("KEYSTORE_PASSWORD") as? String)

    val releaseKeyAlias = System.getenv("KEY_ALIAS")
        ?: System.getenv("ANDROID_KEY_ALIAS")
        ?: System.getenv("ANDROID_RELEASE_KEY_ALIAS")
        ?: (findProperty("keyAlias") as? String)
        ?: (findProperty("KEY_ALIAS") as? String)

    val releaseKeyPassword = System.getenv("KEY_PASSWORD")
        ?: System.getenv("ANDROID_KEY_PASSWORD")
        ?: System.getenv("ANDROID_RELEASE_KEY_PASSWORD")
        ?: (findProperty("keyPassword") as? String)
        ?: (findProperty("KEY_PASSWORD") as? String)
        ?: releaseKeystorePassword

    val releaseKeystoreFile: File? = when {
        !releaseKeystorePath.isNullOrBlank() -> file(releaseKeystorePath)
        !releaseKeystoreBase64.isNullOrBlank() -> {
            val decoded = Base64.getDecoder().decode(releaseKeystoreBase64.trim())
            val target = layout.buildDirectory.file("keystore/release.keystore").get().asFile
            target.parentFile.mkdirs()
            target.writeBytes(decoded)
            target
        }
        else -> null
    }

    val hasReleaseSigning = releaseKeystoreFile != null &&
        releaseKeystoreFile.exists() &&
        !releaseKeystorePassword.isNullOrBlank() &&
        !releaseKeyAlias.isNullOrBlank()

    signingConfigs {
        if (hasReleaseSigning) {
            create("release") {
                storeFile = releaseKeystoreFile
                storePassword = releaseKeystorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
                enableV1Signing = true
                enableV2Signing = true
            }
        }
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {
                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            if (hasReleaseSigning) {
                signingConfig = signingConfigs.getByName("release")
            }
            optimization {
               enable = true
            }
            proguardFiles(
                *fileTree(".") {
                  include("**/*.pro")
                  exclude("build/**")
                }.files.toTypedArray()
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    buildFeatures {
        buildConfig = true
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_1_8
    }
}

rust {
    rootDirRel = "../../../../../ui"
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = file("tauri.build.gradle.kts"))

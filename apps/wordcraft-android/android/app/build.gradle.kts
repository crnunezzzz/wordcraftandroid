plugins { id("com.android.application") }
android {
    namespace = "ai.craft.wordcraft"
    compileSdk = 35
    ndkVersion = "28.2.13676358"
    defaultConfig {
        applicationId = "ai.craft.wordcraft"
        minSdk = 26
        targetSdk = 35
        // The release workflow numbers its builds so each one installs over the last.
        versionCode = System.getenv("CRAFT_ANDROID_VERSION_CODE")?.toIntOrNull() ?: 5
        versionName = System.getenv("CRAFT_ANDROID_VERSION_NAME") ?: "0.8.0-android.7"
        ndk { abiFilters += "arm64-v8a" }
    }
    val craftKeystore = System.getenv("CRAFT_ANDROID_KEYSTORE")
    signingConfigs {
        if (craftKeystore != null) create("craft") {
            storeFile = file(craftKeystore)
            storePassword = System.getenv("CRAFT_ANDROID_KEYSTORE_PASSWORD")
            keyAlias = "craft"
            keyPassword = System.getenv("CRAFT_ANDROID_KEYSTORE_PASSWORD")
        }
    }
    buildTypes {
        release {
            isMinifyEnabled = false
            if (craftKeystore != null) signingConfig = signingConfigs.getByName("craft")
        }
    }
    testOptions { unitTests.isIncludeAndroidResources = true }
    packaging { jniLibs { useLegacyPackaging = false } }
    compileOptions { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }
}
dependencies {
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.robolectric:robolectric:4.14.1")
    implementation(platform("org.jetbrains.kotlin:kotlin-bom:1.8.22"))
    implementation("androidx.games:games-activity:4.4.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
}

import javax.inject.Inject
import org.gradle.process.ExecOperations

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

/**
 * Debug-only defaults for the Settings bucket form, so a dev install starts prefilled. Each value
 * comes from the SONORUS_S3_* environment variable, else from the repository's gitignored `.env`.
 * Only debug builds get them (the secret ends up in the debug APK: do not share it); release
 * builds always get empty strings. Values are never printed.
 */
val bucketDefaultFields = mapOf(
    "DEFAULT_S3_ENDPOINT" to "SONORUS_S3_ENDPOINT",
    "DEFAULT_S3_REGION" to "SONORUS_S3_REGION",
    "DEFAULT_S3_BUCKET" to "SONORUS_S3_BUCKET",
    "DEFAULT_S3_ACCESS_KEY_ID" to "SONORUS_S3_ACCESS_KEY_ID",
    "DEFAULT_S3_SECRET_ACCESS_KEY" to "SONORUS_S3_SECRET_ACCESS_KEY",
    "DEFAULT_S3_PREFIX" to "SONORUS_S3_PREFIX",
)

val dotEnv: Map<String, String> = providers
    .fileContents(rootProject.layout.projectDirectory.file("../.env"))
    .asText.orNull.orEmpty()
    .lineSequence()
    .map { it.trim().removePrefix("export ").trim() }
    .filter { it.isNotEmpty() && !it.startsWith("#") && '=' in it }
    .associate { line ->
        val (key, value) = line.split('=', limit = 2)
        key.trim() to value.trim().removeSurrounding("\"").removeSurrounding("'")
    }

fun bucketDefault(env: String): String =
    providers.environmentVariable(env).orNull?.takeIf { it.isNotBlank() } ?: dotEnv[env].orEmpty()

/** [value] as a Java string literal. */
fun javaString(value: String): String =
    "\"" + value.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", "\\n") + "\""

android {
    namespace = "dev.constnishant.sonorus"
    compileSdk = 37
    ndkVersion = "28.2.13676358"

    defaultConfig {
        applicationId = "dev.constnishant.sonorus"
        minSdk = 34
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += "arm64-v8a" }
    }

    buildTypes {
        debug {
            bucketDefaultFields.forEach { (field, env) ->
                buildConfigField("String", field, javaString(bucketDefault(env)))
            }
        }
        release {
            isMinifyEnabled = false
            bucketDefaultFields.keys.forEach { field -> buildConfigField("String", field, "\"\"") }
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
    packaging { jniLibs { useLegacyPackaging = false } }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.navigation.compose)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.material3)
    implementation(libs.compose.material.icons.extended)
    implementation(libs.compose.ui.tooling.preview)
    debugImplementation(libs.compose.ui.tooling)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.coil.compose)
    implementation(libs.androidx.media3.common)
    implementation(libs.androidx.media3.session)
    implementation("${libs.jna.get()}@aar")
}

// ---------------------------------------------------------------------------------------------
// Rust core: cargo builds libsonorus_core.so for arm64-v8a, uniffi-bindgen generates the Kotlin
// bindings from that same .so. Both are wired into each variant as generated sources.
// ---------------------------------------------------------------------------------------------

/** Builds `sonorus-core` for aarch64-linux-android with the NDK clang toolchain. */
abstract class CargoNdkBuild : DefaultTask() {
    /** Cargo profile: "release" or "android-dev". */
    @get:Input abstract val profile: Property<String>
    @get:Input abstract val minSdk: Property<Int>
    @get:Internal abstract val ndkDir: DirectoryProperty
    @get:Internal abstract val repoRoot: DirectoryProperty
    /** jniLibs root; the library lands in `arm64-v8a/`. */
    @get:OutputDirectory abstract val outDir: DirectoryProperty
    @get:Inject abstract val exec: ExecOperations

    init {
        // Cargo does its own incremental tracking; always ask it.
        outputs.upToDateWhen { false }
    }

    @TaskAction
    fun build() {
        val os = System.getProperty("os.name").lowercase()
        val host = when {
            os.contains("mac") -> "darwin-x86_64"
            os.contains("win") -> "windows-x86_64"
            else -> "linux-x86_64"
        }
        val bin = ndkDir.get().asFile.resolve("toolchains/llvm/prebuilt/$host/bin")
        val clang = bin.resolve("aarch64-linux-android${minSdk.get()}-clang")
        require(clang.exists()) { "NDK clang not found: $clang" }
        val root = repoRoot.get().asFile
        exec.exec {
            workingDir = root
            commandLine(
                "cargo", "build", "-p", "sonorus-core",
                "--target", "aarch64-linux-android", "--profile", profile.get(),
            )
            environment("ANDROID_NDK_HOME", ndkDir.get().asFile.absolutePath)
            environment("CC_aarch64_linux_android", clang.absolutePath)
            environment("CXX_aarch64_linux_android", "${clang.absolutePath}++")
            environment("AR_aarch64_linux_android", bin.resolve("llvm-ar").absolutePath)
            environment("CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER", clang.absolutePath)
        }
        val built = root.resolve("target/aarch64-linux-android/${profile.get()}/libsonorus_core.so")
        val dest = outDir.get().asFile.resolve("arm64-v8a")
        dest.mkdirs()
        built.copyTo(dest.resolve("libsonorus_core.so"), overwrite = true)
    }
}

/** Generates the Kotlin bindings from the built library (UniFFI library mode). */
abstract class UniffiBindgen : DefaultTask() {
    @get:InputFile abstract val library: RegularFileProperty
    @get:Internal abstract val repoRoot: DirectoryProperty
    @get:OutputDirectory abstract val outDir: DirectoryProperty
    @get:Inject abstract val exec: ExecOperations

    @TaskAction
    fun generate() {
        val out = outDir.get().asFile
        out.deleteRecursively()
        out.mkdirs()
        exec.exec {
            workingDir = repoRoot.get().asFile
            commandLine(
                "cargo", "run", "-q", "-p", "uniffi-bindgen", "--",
                "generate", "--library", library.get().asFile.absolutePath,
                "--language", "kotlin", "--no-format", "--out-dir", out.absolutePath,
            )
        }
    }
}

androidComponents {
    onVariants { variant ->
        val name = variant.name.replaceFirstChar { it.uppercase() }
        val repo = rootProject.layout.projectDirectory.dir("..")
        val cargo = tasks.register<CargoNdkBuild>("cargoBuild$name") {
            profile.set(if (variant.buildType == "release") "release" else "android-dev")
            minSdk.set(34)
            ndkDir.set(androidComponents.sdkComponents.ndkDirectory)
            repoRoot.set(repo)
        }
        variant.sources.jniLibs?.addGeneratedSourceDirectory(cargo, CargoNdkBuild::outDir)

        val bindgen = tasks.register<UniffiBindgen>("uniffiBindgen$name") {
            library.set(cargo.flatMap { it.outDir.file("arm64-v8a/libsonorus_core.so") })
            repoRoot.set(repo)
        }
        variant.sources.kotlin?.addGeneratedSourceDirectory(bindgen, UniffiBindgen::outDir)
    }
}

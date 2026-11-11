import javax.inject.Inject
import org.gradle.process.ExecOperations

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

// ---------------------------------------------------------------------------------------------
// Versioning from git tags. The newest `vX.Y.Z[-pre]` tag names the release: versionName is the
// tag without the 'v', versionCode is derived from X.Y.Z. A debug build that is not exactly on
// its tag gets `-dev+<short sha>` appended to versionName. Release builds use the tag as is.
// ---------------------------------------------------------------------------------------------

val repoRootDir = rootProject.layout.projectDirectory.dir("..").asFile

/** Runs git in the repository root. Returns (exit code, trimmed stdout), or null if git cannot run. */
fun gitRun(vararg args: String): Pair<Int, String>? = runCatching {
    val output = providers.exec {
        workingDir = repoRootDir
        commandLine(listOf("git") + args.toList())
        isIgnoreExitValue = true
    }
    val exit = output.result.get().exitValue
    exit to output.standardOutput.asText.get().trim()
}.getOrNull()

val gitTag: String? = gitRun("describe", "--tags", "--match", "v[0-9]*", "--abbrev=0")
    ?.takeIf { it.first == 0 }?.second?.takeIf { it.isNotEmpty() }
val gitExactTag: Boolean = gitRun("describe", "--tags", "--exact-match", "--match", "v[0-9]*")
    ?.first == 0
val gitShortSha: String = gitRun("rev-parse", "--short", "HEAD")
    ?.takeIf { it.first == 0 }?.second?.takeIf { it.isNotEmpty() } ?: "unknown"

/** Release version (tag without 'v'), or null when there is no version tag. */
val releaseVersionName: String? = gitTag?.removePrefix("v")

/** versionCode from the numeric X.Y.Z part of the tag, pre-release suffix ignored. */
val releaseVersionCode: Int = if (gitTag == null) 1 else {
    val numeric = Regex("""(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?""")
        .matchEntire(releaseVersionName!!)
        ?: throw GradleException("Version tag '$gitTag' is not vX.Y.Z or vX.Y.Z-pre; cannot derive versionCode.")
    val (major, minor, patch) = numeric.destructured.toList().map { it.toInt() }
    check(minor < 1000 && patch < 1000) {
        "Version tag '$gitTag': minor and patch must be below 1000 for versionCode (got $minor.$patch)."
    }
    check(major < 2147) { "Version tag '$gitTag': major must be below 2147 for versionCode." }
    major * 1_000_000 + minor * 1_000 + patch
}

/** versionName for a build type: `0.0.0-dev` without any tag; debug builds off the tag get `-dev+sha`. */
fun sonorusVersionName(debug: Boolean): String = when {
    releaseVersionName == null -> "0.0.0-dev"
    debug && !gitExactTag -> "$releaseVersionName-dev+$gitShortSha"
    else -> releaseVersionName
}

logger.lifecycle("Sonorus version: ${sonorusVersionName(debug = false)} ($releaseVersionCode)")

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

/** Release signing: attached only when all four SONORUS_KEYSTORE_* / SONORUS_KEY_* variables are set. */
val releaseSigningEnv: Map<String, String?> = listOf(
    "SONORUS_KEYSTORE_PATH", "SONORUS_KEYSTORE_PASSWORD", "SONORUS_KEY_ALIAS", "SONORUS_KEY_PASSWORD",
).associateWith { providers.environmentVariable(it).orNull?.takeIf { value -> value.isNotEmpty() } }
val releaseSigningReady: Boolean = releaseSigningEnv.values.all { it != null }

android {
    namespace = "dev.constnishant.sonorus"
    compileSdk = 37
    ndkVersion = "28.2.13676358"

    defaultConfig {
        applicationId = "dev.constnishant.sonorus"
        minSdk = 34
        targetSdk = 36
        versionCode = releaseVersionCode
        versionName = sonorusVersionName(debug = false)
        ndk { abiFilters += "arm64-v8a" }
    }

    signingConfigs {
        create("release") {
            if (releaseSigningReady) {
                storeFile = file(releaseSigningEnv.getValue("SONORUS_KEYSTORE_PATH")!!)
                storePassword = releaseSigningEnv.getValue("SONORUS_KEYSTORE_PASSWORD")
                keyAlias = releaseSigningEnv.getValue("SONORUS_KEY_ALIAS")
                keyPassword = releaseSigningEnv.getValue("SONORUS_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        debug {
            bucketDefaultFields.forEach { (field, env) ->
                buildConfigField("String", field, javaString(bucketDefault(env)))
            }
        }
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            if (releaseSigningReady) signingConfig = signingConfigs.getByName("release")
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

// The debug build gets the `-dev+sha` suffix when it is not exactly on the version tag.
androidComponents {
    onVariants { variant ->
        if (variant.buildType == "debug") {
            variant.outputs.forEach { output -> output.versionName.set(sonorusVersionName(debug = true)) }
        }
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.core.splashscreen)
    implementation(libs.androidx.palette.ktx)
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
    implementation(libs.reorderable)
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

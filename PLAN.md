# Sonorus — build plan

Native Android hi-res player: Rust core (`crates/*`) + thin Kotlin/Compose shell (`android/`).
Opus plans/reviews; Haiku subagents (`rust-coder`, `kotlin-coder`) implement one task each.
Each phase must `cargo check --workspace` cleanly and ends with exactly one git commit on `main`.

Conventions (all crates):
- Edition 2024, deps declared in root `[workspace.dependencies]`, crates use `{ workspace = true }`.
- Errors: one `thiserror` enum per crate, `pub type Result<T>`. No `unwrap()`/`expect()` outside tests/examples
  except on invariants documented with a comment.
- Logging via `log` crate (Android: `android_logger` installed by `sonorus-core`). Never log secrets,
  presigned URLs, or full credentials.
- Credentials only from env vars (desktop/dev) or Android Keystore (passed in at runtime by Kotlin).
- Async runtime: tokio. Audio callback thread: no locks, no allocation, no syscalls that block.

Risk notes (tracked):
- `aws-sdk-s3` default HTTPS client uses rustls + `aws-lc-sys` → needs `cmake` + NDK when
  cross-compiling for Android (Phase 9). Fallback: swap to a `ring`-based rustls connector.
- UniFFI loads via JNA → `JNI_OnLoad` never runs → `ndk-context` must be initialized from Kotlin
  (Phase 8 exposes `init_android_context`, Phase 9 calls it in `Application.onCreate`).
- Bit-perfect needs OEM support for `MIXER_BEHAVIOR_BIT_PERFECT`; must degrade gracefully (Phase 11).

---

## Phase 1 — Workspace + `sonorus-storage`

Opus did: root `Cargo.toml` (workspace), `.gitignore`, `crates/storage/Cargo.toml`, stub `lib.rs`.
SDK facts (aws-sdk-s3 1.157): `aws_sdk_s3::config::{Builder, BehaviorVersion, Region, Credentials,
RequestChecksumCalculation, ResponseChecksumValidation, timeout::TimeoutConfig}`,
`aws_sdk_s3::error::{SdkError, ProvideErrorMetadata, DisplayErrorContext}`,
`aws_sdk_s3::primitives::ByteStream`, `aws_sdk_s3::types::{CompletedPart, CompletedMultipartUpload}`,
`aws_sdk_s3::presigning::PresigningConfig`. `SdkError::raw_response() -> Option<&HttpResponse>`,
`HttpResponse::status().as_u16()`.

- [x] **1.1 `crates/storage/src/error.rs` + `config.rs` + `lib.rs`**
  - `error.rs`:
    ```rust
    #[derive(Debug, thiserror::Error)]
    pub enum StorageError {
        #[error("missing environment variable {0}")] MissingEnv(&'static str),
        #[error("invalid storage config: {0}")]       InvalidConfig(String),
        #[error("object not found: {0}")]             NotFound(String),
        #[error("network error: {0}")]                Network(String),
        #[error("S3 error (status {status:?}): {message}")] S3 { status: Option<u16>, message: String },
        #[error("I/O error: {0}")]                    Io(#[from] std::io::Error),
        #[error("upload failed and was aborted: {0}")] UploadAborted(String),
    }
    pub type Result<T> = std::result::Result<T, StorageError>;
    impl StorageError {
        /// Network → true; S3 with status None, 408, 429, or 500..=599 → true; everything else false.
        pub fn is_retryable(&self) -> bool;
    }
    /// Map any SDK error. Status 404 → NotFound(key). TimeoutError/DispatchFailure/ResponseError → Network.
    /// ServiceError/ConstructionFailure/other → S3{status from raw_response, message = DisplayErrorContext}.
    pub(crate) fn from_sdk<E>(err: aws_sdk_s3::error::SdkError<E>, key: &str) -> StorageError
    where E: std::error::Error + Send + Sync + 'static;
    ```
    (Default `R` = `HttpResponse`, so `err.raw_response().map(|r| r.status().as_u16())` gives the status.)
  - `config.rs`:
    ```rust
    #[derive(Clone, PartialEq, Eq)]
    pub struct StorageConfig {
        pub endpoint: String, pub region: String, pub bucket: String,
        pub access_key_id: String, pub secret_access_key: String, pub path_style: bool,
    }
    impl StorageConfig {
        /// Env: SONORUS_S3_ENDPOINT, SONORUS_S3_BUCKET, SONORUS_S3_ACCESS_KEY_ID,
        /// SONORUS_S3_SECRET_ACCESS_KEY (all required → MissingEnv(name) if absent or empty);
        /// SONORUS_S3_REGION optional (if absent: region_from_endpoint(endpoint) else "auto");
        /// SONORUS_S3_PATH_STYLE optional: "1|true|yes" → true, "0|false|no" → false (case-insens.),
        /// other → InvalidConfig; default true. Calls validate() before returning.
        pub fn from_env() -> Result<Self>;
        /// endpoint "https://s3.{region}.backblazeb2.com", path_style true.
        pub fn backblaze_b2(region: &str, bucket: &str, key_id: &str, secret: &str) -> Self;
        /// endpoint "https://{account_id}.r2.cloudflarestorage.com", region "auto", path_style true.
        pub fn cloudflare_r2(account_id: &str, bucket: &str, key_id: &str, secret: &str) -> Self;
        /// All string fields non-empty after trim → else InvalidConfig(field name);
        /// endpoint must start with "https://" or "http://" → else InvalidConfig. A trailing '/' is allowed.
        pub fn validate(&self) -> Result<()>;
    }
    /// "https://s3.us-west-004.backblazeb2.com" → Some("us-west-004"); anything else → None.
    pub fn region_from_endpoint(endpoint: &str) -> Option<String>;
    ```
    Manual `impl fmt::Debug`: shows endpoint, region, bucket, path_style; `access_key_id` as first 4 chars + "…"
    (or "…" if shorter than 8 chars); `secret_access_key` always `"<redacted>"`. No `Display`.
    Unit tests: Debug never contains the secret; region_from_endpoint cases; presets.
  - `lib.rs`: `mod error; mod config; mod client; mod upload;` + `pub use` of every pub item.
- [x] **1.2 `crates/storage/src/client.rs`** (depends on 1.1)
    ```rust
    #[derive(Clone)]
    pub struct Storage { pub(crate) client: aws_sdk_s3::Client, pub(crate) bucket: String }
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ObjectInfo { pub key: String, pub size: u64, pub etag: Option<String>, pub last_modified: Option<i64> } // unix secs
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ObjectMeta { pub key: String, pub size: u64, pub etag: Option<String>, pub content_type: Option<String>, pub last_modified: Option<i64> }
    pub const AUDIO_EXTENSIONS: &[&str] = &["flac", "wav", "m4a", "alac", "aif", "aiff"];
    pub fn is_audio_key(key: &str) -> bool;              // case-insensitive extension match; "dir/" → false
    pub fn content_type_for_key(key: &str) -> &'static str; // flac→audio/flac, wav→audio/wav, m4a|alac→audio/mp4,
                                                         // aif|aiff→audio/aiff, jpg|jpeg→image/jpeg, png→image/png,
                                                         // json→application/json, lrc|txt→text/plain; else application/octet-stream
    impl Storage {
        /// validate(); build aws_sdk_s3::Config directly (no aws-config crate): BehaviorVersion::latest(),
        /// Region, endpoint_url (trailing '/' trimmed), Credentials::new(id, secret, None, None, "sonorus-env"),
        /// force_path_style, RequestChecksumCalculation::WhenRequired, ResponseChecksumValidation::WhenRequired,
        /// TimeoutConfig connect 10 s, read 30 s. Sync; must not need a running runtime.
        pub fn new(config: &StorageConfig) -> Result<Self>;
        pub fn bucket(&self) -> &str;
        /// ListObjectsV2 paginated to completion (continuation token). Skips keys ending with '/'.
        pub async fn list(&self, prefix: &str) -> Result<Vec<ObjectInfo>>;
        pub async fn list_audio(&self, prefix: &str) -> Result<Vec<ObjectInfo>>; // list + is_audio_key filter
        pub async fn head(&self, key: &str) -> Result<ObjectMeta>;               // 404 → NotFound
        /// Range "bytes={start}-{start+len-1}". len == 0 → Ok(empty) without a request.
        /// HTTP 416 (start ≥ size) → Ok(empty). May return fewer than len bytes at EOF.
        pub async fn get_range(&self, key: &str, start: u64, len: u64) -> Result<bytes::Bytes>;
        pub async fn get_bytes(&self, key: &str) -> Result<bytes::Bytes>;
        /// content_type None → content_type_for_key(key).
        pub async fn put_bytes(&self, key: &str, data: bytes::Bytes, content_type: Option<&str>) -> Result<()>;
        /// expires clamped to 1 s ..= 7 days. Returns URL string. Never log it.
        pub async fn presign_get(&self, key: &str, expires: std::time::Duration) -> Result<String>;
        pub async fn delete(&self, key: &str) -> Result<()>; // deleting a missing key is Ok
    }
    ```
    Body collection errors (ByteStream::collect) → Network. All SDK errors via `from_sdk`.
- [x] **1.3 `crates/storage/src/upload.rs`** (depends on 1.1; parallel with 1.2 via the `Storage` fields above)
    ```rust
    pub const PART_SIZE: usize = 8 * 1024 * 1024;
    pub const MAX_PART_ATTEMPTS: u32 = 4;
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UploadProgress { pub bytes_uploaded: u64, pub total_bytes: u64, pub parts_done: u32, pub parts_total: u32 }
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct UploadedPart { pub part_number: i32, pub etag: String }
    impl Storage {
        pub async fn create_multipart(&self, key: &str, content_type: &str) -> Result<String>; // upload_id
        /// One attempt per call is NOT enough: retries up to MAX_PART_ATTEMPTS while err.is_retryable(),
        /// sleeping 500ms * 2^(attempt-1) + fastrand 0..250ms between attempts. Missing ETag → S3 error.
        pub async fn upload_part(&self, key: &str, upload_id: &str, part_number: i32, data: bytes::Bytes) -> Result<UploadedPart>;
        pub async fn complete_multipart(&self, key: &str, upload_id: &str, parts: &[UploadedPart]) -> Result<()>; // parts sorted by number
        pub async fn abort_multipart(&self, key: &str, upload_id: &str) -> Result<()>;
        /// size ≤ PART_SIZE → single put_object (read whole file; empty file allowed).
        /// Else multipart: parts read sequentially from tokio::fs::File (read until PART_SIZE or EOF),
        /// on_progress(..) once with zeros-done before the first part and after each part.
        /// Any part/complete failure → abort_multipart (log abort errors, don't mask) → Err(UploadAborted(msg)).
        /// content_type None → content_type_for_key(key).
        pub async fn upload_file<F>(&self, path: &std::path::Path, key: &str, content_type: Option<&str>, on_progress: F) -> Result<()>
        where F: FnMut(UploadProgress) + Send;
    }
    ```
- [x] **1.4 `crates/storage/examples/ls.rs`** (depends on 1.2): `dotenvy::dotenv().ok()`, `StorageConfig::from_env()`,
  print `{config:?}` (redacted), `list(prefix = argv[1] or "")`, print `"{size:>12} {key}"` per object and a total line;
  errors → eprintln + exit code 1. `#[tokio::main]`.
- [x] **1.R** Opus review, `cargo check --workspace`, `cargo clippy --workspace`, commit "Phase 1: workspace + sonorus-storage".

## Phase 2 — `sonorus-engine::source::RangeSource`
- [ ] 2.1 `crates/engine` crate skeleton (Opus), deps: sonorus-storage, symphonia, tokio, bytes, rtrb, log.
- [ ] 2.2 `source.rs`: `RemoteObject` trait `{ async fn size(); async fn get_range(start, len) -> Result<Bytes>; }`
  (impl for `Storage`+key) so the fetcher is testable with an in-memory fake.
- [ ] 2.3 `RangeSource::open(obj, rt: tokio::runtime::Handle) -> (RangeSource, BufferHandle)`;
  `impl Read + Seek + MediaSource` (`is_seekable=true`, `byte_len=Some(size)`). Shared state:
  `Mutex<Window>` + `Condvar` (decode thread only, never audio thread). Chunks 1 MiB keyed by index;
  window = [pos_chunk-2, pos_chunk+24]; fetcher evicts outside window, never drops in-window chunks on error.
  Retry: exponential backoff 250 ms → 8 s cap, full jitter, infinite while retryable; non-retryable → last_error +
  wake reader with error. Seek updates pos + notifies fetcher (tokio `Notify`). Read blocks up to 45 s
  with no progress → `io::ErrorKind::TimedOut`. Drop → cancel fetcher.
- [ ] 2.4 `BufferHandle { is_buffering(), buffered_ahead_bytes(), last_error() -> Option<String> }` (atomics + small mutex).

## Phase 3 — `engine::decoder::TrackDecoder`
- [ ] 3.1 `TrackDecoder::new(source: Box<dyn MediaSource>, ext_hint: Option<&str>) -> Result<Self>`; probe with hint,
  `FormatOptions { enable_gapless: true, .. }`, default track, codec registry default.
- [ ] 3.2 `next_block(&mut self, out: &mut Vec<f32>) -> Result<Option<usize /*frames*/>>` interleaved f32;
  skip `DecodeError` packets (log, count); reset on `ResetRequired`. Also keep native-int path
  (`next_block_i32`) for bit-perfect int output (Phase 10/11).
- [ ] 3.3 `StreamInfo { codec, sample_rate, channels, bits_per_sample, duration: Option<Duration>, total_frames }`
  + `label()` → "FLAC 24/96", "WAV 16/44.1", "ALAC 24/192".
- [ ] 3.4 `seek(Duration) -> Result<Duration actual>` via `SeekMode::Accurate`, then `decoder.reset()`.

## Phase 4 — Playback pipeline
- [ ] 4.1 `engine::ring`: rtrb `Producer/Consumer<f32>` sized 2 s @ max rate; `OutputSink` trait
  (`start(format, consumer)`, `stop`, `latency`) with a `NullSink` (real-time paced thread) for desktop.
- [ ] 4.2 `engine::player::Player` command channel (`Play(track), Pause, Resume, Stop, Seek, SetQueue…`)
  + event broadcast (`State(Playing|Paused|Buffering|Stopped)`, `Position`, `TrackChanged`, `Error`).
- [ ] 4.3 decode thread: pulls blocks, pushes into ring, flushes ring on seek (generation counter).
- [ ] 4.4 underrun detection in consumer (atomic counter, no alloc) → controller emits Buffering, resumes at threshold.
- [ ] 4.5 position = frames consumed (AtomicU64) / rate, reported every 250 ms.

## Phase 5 — Gapless + queue
- [ ] 5.1 `engine::queue::Queue` (items, index, shuffle order w/ seed, repeat Off/All/One; pure, unit-tested).
- [ ] 5.2 prefetch: open next track's RangeSource+decoder when current has < 15 s left.
- [ ] 5.3 seamless handoff in decode thread when formats match; else drain + reconfigure output.

## Phase 6 — `sonorus-library`
- [ ] 6.1 schema + migrations (rusqlite bundled): tracks, albums, artists, covers, scan_state.
- [ ] 6.2 bucket scan: list_audio, diff by etag/size, fetch head bytes (first 1 MiB, + tail for m4a) → lofty tags.
- [ ] 6.3 cover art extract (embedded or `cover.jpg` in folder) → cache dir, thumbnail later in Kotlin.
- [ ] 6.4 queries: albums, artists, tracks by album, search (FTS5 over title/artist/album).

## Phase 7 — Playlists, favorites, history + bucket sync
- [ ] 7.1 tables + CRUD. 7.2 history (recently played). 7.3 JSON export/import to `_sonorus/playlists.json`,
  `_sonorus/favorites.json` (last-writer-wins by updated_at).

## Phase 8 — `sonorus-core` (UniFFI, cdylib `libsonorus_core.so`)
- [ ] 8.1 UniFFI proc-macro setup, `SonorusCore` object, records/enums mirroring engine/library types.
- [ ] 8.2 async event stream (callback interface `EventListener` or async `next_event()`).
- [ ] 8.3 `init_android_context(context)` + `android_logger` init (JNA/`JNI_OnLoad` gotcha).

## Phase 9 — Android project
- [ ] 9.1 Gradle (Kotlin DSL, version catalog), app id `dev.constnishant.sonorus`, minSdk 34, cargo-ndk task
  building arm64-v8a into `jniLibs`, uniffi-bindgen Kotlin generation.
- [ ] 9.2 Compose theme + navigation (Library, Album, Artist, Search, Now Playing).
- [ ] 9.3 Library/Album/Now-playing screens bound to `SonorusCore`.

## Phase 10 — AAudio output (ndk crate)
- [ ] 10.1 `AAudioSink: OutputSink` float + I24_PACKED/I32 formats, device id selection, exclusive/shared.
- [ ] 10.2 data callback: consume from rtrb only (no locks/alloc), underrun counter; error callback → reopen on device change.

## Phase 11 — Bit-perfect
- [ ] 11.1 Kotlin `UsbAudioRouter`: find USB DAC `AudioDeviceInfo`, `getSupportedMixerAttributes`,
  `setPreferredMixerAttributes(MIXER_BEHAVIOR_BIT_PERFECT)` matching file format/rate/channel mask.
- [ ] 11.2 fallback chain bit-perfect → exclusive AAudio → shared mixer; `SignalPath` record + badge.
- [ ] 11.3 bit-perfect mode disables volume/ReplayGain/EQ/crossfade; rubato resample only if DAC lacks rate.

## Phase 12 — Background playback
- [ ] 12.1 `PlaybackService : MediaSessionService` (foregroundServiceType mediaPlayback), Media3 `SimpleBasePlayer`
  bridging to core. 12.2 audio focus, becoming-noisy + USB detach → pause. 12.3 lock-screen/Bluetooth controls.

## Phase 13 — Upload
- [ ] 13.1 SAF picker → copy-free read via fd; lofty tags in Rust; BLAKE3 hash dedupe.
- [ ] 13.2 upload queue table (upload_id, parts done) → resume via list_parts. Key `Artist/Album/NN - Title.ext`.
- [ ] 13.3 foreground `UploadService` (dataSync) + progress UI.

## Phase 14 — Offline cache
- [ ] 14.1 LRU disk cache of whole objects (size cap), RangeSource reads cache first.
- [ ] 14.2 per-album/playlist download toggle + pinned entries exempt from eviction.

## Phase 15 — Polish
- [ ] lyrics (.lrc sidecar, synced), sleep timer, signal-path screen, settings (bucket creds → Keystore,
  cache size, output mode), adaptive icon, release build (R8, signing via env).

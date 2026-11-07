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

Opus did: `crates/engine` skeleton (deps: sonorus-storage, symphonia 0.6.1 [flac, pcm, wav, aiff, isomp4,
alac, all-meta], rtrb, tokio, bytes, fastrand, log, thiserror). Symphonia 0.6 trait:
`symphonia::core::io::MediaSource: Read + Seek + Send + Sync { fn is_seekable(&self) -> bool; fn byte_len(&self) -> Option<u64>; }`.

- [x] **2.1 `crates/storage/src/client.rs`: `Storage::without_sdk_retries(&self) -> Storage`**
  Clone with `self.client.config().to_builder().retry_config(aws_sdk_s3::config::retry::RetryConfig::disabled()).build()`
  → `Client::from_conf`. Same bucket. Doc: used by the streaming source, which owns its own retry loop.
- [x] **2.2 `crates/engine/src/source.rs`** (+ `lib.rs`: `pub mod source;`)
    ```rust
    pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
    /// Object that can serve byte ranges. Impl'd for StorageObject; tests use an in-memory fake.
    pub trait RemoteObject: Send + Sync + 'static {
        fn get_range(&self, start: u64, len: u64) -> BoxFuture<'_, sonorus_storage::Result<bytes::Bytes>>;
    }
    pub struct StorageObject { pub storage: sonorus_storage::Storage, pub key: String }
    impl RemoteObject for StorageObject { /* Box::pin(async move { self.storage.get_range(&self.key, start, len).await }) */ }

    #[derive(Debug, Clone)]
    pub struct SourceConfig {
        pub chunk_size: u64,          // 1 MiB
        pub chunks_ahead: u64,        // 24
        pub chunks_behind: u64,       // 2
        pub read_timeout: Duration,   // 45 s
        pub backoff_base: Duration,   // 250 ms
        pub backoff_cap: Duration,    // 8 s
    }
    impl Default for SourceConfig { /* values above */ }

    pub struct RangeSource { shared: Arc<Shared>, task: tokio::task::JoinHandle<()> }
    #[derive(Clone)]
    pub struct BufferHandle { shared: Arc<Shared> }

    impl RangeSource {
        /// Spawns the fetcher on `rt`. `size` comes from list/head. Starts fetching chunk 0 immediately.
        pub fn open(object: Arc<dyn RemoteObject>, size: u64, rt: &tokio::runtime::Handle, config: SourceConfig) -> (RangeSource, BufferHandle);
        pub fn size(&self) -> u64;
    }
    impl std::io::Read for RangeSource {}   // blocking; NEVER call from a tokio worker or the audio thread
    impl std::io::Seek for RangeSource {}
    impl symphonia::core::io::MediaSource for RangeSource { is_seekable → true; byte_len → Some(size) }
    impl Drop for RangeSource { /* state.closed = true; notify fetcher; task.abort() */ }

    impl BufferHandle {
        pub fn is_buffering(&self) -> bool;          // AtomicBool
        pub fn buffered_ahead_bytes(&self) -> u64;   // contiguous bytes available from read_pos forward
        pub fn last_error(&self) -> Option<String>;  // most recent fetch error (cleared on next success)
        pub fn read_position(&self) -> u64;
        pub fn size(&self) -> u64;
    }
    ```
    Internal state (private):
    ```rust
    struct Shared {
        size: u64, config: SourceConfig,
        state: std::sync::Mutex<State>,
        data_ready: std::sync::Condvar,        // reader waits here; fetcher notify_all()s
        wake_fetcher: tokio::sync::Notify,     // reader/seek/drop → notify_one()
        buffering: AtomicBool,
    }
    struct State {
        chunks: BTreeMap<u64, Bytes>,          // chunk index → exactly expected_len(index) bytes
        read_pos: u64,
        failed: Option<(u64, String)>,         // non-retryable failure for a chunk index
        last_error: Option<String>,
        closed: bool,
    }
    ```
    Definitions: `last_chunk = (size - 1) / chunk_size` (size == 0 → no chunks at all);
    `expected_len(i) = min(chunk_size, size - i*chunk_size)`; for read position p: `cur = min(p / chunk_size, last_chunk)`;
    window = `[cur.saturating_sub(behind), min(cur + ahead, last_chunk)]`.

    Fetcher task (`async fn fetch_loop(shared, object)`), loop:
    1. Lock. If closed → return. Evict chunks outside window. If `failed` is Some and its index is outside the
       window, clear it. Pick target = first index in `cur..=window_end` not in `chunks` and not the failed index.
       Unlock (never hold the std Mutex across `.await`).
    2. No target → `shared.wake_fetcher.notified().await`; continue.
    3. Fetch target: `let fut = object.get_range(target*chunk_size, expected_len(target)); tokio::pin!(fut);`
       loop `select! { r = &mut fut => break r, _ = wake_fetcher.notified() => { lock; if closed return;
       if target is outside the current window → drop fut, continue outer loop; else keep awaiting fut } }`.
    4. Ok(bytes) with `bytes.len() as u64 == expected_len(target)`: lock; if target still in window insert;
       `last_error = None`; reset attempt counter; unlock; `data_ready.notify_all()`.
       Ok with wrong length → treat as a retryable error "short read: got X of Y bytes".
    5. Err(e) retryable (`e.is_retryable()`): `last_error = Some(e.to_string())`, log::warn, attempt += 1,
       sleep `fastrand` uniform in `0..=min(cap, base * 2^(attempt-1))` (full jitter, saturating), but the sleep
       is a `select!` with `wake_fetcher.notified()` so a seek interrupts it. Never touch existing chunks.
       Retryable errors retry forever (the reader's timeout decides when to give up).
    6. Err(e) not retryable: lock; `failed = Some((target, e.to_string()))`, `last_error = Some(..)`; unlock;
       `data_ready.notify_all()`.

    `Read::read(buf)`: empty buf → Ok(0). Lock. `deadline = now + read_timeout`. Loop:
    - `read_pos >= size` → buffering=false, Ok(0).
    - `idx = read_pos / chunk_size`; chunk present → copy `n = min(buf.len(), chunk.len() - off)` from offset
      `off = read_pos - idx*chunk_size`; `read_pos += n`; buffering = false; if `read_pos / chunk_size != idx`
      notify fetcher (window moved); Ok(n). (One chunk per call is fine; Symphonia's MediaSourceStream buffers.)
    - `failed == Some((idx, msg))` → Err(io::Error::other(msg)).
    - `closed` → Err(io::ErrorKind::BrokenPipe).
    - else buffering = true; notify fetcher; `wait_timeout(guard, deadline - now)`; if now >= deadline →
      buffering stays true, Err(io::Error::new(TimedOut, "no data for {read_timeout:?}")).
    Poisoned mutex → Err(io::Error::other("source state poisoned")) (no unwrap).

    `Seek::seek`: Start(n) → n; Current(d)/End(d) relative to read_pos/size with checked i64 math; negative or
    overflow → Err(InvalidInput). Positions past size are allowed (reads return 0). Set read_pos; if the new
    position's chunk differs from `failed`'s index, clear `failed`; notify fetcher. Ok(new_pos).
    `buffered_ahead_bytes`: sum from read_pos forward over consecutive present chunks (first one counted from
    the in-chunk offset), stop at first gap.

    Tests (`#[cfg(test)]`, plain `#[test]` + `tokio::runtime::Runtime::new()` whose handle is passed in; the
    blocking reads run on the test thread, outside the runtime). Fake `MemObject { data: Bytes,
    fail_plan: Mutex<VecDeque<StorageError>> }` pops one planned error per call before serving. Use small config
    (chunk 1024, ahead 4, behind 1, timeout 2 s, backoff 1–10 ms). Cases: full sequential read of 10_000 random
    bytes equals data (read_to_end); seek(Start(5000)) then read matches; SeekFrom::End(-10); 3 planned
    Network errors then success still reads correctly and last_error is cleared; a planned NotFound → read
    returns Err; size 0 → read returns 0 immediately; fake that never returns (pending forever) with
    timeout 200 ms → read Err TimedOut and is_buffering() true.

## Phase 3 — `engine::decoder::TrackDecoder`

Opus did: fixtures in `crates/engine/tests/fixtures/` (stereo, 0.3 s): `tone_24_96.{wav,flac,m4a(ALAC)}`
(28 800 frames, 24-bit) and `tone_16_44.{wav,flac}` (13 230 frames, 16-bit). FLAC/ALAC are lossless encodes
of the WAVs, so decoded samples must be identical.

Symphonia 0.6.1 facts (verified in source):
- Probe: `symphonia::default::get_probe().probe(&Hint, MediaSourceStream, FormatOptions, MetadataOptions)
  -> Result<Box<dyn FormatReader>>`; `Hint::new().with_extension(ext)`; paths `symphonia::core::formats::
  {FormatOptions, FormatReader, SeekMode, SeekTo, SeekedTo, TrackType, probe::Hint}`, `symphonia::core::io::
  {MediaSource, MediaSourceStream}`, `symphonia::core::meta::MetadataOptions`, `symphonia::core::errors::Error`.
- `format.default_track(TrackType::Audio) -> Option<&Track>`; `Track { id: u32, codec_params: Option<CodecParameters>,
  time_base: Option<TimeBase>, num_frames: Option<u64>, duration: Option<units::Duration>, start_ts, delay, padding }`;
  `codec_params.audio() -> Option<&AudioCodecParameters>` with `sample_rate: Option<u32>`,
  `bits_per_sample: Option<u32>`, `channels: Option<Channels>` (`.count()`), `codec: AudioCodecId`.
- Decoder: `symphonia::default::get_codecs().make_audio_decoder(&AudioCodecParameters, &AudioDecoderOptions)
  -> Result<Box<dyn AudioDecoder>>`; `AudioDecoderOptions::default().gapless(true)` (gapless lives on the DECODER
  in 0.6, not FormatOptions). `decoder.codec_info().short_name` ("flac", "alac", "pcm_s24le", ...), `reset()`.
- `format.next_packet() -> Result<Option<Packet>>` (Ok(None) = end); `Packet { track_id, pts: Timestamp, dur, .. }`.
- `decoder.decode(&packet) -> Result<GenericAudioBufferRef<'_>>`; `.frames()`, `.copy_to_vec_interleaved::<S>(&mut Vec<S>)`
  where `S: symphonia::core::audio::conv::ConvertibleSample` (f32 and i32 both implement it; int→i32 is
  left-justified, i.e. 24-bit sample s becomes s << 8 — lossless).
- `Error::{IoError(io::Error), DecodeError(&str), SeekError(SeekErrorKind), Unsupported(&str), LimitError(&str), ResetRequired}`.
- Seek: `format.seek(SeekMode::Accurate, SeekTo::Time { time: units::Time, track_id: Some(id) }) -> Result<SeekedTo
  { track_id, required_ts: Timestamp, actual_ts: Timestamp }>`. `Time::try_new(secs: i64, nanos: u32) -> Option<Time>`,
  `Timestamp::get() -> i64`, `TimeBase::calc_time(ts) -> Option<Time>`, `TimeBase::calc_timestamp(Time) -> Option<Timestamp>`,
  `Time::as_secs_f64()`. **Accurate seek lands on the packet at/before required_ts; the caller must discard
  decoded frames until required_ts.**

- [x] **3.1 `crates/engine/src/error.rs`** (+ `lib.rs`: `pub mod error; pub use error::{EngineError, Result};`)
    ```rust
    #[derive(Debug, thiserror::Error)]
    pub enum EngineError {
        #[error("unsupported stream: {0}")] Unsupported(String),
        #[error("no audio track")]          NoAudioTrack,
        #[error("I/O error: {0}")]          Io(#[from] std::io::Error),
        #[error("decode error: {0}")]       Decode(String),
        #[error("seek error: {0}")]         Seek(String),
        #[error(transparent)]               Storage(#[from] sonorus_storage::StorageError),
    }
    pub type Result<T> = std::result::Result<T, EngineError>;
    ```
- [x] **3.2 `crates/engine/src/decoder.rs`** (+ `lib.rs`: `pub mod decoder;`)
    ```rust
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StreamInfo {
        pub codec: String,                 // "FLAC" | "ALAC" | "WAV" | "AIFF" | "PCM" | uppercase short_name otherwise
        pub sample_rate: u32,
        pub channels: u16,
        pub bits_per_sample: Option<u32>,
        pub duration: Option<std::time::Duration>,
        pub total_frames: Option<u64>,
    }
    impl StreamInfo {
        /// "FLAC 24/96", "WAV 16/44.1", "ALAC 24/192", "FLAC 24/352.8"; bits None → "FLAC 96 kHz".
        pub fn label(&self) -> String;
    }
    /// 44100 → "44.1", 96000 → "96", 88200 → "88.2", 352800 → "352.8", 11025 → "11.025" (trim trailing zeros).
    pub fn format_khz(rate: u32) -> String;

    pub struct TrackDecoder { /* format: Box<dyn FormatReader>, decoder: Box<dyn AudioDecoder>, track_id: u32,
        codec_params: AudioCodecParameters (clone, for ResetRequired), time_base: Option<TimeBase>, info: StreamInfo,
        position: u64 /*frames*/, discard_until: u64 /*frames*/, skipped_packets: u64 */ }

    impl TrackDecoder {
        /// MediaSourceStream::new(source, Default::default()); hint from ext_hint (lowercased);
        /// FormatOptions/MetadataOptions default; default_track(Audio) else NoAudioTrack; codec_params.audio()
        /// else Unsupported("no audio codec parameters"); sample_rate None → Unsupported; channels None →
        /// Unsupported; decoder with gapless(true). Probe/decoder-creation errors → Unsupported(err.to_string()).
        /// codec label: short_name "flac"→"FLAC", "alac"→"ALAC", starts_with "pcm" → ext_hint "wav"→"WAV",
        /// "aif"|"aiff"→"AIFF", else "PCM"; other → short_name.to_uppercase().
        /// duration: from total_frames / sample_rate if num_frames is Some, else time_base+duration, else None.
        pub fn new(source: Box<dyn MediaSource>, ext_hint: Option<&str>) -> Result<Self>;
        pub fn info(&self) -> &StreamInfo;
        /// Next decoded block, interleaved, into `out` (cleared first). Ok(Some(frames)) with frames > 0,
        /// Ok(None) at end of stream. Generic so the same path yields f32 (shared mixer) or i32 (bit-perfect int).
        pub fn next_block<S: ConvertibleSample>(&mut self, out: &mut Vec<S>) -> Result<Option<usize>>;
        /// Accurate seek; returns the position actually reached (== requested unless clamped).
        /// `to` beyond duration (when known) → clamp to duration (next_block then returns None).
        pub fn seek(&mut self, to: std::time::Duration) -> Result<std::time::Duration>;
        /// Frames delivered so far (after seek: the seek target), i.e. playback position in frames.
        pub fn position_frames(&self) -> u64;
        pub fn skipped_packets(&self) -> u64;
    }
    ```
    `next_block` loop:
    - `format.next_packet()`: Ok(None) → Ok(None). Err(ResetRequired) → recreate decoder from stored codec_params,
      continue. Err(IoError(e)) with kind UnexpectedEof → Ok(None). Err(IoError(e)) otherwise → Err(Io(e)) (this is
      how a RangeSource timeout surfaces — must not be swallowed). Other Err → Decode(err.to_string()).
    - packet.track_id != track_id → continue.
    - `decoder.decode(&packet)`: Err(DecodeError(m)) or Err(IoError(_)) → log::warn, skipped_packets += 1,
      consecutive_bad += 1, if consecutive_bad > 64 → Err(Decode("too many corrupt packets")), continue.
      Other Err → Decode. Ok(buf): consecutive_bad = 0; if buf.frames() == 0 continue;
      `buf.copy_to_vec_interleaved(out)` (clear out first); frames = buf.frames(); channels = info.channels.
      Discard: if position + frames <= discard_until → position += frames, continue; else if position <
      discard_until → drop the first (discard_until - position) * channels samples from `out` (`out.drain(..n)`),
      frames -= that, position = discard_until. Then position += frames; return Ok(Some(frames)).
    - `seek(to)`: clamp to duration if known; Time::try_new(to.as_secs() as i64, to.subsec_nanos()) else Seek err;
      `format.seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(track_id) })` → Err → Seek(err.to_string());
      `decoder.reset()`; convert required_ts / actual_ts to frames via ts_to_frames; position = actual frames;
      discard_until = required frames; return Duration::from_secs_f64(required_frames as f64 / sample_rate as f64).
    - `fn ts_to_frames(&self, ts: Timestamp) -> u64`: time_base Some → calc_time → secs_f64 * sample_rate, rounded;
      None → ts.get() as u64 (clamp negative to 0).
    No unwrap/expect outside tests. Never panic on malformed input.

    Tests (`#[cfg(test)]` in decoder.rs; load fixtures via `std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/..."))` into `Box::new(std::io::Cursor::new(bytes))`):
    1. info + labels: tone_24_96.flac → FLAC/96000/2/Some(24)/"FLAC 24/96"; .wav (hint "wav") → "WAV 24/96";
       .m4a (hint "m4a") → "ALAC 24/96"; tone_16_44.flac → "FLAC 16/44.1". total_frames == Some(28800)/Some(13230)
       for the flacs; duration ≈ 0.3 s (±1 ms).
    2. bit-exact: decode tone_24_96.{wav,flac,m4a} fully as i32 → identical Vec<i32>, length 28800*2; same for
       16_44 wav vs flac; every 24-bit i32 sample has its low 8 bits == 0.
    3. f32 path: frames total 28800 and f32 == (i32 as f64 / 2^31) as f32 for the first 1000 samples.
    4. accurate seek: seek(150 ms) on tone_24_96.flac → returns 150 ms (±1 µs), position_frames() == 14400, and the
       next block's first samples equal the full-decode i32 samples starting at frame 14400 (do this for flac and wav).
    5. seek beyond end (10 s) → Ok, then next_block returns Ok(None) (possibly after zero blocks).
    6. corrupt input: first 4 KiB of tone_24_96.flac followed by 4 KiB of 0x5A bytes then the rest — decoding must
       not panic and must return either Ok blocks or an Err; garbage-only input (8 KiB of 0x5A) → new() Err(Unsupported).
    7. format_khz cases listed above.

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
- [ ] 6.2 bucket scan under optional `SONORUS_S3_PREFIX` (bucket is shared with other files): list_audio, diff by etag/size, fetch head bytes (first 1 MiB, + tail for m4a) → lofty tags.
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

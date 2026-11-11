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

Architecture (threads):
```
controller thread "sonorus-player"  ── owns PlayerState, the OutputSink, emits PlayerEvent
   │  DecoderCmd (std mpsc)          ▲ DecoderMsg (std mpsc)
   ▼                                 │
decode thread "sonorus-decode" (one per loaded track): TrackOpener::open → TrackDecoder → next_block::<i32>
   │  rtrb::Producer<i32>  (left-justified i32 samples, interleaved — lossless up to 32-bit)
   ▼
output callback (NullSink thread now, AAudio in Phase 10): render_f32 / render_i32 from rtrb::Consumer<i32>
shared: Arc<RingStats> (atomics only) — pushed/played sample counters, underruns, volume, decoder_done
```
Rules: a NEW ring + RingStats is created for every load and every seek (sink.stop(); sink.start(new consumer)),
so nothing ever has to flush a ring the callback is reading. The callback never locks, allocates, logs or blocks.
rtrb 0.4.1: `RingBuffer::<T>::new(cap) -> (Producer<T>, Consumer<T>)`; `slots()`; `Producer::write_chunk_uninit(n)
-> Result<WriteChunkUninit, ChunkError>` + `.fill_from_iter(iter) -> usize` (commits what it wrote);
`Consumer::read_chunk(n) -> Result<ReadChunk, ChunkError>` + `.as_slices() -> (&[T], &[T])` + `.commit_all()`.

- [x] **4.1 `crates/engine/src/render.rs`** (AUDIO-THREAD CODE — reviewed line by line)
    ```rust
    pub const RING_SECONDS: usize = 2;
    /// Shared between decode thread (producer side), callback (consumer side) and controller (reader).
    #[derive(Debug)]
    pub struct RingStats {
        pub samples_pushed: AtomicU64,   // decode thread: fetch_add after each committed write
        pub samples_played: AtomicU64,   // callback: fetch_add after each read commit
        pub underruns: AtomicU64,        // callback: +1 per callback that had to zero-fill while !decoder_done
        pub decoder_done: AtomicBool,    // decode thread: true after the last sample is pushed (EOF)
        volume_bits: AtomicU32,          // f32 bits, default 1.0
    }
    impl RingStats {
        pub fn new() -> Self;                       // all zero/false, volume 1.0
        pub fn set_volume(&self, v: f32);           // clamp to 0.0..=1.0, NaN → 1.0; store bits (Relaxed)
        pub fn volume(&self) -> f32;
        pub fn buffered_samples(&self) -> u64;      // pushed.saturating_sub(played) (Acquire loads)
    }
    impl Default for RingStats { fn default() -> Self { Self::new() } }
    /// Capacity = sample_rate * channels * RING_SECONDS (min 4096).
    pub fn new_ring(sample_rate: u32, channels: u16) -> (rtrb::Producer<i32>, rtrb::Consumer<i32>);
    /// Callback renderer for float outputs. `out` is interleaved with `channels` channels.
    /// frames_wanted = out.len() / channels; frames_avail = consumer.slots() / channels (whole frames only);
    /// n = min. read_chunk(n * channels) (cannot fail since n*channels <= slots; on Err treat n = 0);
    /// convert each sample `(s as f32) * (1.0 / 2147483648.0) * volume` into out[..n*channels] walking both
    /// slices; commit_all; zero-fill out[n*channels..]; samples_played.fetch_add(n*channels, Release);
    /// if n < frames_wanted && !decoder_done.load(Acquire) → underruns.fetch_add(1, Relaxed).
    /// channels == 0 → zero-fill and return. Returns n (frames rendered).
    pub fn render_f32(consumer: &mut rtrb::Consumer<i32>, out: &mut [f32], channels: usize, stats: &RingStats) -> usize;
    /// Same contract, integer output, NO volume, samples copied verbatim (bit-perfect path).
    pub fn render_i32(consumer: &mut rtrb::Consumer<i32>, out: &mut [i32], channels: usize, stats: &RingStats) -> usize;
    /// Decode-thread helper: writes as much of `samples` as fits, returns samples written (may be 0).
    /// Writes only whole frames: n = min(samples.len(), producer.slots()) rounded down to a multiple of channels.
    /// Uses write_chunk_uninit(n).fill_from_iter(samples[..n].iter().copied()); then samples_pushed.fetch_add(n, Release).
    pub fn push_frames(producer: &mut rtrb::Producer<i32>, samples: &[i32], channels: usize, stats: &RingStats) -> usize;
    ```
    No allocation, no locks, no `log::`, no panicking index ops in render_*/push_frames (use iterators/zip or
    `get`). Tests: push then render_f32 converts i32::MIN → -1.0 and (1<<30) → 0.5 exactly; volume 0.5 halves;
    render_i32 is bit-exact; partial availability zero-fills the tail and counts one underrun; with decoder_done
    set, zero-fill counts NO underrun; wrap-around (capacity small, push/render repeatedly past capacity) stays
    in order; push_frames never writes a partial frame; buffered_samples tracks push - play.
- [x] **4.2 `crates/engine/src/output.rs`** (parallel with 4.1/4.3; uses render.rs signatures above)
    ```rust
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SampleKind { F32, I32 }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct OutputFormat { pub sample_rate: u32, pub channels: u16, pub sample: SampleKind }
    pub trait OutputSink: Send {
        /// Open/prepare the output for `format`, taking ownership of the ring consumer. Starts PAUSED (silent,
        /// not consuming). Calling start while started = stop() first.
        fn start(&mut self, format: OutputFormat, consumer: rtrb::Consumer<i32>, stats: Arc<RingStats>) -> Result<()>;
        fn play(&mut self) -> Result<()>;   // begin consuming
        fn pause(&mut self) -> Result<()>;  // stop consuming, keep consumer and position
        fn stop(&mut self);                 // close; drops the consumer. Idempotent.
        fn latency(&self) -> Duration;      // best estimate of output latency (NullSink: period)
        fn name(&self) -> &str;
    }
    /// Desktop/test sink: a thread that, while playing, every `period` (10 ms) renders
    /// sample_rate * period frames via render_f32/render_i32 into a buffer preallocated in start(),
    /// paced by an absolute deadline (next += period; sleep until next; if behind by > 100 ms, resync) so
    /// it consumes in real time × `speed`. Controls via Arc<AtomicBool> playing + Arc<AtomicBool> quit;
    /// stop() sets quit and joins the thread.
    pub struct NullSink { /* speed: f32, period: Duration, thread: Option<JoinHandle<()>>, playing, quit */ }
    impl NullSink {
        pub fn new() -> Self;              // speed 1.0
        pub fn with_speed(speed: f32) -> Self; // tests use e.g. 4.0; speed <= 0 or NaN → 1.0
    }
    impl Default for NullSink;  impl Drop for NullSink { stop() }
    ```
    Tests: start+play for 0.25 s at speed 1 with 1 s of pushed samples → samples_played within 20 % of
    0.25 s worth; pause stops consumption (played unchanged over 100 ms); stop is idempotent; start twice OK.
- [x] **4.3 `crates/engine/src/opener.rs`** (parallel)
    ```rust
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct TrackRef { pub id: String, pub key: String, pub size: u64 }
    pub struct OpenedSource { pub source: Box<dyn MediaSource>, pub ext_hint: Option<String>, pub buffer: Option<BufferHandle> }
    pub trait TrackOpener: Send + Sync + 'static {
        /// Blocking; called on the decode thread (never the controller or a tokio worker).
        fn open(&self, track: &TrackRef) -> Result<OpenedSource>;
    }
    /// Lowercased text after the last '.' of the last '/' segment; None if absent.
    pub fn ext_of(key: &str) -> Option<String>;
    pub struct StorageOpener { storage: Storage, rt: tokio::runtime::Handle, config: SourceConfig }
    impl StorageOpener { pub fn new(storage: Storage, rt: tokio::runtime::Handle, config: SourceConfig) -> Self; }
    // open: RangeSource::open(Arc::new(StorageObject::new(&storage, &track.key)), track.size, &rt, config.clone())
    //       → OpenedSource { source: Box::new(src), ext_hint: ext_of(key), buffer: Some(handle) }
    /// Local files: key is a filesystem path relative to `root`. open → std::fs::File (Io error on failure).
    pub struct FileOpener { root: PathBuf }
    impl FileOpener { pub fn new(root: impl Into<PathBuf>) -> Self; }
    ```
    Tests: ext_of cases ("A/b/01 - x.FLAC" → "flac", "noext" → None, "a.b/track" → None); FileOpener opens
    `tests/fixtures/tone_24_96.flac` and TrackDecoder::new on it reports 96000 Hz.
- [x] **4.4 `crates/engine/src/player.rs`** (+ `lib.rs`: `pub mod player;`) (after 4.1–4.3)
    ```rust
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PlayerState { Idle, Loading, Buffering, Playing, Paused }
    #[derive(Debug, Clone, PartialEq)]
    pub enum PlayerEvent {
        StateChanged(PlayerState),
        TrackLoaded { track_id: String, info: StreamInfo },
        Position { track_id: String, position: Duration, duration: Option<Duration> },
        TrackEnded { track_id: String },
        Error { track_id: Option<String>, message: String },
    }
    #[derive(Debug, Clone)]
    pub struct PlayerConfig {
        pub prefill: Duration,            // 1 s: buffered audio required before (re)starting output
        pub position_interval: Duration,  // 250 ms
        pub tick: Duration,               // 20 ms controller poll
        pub output_kind: SampleKind,      // F32 (I32 for bit-perfect later)
    }
    impl Default for PlayerConfig;
    pub struct Player { tx: Sender<Msg>, thread: Option<JoinHandle<()>> }
    impl Player {
        /// Spawns the controller thread "sonorus-player". Events go to the returned receiver
        /// (send errors are ignored if it is dropped).
        pub fn new(sink: Box<dyn OutputSink>, opener: Arc<dyn TrackOpener>, config: PlayerConfig) -> (Player, Receiver<PlayerEvent>);
        pub fn load(&self, track: TrackRef, autoplay: bool);
        pub fn play(&self);  pub fn pause(&self);  pub fn stop(&self);
        pub fn seek(&self, to: Duration);
        pub fn set_volume(&self, volume: f32);
    }
    impl Drop for Player { /* send Shutdown, join the controller thread */ }
    ```
    Private plumbing (one channel into the controller so it can block on a single receiver):
    ```rust
    enum Msg { Cmd(Command), Dec(DecoderMsg) }
    enum Command { Load { track: TrackRef, autoplay: bool }, Play, Pause, Stop, Seek(Duration), SetVolume(f32), Shutdown }
    enum DecoderMsg {                     // every variant carries the session generation; stale ones are ignored
        Opened { generation: u64, info: StreamInfo },
        Seeked { generation: u64, position_frames: u64 },
        Finished { generation: u64 },
        Failed { generation: u64, message: String },
    }
    enum DecoderCmd {                     // controller → decode thread (std mpsc, one channel per session)
        Start { producer: Producer<i32>, stats: Arc<RingStats> },
        Seek { to: Duration, producer: Producer<i32>, stats: Arc<RingStats> },
        Stop,
    }
    struct Session { generation, track: TrackRef, info: Option<StreamInfo>, cmd: Sender<DecoderCmd>,
        stats: Option<Arc<RingStats>>, base_frames: u64, sink_playing: bool, last_underruns: u64,
        pending_seek: Option<Duration>, last_position_emit: Instant }
    ```
    Controller loop: `match rx.recv_timeout(config.tick)` → handle Msg (Disconnected → shut down), then `tick()`.
    `set_state(s)` emits StateChanged only when the state actually changes. Never joins a decode thread (it may
    be blocked in network I/O); it sends `DecoderCmd::Stop` and drops the session — the thread exits on Stop or
    on channel disconnect, and its later messages are ignored by generation.
    - `stop_session()`: sink.stop(); send Stop (ignore error); session = None.
    - Load{track, autoplay}: stop_session(); generation += 1; spawn decode thread "sonorus-decode"; want_play =
      autoplay; session = new; set_state(Loading).
    - Dec Opened (current gen): store info; emit TrackLoaded. If pending_seek → run the seek flow below with it;
      else create ring (`new_ring(rate, channels)`) + `Arc<RingStats>` (set_volume(current volume)); `sink.start(
      OutputFormat { sample_rate, channels, sample: config.output_kind }, consumer, stats.clone())` — Err → emit
      Error, stop_session, set_state(Idle); send `Start { producer, stats }`; base_frames = 0; set_state(if
      want_play { Buffering } else { Paused }).
    - Play: want_play = true; if state == Paused → set_state(Buffering). (Loading: just records want_play.)
    - Pause: want_play = false; if sink_playing → sink.pause(), sink_playing = false; if state is Buffering or
      Playing → set_state(Paused).
    - Stop: stop_session(); set_state(Idle).
    - Seek(to): no session → ignore. info None (still loading) → pending_seek = Some(to). Else: clamp to
      info.duration; sink.stop(); new ring + stats; sink.start(..) (Err → as above); send `Seek { to, producer,
      stats }`; base_frames = round(to_secs * rate); sink_playing = false; last_underruns = 0; set_state(if want_play
      { Buffering } else { Paused }); emit Position immediately.
    - Dec Seeked (current gen): base_frames = position_frames.
    - Dec Finished: nothing (stats.decoder_done is already set by the decode thread).
    - Dec Failed (current gen): emit Error { Some(track.id), message }; stop_session(); set_state(Idle).
    - SetVolume(v): volume = v; if stats → stats.set_volume(v).
    - Shutdown: stop_session(); exit loop.
    `tick()` (only when the session has stats):
    - frames_buffered = stats.buffered_samples() / channels; prefill_frames = prefill_secs * rate; done = decoder_done.
    - Buffering && want_play && (frames_buffered >= prefill_frames || done) → sink.play() (Err → Error + stop +
      Idle), sink_playing = true, set_state(Playing).
    - Playing: u = stats.underruns; if u > last_underruns { last_underruns = u; if !done { sink.pause();
      sink_playing = false; set_state(Buffering) } }. If done && stats.buffered_samples() == 0 → emit final
      Position, emit TrackEnded { id }, stop_session(), set_state(Idle).
    - Position: if state ∈ {Playing, Buffering, Paused} and last_position_emit elapsed ≥ position_interval →
      position = (base_frames + samples_played / channels) / rate; emit Position { id, position, duration }.
    Decode thread `fn decode_main(generation, track, opener, msg_tx: Sender<Msg>, cmd_rx: Receiver<DecoderCmd>)`:
    1. `opener.open(&track)` then `TrackDecoder::new(source, ext_hint)` — Err → send Failed, return.
       send Opened { info }.
    2. Wait (blocking recv) for the first Start or Seek (Seek: run `decoder.seek(to)` and send Seeked) — Stop or
       disconnect → return.
    3. Loop with `block: Vec<i32>` (reused) and `offset`. Commands are handled by one helper
       `fn on_cmd(cmd, &mut decoder, &mut producer, &mut stats, &mut block, &mut offset, &mut eof) -> bool /*keep going*/`:
       Stop → false; Seek → swap producer/stats, block.clear(), offset = 0, eof = false, decoder.seek(to) (Err →
       send Failed, false), send Seeked { position_frames: decoder.position_frames() }; Start → ignore.
       Each iteration: `try_recv` → Ok(cmd) → on_cmd; Disconnected → return.
       If eof → block on `recv()` (Disconnected → return) and on_cmd; continue.
       If offset >= block.len(): `decoder.next_block::<i32>(&mut block)` → Some → offset = 0; None →
       stats.decoder_done.store(true, Release), send Finished, eof = true, continue; Err(e) → send Failed, return.
       `let w = push_frames(&mut producer, block.get(offset..).unwrap_or(&[]), channels, &stats); offset += w;`
       if w == 0 (ring full) → `recv_timeout(5 ms)`: Ok(cmd) → on_cmd; Timeout → continue; Disconnected → return.
    Known limitation (document in a comment): a seek arriving while next_block is blocked on network waits until
    that read returns (≤ the source's read timeout).
    Tests (FileOpener over tests/fixtures + NullSink::with_speed(4.0), PlayerConfig { prefill: 50 ms, tick: 5 ms,
    position_interval: 20 ms, .. }); collect events with recv_timeout and a 3 s overall deadline:
    1. load("tone_24_96.flac", autoplay) → Loading, TrackLoaded (96000 Hz), Buffering, Playing, TrackEnded, Idle in
       that relative order; last Position before TrackEnded is within 30 ms of 300 ms.
    2. load(autoplay=false) → Paused; no TrackEnded within 300 ms; play() → TrackEnded.
    3. load paused, seek(200 ms) → a Position ≥ 199 ms arrives; play() → TrackEnded; no Position after seek < 199 ms.
    4. load("missing.flac") → Error and state Idle.
    5. stall: a test-only opener wrapping the flac bytes in a reader that sleeps 400 ms once after 24 KiB →
       events contain Playing, later Buffering, later Playing again, then TrackEnded.
    6. Drop the Player mid-playback → returns promptly (< 500 ms), no panic.
- [x] **4.5** Opus review + commit.

## Phase 5 — Gapless + queue

- [x] **5.1 `crates/engine/src/queue.rs`** (+ `lib.rs`: `pub mod queue;`) — pure logic, no threads/IO.
    ```rust
    pub type EntryId = u64;
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct QueueEntry { pub id: EntryId, pub track: TrackRef }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub enum RepeatMode { #[default] Off, All, One }
    pub struct Queue {
        entries: Vec<QueueEntry>,   // "original" order (what shuffle-off restores)
        order: Vec<EntryId>,        // play order; == entries' ids when shuffle is off
        pos: Option<usize>,         // index into `order` of the current entry
        shuffle: bool, repeat: RepeatMode, next_id: EntryId, rng: fastrand::Rng,
    }
    impl Queue {
        pub fn new(seed: u64) -> Self;                       // rng = fastrand::Rng::with_seed(seed); ids start at 1
        /// Replace everything. Empty → pos None, returns None. `start` clamped to len-1. Shuffle on →
        /// order = [start entry] + the others shuffled, pos = 0. Shuffle off → order = entries, pos = start.
        pub fn set_tracks(&mut self, tracks: Vec<TrackRef>, start: usize) -> Option<&QueueEntry>;
        pub fn current(&self) -> Option<&QueueEntry>;
        pub fn current_id(&self) -> Option<EntryId>;
        pub fn current_index(&self) -> Option<usize>;        // == pos
        pub fn entries_in_order(&self) -> Vec<&QueueEntry>;  // play order
        pub fn upcoming(&self) -> Vec<&QueueEntry>;          // play order after pos, no wrap
        pub fn get(&self, id: EntryId) -> Option<&QueueEntry>;
        pub fn len(&self) -> usize;  pub fn is_empty(&self) -> bool;
        /// What plays when the current track finishes by itself. One → current. Off → order[pos+1] or None.
        /// All → order[(pos+1) % len] (a single entry repeats itself). pos None → None.
        pub fn peek_auto_next(&self) -> Option<&QueueEntry>;
        pub fn advance_auto(&mut self) -> Option<&QueueEntry>; // moves pos to peek_auto_next; None → pos unchanged
        /// User "next": order[pos+1]; at the end, All → wrap to order[0], Off/One → None (pos unchanged).
        /// (Repeat One only affects auto-advance, never an explicit skip.)
        pub fn skip_next(&mut self) -> Option<&QueueEntry>;
        /// User "previous": pos-1. At pos 0: All → wrap to last; otherwise stay and return current.
        pub fn skip_previous(&mut self) -> Option<&QueueEntry>;
        pub fn jump_to(&mut self, id: EntryId) -> Option<&QueueEntry>; // unknown id → None, pos unchanged
        /// Add to the end of `order` and `entries`. Returns new ids. Does not change pos (None stays None).
        pub fn append(&mut self, tracks: Vec<TrackRef>) -> Vec<EntryId>;
        /// Insert right after pos in `order` (pos None → same as append). Shuffle off → also insert after
        /// the current entry in `entries`; shuffle on → push to the end of `entries`. Returns new ids.
        pub fn play_next(&mut self, tracks: Vec<TrackRef>) -> Vec<EntryId>;
        /// Remove from both vecs. If it was current: the entry now at the same order index becomes current;
        /// if it was last, the new last; if empty, None. Entries before pos removed → pos -= 1. false if unknown.
        pub fn remove(&mut self, id: EntryId) -> bool;
        /// Move within `order` to `to` (clamped to len-1); pos follows the current entry. Shuffle off → apply the
        /// same move to `entries` so they stay identical. false if unknown id.
        pub fn move_entry(&mut self, id: EntryId, to: usize) -> bool;
        pub fn clear(&mut self);
        /// On: order = [current] + others shuffled (pos = 0); with no current, shuffle all, pos None.
        /// Off: order = entries' ids, pos = index of current. No-op if unchanged.
        pub fn set_shuffle(&mut self, on: bool);
        pub fn shuffle(&self) -> bool;
        pub fn set_repeat(&mut self, mode: RepeatMode);  pub fn repeat(&self) -> RepeatMode;
    }
    ```
    Invariants (assert in tests after every operation via a test helper): `order` is a permutation of the
    entries' ids; shuffle off ⇒ order == entries' ids; pos is None iff (empty or never set) else < len.
    No panics on any input (empty queue, unknown ids, huge indices). Tests: each method incl. edge cases above;
    shuffle with the same seed is deterministic; set_shuffle(true) keeps current at pos 0 and set_shuffle(false)
    restores original order with the same current; Repeat All wraps both directions; One repeats on auto but
    not on skip_next; remove current at end / middle / only entry; move_entry of current keeps it current.

- [x] **5.2 `crates/engine/src/player.rs`: decode-side gapless plumbing** (after 5.1)
    New private items:
    ```rust
    enum DecoderSource { Open(TrackRef), Ready(Box<TrackDecoder>) }   // decode_main entry: open, or use a preloaded decoder
    // DecoderCmd gains:
    Next { entry_id: EntryId, decoder: Box<TrackDecoder> },          // queue for gapless continuation (replaces any queued one)
    CancelNext,                                                      // drop the queued next, if not yet switched
    // DecoderMsg gains:
    Advanced { generation: u64, entry_id: EntryId, info: StreamInfo, boundary_samples: u64 },
    Preloaded { generation: u64, entry_id: EntryId, result: Result<Box<TrackDecoder>, String> },
    ```
    - `decode_main(generation, source: DecoderSource, opener, msg_tx, cmd_rx)`: Open → as today; Ready → skip
      opening, send Opened with `decoder.info()` immediately.
    - `DecodeLoop` gets `next: Option<(EntryId, Box<TrackDecoder>)>`. on_cmd: Next → `self.next = Some(..)`;
      CancelNext → `self.next = None`. Seek keeps `next` untouched.
    - At EOF (`next_block` → Ok(None)): if `self.next.take()` is Some((id, dec)): `boundary =
      stats.samples_pushed.load(Acquire)`; `self.decoder = *dec`; `self.channels` unchanged (controller only hands
      off same-channel-count, same-rate decoders); send `Advanced { generation, entry_id: id, info:
      self.decoder.info().clone(), boundary_samples: boundary }`; block.clear(); offset = 0; continue decoding
      (eof stays false, decoder_done NOT set). Else: today's behaviour (decoder_done, Finished, eof = true).
      Also when `eof` is true and a `Next` arrives (handed off late): switch immediately the same way, first
      `stats.decoder_done.store(false, Release)` (it was set at EOF), set eof = false, and resume. If the controller
      already drained and cold-started the track, this thread has been sent Stop and its messages are stale.
    - `fn preload_main(generation, entry_id, track, opener, msg_tx)`: thread "sonorus-preload": open + TrackDecoder::new
      → send `Preloaded { result: Ok(Box::new(decoder)) }` or `Err(e.to_string())`. Never touches any ring.
    - `TrackDecoder` must be `Send` (it holds Box<dyn FormatReader>/Box<dyn AudioDecoder>, both Send in Symphonia
      0.6); add a compile-time assertion `const _: fn() = || { fn is_send<T: Send>() {} is_send::<TrackDecoder>(); };`.
    Controller changes are task 5.3; in 5.2 only wire the new variants so it compiles: on Advanced/Preloaded the
    controller just logs (log::debug) and drops them, and all existing call sites use `DecoderSource::Open`.
    All existing player tests must still pass unchanged.
- [x] **5.3 `crates/engine/src/player.rs`: queue + gapless controller** (after 5.2)
    Public API (Player methods send Commands; `load(track, autoplay)` becomes `set_queue(vec![track], 0, autoplay)`):
    ```rust
    pub fn set_queue(&self, tracks: Vec<TrackRef>, start: usize, autoplay: bool);
    pub fn append(&self, tracks: Vec<TrackRef>);      pub fn play_next(&self, tracks: Vec<TrackRef>);
    pub fn next(&self);  pub fn previous(&self);      pub fn jump_to(&self, id: EntryId);
    pub fn remove(&self, id: EntryId);                pub fn move_entry(&self, id: EntryId, to: usize);
    pub fn set_shuffle(&self, on: bool);              pub fn set_repeat(&self, mode: RepeatMode);
    // PlayerConfig gains: pub preload_lead: Duration (15 s), pub shuffle_seed: u64 (0 → seed from SystemTime nanos)
    // PlayerEvent gains:
    TrackChanged { entry_id: EntryId, track_id: String },
    QueueChanged { entries: Vec<QueueEntry>, current: Option<EntryId>, shuffle: bool, repeat: RepeatMode },
    ```
    Controller state: `queue: Queue`; `preload: Option<Preload>` with
    `enum PreloadState { Opening, Ready(Box<TrackDecoder>), HandedOff }`, `struct Preload { entry_id, track, state, info: Option<StreamInfo> }`;
    Session gains `entry_id: EntryId`, `offset_samples: u64` (ring sample index where the current track starts;
    position = base_frames + (samples_played - offset_samples) / channels), and
    `boundaries: VecDeque<Boundary { entry_id, track: TrackRef, info: StreamInfo, at_samples: u64 }>`.
    Helpers:
    - `start_entry(entry: QueueEntry, autoplay, seek: Option<Duration>)`: stop_session(); if preload is Ready for
      this entry id → take its decoder and spawn decode_main with `DecoderSource::Ready`, else `Open(track)`;
      preload = None; session with entry_id, pending_seek = seek; set_state(Loading); emit TrackChanged.
    - `emit_queue()`: QueueChanged snapshot (entries_in_order cloned).
    - `sync_preload()`: after any queue mutation or track change: let want = queue.peek_auto_next() id. If preload
      exists with a different entry id → if HandedOff send CancelNext; preload = None.
    - `maybe_start_preload()` in tick: if preload None, session opened, want is Some, and (remaining <
      preload_lead, where remaining = duration - position, or duration unknown and decoder_done) → spawn
      preload_main, preload = Opening.
    - Preloaded msg (current generation, entry id == preload's): Err → log::warn, preload = None (fall back to cold
      open later). Ok(dec) → info = dec.info(); if session info has the same sample_rate AND channels → send
      `Next { entry_id, decoder }`, state HandedOff; else state Ready(dec).
    - Advanced msg (current gen): push Boundary { entry_id, track from preload (or queue.get(entry_id)), info,
      at_samples: boundary_samples } onto session.boundaries; preload = None (it has been consumed).
    tick additions (before the existing done/drain check):
    - Boundary crossing: while front boundary b has `samples_played >= b.at_samples`: pop it; emit TrackEnded(old
      track id); if `queue.peek_auto_next().map(|e| e.id) == Some(b.entry_id)` → queue.advance_auto(); session.track =
      b.track; session.entry_id; session.info = b.info; base_frames = 0; offset_samples = b.at_samples; emit
      TrackChanged, TrackLoaded(info), QueueChanged, Position; sync_preload(). Else (queue changed after handoff:
      stale) → advance_auto() and start_entry(that entry or Idle if None, want_play, None); return.
    - Drain end (done && buffered == 0), replacing finish_track: emit final Position + TrackEnded; then
      `queue.advance_auto()` → Some(e) → start_entry(e, true, None) (uses a Ready preload if present = no network
      wait), emit QueueChanged; None → stop_session, set_state(Idle).
    Commands: SetQueue → queue.set_tracks; current Some → start_entry(current, autoplay); else stop + Idle;
    emit_queue. Next → skip_next → Some → start_entry(e, want_play || state was Playing/Buffering) else nothing.
    Previous → if current position > 3 s → seek(0) else skip_previous → start_entry. JumpTo → start_entry.
    Append/PlayNext/Move/SetShuffle/SetRepeat → mutate, emit_queue, sync_preload. Remove(id) → if it was current:
    start_entry(new current) or stop+Idle; emit_queue; sync_preload.
    Seek with non-empty `boundaries` (the decode thread has already moved on to the next track) → start_entry(
    current entry, want_play, Some(to)) instead of seeking in place. Otherwise unchanged; on seek set offset_samples = 0.
    Tests (FileOpener + test config with preload_lead 10 s so preloading starts immediately):
    1. gapless bit-exactness: a test-only `CaptureSink` (OutputSink; its thread drains ALL available samples with
       read_chunk(slots) every 1 ms while playing, appends them to Arc<Mutex<Vec<i32>>>, adds samples_played, never
       zero-fills; output_kind I32). Queue [tone_24_96.flac, tone_24_96.wav] → captured == full decode of the
       flac (i32) followed by full decode of the wav, exactly; events: exactly one Buffering→Playing before the
       first TrackEnded, and no StateChanged(Buffering)/Loading between first TrackEnded and second TrackChanged.
    2. format change: [tone_24_96.flac, tone_16_44.flac] → both TrackEnded, second TrackLoaded reports 44100 Hz.
    3. repeat One on a single track → at least 2 TrackChanged with the same entry id within 3 s; then stop().
    4. next()/previous()/jump_to change TrackChanged ids as the queue dictates; QueueChanged reflects shuffle/repeat.
    5. remove the upcoming entry after handoff (preload HandedOff) → the removed track never gets TrackChanged.
    6. all Phase 4 tests still pass (load() now goes through set_queue).
- [x] **5.4** Opus review + commit.

## Phase 5.5 — Auto-resume after long network drops

Problem: a RangeSource read that gets no data for `read_timeout` (45 s) fails with `io::ErrorKind::TimedOut`;
the player then emits Error, goes Idle and loses the position. Requirement: survive network loss of any length —
keep the position, show Buffering, reopen + seek back, retry with backoff until it works or the user changes track.

- [x] **5.5.1 `crates/engine/src/error.rs` + `decoder.rs`**
  - `impl EngineError { pub fn is_transient(&self) -> bool }`: `Io(e)` with kind TimedOut, Interrupted,
    ConnectionReset, ConnectionAborted or NotConnected → true (NOT BrokenPipe: that means we closed the source);
    `Storage(e)` → `e.is_retryable()`; everything else false.
  - `TrackDecoder::new`: a probe error that is `SymError::IoError(e)` must become `EngineError::Io(e)` (keeps the
    kind, so a timeout while probing is transient); other probe errors stay `Unsupported`. Unit test: a source
    whose `read` returns `Err(TimedOut)` → `new()` returns `Err(e)` with `e.is_transient()`.
- [x] **5.5.2 `crates/engine/src/player.rs`**
  - `DecoderMsg::Failed` gains `transient: bool` (= `err.is_transient()` at every send site; open failures included).
  - `PlayerConfig` gains `recovery_backoff_base: Duration` (2 s) and `recovery_backoff_cap: Duration` (30 s).
  - `PlayerEvent` gains `Recovering { track_id: String, attempt: u32, position: Duration, retry_in: Duration }`.
  - Controller field `recovery: Option<Recovery>`; `struct Recovery { entry_id: EntryId, position: Duration,
    attempt: u32, retry_at: Instant }`.
  - `on_failed(generation, message, transient)`: not current → ignore. `!transient` → `fail(message)` as today
    (also clears `recovery`). Transient:
    - position = `recovery.position` if a recovery for this entry is already running, else the current output
      position (`self.position()`), else the session's `pending_seek`, else 0.
    - attempt = previous attempt + 1 (1 for a fresh failure). delay = 0 for attempt 1, else
      `min(cap, base * 2^(attempt-2))` (saturating).
    - log::warn (message + attempt, no URLs); stop_session() (keeps queue + current entry); recovery = Some{..,
      retry_at: now + delay}; set_state(if want_play { Buffering } else { Paused }); emit Recovering.
  - `tick()`: first thing — if recovery is Some and `now >= retry_at` and there is no session → look up the entry
    (`queue.get(entry_id)`; gone → recovery = None, go Idle) and `start_entry_quiet(entry, want_play,
    Some(position))`, then set_state(Buffering or Paused as above).
  - Split `start_entry` into `start_entry(..)` (announces: emits TrackChanged, clears `recovery`) and
    `start_entry_quiet(..)` (same, but emits no TrackChanged and keeps `recovery`) — recovery must not look like
    a new play to history/UI. Every user action that starts another entry (set_queue, next, previous, jump_to,
    remove-current, stop, drain-end advance, boundary stale restart) goes through the announcing path or
    stop/fail, which clear `recovery`.
  - Recovery succeeded: when the state reaches Playing (prefill met) for the recovered entry → recovery = None.
    While paused, a reopened session that reaches Paused with info loaded also clears it.
  - While recovering with no session, `Pause`/`Play` just update `want_play` and the visible state
    (Paused ↔ Buffering); `Seek(to)` updates `recovery.position` and emits Position.
  - Tests (test-only `FlakyOpener`: the FIRST open of a key returns a reader over tone_24_96.wav whose `read`
    returns `Err(TimedOut)` after 96 KiB; later opens are normal; counts opens. Config: prefill 50 ms, backoff
    base 10 ms, cap 50 ms, NullSink speed 4):
    1. play through a mid-track timeout → events: Recovering{attempt 1}, no Error, no StateChanged(Idle) before the
       final TrackEnded; exactly one TrackChanged for the entry; opens == 2; the first Position after recovery is
       ≥ the last Position before it minus 20 ms (resumed, not restarted).
    2. opener whose first THREE opens fail with a transient error at open time (e.g. returns
       `Err(EngineError::Io(TimedOut))`) → Recovering attempts 1, 2, 3 with non-decreasing retry_in, then plays to
       TrackEnded.
    3. non-transient failure (missing file) still → Error + Idle (existing test keeps passing).
    4. stop() during recovery → Idle, no further opens after 200 ms.
- [x] **5.5.3** Opus review + commit.

## Phase 6 — `sonorus-library`

Opus did: crate skeleton (deps rusqlite 0.41 [bundled, FTS5 compiled in], lofty 0.25, blake3, bytes, tokio,
thiserror, log, sonorus-storage); `StorageConfig.prefix` + `normalize_prefix` (env `SONORUS_S3_PREFIX`);
fixtures `crates/library/tests/fixtures/` laid out like a bucket:
`Artist A/Album One/01 - First Song.flac` (full tags + embedded JPEG cover + LYRICS), `.../02 - Second Song.m4a`
(ALAC, artist "Artist A feat. C", album_artist "Artist A", embedded cover), `Artist B/Album Two/01 - Wave Song.wav`
(INFO tags, no album_artist), `.../03 - Untagged Track.flac` (no tags), `Artist B/Album Two/cover.jpg` (folder art).
lofty 0.25: `lofty::probe::Probe::new(reader).options(ParseOptions::new().read_cover_art(true)).guess_file_type()?.read()?`
→ `TaggedFile`; `use lofty::prelude::{TaggedFileExt, AudioFile, Accessor, ItemKey}`; `primary_tag()`/`first_tag()`;
Accessor `title() artist() album() genre() track() track_total() disk() disk_total() date() -> Option<Timestamp{year:u16,..}>`;
`tag.get_string(ItemKey::AlbumArtist)`, `ItemKey::Lyrics`, `ItemKey::UnsyncLyrics`, `ItemKey::Year`; `tag.pictures()` →
`Picture { pic_type() -> PictureType (CoverFront), mime_type() -> Option<&MimeType> (.as_str()), data() -> &[u8] }`;
`tagged.properties()` → `duration() -> Duration, sample_rate(), bit_depth(), channels(), audio_bitrate()`;
`tagged.file_type()` → `FileType::{Flac, Mp4, Wav, Aiff, ..}`.

- [x] **6.1 `crates/library/src/{error.rs, model.rs, db.rs, query.rs}`** (+ lib.rs mods/re-exports) — SQLite catalogue.
    ```rust
    // error.rs
    #[derive(Debug, thiserror::Error)] pub enum LibraryError {
        #[error("database error: {0}")] Db(#[from] rusqlite::Error),
        #[error("I/O error: {0}")]      Io(#[from] std::io::Error),
        #[error("tag error: {0}")]      Tags(String),
        #[error(transparent)]           Storage(#[from] sonorus_storage::StorageError),
        #[error("not found: {0}")]      NotFound(String),
    }
    pub type Result<T> = std::result::Result<T, LibraryError>;
    // model.rs (all Debug, Clone, PartialEq; Eq where possible)
    pub struct ObjectStamp { pub key: String, pub size: u64, pub etag: Option<String>, pub last_modified: Option<i64> }
    pub struct CoverImage { pub data: Vec<u8>, pub mime: String }
    pub struct TrackMeta { pub title, artist, album_artist, album, genre, lyrics, codec: Option<String>,
        pub track_no, track_total, disc_no, disc_total: Option<u32>, pub year: Option<i32>,
        pub duration_ms: Option<u64>, pub sample_rate: Option<u32>, pub bits_per_sample: Option<u8>,
        pub channels: Option<u8>, pub bitrate_kbps: Option<u32>, pub cover: Option<CoverImage> }   // Default derive
    pub struct Track { pub id: i64, pub key: String, pub size: u64, pub etag: Option<String>, pub title: String,
        pub artist: String, pub album_artist: String, pub album: String, pub album_id: i64, pub artist_id: i64,
        pub track_no: Option<u32>, pub disc_no: Option<u32>, pub year: Option<i32>, pub genre: Option<String>,
        pub duration_ms: Option<u64>, pub sample_rate: Option<u32>, pub bits_per_sample: Option<u8>,
        pub channels: Option<u8>, pub codec: Option<String>, pub cover_id: Option<i64>, pub has_lyrics: bool, pub added_at: i64 }
    pub struct Album { pub id: i64, pub title: String, pub artist: String, pub artist_id: i64, pub year: Option<i32>,
        pub track_count: u32, pub duration_ms: u64, pub cover_id: Option<i64>, pub added_at: i64 }
    pub struct Artist { pub id: i64, pub name: String, pub album_count: u32, pub track_count: u32 }
    pub struct SearchResults { pub tracks: Vec<Track>, pub albums: Vec<Album>, pub artists: Vec<Artist> }
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)] pub enum AlbumSort { #[default] Artist, Title, Year, RecentlyAdded }
    pub struct LibraryStats { pub tracks: u64, pub albums: u64, pub artists: u64, pub total_bytes: u64, pub total_duration_ms: u64 }
    ```
    `db.rs` — `pub struct Library { conn: std::sync::Mutex<rusqlite::Connection>, covers_dir: PathBuf }` (Send + Sync):
    - `open(db_path: &Path, covers_dir: &Path) -> Result<Library>`, `open_in_memory(covers_dir: &Path)`: create covers_dir;
      PRAGMA journal_mode=WAL (skip for memory), foreign_keys=ON, busy_timeout 5000; migrate via `PRAGMA user_version`
      (v0 → v1 creates the schema below in one transaction; unknown newer version → error).
      Schema v1: `artists(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE COLLATE NOCASE)`;
      `covers(id INTEGER PRIMARY KEY, hash TEXT NOT NULL UNIQUE, mime TEXT NOT NULL, file TEXT NOT NULL, bytes INTEGER NOT NULL)`;
      `albums(id INTEGER PRIMARY KEY, title TEXT NOT NULL COLLATE NOCASE, artist_id INTEGER NOT NULL REFERENCES artists(id),
        year INTEGER, cover_id INTEGER REFERENCES covers(id), added_at INTEGER NOT NULL, UNIQUE(title, artist_id))`;
      `tracks(id INTEGER PRIMARY KEY, key TEXT NOT NULL UNIQUE, size INTEGER NOT NULL, etag TEXT, last_modified INTEGER,
        title TEXT NOT NULL, artist TEXT NOT NULL, album_id INTEGER NOT NULL REFERENCES albums(id),
        track_no INTEGER, track_total INTEGER, disc_no INTEGER, disc_total INTEGER, year INTEGER, genre TEXT,
        duration_ms INTEGER, sample_rate INTEGER, bits_per_sample INTEGER, channels INTEGER, bitrate_kbps INTEGER,
        codec TEXT, cover_id INTEGER REFERENCES covers(id), lyrics TEXT, added_at INTEGER NOT NULL, updated_at INTEGER NOT NULL)`;
      indexes on tracks(album_id), albums(artist_id); `CREATE VIRTUAL TABLE track_fts USING fts5(title, artist, album,
        album_artist, tokenize = 'unicode61 remove_diacritics 2')` with rowid = tracks.id.
    - Resolution rules (pure helpers in db.rs, unit-tested): `title` = meta.title (trimmed, non-empty) else file stem with a
      leading track number removed (`"01 - x"`, `"01. x"`, `"01 x"`, `"1-02 x"`) else the stem; `track_no` = meta else that
      leading number (for "1-02" → disc 1, track 2); `artist` = meta.artist else meta.album_artist else "Unknown Artist";
      album artist = meta.album_artist else meta.artist else "Unknown Artist"; `album` = meta.album else the key's parent
      folder name else "Unknown Album". Albums group by (album title, album artist), case-insensitive.
    - `stamps(&self) -> Result<HashMap<String, ObjectStamp>>` (key → stamp) for the scan diff.
    - `upsert_track(&self, stamp: &ObjectStamp, meta: &TrackMeta, cover_id: Option<i64>) -> Result<i64>`: one transaction:
      get-or-create album artist + album (album.year = first non-null; album.cover_id set if NULL and cover_id is Some);
      insert or update the track by key (keep `added_at` on update, set `updated_at` = now); rewrite its track_fts row
      (delete + insert with album_artist). Returns track id.
    - `remove_tracks(&self, keys: &[String]) -> Result<usize>`: delete tracks + fts rows in one transaction.
    - `store_cover(&self, data: &[u8], mime: &str) -> Result<i64>`: hash = blake3 hex; existing hash → its id; else write
      `covers_dir/<hash>.<ext>` (ext from mime: jpeg→jpg, png→png, else bin) via temp file + rename, insert row.
    - `prune(&self) -> Result<PruneStats { albums, artists, covers }>`: delete albums with no tracks, artists with no albums,
      covers referenced by no track and no album (and delete their files; a missing file is fine).
    `query.rs` — `impl Library`:
    - `track(id) -> Result<Track>` (NotFound), `tracks_by_ids(&[i64]) -> Result<Vec<Track>>` (input order, missing skipped),
      `track_by_key(&str) -> Result<Option<Track>>`, `lyrics(track_id) -> Result<Option<String>>`.
    - `albums(sort: AlbumSort, limit: u32, offset: u32) -> Result<Vec<Album>>` (Artist: artist name, year, title; Title: title;
      Year: year DESC NULLS LAST, title; RecentlyAdded: added_at DESC, id DESC); `album(id) -> Result<Album>`;
      `album_tracks(album_id) -> Result<Vec<Track>>` ordered by disc_no NULLS LAST, track_no NULLS LAST, title.
    - `artists(limit, offset) -> Result<Vec<Artist>>` by name; `artist(id)`; `artist_albums(artist_id) -> Vec<Album>` by year, title.
    - `search(query: &str, limit: u32) -> Result<SearchResults>`: split on whitespace, drop FTS-special chars (keep
      alphanumerics incl. unicode, `'`), each token → `"token"*`, joined with AND (empty → empty results); tracks via
      `track_fts MATCH ? ORDER BY bm25(track_fts) LIMIT ?`; albums and artists: `title/name LIKE '%' || ? || '%'` per token
      (all tokens must match), limited, escaping `%`/`_`.
    - `cover_path(cover_id) -> Result<Option<PathBuf>>`; `stats() -> Result<LibraryStats>`.
    Track rows are built by one shared `fn row_to_track(&Row)` over a single SELECT joining albums + artists.
    Tests (in-memory DB + tempdir covers dir under `std::env::temp_dir()/sonorus-test-<random>`): resolution helpers;
    upsert creates artist/album once for two tracks of the same album (case differences in album name still group);
    update keeps added_at and id; album_tracks ordering; remove + prune cleans orphans and cover files; store_cover dedupes;
    search: prefix ("firs" finds "First Song"), diacritics ("beyonce" finds "Beyoncé"), multi-token AND, special chars
    (`"`, `*`, `(`) don't error; albums sorts; stats.
- [x] **6.2 `crates/library/src/{remote.rs, tags.rs}`** (parallel with 6.1; uses only model.rs types — 6.1 owns model.rs,
    so 6.2 must not edit it; if model.rs is not there yet, wait and retry).
    - `remote.rs`: `pub struct RemoteReader { storage: Storage, key: String, size: u64, rt: tokio::runtime::Handle,
      blocks: BTreeMap<u64, Bytes>, pos: u64, block_size: u64 /*256 KiB*/, fetched: u64, max_fetch: u64 /*32 MiB*/ }`;
      `new(storage, key, size, rt) -> Self`; `fetched_bytes()`; `impl Read + Seek`. Read: block = pos / block_size; on miss
      `rt.block_on(storage.get_range(key, block*bs, min(bs, size - block*bs)))` (map errors to io::Error::other; a fetch
      that would exceed max_fetch → io::Error::other("tag read budget exceeded")); copy from the block; keep at most 64
      blocks (evict the lowest index ≠ current). Seek like a file (End relative to size; negative → InvalidInput).
      MUST be used from a blocking thread (spawn_blocking), never a tokio worker — document it.
    - `tags.rs`: `pub fn read_meta<R: Read + Seek>(reader: R, ext_hint: Option<&str>) -> Result<TrackMeta>`: lofty Probe
      with read_cover_art(true); if `guess_file_type` finds nothing and the hint is flac/m4a/wav/aif/aiff use
      `set_file_type`; tag = primary_tag().or(first_tag()); fill TrackMeta (strings trimmed, empty → None; year from
      date().year or ItemKey::Year parsed leading 4 digits; lyrics from Lyrics else UnsyncLyrics); properties →
      duration_ms (0 → None), sample_rate, bits_per_sample, channels, bitrate_kbps; codec from file_type: Flac→"FLAC",
      Mp4→"ALAC", Wav→"WAV", Aiff→"AIFF", other →
      Debug name uppercased; cover = CoverFront picture else first picture → CoverImage { data, mime (default
      "image/jpeg") }. lofty errors → LibraryError::Tags(e.to_string()).
      `pub fn folder_cover_key(audio_key: &str, all_keys: &HashSet<String>) -> Option<String>`: same folder, first existing
      of cover.jpg, cover.png, folder.jpg, folder.png, front.jpg, front.png (case-insensitive match against the set).
    Tests: read_meta on each fixture via `std::fs::File` (all four: titles, artists, album_artist, track/disc numbers,
    year 2021, genre, lyrics present on the flac, sample_rate/bits/channels, duration ≈ 300 ms, codecs FLAC/ALAC/WAV/FLAC,
    covers present on flac + m4a with mime image/jpeg, absent on wav/untagged); folder_cover_key cases.
- [x] **6.3 `crates/library/src/scan.rs`** + `examples/scan.rs` (after 6.1 + 6.2)
    ```rust
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct ScanProgress { pub total: u32, pub done: u32, pub added: u32, pub updated: u32, pub removed: u32, pub failed: u32 }
    #[derive(Debug, Clone)] pub struct ScanFailure { pub key: String, pub error: String }
    pub struct ScanReport { pub progress: ScanProgress, pub failures: Vec<ScanFailure>, pub prune: PruneStats }
    pub trait MetaSource: Send + Sync + 'static {      // lets tests scan a local folder instead of the bucket
        fn list(&self, prefix: &str) -> BoxFuture<'_, Result<Vec<ObjectStamp>>>;   // ALL objects (audio + images)
        fn read_meta(&self, stamp: &ObjectStamp) -> BoxFuture<'_, Result<TrackMeta>>;
        fn get_bytes(&self, key: &str) -> BoxFuture<'_, Result<Vec<u8>>>;          // folder covers
    }
    pub struct BucketSource { storage: Storage, rt: Handle }   // read_meta = spawn_blocking(RemoteReader + tags::read_meta)
    pub struct DirSource { root: PathBuf }                      // keys are '/'-joined paths relative to root
    pub async fn scan(library: &Library, source: &dyn MetaSource, prefix: &str, concurrency: usize,
                      on_progress: impl FnMut(&ScanProgress) + Send) -> Result<ScanReport>;
    ```
    scan: list → audio stamps (`sonorus_storage::is_audio_key`) + key set; diff with `library.stamps()` by key: new →
    add, size or etag differs → update, DB keys not listed → remove (`remove_tracks`). Read metadata for add/update with up
    to `concurrency` (min 1) concurrent `read_meta` futures (tokio::task::JoinSet over Arc'd source, or
    `futures`-free manual pool); for each result in completion order: cover = embedded → store_cover, else folder cover
    (cached per folder key: fetch once via get_bytes, store_cover once) → `upsert_track`; failures counted + collected
    (never abort the scan); on_progress after every file and at the end. Finally prune(). Library DB work happens on the
    calling task (it is quick); only read_meta runs concurrently.
    `examples/scan.rs`: dotenv, StorageConfig::from_env, Storage::new, Library::open("sonorus.db", "covers"), scan with
    BucketSource, prefix = config.prefix, concurrency 4, print progress lines and a final stats() summary + failures.
    Tests (DirSource over the fixtures dir, temp DB): first scan adds 4, albums = 2 ("Album One" by Artist A with 2
    tracks, cover from embedded art; "Album Two" by Artist B with 2 tracks — wav + untagged), untagged track titled
    "Untagged Track" #3 in "Album Two" (folder name) with the folder cover.jpg; second scan → 0 added/updated/removed;
    after copying the fixtures to a temp dir and deleting one file + changing another's size → removed 1, updated 1;
    search("wave") finds the wav; prune removes nothing when nothing is orphaned.
- [x] **6.4** Opus review + commit. Review fixes: store_cover re-creates a missing file; album/artist search via FTS column filters (diacritic-insensitive); untagged files take the artist from the grandparent folder (`Artist/Album/track`); a prefix-limited scan only removes tracks under that prefix; scan takes `Arc<dyn MetaSource>` (JoinSet needs `'static`).

## Phase 7 — Playlists, favorites, history + bucket sync (crate `sonorus-library`)

Opus did: workspace deps serde (derive) + serde_json (+ fastrand in library); schema **v2** in `db.rs` (`SCHEMA_V2`,
migration loop v1→v2): `playlists(id, uuid UNIQUE, name, created_at, updated_at, deleted)`,
`playlist_items(id, playlist_id → playlists ON DELETE CASCADE, position, track_key)`,
`favorites(track_key PK, favorite, updated_at)`, `history(id, track_key, played_at, played_ms)`. User data is keyed by
**object key** (stable across rescans/devices), never by track id; its timestamps are **Unix ms**.
`db.rs`: `pub(crate) fn now_ms() -> i64`, `pub(crate) fn next_stamp(prev) -> i64` (= max(now, prev+1); use it for
every user edit so LWW always sees edits as newer). `LibraryError::Sync(String)`. model.rs: `Playlist`,
`PlaylistEntry{position, track_key, track: Option<Track>}`, serde `PlaylistRecord{uuid, name, created_at, updated_at,
deleted, tracks: Vec<String>}`, `FavoriteRecord{track_key, favorite, updated_at}`. Stub modules playlists/favorites/
history/sync. Tracks are resolved from keys through `TRACK_SELECT` (query.rs; make it `pub(crate)` with
`row_to_track`).

- [x] **7.1 `playlists.rs`** (`impl Library`): `create_playlist(name) -> Result<Playlist>` (uuid = 32 hex chars from two
  `fastrand::u64(..)`, trimmed name, empty → "New Playlist"), `rename_playlist(id, name)`, `delete_playlist(id)`
  (tombstone: deleted = 1, items removed, updated_at bumped), `playlists() -> Vec<Playlist>` (not deleted, by name
  NOCASE then id), `playlist(id) -> Playlist` (NotFound if missing/deleted), `playlist_entries(id) -> Vec<PlaylistEntry>`
  (position order, LEFT JOIN tracks by key), `add_to_playlist(id, &[String])` (append; duplicates allowed),
  `remove_from_playlist(id, position)`, `move_in_playlist(id, from, to)` (Vec::remove/insert semantics),
  `playlist_records() -> Vec<PlaylistRecord>` (ALL incl. tombstones, sorted by uuid),
  `apply_playlist_records(&[PlaylistRecord]) -> Result<u32>` (per uuid: insert if unknown (tombstones too); overwrite
  name/deleted/updated_at/items only if incoming updated_at > local; never re-stamp; returns rows changed).
  Every mutation: one transaction, rewrite the whole item list (delete + reinsert 0..n), updated_at = next_stamp(old);
  mutating a deleted or missing playlist → NotFound; out-of-range position → NotFound.
- [x] **7.2 `favorites.rs` + `history.rs`** (`impl Library`): `set_favorite(key, bool)` (upsert, next_stamp),
  `is_favorite(key) -> bool`, `favorite_keys() -> HashSet<String>`, `favorites() -> Vec<Track>` (catalogued only,
  newest first), `favorite_records()` (all rows incl. favorite = false, sorted by key), `apply_favorite_records(&[..])
  -> Result<u32>` (LWW like playlists). History: `record_play(key, played_ms: u64)` (played_at = now_ms; then trim to
  the newest `HISTORY_LIMIT = 5000` rows), `recently_played(limit) -> Vec<Track>` (distinct key by latest play,
  catalogued only), `recent_albums(limit) -> Vec<Album>` (distinct album by latest play), `clear_history()`.
  History is device-local (not synced). The caller decides when a play counts (Phase 8/12).
- [x] **7.3 `sync.rs`**: docs `PlaylistsDoc{version: 1, playlists}`, `FavoritesDoc{version: 1, favorites}` (serde,
  pretty JSON, records sorted). Pure `merge_playlists(local, remote) -> Vec<PlaylistRecord>` /
  `merge_favorites(..)`: union by id; winner = max by (updated_at, then deterministic tie-break: deleted/!favorite
  wins, then name, then tracks) so every device converges. `trait UserDataStore: Send + Sync { get(key) ->
  BoxFuture<Result<Option<Vec<u8>>>>; put(key, Vec<u8>) -> BoxFuture<Result<()>> }`; `BucketStore{storage, prefix}`
  (keys `{prefix}_sonorus/playlists.json`, `{prefix}_sonorus/favorites.json`; NotFound → None; content type
  application/json); `MemoryStore` (pub, for tests and the app's offline mode). `sync_user_data(&Library,
  &dyn UserDataStore) -> Result<SyncReport{playlists_applied, favorites_applied, playlists_pushed, favorites_pushed}>`:
  per doc: get remote → parse (bad JSON or version > 1 → `LibraryError::Sync`, nothing written) → merge with local
  records → `apply_*` → put the merged doc only if it differs from the remote one.
- [x] **7.4** Opus review + commit. Review fix: `apply_playlist_records`/`apply_favorite_records` use the same
  winner rule as the merge (`sync::playlist_key`/`favorite_key`, incl. the equal-timestamp tie-break) instead of
  "strictly newer", otherwise two devices with equal timestamps never converge (test
  `equal_timestamps_converge_across_devices`). sync validates BOTH remote docs before applying anything.

## Phase 8 — `sonorus-core` (UniFFI 0.32, cdylib `libsonorus_core.so`)

Opus did: crate `crates/core` (lib name `sonorus_core`, crate-type cdylib + lib; deps uniffi 0.32, storage, engine,
library, tokio, thiserror, log; android_logger 0.15 on Android), `crates/uniffi-bindgen` (bin, uniffi `cli`),
`crates/core/uniffi.toml` (Kotlin package `dev.constnishant.sonorus.core`), `scripts/gen-kotlin.sh`, lib.rs
(`setup_scaffolding!`, `init_logging()`, `core_version()`). Verified: proc-macro records/objects/errors and
`#[uniffi::export(foreign)]` traits (`Arc<dyn Trait>`) generate Kotlin in library mode; the full library stack
(aws-lc-sys, bundled SQLite) cross-compiles for `aarch64-linux-android` with NDK r28 (clang 34 linker).
Findings: (1) **no ndk-context/JNI needed** — AAudio, logcat and TLS below need no JavaVM, so the JNA/`JNI_OnLoad`
gotcha does not apply. (2) **Android TLS blocker**: rustls-native-certs finds no roots on Android (only a Termux path),
so every HTTPS call would fail → 8.3. API rule: all exported methods are blocking (call from `Dispatchers.IO`); ids are
library i64 ids (engine `TrackRef.id` = id as string); times are ms (u64). Credentials come from Kotlin
(Keystore-backed settings in Phase 15), never logged (`S3Settings` Debug redacts).

- [x] **8.1 `error.rs` + `types.rs`** — `CoreError` (uniffi::Error) + conversions; all records/enums + converters.
- [x] **8.2 `core.rs` + `events.rs`** — `SonorusCore` object, `EventListener` foreign trait, event pump with
  play-time accounting (record_play when listened ≥ min(30 s, duration/2); position deltas > 2 s ignored as seeks).
- [x] **8.3 storage: Android trust store** — on `target_os = "android"` build the SDK HTTP client
  (aws-smithy-http-client, rustls + aws-lc) with `TrustStore::empty()` + the PEM blocks of every file in
  `/apex/com.android.conscrypt/cacerts` (Android 14+) or else `/system/etc/security/cacerts`.
- [x] **8.4** Opus review: host tests (local-dir backend: scan fixtures, query, play with NullSink, events,
  history), Kotlin generation, `aarch64-linux-android` build of sonorus-core; commit. Review fixes: Android CA certs
  are pre-validated with rustls (`tls::is_valid_root`) because the SDK `.expect()`s on bad PEMs at connector build;
  PlayTracker starts a new entry at position 0 (time before the first Position report counted). Release profile:
  thin LTO, 1 CGU, stripped → arm64 `libsonorus_core.so` 15 MB (needs only liblog/libdl/libm/libc).
  NOTE: until Phase 10 the core plays into `NullSink` (silent, real-time paced).

## Phase 9 — Android app (Kotlin + Compose)

Opus did: `android/` Gradle project — wrapper 9.8.2 (daemon JVM 17 via gradle-daemon-jvm.properties), version
catalog (AGP 9.5.1 built-in Kotlin, Kotlin/Compose plugin 2.4.21, Compose BOM 2026.09.00, navigation-compose 2.10.2,
lifecycle 2.11.0, activity-compose 1.13.0, coroutines 1.11.0, Coil 3.6.3, JNA 5.19.1 aar), app id
`dev.constnishant.sonorus`, compileSdk 37 (AndroidX needs it) / targetSdk 36 / minSdk 34, arm64-v8a only.
Per-variant tasks in `app/build.gradle.kts`: `cargoBuild<Variant>` (cargo + NDK r28 clang env → jniLibs; debug uses
cargo profile `android-dev` = release without LTO, release uses `release`) and `uniffiBindgen<Variant>` (library mode
on the built .so → generated Kotlin source dir). Fixes found: Cargo release `strip = "debuginfo"` (not "symbols": the
bindgen reads UniFFI metadata symbols; AGP strips the packaged copy); CoreError fields renamed `message` → `detail`
(clashed with `Throwable.message`). Generated API: `android/app/build/generated/kotlin/uniffiBindgenDebug/dev/
constnishant/sonorus/core/sonorus_core.kt` (u32 → `UInt`, u64 → `ULong`, ids `Long`, `CoreException.*(detail)`,
`CoreEvent.*`). Verified on a vivo V2338 (Android 16) against the B2 bucket with prefix `sonorus-test`.
NOTE: audio is silent until Phase 10 (NullSink).

- [x] **9.1 data layer** (`data/`): `BucketSettingsStore` (AndroidKeyStore AES-256-GCM for key id + secret; prefs for
  the rest), `SonorusRepository` (core created on IO, `call {}` helper, fire-and-forget player commands, error flow,
  library-changed flow), `CoreEvents: EventListener` → `StateFlow<PlaybackUi>` / `StateFlow<ScanUi>`.
- [x] **9.2 shell**: theme (dark-first Material 3), navigation (bottom bar Home / Library / Search, routes for album,
  artist, playlist, now playing, settings), mini player, Now Playing screen, Settings (bucket form, scan, sync).
- [x] **9.3 browse screens**: Home (recently played, recent albums), Library tabs (Albums grid, Artists, Playlists),
  Album, Artist, Playlist detail, Search; track rows with favorite toggle and "play next / add to queue / add to
  playlist" menu.
- [x] **9.4** Opus review + on-device test + commit. Fixes from the device run: `SonorusCore::check_backend()`
  (one ListObjectsV2 of max 1 key) runs after "Save & connect", so the screen only says "Connected" when the
  credentials work, and says so when the prefix is empty; `StorageError::AccessDenied` / `CoreError::AccessDenied`
  for HTTP 401/403; `data/ErrorText.kt` maps every `CoreException` to short UI text (raw detail goes to logcat);
  debug builds prefill the bucket form from `SONORUS_S3_*` env vars or the repo `.env` (`BuildConfig.DEFAULT_S3_*`,
  empty in release; the debug APK then holds the secret, so it is not for sharing); `data_extraction_rules.xml`
  excludes everything from backup/transfer; Home shelves snap back to the newest item; readable scan summary.

## Phase 10 — AAudio output (ndk-sys)

Opus decided: raw `ndk-sys` 0.6 FFI with our own small wrapper instead of the `ndk` crate (its `AudioStream` Drop
unwraps the close status, and its `AudioFormat` has no PCM_I32 / PCM_I24_PACKED). Module `sonorus_engine::aaudio`:
`mod.rs` (host-testable config/info/negotiation helpers), `stream.rs` + `sink.rs` (`cfg(target_os = "android")`).
The ring consumer lives in a boxed `CallbackState` passed as the data-callback user data and is reclaimed after
`AAudioStream_close` (AAudio guarantees no callback after close), so the sink can reopen without losing it. Seeks
reopen the stream (player calls stop + start); measure on device before optimizing.

- [x] 10.1 `SampleKind::I24Packed` + `render_i24_packed`; `AAudioSink: OutputSink` (float / I32 / I24_PACKED,
  usage MEDIA + content MUSIC, shared+power-saving by default, exclusive forces low-latency, optional device id,
  negotiated format checked after open) and `AAudioHandle` (stream info, config, xruns).
- [x] 10.2 data callback renders from rtrb only (no locks/alloc/logging); underruns via `RingStats`; error callback
  only `try_send`s to the "sonorus-aaudio" supervisor thread, which closes and reopens (5 tries, 100 ms → 1.6 s
  backoff) and resumes if it was playing; a failed reopen parks the state and `play()` retries.
- [x] 10.3 core: AAudio sink on Android (NullSink elsewhere); `set_output_settings` / `output_settings` /
  `output_info`; Kotlin Settings "Output" section (sample format, exclusive, live stream info), persisted.
- [x] 10.4 Opus review + on-device test + commit. Review fix: a failed `AAudioStream_close` now leaks the callback
  boxes instead of freeing them (a callback might still run). Device (vivo V2338, speaker, device 3): 24/96 FLAC
  opens 96000 Hz F32 Shared/PowerSaving, burst 3844, buffer 7688 (mixer resamples to 48 kHz); position tracks wall
  time; seek = close + reopen in ~16 ms, 0 xruns; pause/resume → AudioPlaybackConfiguration paused/started; track
  change 96 k → 44.1 k reopens in ~28 ms; I32 and I24_PACKED open as requested; Exclusive is granted as Shared
  (no MMAP for 44.1 k / I24 on this phone) — Phase 11 territory. Also: Settings shows "AAudio"; re-selecting a
  bottom tab returns to its top screen and the tab stays highlighted on detail screens.
  Closed in Phase 11: the audio policy client attributes show CONTENT_TYPE_MUSIC / USAGE_MEDIA (UNKNOWN is only how
  AudioPlaybackConfiguration reports AAudio players); the reopen after a USB DAC unplug was exercised (11.4).

## Phase 11 — Bit-perfect

Opus decided: the AAudio sink asks a `RoutePlanner` (engine trait; implemented in Kotlin through a UniFFI foreign
trait) for device id + sample kind + sharing before EVERY open (start, seek, track change, reopen after a
disconnect), so the Kotlin side can set the USB mixer attributes for that exact format first; after the open the sink
reports the resulting `SignalPath` back. The planner is called without the sink mutex held (it does binder calls).
Bit-perfect uses integer sample kinds only (no volume is applied on those paths), which covers 11.3's "disables
volume"; ReplayGain/EQ/crossfade do not exist yet. Rubato is deferred: when the DAC has no bit-perfect mode at the
track's rate, the path falls back to the Android mixer (which resamples) and the badge says so. USB DAC detach
pauses playback (pulled forward from 12.2 so audio never jumps to the speaker).

- [x] 11.1 engine: `OutputFormat.source_bits`, `SampleKind::I16` + `render_i16`, `aaudio::{RoutePlanner,
  RouteRequest, RoutePlan, SignalPath, PathMode}`, sink consults the planner on every open and reports `opened`.
- [x] 11.2 core: foreign trait `OutputRouter` + records, `set_output_router`, `signal_path()`.
- [x] 11.3 Kotlin `UsbAudioRouter`: USB DAC via `AudioDeviceCallback`, `getSupportedMixerAttributes` →
  `setPreferredMixerAttributes(USAGE_MEDIA, dac, MIXER_BEHAVIOR_BIT_PERFECT)` matching rate/channels with the
  narrowest lossless integer encoding; fallback bit-perfect → exclusive → mixer; "Bit-perfect to USB DAC" setting;
  signal-path badge in Now Playing; detach → pause.
- [x] 11.4 Opus review + on-device test with the user's USB DAC (bit-perfect open, format switches across tracks,
  unplug → pause + reopen on speaker, replug) + commit.
  Added in review: `RoutePlan.hold` / `RoutePlanInfo.hold` — for 3 s after the DAC disappears the router asks the
  supervisor to reopen the stream stopped, so an unplug can never start the phone speaker even if the pause lands
  late (it relied on the 100 ms retry backoff before). The signal-path dialog names the DAC for default-route
  streams (Android moves them to a replugged DAC without a reopen, so the stream's device id goes stale).
  Device test (vivo V2338, Android 16, KT02H20 HIFI Audio dongle: 16/24/32-bit at 48–384 kHz, no 44.1 k): this
  phone's audio policy has NO bit-perfect mix port, so `getSupportedMixerAttributes` offers no
  MIXER_BEHAVIOR_BIT_PERFECT entry and every track correctly falls back to Mixer. 24/96 plays through
  `hifi_playback` at 96 kHz / PCM_32 to the DAC (native rate, only the stream volume applied); 44.1 k is resampled
  by the mixer (the DAC lacks 44.1 k). Unplug while playing: DISCONNECTED → DAC removed (+24 ms) → pause (+32 ms) →
  held reopen on the speaker (+145 ms), speaker player stays idle, UI shows paused. Replug while paused: Android
  reroutes the paused stream to the new USB port; play resumes on the DAC. Not verifiable here: the bit-perfect
  branch itself (needs a phone whose HAL declares a bit-perfect output, e.g. Pixel 8+ on Android 14+).

## Phase 12 — Background playback
- [x] 12.1 `PlaybackService : MediaSessionService` (foregroundServiceType mediaPlayback), Media3 `SimpleBasePlayer`
  bridging to core. 12.2 audio focus, becoming-noisy + USB detach → pause. 12.3 lock-screen/Bluetooth controls.
  Opus decided: Media3 1.11.1. `SonorusPlayer` mirrors `CoreEvents.playback` (queue → playlist, uid = entry id;
  stop = pause, the queue is kept; SEEK_TO_MEDIA_ITEM → `jumpTo`). Position ticks that stay on the extrapolated line
  (< 1 s drift) are not republished, so the queue is not rebuilt 4×/s. Cover bytes for the current track are loaded
  on IO (downscaled above 300 KB) into an 8-entry LRU. `AudioFocusGuard` lives in the repository and watches core
  state, so play from the app's own screens takes focus too: LOSS → pause + abandon, LOSS_TRANSIENT → pause and
  resume on gain, CAN_DUCK → system ducking, user pause keeps focus 30 s. MainActivity binds the service through a
  MediaController while started. The service is exported (standard for media apps: Assistant, Wear, Auto); limiting
  controllers in `onConnect` is a possible hardening later.
  Device test (vivo V2338): playback continues in the background; foreground service with the media notification
  (title, artist, cover); media keys play/pause/next/previous (restart after 3 s, previous at 0, next at the end of
  the queue pauses); YouTube taking focus pauses Sonorus and releases focus; a transient system sound ducks and
  gives focus back. Not tested: wired-headphone becoming-noisy (no wired headset; USB detach covered in 11.4),
  Bluetooth AVRCP, a phone call. Next-track start takes ~3 s for a 96 k file from the bucket (no prefetch yet).

## Phase 13 — Upload
- [x] 13.1 SAF picker → copy-free read via fd; lofty tags in Rust; BLAKE3 hash dedupe.
- [x] 13.2 upload queue table (upload_id, parts done) → resume via list_parts. Key `Artist/Album/NN - Title.ext`.
- [x] 13.3 foreground `UploadService` (dataSync) + progress UI.
  Opus decided: Kotlin detaches the SAF descriptor and the core owns it (`upload_prepare` / `upload_run(job, fd)`);
  the core never inserts catalogue rows, the service runs the incremental scan once the queue drains. Dedupe: BLAKE3
  of the whole file against the `uploads` table (done/queued/uploading) and the `x-amz-meta-blake3` of an object
  already at the target key; a different object there gets ` (2)`, ` (3)`… Key components are sanitized (`/\:*?"<>|`
  and control chars → `_`, 120-byte cap); `D-NN` when disc_total > 1. Storage failures leave the job Failed with its
  upload id, so Retry resumes; "Clear finished" keeps failed jobs (they may own a partial multipart upload, which
  only a retry or a cancel cleans up). `UploadJobInfo.source_uri` lets the service re-open the source after a
  restart; MainActivity.onStart restarts pending jobs (dataSync may only start from the foreground).
  Device test (vivo V2338, B2): 88 MB FLAC 24/96 uploaded in 11 parts to
  `sonorus-test/Anuv Jain/Arz Kiya Hai _ Coke Studio Bharat/01 - … .flac` (`|` sanitized), scanned and played;
  re-adding it after "Clear finished" → "already uploaded" via the bucket hash; a 68 MB FLAC force-stopped at
  16 MiB (2 of 9 parts) and resumed on reopen to done, then scanned. Also fixed: the Up next sheet crashed on open
  (a ULong LazyColumn key is not Bundle-storable).
  Not tested: Android 15 dataSync timeout, a source deleted between prepare and run, cancel mid-upload.

## Phase 14 — Offline cache
Opus decided: streamed chunks are tee'd into a sparse `.part` file through a `CachingObject` wrapping the
`RemoteObject` (RangeSource unchanged); complete entries are read from disk with no network. LRU by mtime with a size
cap, plus a TTL on unpinned entries (default 30 days; `Some(0)` = never). Pinned albums/playlists are never evicted or
expired and are filled by a background download worker gated on network state (no downloads on metered networks
unless allowed). The cache is scoped per bucket (`data_dir/cache/<hash(endpoint|bucket|prefix)>`).
- [x] 14.1 engine: `TrackCache` (LRU, TTL, pins, chunk map), `CachingObject`, `download`, `CachingOpener`.
- [x] 14.2 core: cache settings, pins table (schema v4), download worker + network gating, purge every 6 h,
  bucket-scoped cache dir, events `DownloadProgress` / `DownloadsIdle`.
- [x] 14.3 Kotlin: Download toggle + status on Album/Playlist, Settings → Downloads & cache, NetworkCallback.
- [x] 14.4 device test + commit. Vivo V2338 on Wi-Fi: pinning "Long Tones" downloaded both tracks into
  `files/sonorus/cache/<bucket id>/` as complete `.audio` entries (2 805 174 + 14 189 480 bytes, matching the objects);
  in airplane mode the pinned A440 track played from the cache. Not tested on device: TTL expiry and LRU eviction
  (covered by engine unit tests with back-dated mtimes), metered gating (the user enabled downloads on mobile data).

## Phase 15 — Brand and design system
- [x] Adaptive "resonance rings" icon (gold dot + teal arcs on #0E1014; foreground/background/monochrome, 66dp safe
  zone) in `res/mipmap/` (minSdk 34, so no `-anydpi-v26` qualifier; plain `mipmap-anydpi` is not found by AAPT2),
  notification icon, splash screen (core-splashscreen) + Material3 DayNight window theme. Source SVG in `docs/brand/`.
- [x] Inter 4.1 (bundled, OFL; licence in assets + About → Open-source licenses) typography scale, shape + spacing
  tokens (`Type.kt`, `Shape.kt`, `Dimens.kt`), shared `SonorusChip` / `QualityBadge`.
- [x] All UI strings in `strings.xml` (275 entries incl. plurals). ViewModels that build text are `AndroidViewModel`s;
  `CoreException.userMessage(context, @StringRes fallback)`.
- [x] Settings redesigned (pulled forward from Phase 16 at the user's request): a minimal list of six categories
  (Bucket, Library, Playback, Downloads & cache, Upload music, About) with one-line summaries, each opening its own
  page inside the same route (`AnimatedContent` + `BackHandler`, one `SettingsViewModel`). Shared parts in
  `SettingsParts.kt`: `SettingsRow`, `SettingsSwitch` (whole row toggles), `SettingsChoice` (radio dialog),
  `SettingsGroupLabel`, `SettingsNote`, `SettingsStatus`. Disconnect now asks for confirmation.

## Phase 16 — UI/UX overhaul (artwork-tinted dark)
- [x] Single inset owner: root Scaffold `contentWindowInsets = WindowInsets(0)` + NavHost
  `padding(innerPadding).consumeWindowInsets(innerPadding)`; Scaffold screens inset via TopAppBar, Settings and
  Now Playing via `safeDrawing`. Dark `surface` = `background` (#0E1014) so lists and bars don't band.
- [x] Artwork colors (`ui/components/ArtworkColors.kt`: palette-ktx 1.1.0 on a ~128 px decode, LRU(64), dominant
  darkened to luminance ≤ 0.18, animated). Gradient on Now Playing (+ 48dp blurred cover backdrop), Album and Artist
  headers, mini player tint.
- [x] Now Playing: cover swipe to skip / swipe down to dismiss (axis locked after 16 px), top-bar drag to dismiss,
  AnimatedContent play/pause, haptics, landscape two-pane. Mini player AnimatedVisibility + swipe to skip.
- [x] Queue and playlist: drag to reorder (sh.calvin.reorderable 3.1.0, one move call on drop), swipe left to remove
  with Undo (queue: append + move, new entry id; playlist: add + move). Library tabs on a HorizontalPager; A–Z fast
  scroll on Artists (> 30, matches `COLLATE NOCASE`); pull-to-refresh on Home/Library starts a scan; artist collage.
- [x] Nav transitions (slide+fade 250 ms; Now Playing slides up/down 350 ms); `Role.Button` on every clickable.
  (Settings subpages done in Phase 15.)

## Phase 17 — Playback quality
- [x] Prefetch + gapless were already done in Phase 5 (preload 15 s before the end, same-format hand-off). Added a
  bit-depth guard: `OutputSink::opened_kind()` reports the stream actually open, and a hand-off also needs
  `lossless_for(kind, current_bits, next_bits)` (I16 never takes a 24-bit track; otherwise cold start).
- [x] ReplayGain: library schema v5 (`rg_*` columns + `needs_reread` so the next scan re-reads tags; lofty maps ID3
  TXXX). Engine `gain.rs` (`linear_gain`: Track/Album with fallback, preamp, peak limit, clamp 0..4) applied
  decode-side per decoder (sample-exact at gapless boundaries) only when the open stream is F32; integer paths are
  bit-identical. Core `setReplayGain` / `SignalPathInfo.replayGainDb`; Settings → Playback → Volume leveling; signal
  path "Volume" row.
- [x] Sleep timer: engine `SleepTimer { At, EndOfTrack }` (10 s linear fade via `RingStats.fade` on F32, pause on
  integer; end of track suppresses the hand-off and loads the next entry paused). Core `setSleepTimer` /
  `sleepTimer()` / `CoreEvent.SleepTimerChanged`; Now Playing bedtime button, picker sheet and countdown chip.

## Phase 18 — Lyrics
- [x] LRC parser (`crates/library/src/lrc.rs`: multi-timestamp lines, `[offset:]`, metadata tags, enhanced `<..>`
  word stamps stripped, BOM/CRLF, empty timed lines kept as instrumental gaps).
- [x] `.lrc` sidecars (same folder + stem) in schema v6 `lyrics_sidecars`, fetched only when new/changed (≤ 512 KiB,
  UTF-8 then Latin-1), removed with the track or the object. Preference: synced sidecar > synced embedded > plain.
  Core `trackLyrics(trackId) -> LyricsInfo?` (replaces `lyrics()`); `hasLyrics` covers sidecars.
- [x] Now Playing lyrics pane (`LyricsPane.kt`): replaces the cover (portrait) / left pane (landscape), current line
  highlighted and centered with frame-interpolated position, manual scroll pauses auto-centering 4 s, tap to seek,
  plain-text fallback. A track without lyrics always shows its cover.
- [x] Device check: sidecar picked up by the scan; auto-centering fixed (it ignored the half-viewport top padding;
  now one `animateScrollToItem(index, scrollOffset = itemHeight / 2)`). Test data: `sonorus-test/Test Tones/Long
  Tones/01 - A440 Hi-Res.lrc` (45 lines, 4 s apart); "First Song" embeds a one-line LRC.

## Phase 19 — Discovery and stats
- [ ] Smart playlists (most played, recently added, hi-res only, never played, favorites); stats screen from
  `history`; recent searches + format filters; folder view.

## Phase 20 — Bucket manager (CRUD, confined to the configured prefix)
- [ ] storage: `list_folder` (delimiter), `copy_object`, batch `delete_objects`, small `put_object`.
- [ ] core: browse, rename/move (copy → verify → delete; catalogue key updated in place so favorites/playlists/
  history survive; cache entry renamed/dropped), delete objects/folders (catalogue + cache + upload rows purged),
  edit tags (download → lofty write → re-upload → rescan), set album cover; prefix guard on every key.
- [ ] Android "Manage bucket": breadcrumb browser, multi-select, rename/move/edit tags/delete with confirmations
  (typed name for folder deletes), "Upload here", entry points from Settings and long-press menus.

## Phase 21 — CI/CD and releases (GitHub Actions)
- [ ] Prerequisite: GitHub repo as `origin` (user creates it; push only with approval) after a full-history
  secrets scan.
- [ ] Conventional Commits from here on; `v0.13.0` anchor tag; versionName from the latest tag, versionCode =
  major·1e6 + minor·1e3 + patch; release-please PR → tag + GitHub Release with CHANGELOG.
- [ ] `ci.yml` (Rust fmt/clippy/test with a pinned `rust-toolchain.toml`; Android assembleDebug + lint with NDK
  28.2.13676358); `release.yml` on release published: R8 release APK + AAB signed from secrets
  (`ANDROID_KEYSTORE_B64` + 3 passwords/alias), credential-leak check on the dex, assets + SHA256SUMS attached.

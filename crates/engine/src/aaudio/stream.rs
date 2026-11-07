//! Minimal safe wrapper over one AAudio output stream (Android only).
//!
//! Ownership model:
//! - `Stream::open` leaks two boxes into raw pointers handed to AAudio as callback user data:
//!   the [`CallbackState`] (consumed by the data callback) and the [`ErrorContext`] (read by the
//!   error callback).
//! - They are reclaimed only in `Stream::close` (also run by `Drop`), after
//!   `AAudioStream_close` has returned OK. AAudio guarantees that no callback runs after a
//!   successful close, so nothing else can reach the boxes at that point. If close fails, the
//!   boxes are leaked on purpose: a callback might still run, and freeing them would be a
//!   use-after-free. Losing one ring consumer is the lesser harm.

use std::ffi::{CStr, c_void};
use std::mem::ManuallyDrop;
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::mpsc::SyncSender;

use ndk_sys::{
    AAUDIO_CALLBACK_RESULT_CONTINUE, AAUDIO_CONTENT_TYPE_MUSIC, AAUDIO_DIRECTION_OUTPUT,
    AAUDIO_FORMAT_PCM_FLOAT, AAUDIO_FORMAT_PCM_I16, AAUDIO_FORMAT_PCM_I24_PACKED,
    AAUDIO_FORMAT_PCM_I32, AAUDIO_OK, AAUDIO_PERFORMANCE_MODE_LOW_LATENCY,
    AAUDIO_PERFORMANCE_MODE_NONE, AAUDIO_PERFORMANCE_MODE_POWER_SAVING,
    AAUDIO_SHARING_MODE_EXCLUSIVE, AAUDIO_SHARING_MODE_SHARED, AAUDIO_UNSPECIFIED,
    AAUDIO_USAGE_MEDIA, AAudio_convertResultToText, AAudio_createStreamBuilder, AAudioStream,
    AAudioStream_close, AAudioStream_getBufferSizeInFrames, AAudioStream_getChannelCount,
    AAudioStream_getDeviceId, AAudioStream_getFormat, AAudioStream_getFramesPerBurst,
    AAudioStream_getPerformanceMode, AAudioStream_getSampleRate, AAudioStream_getSharingMode,
    AAudioStream_getXRunCount, AAudioStream_requestPause, AAudioStream_requestStart,
    AAudioStream_requestStop, AAudioStreamBuilder, AAudioStreamBuilder_delete,
    AAudioStreamBuilder_openStream, AAudioStreamBuilder_setChannelCount,
    AAudioStreamBuilder_setContentType, AAudioStreamBuilder_setDataCallback,
    AAudioStreamBuilder_setDeviceId, AAudioStreamBuilder_setDirection,
    AAudioStreamBuilder_setErrorCallback, AAudioStreamBuilder_setFormat,
    AAudioStreamBuilder_setPerformanceMode, AAudioStreamBuilder_setSampleRate,
    AAudioStreamBuilder_setSharingMode, AAudioStreamBuilder_setUsage,
};

use super::{Performance, RoutePlan, Sharing, StreamInfo, check_negotiated};
use crate::output::{OutputFormat, SampleKind};
use crate::render::{self, RingStats};

/// Everything the data callback needs. Owned by the callback while the stream is open.
pub(crate) struct CallbackState {
    /// Ring consumer of left-justified `i32` samples.
    pub consumer: rtrb::Consumer<i32>,
    /// Shared ring statistics (underruns, samples played).
    pub stats: Arc<RingStats>,
    /// Interleaved channel count of the opened stream.
    pub channels: usize,
    /// Sample representation of the opened stream.
    pub sample: SampleKind,
}

/// Context for the error callback: which open generation failed, and where to report it.
pub(crate) struct ErrorContext {
    /// Generation of the stream this context belongs to.
    pub generation: u64,
    /// Channel to the supervisor thread. The error callback only uses `try_send`.
    pub tx: SyncSender<SupervisorMsg>,
}

/// Messages to the supervisor thread.
pub(crate) enum SupervisorMsg {
    /// The stream reported an error (typically a disconnect) while it was of `generation`.
    Disconnected { generation: u64, error: i32 },
    /// The sink is being dropped; the supervisor exits.
    Quit,
}

/// An open AAudio output stream. Closed by [`Stream::close`] or on drop.
pub(crate) struct Stream {
    ptr: NonNull<AAudioStream>,
    cb: *mut CallbackState,
    err: *mut ErrorContext,
}

// SAFETY: the raw pointers are only dereferenced by the data/error callbacks (which AAudio runs on
// its own threads while the stream is open) and by `close`, which runs on the owner after AAudio
// has stopped calling them. The owner never uses `cb`/`err` while the callbacks may run, so moving
// the `Stream` to another thread does not create shared mutable access.
unsafe impl Send for Stream {}

impl Stream {
    /// Opens an output stream for `format` (rate and channels) with `plan` (sample kind, sharing,
    /// performance and device), without starting it. The callback renders `plan.sample`.
    ///
    /// On failure the `state` box is returned to the caller so the ring consumer is not lost
    /// (`None` only if the stream opened but then failed to close, see the module docs). On
    /// success the stream owns both boxes until it is closed.
    pub(crate) fn open(
        format: OutputFormat,
        plan: &RoutePlan,
        mut state: Box<CallbackState>,
        err: Box<ErrorContext>,
    ) -> Result<Stream, (Option<Box<CallbackState>>, String)> {
        let rate = match i32::try_from(format.sample_rate) {
            Ok(rate) => rate,
            Err(_) => return Err((Some(state), "sample rate out of range".to_owned())),
        };
        // The ring is left-justified i32 whatever the device format, so any kind can render it.
        state.sample = plan.sample;
        let requested = OutputFormat {
            sample: plan.sample,
            ..format
        };

        let mut builder: *mut AAudioStreamBuilder = ptr::null_mut();
        // SAFETY: `builder` is a valid out-pointer for the call.
        let r = unsafe { AAudio_createStreamBuilder(&mut builder) };
        if r != AAUDIO_OK {
            return Err((Some(state), error_text(r)));
        }
        if builder.is_null() {
            return Err((Some(state), "AAudio returned no stream builder".to_owned()));
        }

        // SAFETY: `builder` is live (created above, deleted below exactly once); each setter only
        // stores a value into the builder.
        unsafe {
            AAudioStreamBuilder_setDirection(builder, AAUDIO_DIRECTION_OUTPUT as i32);
            AAudioStreamBuilder_setSampleRate(builder, rate);
            AAudioStreamBuilder_setChannelCount(builder, i32::from(format.channels));
            AAudioStreamBuilder_setFormat(builder, format_code(plan.sample));
            AAudioStreamBuilder_setSharingMode(builder, sharing_code(plan.sharing));
            AAudioStreamBuilder_setPerformanceMode(builder, performance_code(plan.performance));
            AAudioStreamBuilder_setUsage(builder, AAUDIO_USAGE_MEDIA as i32);
            AAudioStreamBuilder_setContentType(builder, AAUDIO_CONTENT_TYPE_MUSIC as i32);
            AAudioStreamBuilder_setDeviceId(
                builder,
                plan.device_id.unwrap_or(AAUDIO_UNSPECIFIED as i32),
            );
        }

        let cb = Box::into_raw(state);
        let err_raw = Box::into_raw(err);
        // SAFETY: the user-data pointers come from `Box::into_raw` above. They stay valid until
        // `close` reclaims them, or until the open-failure path below reclaims them.
        unsafe {
            AAudioStreamBuilder_setDataCallback(builder, Some(data_callback), cb as *mut c_void);
            AAudioStreamBuilder_setErrorCallback(
                builder,
                Some(error_callback),
                err_raw as *mut c_void,
            );
        }

        let mut stream: *mut AAudioStream = ptr::null_mut();
        // SAFETY: `builder` is live and `stream` is a valid out-pointer.
        let r = unsafe { AAudioStreamBuilder_openStream(builder, &mut stream) };
        // SAFETY: the builder is not used after this point. Its delete result is ignored.
        unsafe {
            AAudioStreamBuilder_delete(builder);
        }

        let opened = if r == AAUDIO_OK {
            NonNull::new(stream)
        } else {
            None
        };
        let Some(ptr) = opened else {
            let msg = if r == AAUDIO_OK {
                "AAudio returned no stream".to_owned()
            } else {
                error_text(r)
            };
            // SAFETY: openStream failed, so AAudio holds no stream that could call the callbacks.
            // Both pointers came from `Box::into_raw` above and are reclaimed exactly once here.
            let state = unsafe { Box::from_raw(cb) };
            // SAFETY: as above.
            drop(unsafe { Box::from_raw(err_raw) });
            return Err((Some(state), msg));
        };

        let stream = Stream {
            ptr,
            cb,
            err: err_raw,
        };

        // SAFETY: `ptr` is the stream just opened; these getters only read its properties.
        let (actual_rate, actual_channels, actual_format) = unsafe {
            (
                AAudioStream_getSampleRate(ptr.as_ptr()),
                AAudioStream_getChannelCount(ptr.as_ptr()),
                AAudioStream_getFormat(ptr.as_ptr()),
            )
        };
        let rate = u32::try_from(actual_rate).unwrap_or(0);
        let channels = u16::try_from(actual_channels).unwrap_or(0);
        match check_negotiated(requested, rate, channels, sample_from_code(actual_format)) {
            Ok(()) => Ok(stream),
            Err(msg) => {
                let state = stream.close();
                Err((state, msg))
            }
        }
    }

    /// Snapshot of the stream's actual properties.
    pub(crate) fn info(&self) -> StreamInfo {
        let p = self.ptr.as_ptr();
        // SAFETY: `ptr` is an open stream owned by `self`; these getters only read properties.
        let (rate, channels, format, sharing, performance, device, burst, buffer) = unsafe {
            (
                AAudioStream_getSampleRate(p),
                AAudioStream_getChannelCount(p),
                AAudioStream_getFormat(p),
                AAudioStream_getSharingMode(p),
                AAudioStream_getPerformanceMode(p),
                AAudioStream_getDeviceId(p),
                AAudioStream_getFramesPerBurst(p),
                AAudioStream_getBufferSizeInFrames(p),
            )
        };
        StreamInfo {
            sample_rate: u32::try_from(rate).unwrap_or(0),
            channels: u16::try_from(channels).unwrap_or(0),
            // `open` only succeeds when the format matched the request, which is a known code,
            // so the fallback is not reached for a stream that was opened by this wrapper.
            sample: sample_from_code(format).unwrap_or(SampleKind::F32),
            sharing: sharing_from_code(sharing),
            performance: performance_from_code(performance),
            device_id: device,
            frames_per_burst: u32::try_from(burst).unwrap_or(0),
            buffer_frames: u32::try_from(buffer).unwrap_or(0),
        }
    }

    /// Xrun count reported by AAudio so far.
    pub(crate) fn xruns(&self) -> i32 {
        // SAFETY: `ptr` is an open stream owned by `self`; the getter only reads a counter.
        unsafe { AAudioStream_getXRunCount(self.ptr.as_ptr()) }
    }

    /// Starts the stream (begins consuming the ring).
    pub(crate) fn request_start(&self) -> Result<(), String> {
        // SAFETY: `ptr` is an open stream owned by `self`.
        let r = unsafe { AAudioStream_requestStart(self.ptr.as_ptr()) };
        status(r)
    }

    /// Pauses the stream (stops consuming, keeps the position).
    pub(crate) fn request_pause(&self) -> Result<(), String> {
        // SAFETY: `ptr` is an open stream owned by `self`.
        let r = unsafe { AAudioStream_requestPause(self.ptr.as_ptr()) };
        status(r)
    }

    /// Stops and closes the stream and returns the callback state (and with it the consumer).
    /// `None` when AAudio failed to close the stream; the state is then leaked (module docs).
    pub(crate) fn close(self) -> Option<Box<CallbackState>> {
        // `ManuallyDrop` keeps `Drop` from closing the stream a second time.
        let mut this = ManuallyDrop::new(self);
        this.close_inner()
    }

    /// Shared close path for `close` and `Drop`. Must be called at most once per stream.
    fn close_inner(&mut self) -> Option<Box<CallbackState>> {
        // SAFETY: `ptr` is an open stream that has not been closed yet (`close` consumes the
        // stream and `Drop` runs once, with `close` suppressed by `ManuallyDrop`).
        let r = unsafe {
            let _ = AAudioStream_requestStop(self.ptr.as_ptr());
            AAudioStream_close(self.ptr.as_ptr())
        };
        if r != AAUDIO_OK {
            log::error!(
                "aaudio: close failed ({}); leaking the callback state",
                error_text(r)
            );
            return None;
        }
        // SAFETY: `AAudioStream_close` returned OK, so AAudio no longer runs the data or error
        // callback and will not run them again. Nothing else holds these pointers, and each was
        // created by `Box::into_raw` in `open` and is reclaimed exactly once, here.
        unsafe {
            drop(Box::from_raw(self.err));
            Some(Box::from_raw(self.cb))
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // The state is dropped here, which drops the ring consumer.
        drop(self.close_inner());
    }
}

/// Data callback: renders the ring into AAudio's buffer. Audio thread: no allocation, locks,
/// logging or panics.
///
/// # Safety
/// Called by AAudio. `user_data` must be the `CallbackState` pointer given to `open`, and
/// `audio_data` must point to `num_frames * channels` samples of the negotiated format.
unsafe extern "C" fn data_callback(
    _stream: *mut AAudioStream,
    user_data: *mut c_void,
    audio_data: *mut c_void,
    num_frames: i32,
) -> i32 {
    let cont = AAUDIO_CALLBACK_RESULT_CONTINUE as i32;
    if user_data.is_null() || audio_data.is_null() {
        return cont;
    }
    let Ok(frames) = usize::try_from(num_frames) else {
        return cont;
    };
    if frames == 0 {
        return cont;
    }

    // SAFETY: `user_data` is the `CallbackState` box pointer, alive until `close` returns. While
    // the stream runs, only this callback touches it (`close` reclaims it only after AAudio has
    // stopped calling back).
    let state = unsafe { &mut *(user_data as *mut CallbackState) };
    let samples = frames.saturating_mul(state.channels);
    match state.sample {
        SampleKind::F32 => {
            // SAFETY: a PCM_FLOAT stream buffer holds `frames * channels` f32 values, valid for
            // the duration of this callback.
            let out = unsafe { std::slice::from_raw_parts_mut(audio_data as *mut f32, samples) };
            render::render_f32(&mut state.consumer, out, state.channels, &state.stats);
        }
        SampleKind::I32 => {
            // SAFETY: a PCM_I32 stream buffer holds `frames * channels` i32 values, valid for
            // the duration of this callback.
            let out = unsafe { std::slice::from_raw_parts_mut(audio_data as *mut i32, samples) };
            render::render_i32(&mut state.consumer, out, state.channels, &state.stats);
        }
        SampleKind::I24Packed => {
            // SAFETY: a PCM_I24_PACKED stream buffer holds `frames * channels` samples of 3 bytes
            // each, valid for the duration of this callback.
            let out = unsafe {
                std::slice::from_raw_parts_mut(audio_data as *mut u8, samples.saturating_mul(3))
            };
            render::render_i24_packed(&mut state.consumer, out, state.channels, &state.stats);
        }
        SampleKind::I16 => {
            // SAFETY: a PCM_I16 stream buffer holds `frames * channels` i16 values, valid for the
            // duration of this callback.
            let out = unsafe { std::slice::from_raw_parts_mut(audio_data as *mut i16, samples) };
            render::render_i16(&mut state.consumer, out, state.channels, &state.stats);
        }
    }
    cont
}

/// Error callback: reports the error to the supervisor. Nothing else is allowed here (AAudio
/// forbids closing the stream from this callback).
///
/// # Safety
/// Called by AAudio. `user_data` must be the `ErrorContext` pointer given to `open`.
unsafe extern "C" fn error_callback(
    _stream: *mut AAudioStream,
    user_data: *mut c_void,
    error: i32,
) {
    if user_data.is_null() {
        return;
    }
    // SAFETY: `user_data` is the `ErrorContext` box pointer, alive until `close` returns. AAudio
    // does not call the error callback after close, and only shared reads happen here.
    let ctx = unsafe { &*(user_data as *const ErrorContext) };
    let _ = ctx.tx.try_send(SupervisorMsg::Disconnected {
        generation: ctx.generation,
        error,
    });
}

/// Text for an AAudio result code (the numeric code is used if AAudio has no text).
pub(crate) fn error_text(code: i32) -> String {
    // SAFETY: AAudio returns a pointer to a static NUL-terminated string, or null.
    let text = unsafe { AAudio_convertResultToText(code) };
    if text.is_null() {
        return format!("error {code}");
    }
    // SAFETY: `text` is non-null and NUL-terminated (see above).
    unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

/// Maps an AAudio result code to `Ok` or its text.
fn status(code: i32) -> Result<(), String> {
    if code == AAUDIO_OK {
        Ok(())
    } else {
        Err(error_text(code))
    }
}

/// AAudio format code for a sample kind.
fn format_code(sample: SampleKind) -> i32 {
    match sample {
        SampleKind::F32 => AAUDIO_FORMAT_PCM_FLOAT,
        SampleKind::I32 => AAUDIO_FORMAT_PCM_I32,
        SampleKind::I24Packed => AAUDIO_FORMAT_PCM_I24_PACKED,
        SampleKind::I16 => AAUDIO_FORMAT_PCM_I16,
    }
}

/// Sample kind for an AAudio format code, `None` if unknown.
fn sample_from_code(code: i32) -> Option<SampleKind> {
    if code == AAUDIO_FORMAT_PCM_FLOAT {
        Some(SampleKind::F32)
    } else if code == AAUDIO_FORMAT_PCM_I32 {
        Some(SampleKind::I32)
    } else if code == AAUDIO_FORMAT_PCM_I24_PACKED {
        Some(SampleKind::I24Packed)
    } else if code == AAUDIO_FORMAT_PCM_I16 {
        Some(SampleKind::I16)
    } else {
        None
    }
}

fn sharing_code(sharing: Sharing) -> i32 {
    match sharing {
        Sharing::Shared => AAUDIO_SHARING_MODE_SHARED as i32,
        Sharing::Exclusive => AAUDIO_SHARING_MODE_EXCLUSIVE as i32,
    }
}

/// Sharing mode from an AAudio code; unknown codes map to `Shared`.
fn sharing_from_code(code: i32) -> Sharing {
    if code == AAUDIO_SHARING_MODE_EXCLUSIVE as i32 {
        Sharing::Exclusive
    } else {
        Sharing::Shared
    }
}

fn performance_code(performance: Performance) -> i32 {
    match performance {
        Performance::None => AAUDIO_PERFORMANCE_MODE_NONE as i32,
        Performance::PowerSaving => AAUDIO_PERFORMANCE_MODE_POWER_SAVING as i32,
        Performance::LowLatency => AAUDIO_PERFORMANCE_MODE_LOW_LATENCY as i32,
    }
}

/// Performance mode from an AAudio code; unknown codes map to `None`.
fn performance_from_code(code: i32) -> Performance {
    if code == AAUDIO_PERFORMANCE_MODE_POWER_SAVING as i32 {
        Performance::PowerSaving
    } else if code == AAUDIO_PERFORMANCE_MODE_LOW_LATENCY as i32 {
        Performance::LowLatency
    } else {
        Performance::None
    }
}

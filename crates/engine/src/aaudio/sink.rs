//! [`OutputSink`] implementation on AAudio (Android only), plus the shared handle the UI uses to
//! read and change the configuration.
//!
//! One mutex guards the session state. The data and error callbacks never touch it. A supervisor
//! thread ("sonorus-aaudio") reopens the stream after an error reported by the error callback.

use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::stream::{CallbackState, ErrorContext, Stream, SupervisorMsg, error_text};
use super::{AAudioConfig, StreamInfo, latency_for};
use crate::error::{EngineError, Result};
use crate::output::{OutputFormat, OutputSink};
use crate::render::RingStats;

/// Capacity of the supervisor channel.
const CHANNEL_CAPACITY: usize = 8;

/// Delay before each reopen attempt (100 ms .. 1.6 s, five attempts).
const RETRY_BACKOFF_MS: [u64; 5] = [100, 200, 400, 800, 1600];

const THREAD_NAME: &str = "sonorus-aaudio";

/// Session state shared by the sink, its handles and the supervisor.
struct Inner {
    config: AAudioConfig,
    format: Option<OutputFormat>,
    stream: Option<Stream>,
    /// Callback state of a stream that is not open (failed open, or waiting for a reopen).
    parked: Option<Box<CallbackState>>,
    playing: bool,
    /// Bumped on every open attempt; error contexts carry the value they were created with.
    generation: u64,
    info: Option<StreamInfo>,
    tx: SyncSender<SupervisorMsg>,
}

fn lock(shared: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Opens the stream for the current format. On success the stream is stored (stopped). On
/// failure the state is parked for a later reopen and the message is returned.
fn open_locked(inner: &mut Inner, state: Box<CallbackState>) -> std::result::Result<(), String> {
    let Some(format) = inner.format else {
        return Err("no output format".to_owned());
    };
    inner.info = None;
    inner.generation = inner.generation.wrapping_add(1);
    let err = Box::new(ErrorContext {
        generation: inner.generation,
        tx: inner.tx.clone(),
    });
    match Stream::open(format, &inner.config, state, err) {
        Ok(stream) => {
            let info = stream.info();
            log::info!(
                "aaudio: opened {} Hz, {} ch, {:?}, {:?}/{:?}, device {}, burst {}, buffer {}",
                info.sample_rate,
                info.channels,
                info.sample,
                info.sharing,
                info.performance,
                info.device_id,
                info.frames_per_burst,
                info.buffer_frames
            );
            inner.info = Some(info);
            inner.stream = Some(stream);
            Ok(())
        }
        Err((state, msg)) => {
            inner.parked = state;
            Err(msg)
        }
    }
}

/// Supervisor loop: reopens the stream after errors until told to quit.
fn supervise(shared: Arc<Mutex<Inner>>, rx: Receiver<SupervisorMsg>) {
    while let Ok(msg) = rx.recv() {
        match msg {
            SupervisorMsg::Quit => return,
            SupervisorMsg::Disconnected { generation, error } => {
                reopen(&shared, generation, error);
            }
        }
    }
}

/// Reopens the stream after an error from `generation`. The lock is released while sleeping.
fn reopen(shared: &Mutex<Inner>, generation: u64, error: i32) {
    log::warn!(
        "aaudio: stream error {error} ({}), reopening",
        error_text(error)
    );
    let mut expected = generation;
    for (attempt, delay) in RETRY_BACKOFF_MS.iter().enumerate() {
        thread::sleep(Duration::from_millis(*delay));

        let mut guard = lock(shared);
        let inner = &mut *guard;
        // Stopped or replaced since the error: nothing left to reopen.
        if inner.generation != expected || (inner.stream.is_none() && inner.parked.is_none()) {
            return;
        }
        let state = match inner.stream.take() {
            Some(stream) => stream.close(),
            None => inner.parked.take(),
        };
        let Some(state) = state else {
            log::error!("aaudio: the ring was lost with the old stream; cannot reopen");
            return;
        };

        match open_locked(inner, state) {
            Ok(()) => {
                if inner.playing
                    && let Some(stream) = inner.stream.as_ref()
                    && let Err(msg) = stream.request_start()
                {
                    log::warn!("aaudio: restart after reopen failed: {msg}");
                }
                return;
            }
            Err(msg) => {
                expected = inner.generation;
                log::warn!(
                    "aaudio: reopen attempt {} of {} failed: {msg}",
                    attempt + 1,
                    RETRY_BACKOFF_MS.len()
                );
            }
        }
    }
    log::error!("aaudio: could not reopen the output; will retry on play");
}

/// AAudio output sink. Owns the supervisor thread; see the module docs.
pub struct AAudioSink {
    shared: Arc<Mutex<Inner>>,
    tx: SyncSender<SupervisorMsg>,
    supervisor: Option<JoinHandle<()>>,
}

impl AAudioSink {
    /// Creates the sink and starts its supervisor thread. No stream is opened until `start`.
    pub fn new(config: AAudioConfig) -> Result<AAudioSink> {
        let (tx, rx) = sync_channel(CHANNEL_CAPACITY);
        let shared = Arc::new(Mutex::new(Inner {
            config,
            format: None,
            stream: None,
            parked: None,
            playing: false,
            generation: 0,
            info: None,
            tx: tx.clone(),
        }));
        let supervisor_shared = Arc::clone(&shared);
        let supervisor = thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || supervise(supervisor_shared, rx))?;
        Ok(AAudioSink {
            shared,
            tx,
            supervisor: Some(supervisor),
        })
    }

    /// Cloneable handle for reading and changing configuration and stream info.
    pub fn handle(&self) -> AAudioHandle {
        AAudioHandle {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl OutputSink for AAudioSink {
    fn start(
        &mut self,
        format: OutputFormat,
        consumer: rtrb::Consumer<i32>,
        stats: Arc<RingStats>,
    ) -> Result<()> {
        self.stop();
        if format.channels == 0 || format.sample_rate == 0 {
            return Err(EngineError::Unsupported(
                "invalid output format".to_string(),
            ));
        }

        let state = Box::new(CallbackState {
            consumer,
            stats,
            channels: usize::from(format.channels),
            sample: format.sample,
        });
        let mut guard = lock(&self.shared);
        let inner = &mut *guard;
        inner.format = Some(format);
        inner.playing = false;
        if let Err(msg) = open_locked(inner, state) {
            inner.parked = None;
            inner.format = None;
            return Err(EngineError::Unsupported(format!("AAudio: {msg}")));
        }
        Ok(())
    }

    fn play(&mut self) -> Result<()> {
        let mut guard = lock(&self.shared);
        let inner = &mut *guard;
        if inner.stream.is_none()
            && inner.format.is_some()
            && let Some(state) = inner.parked.take()
            && let Err(msg) = open_locked(inner, state)
        {
            log::warn!("aaudio: reopen on play failed: {msg}");
        }
        let Some(stream) = inner.stream.as_ref() else {
            return Err(EngineError::Unsupported(
                "AAudio stream is not open".to_string(),
            ));
        };
        stream
            .request_start()
            .map_err(|msg| EngineError::Unsupported(format!("AAudio: {msg}")))?;
        inner.playing = true;
        Ok(())
    }

    fn pause(&mut self) -> Result<()> {
        let mut guard = lock(&self.shared);
        let inner = &mut *guard;
        inner.playing = false;
        if let Some(stream) = inner.stream.as_ref() {
            stream
                .request_pause()
                .map_err(|msg| EngineError::Unsupported(format!("AAudio: {msg}")))?;
        }
        Ok(())
    }

    fn stop(&mut self) {
        let mut guard = lock(&self.shared);
        let inner = &mut *guard;
        if let Some(stream) = inner.stream.take() {
            let xruns = stream.xruns();
            let state = stream.close();
            log::info!("aaudio: closed, xruns {xruns}");
            drop(state);
        }
        inner.parked = None;
        inner.format = None;
        inner.info = None;
        inner.playing = false;
    }

    fn latency(&self) -> Duration {
        let inner = lock(&self.shared);
        inner.info.map_or(Duration::ZERO, |i| {
            latency_for(i.buffer_frames, i.sample_rate)
        })
    }

    fn name(&self) -> &str {
        "aaudio"
    }
}

impl Drop for AAudioSink {
    fn drop(&mut self) {
        self.stop();
        // If the supervisor already exited, the send fails; nothing to do then.
        let _ = self.tx.send(SupervisorMsg::Quit);
        if let Some(handle) = self.supervisor.take() {
            // A panicked supervisor has nothing left to clean up; ignore the join error.
            let _ = handle.join();
        }
    }
}

/// Cloneable handle to the sink's configuration and stream state.
#[derive(Clone)]
pub struct AAudioHandle {
    shared: Arc<Mutex<Inner>>,
}

impl AAudioHandle {
    /// Info of the currently open stream, `None` when closed.
    pub fn stream_info(&self) -> Option<StreamInfo> {
        lock(&self.shared).info
    }

    /// Current configuration.
    pub fn config(&self) -> AAudioConfig {
        lock(&self.shared).config
    }

    /// Stores a new configuration for the next open. Does not reopen a live stream.
    pub fn set_config(&self, config: AAudioConfig) {
        lock(&self.shared).config = config;
    }

    /// Xrun count of the open stream, `None` when closed.
    pub fn xruns(&self) -> Option<i32> {
        lock(&self.shared).stream.as_ref().map(Stream::xruns)
    }
}

//! [`OutputSink`] implementation on AAudio (Android only), plus the shared handle the UI uses to
//! read and change the configuration.
//!
//! One mutex guards the session state. The data and error callbacks never touch it. A supervisor
//! thread ("sonorus-aaudio") reopens the stream after an error reported by the error callback.
//!
//! Every open is two-phase: the [`RoutePlanner`] is asked for a plan without the lock held, then
//! the stream is opened under the lock with that plan. The outcome is stored for
//! [`AAudioHandle::signal_path`] and reported to the planner after the lock is released.

use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::stream::{CallbackState, ErrorContext, Stream, SupervisorMsg, error_text};
use super::{
    AAudioConfig, DefaultPlanner, PathMode, RoutePlan, RoutePlanner, RouteRequest, SignalPath,
    StreamInfo, latency_for, path_mode,
};
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
    /// Chooses the route for each open.
    planner: Arc<dyn RoutePlanner>,
    /// Plan of the current or parked stream.
    plan: Option<RoutePlan>,
    /// Outcome of the last open attempt.
    last_path: Option<SignalPath>,
}

fn lock(shared: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Asks `planner` for the route of `format`. Must be called without the sink lock.
fn plan_route(planner: &dyn RoutePlanner, format: OutputFormat, config: AAudioConfig) -> RoutePlan {
    planner.plan(&RouteRequest {
        sample_rate: format.sample_rate,
        channels: format.channels,
        source_bits: format.source_bits,
        requested: format.sample,
        config,
    })
}

/// Path record for an attempt that did not open a stream.
fn failed_path(
    plan: RoutePlan,
    source_rate: u32,
    source_bits: Option<u32>,
    error: String,
) -> SignalPath {
    SignalPath {
        mode: PathMode::Mixer,
        plan,
        stream: None,
        error: Some(error),
        source_rate,
        source_bits,
    }
}

/// Opens the stream for the session format with `plan`. On success the stream is stored (stopped).
/// On failure the state is parked for a later reopen. The outcome is stored in `last_path` and
/// returned; the caller reports it to the planner after releasing the lock.
fn open_with_plan(inner: &mut Inner, state: Box<CallbackState>, plan: RoutePlan) -> SignalPath {
    inner.info = None;
    inner.plan = Some(plan);
    let source = inner.format;
    let (source_rate, source_bits) = source.map_or((0, None), |f| (f.sample_rate, f.source_bits));
    let path = match source {
        None => {
            inner.parked = Some(state);
            failed_path(
                plan,
                source_rate,
                source_bits,
                "no output format".to_owned(),
            )
        }
        Some(format) => {
            inner.generation = inner.generation.wrapping_add(1);
            log::info!(
                "aaudio: plan device {:?}, {:?}, {:?}/{:?}, bit-perfect {}",
                plan.device_id,
                plan.sample,
                plan.sharing,
                plan.performance,
                plan.bit_perfect
            );
            let err = Box::new(ErrorContext {
                generation: inner.generation,
                tx: inner.tx.clone(),
            });
            match Stream::open(format, &plan, state, err) {
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
                    let mode = path_mode(&plan, &info);
                    inner.info = Some(info);
                    inner.stream = Some(stream);
                    SignalPath {
                        mode,
                        plan,
                        stream: Some(info),
                        error: None,
                        source_rate,
                        source_bits,
                    }
                }
                Err((state, msg)) => {
                    inner.parked = state;
                    failed_path(plan, source_rate, source_bits, msg)
                }
            }
        }
    };
    inner.last_path = Some(path.clone());
    path
}

/// Opens the parked state (after a failed open or reopen) with a fresh plan, if a stream is wanted
/// and not open. The planner runs without the lock.
fn reopen_parked(shared: &Mutex<Inner>) {
    let (planner, format, config) = {
        let inner = lock(shared);
        if inner.stream.is_some() || inner.parked.is_none() {
            return;
        }
        let Some(format) = inner.format else {
            return;
        };
        (Arc::clone(&inner.planner), format, inner.config)
    };
    let plan = plan_route(planner.as_ref(), format, config);
    let path = {
        let mut guard = lock(shared);
        let inner = &mut *guard;
        // Stopped, or reopened by another caller while planning: nothing to do.
        if inner.stream.is_some() || inner.format != Some(format) {
            return;
        }
        let Some(state) = inner.parked.take() else {
            return;
        };
        open_with_plan(inner, state, plan)
    };
    planner.opened(&path);
    if let Some(msg) = &path.error {
        log::warn!("aaudio: reopen on play failed: {msg}");
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

/// Reopens the stream after an error from `generation`. The lock is released while sleeping and
/// while the planner runs. The state is parked during planning so `stop` and `play` see it.
fn reopen(shared: &Mutex<Inner>, generation: u64, error: i32) {
    log::warn!(
        "aaudio: stream error {error} ({}), reopening",
        error_text(error)
    );
    let mut expected = generation;
    for (attempt, delay) in RETRY_BACKOFF_MS.iter().enumerate() {
        thread::sleep(Duration::from_millis(*delay));

        // Phase 1 (locked): close the old stream or take the parked state, and park it again.
        let (planner, format, config) = {
            let mut guard = lock(shared);
            let inner = &mut *guard;
            // Stopped or replaced since the error: nothing left to reopen.
            if inner.generation != expected || (inner.stream.is_none() && inner.parked.is_none()) {
                return;
            }
            let Some(format) = inner.format else {
                return;
            };
            let state = match inner.stream.take() {
                Some(stream) => stream.close(),
                None => inner.parked.take(),
            };
            let Some(state) = state else {
                log::error!("aaudio: the ring was lost with the old stream; cannot reopen");
                return;
            };
            inner.parked = Some(state);
            (Arc::clone(&inner.planner), format, inner.config)
        };

        // Phase 2 (unlocked): the device may be gone, so plan again on every attempt.
        let plan = plan_route(planner.as_ref(), format, config);

        // Phase 3 (locked): open with the plan.
        let path = {
            let mut guard = lock(shared);
            let inner = &mut *guard;
            // Stopped, or replaced by `play` while planning: leave it to the new owner.
            if inner.generation != expected
                || inner.stream.is_some()
                || inner.format != Some(format)
            {
                return;
            }
            let Some(state) = inner.parked.take() else {
                return;
            };
            let hold = plan.hold;
            let path = open_with_plan(inner, state, plan);
            if hold {
                // Stays stopped until an explicit play, even if the open failed and retries.
                inner.playing = false;
                if path.error.is_none() {
                    log::info!("aaudio: reopened held (output device removed); waiting for play");
                }
            } else if path.error.is_none()
                && inner.playing
                && let Some(stream) = inner.stream.as_ref()
                && let Err(msg) = stream.request_start()
            {
                log::warn!("aaudio: restart after reopen failed: {msg}");
            }
            expected = inner.generation;
            path
        };
        planner.opened(&path);

        let Some(msg) = path.error else {
            return;
        };
        log::warn!(
            "aaudio: reopen attempt {} of {} failed: {msg}",
            attempt + 1,
            RETRY_BACKOFF_MS.len()
        );
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
    /// The planner is [`DefaultPlanner`] until one is set.
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
            planner: Arc::new(DefaultPlanner),
            plan: None,
            last_path: None,
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

    /// Like [`AAudioSink::new`], with `planner` installed before the first open.
    pub fn with_planner(
        config: AAudioConfig,
        planner: Arc<dyn RoutePlanner>,
    ) -> Result<AAudioSink> {
        let sink = AAudioSink::new(config)?;
        lock(&sink.shared).planner = planner;
        Ok(sink)
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
        // Phase 1 (unlocked): plan the route.
        let (planner, config) = {
            let inner = lock(&self.shared);
            (Arc::clone(&inner.planner), inner.config)
        };
        let plan = plan_route(planner.as_ref(), format, config);

        // Phase 2 (locked): open with the plan.
        let path = {
            let mut guard = lock(&self.shared);
            let inner = &mut *guard;
            inner.format = Some(format);
            inner.playing = false;
            let path = open_with_plan(inner, state, plan);
            if path.error.is_some() {
                inner.parked = None;
                inner.format = None;
            }
            path
        };
        planner.opened(&path);
        if let Some(msg) = path.error {
            return Err(EngineError::Unsupported(format!("AAudio: {msg}")));
        }
        Ok(())
    }

    fn play(&mut self) -> Result<()> {
        reopen_parked(&self.shared);
        let mut guard = lock(&self.shared);
        let inner = &mut *guard;
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
            log::info!("aaudio: closed, xruns {xruns}, plan {:?}", inner.plan);
            drop(state);
        }
        inner.parked = None;
        inner.format = None;
        inner.info = None;
        inner.plan = None;
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

    /// Planner used by the next open (and every reopen).
    pub fn set_planner(&self, planner: Arc<dyn RoutePlanner>) {
        lock(&self.shared).planner = planner;
    }

    /// Outcome of the last open attempt, `None` before the first one.
    pub fn signal_path(&self) -> Option<SignalPath> {
        lock(&self.shared).last_path.clone()
    }

    /// Xrun count of the open stream, `None` when closed.
    pub fn xruns(&self) -> Option<i32> {
        lock(&self.shared).stream.as_ref().map(Stream::xruns)
    }
}

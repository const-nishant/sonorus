//! Playback controller: the state machine that drives the output sink and one decode thread
//! per loaded track.
//!
//! Threads: the caller talks to the controller thread `sonorus-player` through [`Player`];
//! each load spawns a `sonorus-decode` thread that opens and decodes the track and pushes
//! left-justified `i32` samples into a per-session ring. Every load and every seek creates a
//! new ring and a new [`RingStats`], so nothing ever flushes a ring the output is reading.
//!
//! Known limitation: a seek that arrives while the decode thread is blocked inside
//! `next_block` (for example waiting on a network read) takes effect only after that read
//! returns, which is bounded by the source's read timeout.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rtrb::Producer;

use crate::decoder::{StreamInfo, TrackDecoder};
use crate::opener::{OpenedSource, TrackOpener, TrackRef};
use crate::output::{OutputFormat, OutputSink, SampleKind};
use crate::render::{self, RingStats};

/// Name of the controller thread.
const PLAYER_THREAD: &str = "sonorus-player";
/// Name of the per-session decode thread.
const DECODE_THREAD: &str = "sonorus-decode";
/// How long the decode thread waits for a command while its ring is full.
const RING_FULL_WAIT: Duration = Duration::from_millis(5);

/// Playback state as seen by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerState {
    Idle,
    Loading,
    Buffering,
    Playing,
    Paused,
}

/// Events emitted by the controller thread.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    StateChanged(PlayerState),
    TrackLoaded {
        track_id: String,
        info: StreamInfo,
    },
    Position {
        track_id: String,
        position: Duration,
        duration: Option<Duration>,
    },
    TrackEnded {
        track_id: String,
    },
    Error {
        track_id: Option<String>,
        message: String,
    },
}

/// Tuning knobs for the controller.
#[derive(Debug, Clone)]
pub struct PlayerConfig {
    /// Buffered audio required before (re)starting output.
    pub prefill: Duration,
    /// Minimum time between `Position` events.
    pub position_interval: Duration,
    /// Controller poll interval when no message arrives.
    pub tick: Duration,
    /// Sample kind requested from the output sink.
    pub output_kind: SampleKind,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            prefill: Duration::from_secs(1),
            position_interval: Duration::from_millis(250),
            tick: Duration::from_millis(20),
            output_kind: SampleKind::F32,
        }
    }
}

/// Handle to a running playback controller. Dropping it shuts the controller down.
pub struct Player {
    tx: Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    /// Spawns the controller thread "sonorus-player". Events go to the returned receiver
    /// (send errors are ignored if it is dropped).
    pub fn new(
        sink: Box<dyn OutputSink>,
        opener: Arc<dyn TrackOpener>,
        config: PlayerConfig,
    ) -> (Player, Receiver<PlayerEvent>) {
        let (tx, rx) = mpsc::channel::<Msg>();
        let (events_tx, events_rx) = mpsc::channel::<PlayerEvent>();
        let controller = Controller {
            sink,
            opener,
            config,
            events: events_tx.clone(),
            rx,
            self_tx: tx.clone(),
            session: None,
            state: PlayerState::Idle,
            want_play: false,
            volume: 1.0,
            generation: 0,
        };
        let thread = match thread::Builder::new()
            .name(PLAYER_THREAD.to_owned())
            .spawn(move || controller.run())
        {
            Ok(handle) => Some(handle),
            Err(e) => {
                // No controller means no events; report the failure once and keep the handle
                // inert so that later calls are harmless no-ops.
                let _ = events_tx.send(PlayerEvent::Error {
                    track_id: None,
                    message: format!("failed to start player thread: {e}"),
                });
                None
            }
        };
        (Player { tx, thread }, events_rx)
    }

    /// Loads `track`, replacing any current track. Starts playing once buffered when `autoplay`.
    pub fn load(&self, track: TrackRef, autoplay: bool) {
        self.send(Command::Load { track, autoplay });
    }

    /// Requests playback (buffers first if needed).
    pub fn play(&self) {
        self.send(Command::Play);
    }

    /// Pauses playback, keeping the position.
    pub fn pause(&self) {
        self.send(Command::Pause);
    }

    /// Stops the current track and returns to `Idle`.
    pub fn stop(&self) {
        self.send(Command::Stop);
    }

    /// Seeks to `to` (clamped to the track duration once it is known).
    pub fn seek(&self, to: Duration) {
        self.send(Command::Seek(to));
    }

    /// Sets the output volume, clamped to `0.0..=1.0` by the renderer.
    pub fn set_volume(&self, volume: f32) {
        self.send(Command::SetVolume(volume));
    }

    fn send(&self, cmd: Command) {
        // The controller only disappears after Shutdown or a panic; both leave nothing to do.
        let _ = self.tx.send(Msg::Cmd(cmd));
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Cmd(Command::Shutdown));
        if let Some(handle) = self.thread.take() {
            // A panicked controller has already reported nothing further; ignore the join error.
            let _ = handle.join();
        }
    }
}

/// Everything the controller thread receives: caller commands and decode-thread reports.
enum Msg {
    Cmd(Command),
    Dec(DecoderMsg),
}

enum Command {
    Load { track: TrackRef, autoplay: bool },
    Play,
    Pause,
    Stop,
    Seek(Duration),
    SetVolume(f32),
    Shutdown,
}

/// Decode-thread reports. Every variant carries the session generation; stale ones are ignored.
enum DecoderMsg {
    Opened {
        generation: u64,
        info: StreamInfo,
    },
    Seeked {
        generation: u64,
        /// Matches `Session::seek_seq` of the seek that produced it; older seeks are ignored.
        seq: u64,
        position_frames: u64,
    },
    Finished {
        generation: u64,
    },
    Failed {
        generation: u64,
        message: String,
    },
}

/// Controller to decode-thread commands.
enum DecoderCmd {
    Start {
        producer: Producer<i32>,
        stats: Arc<RingStats>,
    },
    Seek {
        to: Duration,
        seq: u64,
        producer: Producer<i32>,
        stats: Arc<RingStats>,
    },
    Stop,
}

/// The currently loaded track and its decode thread.
struct Session {
    generation: u64,
    track: TrackRef,
    info: Option<StreamInfo>,
    cmd: Sender<DecoderCmd>,
    stats: Option<Arc<RingStats>>,
    /// Frames of the track before the current ring's first sample (seek target).
    base_frames: u64,
    /// Whether the output sink is currently consuming.
    sink_playing: bool,
    /// Underrun count already handled in the current ring.
    last_underruns: u64,
    /// Seek requested before the stream was opened.
    pending_seek: Option<Duration>,
    /// Incremented per seek so a late `Seeked` from an earlier seek cannot move `base_frames`.
    seek_seq: u64,
    last_position_emit: Instant,
}

struct Controller {
    sink: Box<dyn OutputSink>,
    opener: Arc<dyn TrackOpener>,
    config: PlayerConfig,
    events: Sender<PlayerEvent>,
    rx: Receiver<Msg>,
    /// Cloned into each decode thread.
    self_tx: Sender<Msg>,
    session: Option<Session>,
    state: PlayerState,
    want_play: bool,
    volume: f32,
    generation: u64,
}

impl Controller {
    fn run(mut self) {
        loop {
            match self.rx.recv_timeout(self.config.tick) {
                Ok(msg) => {
                    if !self.handle(msg) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.tick();
        }
        self.stop_session();
    }

    /// Returns false when the controller must exit.
    fn handle(&mut self, msg: Msg) -> bool {
        match msg {
            Msg::Cmd(cmd) => {
                let shutdown = matches!(cmd, Command::Shutdown);
                self.on_command(cmd);
                !shutdown
            }
            Msg::Dec(msg) => {
                self.on_decoder(msg);
                true
            }
        }
    }

    fn on_command(&mut self, cmd: Command) {
        match cmd {
            Command::Load { track, autoplay } => self.on_load(track, autoplay),
            Command::Play => self.on_play(),
            Command::Pause => self.on_pause(),
            Command::Stop => self.on_stop(),
            Command::Seek(to) => self.on_seek(to),
            Command::SetVolume(volume) => self.on_set_volume(volume),
            Command::Shutdown => self.stop_session(),
        }
    }

    fn on_decoder(&mut self, msg: DecoderMsg) {
        match msg {
            DecoderMsg::Opened { generation, info } => self.on_opened(generation, info),
            DecoderMsg::Seeked {
                generation,
                seq,
                position_frames,
            } => self.on_seeked(generation, seq, position_frames),
            // Nothing to do: the decode thread already set `decoder_done` and the tick notices
            // it once the ring drains. The generation check only keeps stale reports inert.
            DecoderMsg::Finished { generation } => {
                let _ = self.current_mut(generation);
            }
            DecoderMsg::Failed {
                generation,
                message,
            } => self.on_failed(generation, message),
        }
    }

    fn on_load(&mut self, track: TrackRef, autoplay: bool) {
        self.stop_session();
        self.generation += 1;
        let generation = self.generation;

        let (cmd_tx, cmd_rx) = mpsc::channel::<DecoderCmd>();
        let spawned = thread::Builder::new()
            .name(DECODE_THREAD.to_owned())
            .spawn({
                let opener = Arc::clone(&self.opener);
                let msg_tx = self.self_tx.clone();
                let thread_track = track.clone();
                move || decode_main(generation, thread_track, opener, msg_tx, cmd_rx)
            });
        if let Err(e) = spawned {
            self.emit(PlayerEvent::Error {
                track_id: Some(track.id),
                message: format!("failed to start decode thread: {e}"),
            });
            self.set_state(PlayerState::Idle);
            return;
        }

        self.want_play = autoplay;
        self.session = Some(Session {
            generation,
            track,
            info: None,
            cmd: cmd_tx,
            stats: None,
            base_frames: 0,
            sink_playing: false,
            last_underruns: 0,
            pending_seek: None,
            seek_seq: 0,
            last_position_emit: Instant::now(),
        });
        self.set_state(PlayerState::Loading);
    }

    fn on_play(&mut self) {
        self.want_play = true;
        if self.state == PlayerState::Paused {
            self.set_state(PlayerState::Buffering);
        }
    }

    fn on_pause(&mut self) {
        self.want_play = false;
        let was_playing = match self.session.as_mut() {
            Some(s) if s.sink_playing => {
                s.sink_playing = false;
                true
            }
            _ => false,
        };
        let paused = if was_playing {
            self.sink.pause()
        } else {
            Ok(())
        };
        if let Err(e) = paused {
            self.fail(format!("output: {e}"));
            return;
        }
        if matches!(self.state, PlayerState::Buffering | PlayerState::Playing) {
            self.set_state(PlayerState::Paused);
        }
    }

    fn on_stop(&mut self) {
        self.stop_session();
        self.set_state(PlayerState::Idle);
    }

    fn on_seek(&mut self, to: Duration) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session.info.is_none() {
            session.pending_seek = Some(to);
            return;
        }
        self.seek_to(to);
    }

    fn on_set_volume(&mut self, volume: f32) {
        self.volume = volume;
        if let Some(stats) = self.session.as_ref().and_then(|s| s.stats.as_ref()) {
            stats.set_volume(volume);
        }
    }

    fn current_mut(&mut self, generation: u64) -> Option<&mut Session> {
        self.session.as_mut().filter(|s| s.generation == generation)
    }

    fn on_opened(&mut self, generation: u64, info: StreamInfo) {
        let Some(session) = self.current_mut(generation) else {
            return;
        };
        session.info = Some(info.clone());
        let track_id = session.track.id.clone();
        let pending = session.pending_seek.take();
        self.emit(PlayerEvent::TrackLoaded { track_id, info });
        match pending {
            Some(to) => self.seek_to(to),
            None => self.begin_playback(),
        }
    }

    fn on_seeked(&mut self, generation: u64, seq: u64, position_frames: u64) {
        if let Some(session) = self.current_mut(generation)
            && session.seek_seq == seq
        {
            session.base_frames = position_frames;
        }
    }

    fn on_failed(&mut self, generation: u64, message: String) {
        if self.current_mut(generation).is_none() {
            return;
        }
        self.fail(message);
    }

    /// Starts the output on a fresh ring and hands the producer to the decode thread.
    fn begin_playback(&mut self) {
        let Some(info) = self.session.as_ref().and_then(|s| s.info.clone()) else {
            return;
        };
        let Some((producer, stats)) = self.start_ring(&info) else {
            return;
        };
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.stats = Some(Arc::clone(&stats));
        session.base_frames = 0;
        session.sink_playing = false;
        session.last_underruns = 0;
        // A failed send means the decode thread already exited; nothing more will arrive.
        let _ = session.cmd.send(DecoderCmd::Start { producer, stats });
        let next = if self.want_play {
            PlayerState::Buffering
        } else {
            PlayerState::Paused
        };
        self.set_state(next);
    }

    /// Seeks an opened stream: new ring and output, decoder told to seek, position emitted.
    fn seek_to(&mut self, to: Duration) {
        let Some(info) = self.session.as_ref().and_then(|s| s.info.clone()) else {
            return;
        };
        let to = match info.duration {
            Some(duration) => to.min(duration),
            None => to,
        };
        self.sink.stop();
        let Some((producer, stats)) = self.start_ring(&info) else {
            return;
        };
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.pending_seek = None;
        session.seek_seq = session.seek_seq.wrapping_add(1);
        let seq = session.seek_seq;
        session.stats = Some(Arc::clone(&stats));
        session.base_frames = duration_to_frames(to, info.sample_rate);
        session.sink_playing = false;
        session.last_underruns = 0;
        // A failed send means the decode thread already exited; nothing more will arrive.
        let _ = session.cmd.send(DecoderCmd::Seek {
            to,
            seq,
            producer,
            stats,
        });
        let next = if self.want_play {
            PlayerState::Buffering
        } else {
            PlayerState::Paused
        };
        self.set_state(next);
        self.emit_position();
    }

    /// Creates a ring and its stats, and starts the (paused) sink on them. Reports errors itself.
    fn start_ring(&mut self, info: &StreamInfo) -> Option<(Producer<i32>, Arc<RingStats>)> {
        let (producer, consumer) = render::new_ring(info.sample_rate, info.channels);
        let stats = Arc::new(RingStats::new());
        stats.set_volume(self.volume);
        let format = OutputFormat {
            sample_rate: info.sample_rate,
            channels: info.channels,
            sample: self.config.output_kind,
        };
        match self.sink.start(format, consumer, Arc::clone(&stats)) {
            Ok(()) => Some((producer, stats)),
            Err(e) => {
                self.fail(format!("output: {e}"));
                None
            }
        }
    }

    /// Reports `message` for the current track, drops the session and goes idle.
    fn fail(&mut self, message: String) {
        let track_id = self.session.as_ref().map(|s| s.track.id.clone());
        self.emit(PlayerEvent::Error { track_id, message });
        self.stop_session();
        self.set_state(PlayerState::Idle);
    }

    /// Stops the sink and tells the decode thread to exit. Never joins the decode thread.
    fn stop_session(&mut self) {
        self.sink.stop();
        if let Some(session) = self.session.take() {
            // The decode thread may already be gone; that is fine.
            let _ = session.cmd.send(DecoderCmd::Stop);
        }
    }

    fn set_state(&mut self, state: PlayerState) {
        if self.state != state {
            self.state = state;
            self.emit(PlayerEvent::StateChanged(state));
        }
    }

    fn set_sink_playing(&mut self, playing: bool) {
        if let Some(session) = self.session.as_mut() {
            session.sink_playing = playing;
        }
    }

    fn emit(&self, event: PlayerEvent) {
        // The caller may have dropped the receiver; events are then irrelevant.
        let _ = self.events.send(event);
    }

    fn position_due(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.last_position_emit.elapsed() >= self.config.position_interval)
    }

    /// Emits `Position` from the current session's stats (so positions after a seek are
    /// relative to the seek target).
    fn emit_position(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let (Some(stats), Some(info)) = (session.stats.as_ref(), session.info.as_ref()) else {
            return;
        };
        if info.sample_rate == 0 || info.channels == 0 {
            return;
        }
        let played_frames = stats.samples_played.load(Ordering::Acquire) / u64::from(info.channels);
        let position = frames_to_duration(
            session.base_frames.saturating_add(played_frames),
            info.sample_rate,
        );
        let event = PlayerEvent::Position {
            track_id: session.track.id.clone(),
            position,
            duration: info.duration,
        };
        session.last_position_emit = Instant::now();
        self.emit(event);
    }

    /// Final position, `TrackEnded`, then tear down and go idle.
    fn finish_track(&mut self) {
        self.emit_position();
        if let Some(track_id) = self.session.as_ref().map(|s| s.track.id.clone()) {
            self.emit(PlayerEvent::TrackEnded { track_id });
        }
        self.stop_session();
        self.set_state(PlayerState::Idle);
    }

    /// Periodic state machine step: prefill → play, underrun → rebuffer, end detection,
    /// position reporting. Does nothing without a session that has stats.
    fn tick(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let Some(stats) = session.stats.clone() else {
            return;
        };
        let Some(info) = session.info.as_ref() else {
            return;
        };
        if info.sample_rate == 0 || info.channels == 0 {
            return;
        }
        let channels = u64::from(info.channels);
        let prefill_frames = duration_to_frames(self.config.prefill, info.sample_rate);
        let buffered_frames = stats.buffered_samples() / channels;
        let done = stats.decoder_done.load(Ordering::Acquire);

        if self.state == PlayerState::Buffering
            && self.want_play
            && (buffered_frames >= prefill_frames || done)
        {
            if let Err(e) = self.sink.play() {
                self.fail(format!("output: {e}"));
                return;
            }
            self.set_sink_playing(true);
            self.set_state(PlayerState::Playing);
        }

        if self.state == PlayerState::Playing {
            let underruns = stats.underruns.load(Ordering::Relaxed);
            let fresh_underrun = match self.session.as_mut() {
                Some(s) if underruns > s.last_underruns => {
                    s.last_underruns = underruns;
                    true
                }
                _ => false,
            };
            if fresh_underrun && !done {
                if let Err(e) = self.sink.pause() {
                    self.fail(format!("output: {e}"));
                    return;
                }
                self.set_sink_playing(false);
                self.set_state(PlayerState::Buffering);
            }
            if done && stats.buffered_samples() == 0 {
                self.finish_track();
                return;
            }
        }

        if matches!(
            self.state,
            PlayerState::Playing | PlayerState::Buffering | PlayerState::Paused
        ) && self.position_due()
        {
            self.emit_position();
        }
    }
}

/// Frames → duration at `rate` Hz. Returns zero for a zero rate.
fn frames_to_duration(frames: u64, rate: u32) -> Duration {
    if rate == 0 {
        return Duration::ZERO;
    }
    Duration::try_from_secs_f64(frames as f64 / f64::from(rate)).unwrap_or_default()
}

/// Duration → frames at `rate` Hz, rounded to the nearest frame.
fn duration_to_frames(duration: Duration, rate: u32) -> u64 {
    (duration.as_secs_f64() * f64::from(rate)).round() as u64
}

/// Decode-thread state once the ring exists.
struct DecodeLoop {
    generation: u64,
    msg_tx: Sender<Msg>,
    decoder: TrackDecoder,
    channels: usize,
    producer: Producer<i32>,
    stats: Arc<RingStats>,
    /// Current decoded block (interleaved) and the offset of the first unpushed sample.
    block: Vec<i32>,
    offset: usize,
    /// End of stream reached; only commands are processed from here on.
    eof: bool,
}

impl DecodeLoop {
    /// Pushes decoded audio until a command stops the thread or the channel disconnects.
    fn run(&mut self, cmd_rx: &Receiver<DecoderCmd>) {
        loop {
            match cmd_rx.try_recv() {
                Ok(cmd) => {
                    if !self.on_cmd(cmd) {
                        return;
                    }
                }
                Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => {}
            }

            if self.eof {
                match cmd_rx.recv() {
                    Ok(cmd) => {
                        if !self.on_cmd(cmd) {
                            return;
                        }
                    }
                    Err(_) => return,
                }
                continue;
            }

            if self.offset >= self.block.len() {
                match self.decoder.next_block::<i32>(&mut self.block) {
                    Ok(Some(_)) => self.offset = 0,
                    Ok(None) => {
                        self.stats.decoder_done.store(true, Ordering::Release);
                        report(
                            &self.msg_tx,
                            DecoderMsg::Finished {
                                generation: self.generation,
                            },
                        );
                        self.eof = true;
                    }
                    Err(e) => {
                        report(
                            &self.msg_tx,
                            DecoderMsg::Failed {
                                generation: self.generation,
                                message: e.to_string(),
                            },
                        );
                        return;
                    }
                }
                continue;
            }

            let rest = self.block.get(self.offset..).unwrap_or(&[]);
            let written = render::push_frames(&mut self.producer, rest, self.channels, &self.stats);
            self.offset += written;
            if written == 0 {
                match cmd_rx.recv_timeout(RING_FULL_WAIT) {
                    Ok(cmd) => {
                        if !self.on_cmd(cmd) {
                            return;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        }
    }

    /// Applies one command. Returns false when the thread must exit.
    fn on_cmd(&mut self, cmd: DecoderCmd) -> bool {
        match cmd {
            DecoderCmd::Stop => false,
            // The first Start is consumed before the loop; any later one is stale.
            DecoderCmd::Start { .. } => true,
            DecoderCmd::Seek {
                to,
                seq,
                producer,
                stats,
            } => {
                self.producer = producer;
                self.stats = stats;
                self.block.clear();
                self.offset = 0;
                self.eof = false;
                seek_decoder(&mut self.decoder, to, seq, self.generation, &self.msg_tx)
            }
        }
    }
}

/// Body of the `sonorus-decode` thread for one session.
fn decode_main(
    generation: u64,
    track: TrackRef,
    opener: Arc<dyn TrackOpener>,
    msg_tx: Sender<Msg>,
    cmd_rx: Receiver<DecoderCmd>,
) {
    let opened = match opener.open(&track) {
        Ok(opened) => opened,
        Err(e) => {
            report(
                &msg_tx,
                DecoderMsg::Failed {
                    generation,
                    message: e.to_string(),
                },
            );
            return;
        }
    };
    let OpenedSource {
        source,
        ext_hint,
        buffer,
    } = opened;
    // Keeps the source's buffer handle alive for as long as this session decodes.
    let _buffer = buffer;

    let mut decoder = match TrackDecoder::new(source, ext_hint.as_deref()) {
        Ok(decoder) => decoder,
        Err(e) => {
            report(
                &msg_tx,
                DecoderMsg::Failed {
                    generation,
                    message: e.to_string(),
                },
            );
            return;
        }
    };
    let info = decoder.info().clone();
    let channels = usize::from(info.channels);
    report(&msg_tx, DecoderMsg::Opened { generation, info });

    // Wait for the first ring: either a plain start or a seek before playback.
    let (producer, stats) = match cmd_rx.recv() {
        Ok(DecoderCmd::Start { producer, stats }) => (producer, stats),
        Ok(DecoderCmd::Seek {
            to,
            seq,
            producer,
            stats,
        }) => {
            if !seek_decoder(&mut decoder, to, seq, generation, &msg_tx) {
                return;
            }
            (producer, stats)
        }
        Ok(DecoderCmd::Stop) | Err(_) => return,
    };

    let mut decode = DecodeLoop {
        generation,
        msg_tx,
        decoder,
        channels,
        producer,
        stats,
        block: Vec::new(),
        offset: 0,
        eof: false,
    };
    decode.run(&cmd_rx);
}

/// Seeks the decoder and reports `Seeked` (or `Failed`). Returns false on failure.
fn seek_decoder(
    decoder: &mut TrackDecoder,
    to: Duration,
    seq: u64,
    generation: u64,
    msg_tx: &Sender<Msg>,
) -> bool {
    match decoder.seek(to) {
        Ok(_) => {
            report(
                msg_tx,
                DecoderMsg::Seeked {
                    generation,
                    seq,
                    position_frames: decoder.position_frames(),
                },
            );
            true
        }
        Err(e) => {
            report(
                msg_tx,
                DecoderMsg::Failed {
                    generation,
                    message: e.to_string(),
                },
            );
            false
        }
    }
}

fn report(tx: &Sender<Msg>, msg: DecoderMsg) {
    // The controller may already be gone; its messages are then irrelevant.
    let _ = tx.send(Msg::Dec(msg));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use crate::opener::FileOpener;
    use crate::output::NullSink;
    use std::io::{self, Cursor, Read, Seek, SeekFrom};
    use symphonia::core::io::MediaSource;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    /// Overall budget for collecting events in one test.
    const DEADLINE: Duration = Duration::from_secs(3);

    type Step = (&'static str, fn(&PlayerEvent) -> bool);

    fn test_config() -> PlayerConfig {
        PlayerConfig {
            prefill: Duration::from_millis(50),
            tick: Duration::from_millis(5),
            position_interval: Duration::from_millis(20),
            ..PlayerConfig::default()
        }
    }

    fn fixture_opener() -> Arc<dyn TrackOpener> {
        Arc::new(FileOpener::new(FIXTURES))
    }

    fn track(key: &str) -> TrackRef {
        TrackRef {
            id: key.to_string(),
            key: key.to_string(),
            size: 0,
        }
    }

    fn player_with(opener: Arc<dyn TrackOpener>, speed: f32) -> (Player, Receiver<PlayerEvent>) {
        let sink = Box::new(NullSink::with_speed(speed));
        Player::new(sink, opener, test_config())
    }

    /// Collects events until `done` is satisfied by everything received so far, or the deadline.
    fn collect_until(
        rx: &Receiver<PlayerEvent>,
        done: impl Fn(&[PlayerEvent]) -> bool,
    ) -> Vec<PlayerEvent> {
        let deadline = Instant::now() + DEADLINE;
        let mut events = Vec::new();
        while !done(&events) {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(event) => events.push(event),
                Err(_) => break,
            }
        }
        events
    }

    /// Collects every event that arrives within `span`.
    fn collect_for(rx: &Receiver<PlayerEvent>, span: Duration) -> Vec<PlayerEvent> {
        let deadline = Instant::now() + span;
        let mut events = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(event) => events.push(event),
                Err(_) => break,
            }
        }
        events
    }

    /// Asserts that `steps` occur as a subsequence of `events`, in that order.
    fn assert_in_order(events: &[PlayerEvent], steps: &[Step]) {
        let mut cursor = 0;
        for (name, pred) in steps {
            match events
                .iter()
                .enumerate()
                .skip(cursor)
                .find(|(_, e)| pred(e))
            {
                Some((i, _)) => cursor = i + 1,
                None => panic!("expected {name} after event {cursor}; got {events:#?}"),
            }
        }
    }

    fn has_state(events: &[PlayerEvent], state: PlayerState) -> bool {
        events
            .iter()
            .any(|e| matches!(e, PlayerEvent::StateChanged(s) if *s == state))
    }

    fn has_ended(events: &[PlayerEvent]) -> bool {
        events
            .iter()
            .any(|e| matches!(e, PlayerEvent::TrackEnded { .. }))
    }

    /// The last event is the return to `Idle` (used as the end-of-test marker).
    fn ends_idle(events: &[PlayerEvent]) -> bool {
        matches!(
            events.last(),
            Some(PlayerEvent::StateChanged(PlayerState::Idle))
        )
    }

    fn is_state(e: &PlayerEvent, state: PlayerState) -> bool {
        matches!(e, PlayerEvent::StateChanged(s) if *s == state)
    }

    fn position_of(e: &PlayerEvent) -> Option<Duration> {
        match e {
            PlayerEvent::Position { position, .. } => Some(*position),
            _ => None,
        }
    }

    #[test]
    fn autoplay_plays_track_to_end_in_order() {
        let (player, rx) = player_with(fixture_opener(), 4.0);
        player.load(track("tone_24_96.flac"), true);
        let events = collect_until(&rx, ends_idle);

        let steps: [Step; 6] = [
            ("Loading", |e| is_state(e, PlayerState::Loading)),
            (
                "TrackLoaded at 96 kHz",
                |e| matches!(e, PlayerEvent::TrackLoaded { info, .. } if info.sample_rate == 96_000),
            ),
            ("Buffering", |e| is_state(e, PlayerState::Buffering)),
            ("Playing", |e| is_state(e, PlayerState::Playing)),
            ("TrackEnded", |e| {
                matches!(e, PlayerEvent::TrackEnded { .. })
            }),
            ("Idle", |e| is_state(e, PlayerState::Idle)),
        ];
        assert_in_order(&events, &steps);

        let ended = events
            .iter()
            .position(|e| matches!(e, PlayerEvent::TrackEnded { .. }))
            .expect("track ended");
        let last_position = events[..ended]
            .iter()
            .rev()
            .find_map(position_of)
            .expect("position before end");
        assert!(
            last_position.abs_diff(Duration::from_millis(300)) <= Duration::from_millis(30),
            "last position {last_position:?}"
        );
    }

    #[test]
    fn manual_load_waits_for_play() {
        let (player, rx) = player_with(fixture_opener(), 4.0);
        player.load(track("tone_24_96.flac"), false);
        let before = collect_until(&rx, |e| has_state(e, PlayerState::Paused));
        assert!(has_state(&before, PlayerState::Paused), "{before:#?}");

        let quiet = collect_for(&rx, Duration::from_millis(300));
        assert!(!has_ended(&quiet), "track ended while paused");

        player.play();
        let after = collect_until(&rx, has_ended);
        assert!(has_ended(&after), "{after:#?}");
    }

    #[test]
    fn seek_while_paused_reports_target_then_plays_to_end() {
        let (player, rx) = player_with(fixture_opener(), 4.0);
        player.load(track("tone_24_96.flac"), false);
        let _ = collect_until(&rx, |e| has_state(e, PlayerState::Paused));

        player.seek(Duration::from_millis(200));
        let reached_target = |e: &[PlayerEvent]| {
            e.iter()
                .filter_map(position_of)
                .any(|p| p >= Duration::from_millis(199))
        };
        let mut events = collect_until(&rx, reached_target);
        assert!(
            reached_target(&events),
            "no position at seek target: {events:#?}"
        );

        player.play();
        events.extend(collect_until(&rx, has_ended));
        assert!(has_ended(&events), "{events:#?}");

        // Events queued before the seek took effect may still show 0 ms; judge from the first
        // post-seek position onwards.
        let first_after_seek = events
            .iter()
            .position(|e| position_of(e).is_some_and(|p| p >= Duration::from_millis(199)))
            .expect("post-seek position");
        for event in &events[first_after_seek..] {
            if let Some(p) = position_of(event) {
                assert!(
                    p >= Duration::from_millis(199),
                    "position went back to {p:?}"
                );
            }
        }
    }

    #[test]
    fn rapid_double_seek_keeps_latest_target() {
        let (player, rx) = player_with(fixture_opener(), 4.0);
        player.load(track("tone_24_96.flac"), false);
        let _ = collect_until(&rx, |e| has_state(e, PlayerState::Paused));

        // The first seek's Seeked report may arrive after the second seek was issued; it must
        // not drag the position back to 50 ms.
        player.seek(Duration::from_millis(50));
        player.seek(Duration::from_millis(250));
        let events = collect_for(&rx, Duration::from_millis(150));
        let positions: Vec<Duration> = events.iter().filter_map(position_of).collect();
        let last = positions.last().copied().expect("positions after seeks");
        assert!(
            last >= Duration::from_millis(249),
            "position fell back: {positions:?}"
        );
    }

    #[test]
    fn missing_file_reports_error_and_goes_idle() {
        let (player, rx) = player_with(fixture_opener(), 4.0);
        player.load(track("missing.flac"), true);
        let events = collect_until(&rx, ends_idle);

        let error_at = events.iter().position(|e| {
            matches!(e, PlayerEvent::Error { track_id: Some(id), .. } if id.as_str() == "missing.flac")
        });
        assert!(error_at.is_some(), "no error event: {events:#?}");
        assert!(ends_idle(&events), "not idle at end: {events:#?}");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, PlayerEvent::TrackLoaded { .. })),
            "missing track must not load"
        );
    }

    /// Cumulative bytes returned by `read` before the stall. Symphonia's MediaSourceStream reads
    /// ahead 64 KiB during open, and the probe seeks to the end of the file, so the 50 KiB FLAC
    /// fixture is fully read before any audio is decoded and cannot stall mid-stream. The 172 KiB
    /// WAV fixture (same tone) is used instead, stalled at 80 KiB: past the probe, with about
    /// 140 ms of audio already decoded into the ring, so the 50 ms prefill starts output and the
    /// output then drains the ring during the stall.
    const STALL_AFTER: u64 = 80 * 1024;
    const STALL_FIXTURE: &str = "tone_24_96.wav";
    const STALL_FOR: Duration = Duration::from_millis(400);
    /// Reads are capped so the stall lands mid-stream rather than on the first buffer fill.
    const READ_CHUNK: usize = 4096;

    /// Cursor over fixture bytes that sleeps once when the cumulative bytes read first reach
    /// `STALL_AFTER`, and returns at most `READ_CHUNK` bytes per read.
    struct SlowReader {
        inner: Cursor<Vec<u8>>,
        read_total: u64,
        stalled: bool,
    }

    impl Read for SlowReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let cap = buf.len().min(READ_CHUNK);
            let n = self.inner.read(&mut buf[..cap])?;
            self.read_total += n as u64;
            if !self.stalled && self.read_total >= STALL_AFTER {
                self.stalled = true;
                thread::sleep(STALL_FOR);
            }
            Ok(n)
        }
    }

    impl Seek for SlowReader {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    impl MediaSource for SlowReader {
        fn is_seekable(&self) -> bool {
            true
        }

        fn byte_len(&self) -> Option<u64> {
            Some(self.inner.get_ref().len() as u64)
        }
    }

    struct SlowOpener {
        bytes: Vec<u8>,
    }

    impl TrackOpener for SlowOpener {
        fn open(&self, _track: &TrackRef) -> Result<OpenedSource> {
            Ok(OpenedSource {
                source: Box::new(SlowReader {
                    inner: Cursor::new(self.bytes.clone()),
                    read_total: 0,
                    stalled: false,
                }),
                ext_hint: Some("wav".to_string()),
                buffer: None,
            })
        }
    }

    #[test]
    fn stall_causes_rebuffer_then_resumes() {
        let bytes = std::fs::read(format!("{FIXTURES}/{STALL_FIXTURE}")).expect("fixture");
        let (player, rx) = player_with(Arc::new(SlowOpener { bytes }), 4.0);
        player.load(track(STALL_FIXTURE), true);
        let events = collect_until(&rx, ends_idle);

        let steps: [Step; 4] = [
            ("Playing", |e| is_state(e, PlayerState::Playing)),
            ("Buffering", |e| is_state(e, PlayerState::Buffering)),
            ("Playing again", |e| is_state(e, PlayerState::Playing)),
            ("TrackEnded", |e| {
                matches!(e, PlayerEvent::TrackEnded { .. })
            }),
        ];
        assert_in_order(&events, &steps);
    }

    #[test]
    fn drop_mid_playback_returns_promptly() {
        // Speed 1: the 300 ms track is still playing when the player is dropped.
        let (player, rx) = player_with(fixture_opener(), 1.0);
        player.load(track("tone_24_96.flac"), true);
        let _ = collect_until(&rx, |e| has_state(e, PlayerState::Playing));

        let started = Instant::now();
        drop(player);
        let took = started.elapsed();
        assert!(took < Duration::from_millis(500), "drop took {took:?}");
    }
}

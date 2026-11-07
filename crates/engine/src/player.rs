//! Playback controller: the state machine that drives the output sink, the play queue and one
//! decode thread per session.
//!
//! Threads: the caller talks to the controller thread `sonorus-player` through [`Player`];
//! each session spawns a `sonorus-decode` thread that opens and decodes the track and pushes
//! left-justified `i32` samples into a per-session ring. Every session and every seek creates a
//! new ring and a new [`RingStats`], so nothing ever flushes a ring the output is reading.
//!
//! Gapless: while a track plays, the next queued track is opened on a `sonorus-preload`
//! thread. When it has the same sample rate and channel count as the output, it is handed to
//! the decode thread, which switches to it at end of stream and reports a boundary. The
//! controller crosses the boundary when the output reaches it. A queue change after the
//! hand-off makes the boundary stale; playback then restarts on the queue's current entry.
//!
//! Known limitation: a seek that arrives while the decode thread is blocked inside
//! `next_block` (for example waiting on a network read) takes effect only after that read
//! returns, which is bounded by the source's read timeout.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rtrb::Producer;

use crate::decoder::{StreamInfo, TrackDecoder};
use crate::opener::{OpenedSource, TrackOpener, TrackRef};
use crate::output::{OutputFormat, OutputSink, SampleKind};
use crate::queue::{EntryId, Queue, QueueEntry, RepeatMode};
use crate::render::{self, RingStats};
use crate::source::BufferHandle;

/// Name of the controller thread.
const PLAYER_THREAD: &str = "sonorus-player";
/// Name of the per-session decode thread.
const DECODE_THREAD: &str = "sonorus-decode";
/// Name of the thread that opens the next track ahead of time.
const PRELOAD_THREAD: &str = "sonorus-preload";
/// How long the decode thread waits for a command while its ring is full.
const RING_FULL_WAIT: Duration = Duration::from_millis(5);
/// `previous` restarts the current track when it is further in than this.
const RESTART_THRESHOLD: Duration = Duration::from_secs(3);

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
    /// The current queue entry changed (a track started, or the output moved to the next one).
    TrackChanged {
        entry_id: EntryId,
        track_id: String,
    },
    /// Snapshot of the queue, in play order, after any change to it or to the current entry.
    QueueChanged {
        entries: Vec<QueueEntry>,
        current: Option<EntryId>,
        shuffle: bool,
        repeat: RepeatMode,
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
    /// How long before the end of the current track the next queued track is opened.
    pub preload_lead: Duration,
    /// Seed for shuffle order. 0 seeds from the system clock.
    pub shuffle_seed: u64,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            prefill: Duration::from_secs(1),
            position_interval: Duration::from_millis(250),
            tick: Duration::from_millis(20),
            output_kind: SampleKind::F32,
            preload_lead: Duration::from_secs(15),
            shuffle_seed: 0,
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
        let queue = Queue::new(resolve_seed(config.shuffle_seed));
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
            queue,
            preload: None,
            preload_failed: None,
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

    /// Loads `track` as the whole queue. Same as `set_queue(vec![track], 0, autoplay)`.
    pub fn load(&self, track: TrackRef, autoplay: bool) {
        self.set_queue(vec![track], 0, autoplay);
    }

    /// Replaces the queue with `tracks` and starts the entry at index `start` (clamped to the
    /// last index). Starts playing once buffered when `autoplay`.
    pub fn set_queue(&self, tracks: Vec<TrackRef>, start: usize, autoplay: bool) {
        self.send(Command::SetQueue {
            tracks,
            start,
            autoplay,
        });
    }

    /// Adds `tracks` to the end of the queue.
    pub fn append(&self, tracks: Vec<TrackRef>) {
        self.send(Command::Append(tracks));
    }

    /// Inserts `tracks` right after the current entry.
    pub fn play_next(&self, tracks: Vec<TrackRef>) {
        self.send(Command::PlayNext(tracks));
    }

    /// Skips to the next queue entry (wraps with repeat All).
    pub fn next(&self) {
        self.send(Command::Next);
    }

    /// Restarts the current track when it is more than 3 s in, otherwise goes to the previous
    /// queue entry.
    pub fn previous(&self) {
        self.send(Command::Previous);
    }

    /// Starts the queue entry `id`. Unknown ids are ignored.
    pub fn jump_to(&self, id: EntryId) {
        self.send(Command::JumpTo(id));
    }

    /// Removes the entry `id` from the queue. Removing the current entry starts the one that
    /// takes its place, or goes idle.
    pub fn remove(&self, id: EntryId) {
        self.send(Command::Remove(id));
    }

    /// Moves the entry `id` to position `to` in the queue.
    pub fn move_entry(&self, id: EntryId, to: usize) {
        self.send(Command::MoveEntry { id, to });
    }

    /// Turns shuffle on or off. The current entry keeps playing.
    pub fn set_shuffle(&self, on: bool) {
        self.send(Command::SetShuffle(on));
    }

    /// Sets the repeat mode.
    pub fn set_repeat(&self, mode: RepeatMode) {
        self.send(Command::SetRepeat(mode));
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
    SetQueue {
        tracks: Vec<TrackRef>,
        start: usize,
        autoplay: bool,
    },
    Append(Vec<TrackRef>),
    PlayNext(Vec<TrackRef>),
    Next,
    Previous,
    JumpTo(EntryId),
    Remove(EntryId),
    MoveEntry {
        id: EntryId,
        to: usize,
    },
    SetShuffle(bool),
    SetRepeat(RepeatMode),
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
    /// The decode thread switched to a queued track at the end of the previous one.
    Advanced {
        generation: u64,
        entry_id: EntryId,
        info: StreamInfo,
        /// `samples_pushed` of the ring at the switch: the first sample of the new track is at
        /// this index.
        boundary_samples: u64,
    },
    /// A seek was refused because the decode thread had already switched to the next track.
    SeekRejected {
        generation: u64,
        seq: u64,
    },
    /// Result of a `preload_main` run.
    Preloaded {
        generation: u64,
        entry_id: EntryId,
        result: Result<Box<TrackDecoder>, String>,
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
        /// Number of `Advanced` reports the controller has received for this session. If the
        /// decode thread has switched more often, the seek targets a track it no longer decodes
        /// and is rejected.
        track_seq: u64,
        producer: Producer<i32>,
        stats: Arc<RingStats>,
    },
    /// Queues a gapless continuation, replacing any queued one.
    Next {
        entry_id: EntryId,
        decoder: Box<TrackDecoder>,
    },
    /// Drops the queued continuation if it has not been switched to yet.
    CancelNext,
    Stop,
}

/// What `decode_main` starts from: open the track, or use a decoder that is already probed.
enum DecoderSource {
    Open(TrackRef),
    Ready(Box<TrackDecoder>),
}

// The decoder is moved into the decode thread (`Next`, `Preloaded`), so it must be `Send`.
const _: fn() = || {
    fn is_send<T: Send>() {}
    is_send::<TrackDecoder>();
};

/// The next queue entry, opened ahead of time.
struct Preload {
    entry_id: EntryId,
    track: TrackRef,
    state: PreloadState,
    /// Stream description once the preload has finished opening.
    info: Option<StreamInfo>,
}

enum PreloadState {
    /// `preload_main` is still running.
    Opening,
    /// Opened and probed, waiting for a session that it can hand off to.
    Ready(Box<TrackDecoder>),
    /// Sent to the decode thread as its queued continuation.
    HandedOff,
}

/// A track the decode thread has switched to whose samples start at `at_samples` in the ring.
struct Boundary {
    entry_id: EntryId,
    track: TrackRef,
    info: StreamInfo,
    at_samples: u64,
}

/// The currently loaded track and its decode thread.
struct Session {
    generation: u64,
    /// Queue entry whose track is being output (the boundary's entry once crossed).
    entry_id: EntryId,
    track: TrackRef,
    info: Option<StreamInfo>,
    cmd: Sender<DecoderCmd>,
    stats: Option<Arc<RingStats>>,
    /// Frames of the track before the current ring's first sample (seek target).
    base_frames: u64,
    /// Ring sample index at which the current track starts.
    offset_samples: u64,
    /// Tracks the decode thread has switched to, in order, not yet reached by the output.
    boundaries: VecDeque<Boundary>,
    /// Whether the output sink is currently consuming.
    sink_playing: bool,
    /// Underrun count already handled in the current ring.
    last_underruns: u64,
    /// Seek requested before the stream was opened.
    pending_seek: Option<Duration>,
    /// Incremented per seek so a late `Seeked` from an earlier seek cannot move `base_frames`.
    seek_seq: u64,
    /// Target of the latest seek, replayed as a cold restart if the seek is rejected.
    seek_target: Option<Duration>,
    /// `Advanced` reports received for this session (compared against the decode thread's count).
    advances: u64,
    last_position_emit: Instant,
}

impl Session {
    /// Frames of the current track that the output has played.
    fn position_frames(&self, channels: u16) -> u64 {
        let played = self
            .stats
            .as_ref()
            .map_or(0, |s| s.samples_played.load(Ordering::Acquire));
        let frames = played.saturating_sub(self.offset_samples) / u64::from(channels.max(1));
        self.base_frames.saturating_add(frames)
    }
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
    queue: Queue,
    preload: Option<Preload>,
    /// Entry whose preload failed; not retried (the cold open at the track's end reports it).
    preload_failed: Option<EntryId>,
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
            Command::SetQueue {
                tracks,
                start,
                autoplay,
            } => self.on_set_queue(tracks, start, autoplay),
            Command::Append(tracks) => {
                self.queue.append(tracks);
                self.queue_edited();
            }
            Command::PlayNext(tracks) => {
                self.queue.play_next(tracks);
                self.queue_edited();
            }
            Command::Next => self.on_next(),
            Command::Previous => self.on_previous(),
            Command::JumpTo(id) => self.on_jump_to(id),
            Command::Remove(id) => self.on_remove(id),
            Command::MoveEntry { id, to } => {
                self.queue.move_entry(id, to);
                self.queue_edited();
            }
            Command::SetShuffle(on) => {
                self.queue.set_shuffle(on);
                self.queue_edited();
            }
            Command::SetRepeat(mode) => {
                self.queue.set_repeat(mode);
                self.queue_edited();
            }
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
            DecoderMsg::Advanced {
                generation,
                entry_id,
                info,
                boundary_samples,
            } => self.on_advanced(generation, entry_id, info, boundary_samples),
            DecoderMsg::Preloaded {
                generation,
                entry_id,
                result,
            } => self.on_preloaded(generation, entry_id, result),
            DecoderMsg::SeekRejected { generation, seq } => self.on_seek_rejected(generation, seq),
        }
    }

    fn on_set_queue(&mut self, tracks: Vec<TrackRef>, start: usize, autoplay: bool) {
        self.queue.set_tracks(tracks, start);
        match self.queue.current().cloned() {
            Some(entry) => self.start_entry(entry, autoplay, None),
            None => self.go_idle(),
        }
        self.emit_queue();
        self.sync_preload();
    }

    fn on_next(&mut self) {
        if let Some(entry) = self.queue.skip_next().cloned() {
            self.switch_to(entry);
        }
    }

    fn on_previous(&mut self) {
        if self.position().is_some_and(|p| p > RESTART_THRESHOLD) {
            self.on_seek(Duration::ZERO);
            return;
        }
        if let Some(entry) = self.queue.skip_previous().cloned() {
            self.switch_to(entry);
        }
    }

    fn on_jump_to(&mut self, id: EntryId) {
        if let Some(entry) = self.queue.jump_to(id).cloned() {
            self.switch_to(entry);
        }
    }

    fn on_remove(&mut self, id: EntryId) {
        let was_current = self.queue.current_id() == Some(id);
        if self.queue.remove(id) && was_current {
            match self.queue.current().cloned() {
                Some(entry) => {
                    let autoplay = self.autoplay_intent();
                    self.start_entry(entry, autoplay, None);
                }
                None => self.go_idle(),
            }
        }
        self.queue_edited();
    }

    /// Starts `entry` after a transport command, playing when the output was or is playing.
    fn switch_to(&mut self, entry: QueueEntry) {
        let autoplay = self.autoplay_intent();
        self.start_entry(entry, autoplay, None);
        self.emit_queue();
    }

    /// Whether a track started by a transport command should play right away.
    fn autoplay_intent(&self) -> bool {
        self.want_play || matches!(self.state, PlayerState::Playing | PlayerState::Buffering)
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
        self.go_idle();
    }

    fn on_seek(&mut self, to: Duration) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if session.info.is_none() {
            session.pending_seek = Some(to);
            return;
        }
        // Boundaries queued: the decode thread is known to be on a later track, so seeking in
        // place would aim at the wrong one. Restart the current entry at `to` instead. A switch
        // still in flight is caught by `track_seq` and answered with `SeekRejected`.
        if !session.boundaries.is_empty() {
            let entry_id = session.entry_id;
            if let Some(entry) = self.queue.get(entry_id).cloned() {
                self.start_entry(entry, self.want_play, Some(to));
            }
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

    fn is_current(&self, generation: u64) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.generation == generation)
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

    /// The decode thread had already moved to the next track when the seek arrived: replay the
    /// seek as a cold restart of the entry being output.
    fn on_seek_rejected(&mut self, generation: u64, seq: u64) {
        let Some(session) = self.current_mut(generation) else {
            return;
        };
        if session.seek_seq != seq {
            return;
        }
        let target = session.seek_target;
        let entry_id = session.entry_id;
        if let Some(entry) = self.queue.get(entry_id).cloned() {
            self.start_entry(entry, self.want_play, target);
        }
    }

    fn on_failed(&mut self, generation: u64, message: String) {
        if self.current_mut(generation).is_none() {
            return;
        }
        self.fail(message);
    }

    /// The decode thread moved on to `entry_id`'s track at the end of the current one.
    fn on_advanced(
        &mut self,
        generation: u64,
        entry_id: EntryId,
        info: StreamInfo,
        boundary_samples: u64,
    ) {
        if !self.is_current(generation) {
            return;
        }
        let track = match self.preload.take_if(|p| p.entry_id == entry_id) {
            Some(preload) => preload.track,
            None => match self.queue.get(entry_id) {
                Some(entry) => entry.track.clone(),
                None => {
                    // Removed from the queue after the hand-off. Its boundary can only be
                    // stale, so the track itself is never shown.
                    log::warn!("hand-off to entry {entry_id}, which is no longer queued");
                    TrackRef {
                        id: String::new(),
                        key: String::new(),
                        size: 0,
                    }
                }
            },
        };
        if let Some(session) = self.session.as_mut() {
            session.advances = session.advances.wrapping_add(1);
            session.boundaries.push_back(Boundary {
                entry_id,
                track,
                info,
                at_samples: boundary_samples,
            });
        }
    }

    /// The preload thread finished opening `entry_id`.
    fn on_preloaded(
        &mut self,
        generation: u64,
        entry_id: EntryId,
        result: Result<Box<TrackDecoder>, String>,
    ) {
        if !self.is_current(generation) {
            return;
        }
        // Results for an entry that is no longer being preloaded are dropped with the decoder.
        let wanted = self
            .preload
            .as_ref()
            .is_some_and(|p| p.entry_id == entry_id && matches!(p.state, PreloadState::Opening));
        if !wanted {
            return;
        }
        let decoder = match result {
            Ok(decoder) => decoder,
            Err(message) => {
                log::warn!("preload of entry {entry_id} failed: {message}");
                self.preload = None;
                self.preload_failed = Some(entry_id);
                return;
            }
        };
        let info = decoder.info().clone();
        let (Some(session), Some(preload)) = (self.session.as_ref(), self.preload.as_mut()) else {
            return;
        };
        preload.info = Some(info);
        // The hand-off keeps the ring's format, so only a matching stream can continue it.
        let fits = match (session.info.as_ref(), preload.info.as_ref()) {
            (Some(current), Some(next)) => {
                current.sample_rate == next.sample_rate && current.channels == next.channels
            }
            _ => false,
        };
        if fits {
            // A failed send means the decode thread already exited; nothing will switch.
            let _ = session.cmd.send(DecoderCmd::Next { entry_id, decoder });
            preload.state = PreloadState::HandedOff;
        } else {
            preload.state = PreloadState::Ready(decoder);
        }
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
        session.seek_target = Some(to);
        let seq = session.seek_seq;
        let track_seq = session.advances;
        session.stats = Some(Arc::clone(&stats));
        session.base_frames = duration_to_frames(to, info.sample_rate);
        session.offset_samples = 0;
        session.sink_playing = false;
        session.last_underruns = 0;
        // A failed send means the decode thread already exited; nothing more will arrive.
        let _ = session.cmd.send(DecoderCmd::Seek {
            to,
            seq,
            track_seq,
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

    /// Starts `entry` as the current track. A Ready preload of the same entry is used as is;
    /// anything else is opened. Stops the previous session first.
    fn start_entry(&mut self, entry: QueueEntry, autoplay: bool, seek: Option<Duration>) {
        // Taken before stop_session, which would drop it.
        let preload = self.preload.take();
        self.stop_session();

        let QueueEntry {
            id: entry_id,
            track,
        } = entry;
        let track_id = track.id.clone();
        let source = match preload {
            Some(Preload {
                entry_id: preloaded,
                state: PreloadState::Ready(decoder),
                ..
            }) if preloaded == entry_id => DecoderSource::Ready(decoder),
            _ => DecoderSource::Open(track.clone()),
        };

        self.generation += 1;
        let generation = self.generation;
        let (cmd_tx, cmd_rx) = mpsc::channel::<DecoderCmd>();
        let spawned = thread::Builder::new()
            .name(DECODE_THREAD.to_owned())
            .spawn({
                let opener = Arc::clone(&self.opener);
                let msg_tx = self.self_tx.clone();
                move || decode_main(generation, source, opener, msg_tx, cmd_rx)
            });
        if let Err(e) = spawned {
            self.emit(PlayerEvent::Error {
                track_id: Some(track_id),
                message: format!("failed to start decode thread: {e}"),
            });
            self.set_state(PlayerState::Idle);
            return;
        }

        self.want_play = autoplay;
        self.session = Some(Session {
            generation,
            entry_id,
            track,
            info: None,
            cmd: cmd_tx,
            stats: None,
            base_frames: 0,
            offset_samples: 0,
            boundaries: VecDeque::new(),
            sink_playing: false,
            last_underruns: 0,
            pending_seek: seek,
            seek_seq: 0,
            seek_target: None,
            advances: 0,
            last_position_emit: Instant::now(),
        });
        self.set_state(PlayerState::Loading);
        self.emit(PlayerEvent::TrackChanged { entry_id, track_id });
    }

    /// Reports `message` for the current track, drops the session and goes idle.
    fn fail(&mut self, message: String) {
        let track_id = self.session.as_ref().map(|s| s.track.id.clone());
        self.emit(PlayerEvent::Error { track_id, message });
        self.stop_session();
        self.set_state(PlayerState::Idle);
    }

    /// Stops the session and goes idle.
    fn go_idle(&mut self) {
        self.stop_session();
        self.set_state(PlayerState::Idle);
    }

    /// Stops the sink, drops any preload and tells the decode thread to exit. Never joins the
    /// decode thread.
    fn stop_session(&mut self) {
        self.sink.stop();
        self.preload = None;
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

    /// Emits a `QueueChanged` snapshot of the queue.
    fn emit_queue(&self) {
        let entries = self.queue.entries_in_order().into_iter().cloned().collect();
        self.emit(PlayerEvent::QueueChanged {
            entries,
            current: self.queue.current_id(),
            shuffle: self.queue.shuffle(),
            repeat: self.queue.repeat(),
        });
    }

    /// After a queue edit: snapshot the queue and re-check the preload against it.
    fn queue_edited(&mut self) {
        self.emit_queue();
        self.sync_preload();
    }

    /// Drops the preload when auto-advance no longer wants it, cancelling a queued hand-off.
    fn sync_preload(&mut self) {
        let want = self.queue.peek_auto_next().map(|e| e.id);
        let Some(preload) = self.preload.as_ref() else {
            return;
        };
        if Some(preload.entry_id) == want {
            return;
        }
        if matches!(preload.state, PreloadState::HandedOff)
            && let Some(session) = self.session.as_ref()
        {
            let _ = session.cmd.send(DecoderCmd::CancelNext);
        }
        self.preload = None;
    }

    /// Opens the entry auto-advance will play, once the current track is near its end.
    fn maybe_start_preload(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        // While boundaries are queued the decode thread is already past the output: the next
        // hand-off target is only known once they are crossed.
        if self.preload.is_some() || !session.boundaries.is_empty() {
            return;
        }
        let (Some(info), Some(stats)) = (session.info.as_ref(), session.stats.as_ref()) else {
            return;
        };
        let Some(want) = self.queue.peek_auto_next() else {
            return;
        };
        if self.preload_failed == Some(want.id) {
            return;
        }
        let due = match info.duration {
            Some(duration) => {
                let position =
                    frames_to_duration(session.position_frames(info.channels), info.sample_rate);
                duration.saturating_sub(position) < self.config.preload_lead
            }
            None => stats.decoder_done.load(Ordering::Acquire),
        };
        if !due {
            return;
        }
        let entry_id = want.id;
        let track = want.track.clone();
        let generation = session.generation;
        let spawned = thread::Builder::new()
            .name(PRELOAD_THREAD.to_owned())
            .spawn({
                let opener = Arc::clone(&self.opener);
                let msg_tx = self.self_tx.clone();
                let thread_track = track.clone();
                move || preload_main(generation, entry_id, thread_track, opener, msg_tx)
            });
        match spawned {
            Ok(_) => {
                self.preload = Some(Preload {
                    entry_id,
                    track,
                    state: PreloadState::Opening,
                    info: None,
                });
            }
            Err(e) => {
                log::warn!("failed to start preload thread: {e}");
                self.preload_failed = Some(entry_id);
            }
        }
    }

    /// Crosses every boundary the output has reached (several can fall in one tick for short
    /// tracks). Returns true when a stale boundary restarted playback, which ends the tick.
    fn cross_boundaries(&mut self) -> bool {
        loop {
            let Some(session) = self.session.as_mut() else {
                return false;
            };
            let played = session
                .stats
                .as_ref()
                .map_or(0, |s| s.samples_played.load(Ordering::Acquire));
            if !session
                .boundaries
                .front()
                .is_some_and(|b| played >= b.at_samples)
            {
                return false;
            }
            let Some(boundary) = session.boundaries.pop_front() else {
                return false;
            };
            let ended_id = session.track.id.clone();
            self.emit(PlayerEvent::TrackEnded { track_id: ended_id });

            if self.queue.peek_auto_next().map(|e| e.id) != Some(boundary.entry_id) {
                // The queue changed after the hand-off: play what it now says instead.
                match self.queue.advance_auto().cloned() {
                    Some(entry) => {
                        self.start_entry(entry, self.want_play, None);
                        self.emit_queue();
                    }
                    None => self.go_idle(),
                }
                return true;
            }

            self.queue.advance_auto();
            let entry_id = boundary.entry_id;
            let at_samples = boundary.at_samples;
            let track_id = boundary.track.id.clone();
            let info = boundary.info;
            if let Some(session) = self.session.as_mut() {
                session.track = boundary.track;
                session.entry_id = entry_id;
                session.info = Some(info.clone());
                session.base_frames = 0;
                session.offset_samples = at_samples;
            }
            self.emit(PlayerEvent::TrackChanged {
                entry_id,
                track_id: track_id.clone(),
            });
            self.emit(PlayerEvent::TrackLoaded { track_id, info });
            self.emit_queue();
            self.emit_position();
            self.sync_preload();
        }
    }

    /// Frames → the output position of the current track, if there is a session with a stream.
    fn position(&self) -> Option<Duration> {
        let session = self.session.as_ref()?;
        let info = session.info.as_ref()?;
        Some(frames_to_duration(
            session.position_frames(info.channels),
            info.sample_rate,
        ))
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
        let Some(info) = session.info.as_ref() else {
            return;
        };
        if session.stats.is_none() || info.sample_rate == 0 || info.channels == 0 {
            return;
        }
        let position = frames_to_duration(session.position_frames(info.channels), info.sample_rate);
        let event = PlayerEvent::Position {
            track_id: session.track.id.clone(),
            position,
            duration: info.duration,
        };
        session.last_position_emit = Instant::now();
        self.emit(event);
    }

    /// The output has played out the last track: final position, `TrackEnded`, then the next
    /// queue entry (cold or preloaded) or idle.
    fn drain_end(&mut self) {
        self.emit_position();
        if let Some(track_id) = self.session.as_ref().map(|s| s.track.id.clone()) {
            self.emit(PlayerEvent::TrackEnded { track_id });
        }
        match self.queue.advance_auto().cloned() {
            Some(entry) => {
                self.start_entry(entry, true, None);
                self.emit_queue();
            }
            None => self.go_idle(),
        }
    }

    /// Periodic state machine step: boundary crossing, prefill → play, underrun → rebuffer,
    /// end detection, preload start, position reporting. Does nothing without a session that
    /// has stats.
    fn tick(&mut self) {
        if self.cross_boundaries() {
            return;
        }
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
                self.drain_end();
                return;
            }
        }

        self.maybe_start_preload();

        if matches!(
            self.state,
            PlayerState::Playing | PlayerState::Buffering | PlayerState::Paused
        ) && self.position_due()
        {
            self.emit_position();
        }
    }
}

/// Shuffle seed: `configured` when non-zero, otherwise nanoseconds since the Unix epoch.
fn resolve_seed(configured: u64) -> u64 {
    if configured != 0 {
        return configured;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |d| u64::try_from(d.as_nanos()).unwrap_or(1))
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
    /// Queued gapless continuation, switched to at EOF (or at once if EOF was already reached).
    next: Option<(EntryId, Box<TrackDecoder>)>,
    channels: usize,
    producer: Producer<i32>,
    stats: Arc<RingStats>,
    /// Current decoded block (interleaved) and the offset of the first unpushed sample.
    block: Vec<i32>,
    offset: usize,
    /// End of stream reached; only commands are processed from here on.
    eof: bool,
    /// Number of gapless switches performed (see `DecoderCmd::Seek::track_seq`).
    track_seq: u64,
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
                    // Only reached once the previous block is fully pushed (`offset == len`).
                    Ok(None) => match self.next.take() {
                        Some((entry_id, decoder)) => self.switch(entry_id, decoder),
                        None => {
                            self.stats.decoder_done.store(true, Ordering::Release);
                            report(
                                &self.msg_tx,
                                DecoderMsg::Finished {
                                    generation: self.generation,
                                },
                            );
                            self.eof = true;
                        }
                    },
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

    /// Gapless switch to `decoder`. Callers guarantee the previous decoder's last block is fully
    /// pushed, so nothing sits between its last sample and the first sample of `decoder`.
    fn switch(&mut self, entry_id: EntryId, decoder: Box<TrackDecoder>) {
        // Read before any sample of the new decoder is pushed.
        let boundary_samples = self.stats.samples_pushed.load(Ordering::Acquire);
        self.track_seq = self.track_seq.wrapping_add(1);
        self.decoder = *decoder;
        self.block.clear();
        self.offset = 0;
        report(
            &self.msg_tx,
            DecoderMsg::Advanced {
                generation: self.generation,
                entry_id,
                info: self.decoder.info().clone(),
                boundary_samples,
            },
        );
    }

    /// Applies one command. Returns false when the thread must exit.
    fn on_cmd(&mut self, cmd: DecoderCmd) -> bool {
        match cmd {
            DecoderCmd::Stop => false,
            // The first Start is consumed before the loop; any later one is stale.
            DecoderCmd::Start { .. } => true,
            DecoderCmd::Next { entry_id, decoder } => {
                if self.eof {
                    // EOF was already reported and every sample is pushed: hand off now and
                    // resume decoding. The EOF `decoder_done` is withdrawn for the new track.
                    self.stats.decoder_done.store(false, Ordering::Release);
                    self.eof = false;
                    self.switch(entry_id, decoder);
                } else {
                    self.next = Some((entry_id, decoder));
                }
                true
            }
            DecoderCmd::CancelNext => {
                self.next = None;
                true
            }
            DecoderCmd::Seek {
                to,
                seq,
                track_seq,
                producer,
                stats,
            } => {
                if track_seq != self.track_seq {
                    // The seek was meant for a track this thread has already finished. The
                    // controller cold-restarts that track and stops this thread.
                    report(
                        &self.msg_tx,
                        DecoderMsg::SeekRejected {
                            generation: self.generation,
                            seq,
                        },
                    );
                    return true;
                }
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

/// Opens `track` through `opener` and probes it. The buffer handle is only a view of the
/// source's read-ahead state; the decoder owns the source itself.
fn open_track(
    track: &TrackRef,
    opener: &dyn TrackOpener,
) -> crate::error::Result<(TrackDecoder, Option<BufferHandle>)> {
    let OpenedSource {
        source,
        ext_hint,
        buffer,
    } = opener.open(track)?;
    let decoder = TrackDecoder::new(source, ext_hint.as_deref())?;
    Ok((decoder, buffer))
}

/// Body of the `sonorus-decode` thread for one session.
fn decode_main(
    generation: u64,
    source: DecoderSource,
    opener: Arc<dyn TrackOpener>,
    msg_tx: Sender<Msg>,
    cmd_rx: Receiver<DecoderCmd>,
) {
    let (mut decoder, _buffer) = match source {
        DecoderSource::Open(track) => match open_track(&track, opener.as_ref()) {
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
        },
        DecoderSource::Ready(decoder) => (*decoder, None),
    };
    let info = decoder.info().clone();
    let channels = usize::from(info.channels);
    report(&msg_tx, DecoderMsg::Opened { generation, info });

    // Wait for the first ring: either a plain start or a seek before playback. A continuation
    // queued before the ring exists is kept for the decode loop.
    let mut next = None;
    let (producer, stats) = loop {
        match cmd_rx.recv() {
            Ok(DecoderCmd::Start { producer, stats }) => break (producer, stats),
            // No switch can happen before the loop starts, so `track_seq` is always 0 here.
            Ok(DecoderCmd::Seek {
                to,
                seq,
                producer,
                stats,
                ..
            }) => {
                if !seek_decoder(&mut decoder, to, seq, generation, &msg_tx) {
                    return;
                }
                break (producer, stats);
            }
            Ok(DecoderCmd::Next { entry_id, decoder }) => next = Some((entry_id, decoder)),
            Ok(DecoderCmd::CancelNext) => next = None,
            Ok(DecoderCmd::Stop) | Err(_) => return,
        }
    };

    let mut decode = DecodeLoop {
        generation,
        msg_tx,
        decoder,
        next,
        channels,
        producer,
        stats,
        block: Vec::new(),
        offset: 0,
        eof: false,
        track_seq: 0,
    };
    decode.run(&cmd_rx);
}

/// Body of a preload thread (`sonorus-preload`): opens and probes `track` without touching any
/// ring, then reports the decoder or the error as `Preloaded`.
fn preload_main(
    generation: u64,
    entry_id: EntryId,
    track: TrackRef,
    opener: Arc<dyn TrackOpener>,
    msg_tx: Sender<Msg>,
) {
    let result = open_track(&track, opener.as_ref())
        .map(|(decoder, _buffer)| Box::new(decoder))
        .map_err(|e| e.to_string());
    report(
        &msg_tx,
        DecoderMsg::Preloaded {
            generation,
            entry_id,
            result,
        },
    );
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
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;
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
            preload_lead: Duration::from_secs(10),
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

    /// Entry ids of every `TrackChanged`, in order.
    fn changed_ids(events: &[PlayerEvent]) -> Vec<EntryId> {
        events
            .iter()
            .filter_map(|e| match e {
                PlayerEvent::TrackChanged { entry_id, .. } => Some(*entry_id),
                _ => None,
            })
            .collect()
    }

    /// `(entry ids in play order, current, shuffle, repeat)` of every `QueueChanged`.
    type QueueSnapshot = (Vec<EntryId>, Option<EntryId>, bool, RepeatMode);

    fn queue_snapshots(events: &[PlayerEvent]) -> Vec<QueueSnapshot> {
        events
            .iter()
            .filter_map(|e| match e {
                PlayerEvent::QueueChanged {
                    entries,
                    current,
                    shuffle,
                    repeat,
                } => Some((
                    entries.iter().map(|x| x.id).collect(),
                    *current,
                    *shuffle,
                    *repeat,
                )),
                _ => None,
            })
            .collect()
    }

    /// Waits for the next `TrackChanged` and returns its entry id.
    fn next_track_changed(rx: &Receiver<PlayerEvent>) -> Option<EntryId> {
        let events = collect_until(rx, |e| !changed_ids(e).is_empty());
        changed_ids(&events).last().copied()
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

    /// Fully decodes a fixture through `TrackDecoder` (the reference output).
    fn decode_fixture(name: &str, ext: &str) -> Vec<i32> {
        let file = std::fs::File::open(format!("{FIXTURES}/{name}")).expect("fixture");
        let mut dec = TrackDecoder::new(Box::new(file), Some(ext)).expect("probe fixture");
        let mut all = Vec::new();
        let mut block = Vec::<i32>::new();
        while dec.next_block(&mut block).expect("decode").is_some() {
            all.extend_from_slice(&block);
        }
        all
    }

    /// Names of the decode-thread messages, for readable assertions.
    fn msg_names(msgs: &[DecoderMsg]) -> Vec<&'static str> {
        msgs.iter()
            .map(|m| match m {
                DecoderMsg::Opened { .. } => "Opened",
                DecoderMsg::Seeked { .. } => "Seeked",
                DecoderMsg::Finished { .. } => "Finished",
                DecoderMsg::Failed { .. } => "Failed",
                DecoderMsg::Advanced { .. } => "Advanced",
                DecoderMsg::Preloaded { .. } => "Preloaded",
                DecoderMsg::SeekRejected { .. } => "SeekRejected",
            })
            .collect()
    }

    #[test]
    fn next_track_switches_gaplessly_at_eof() {
        const FLAC_SAMPLES: u64 = 28_800 * 2;
        let flac = decode_fixture("tone_24_96.flac", "flac");
        let wav = decode_fixture("tone_24_96.wav", "wav");
        assert_eq!(flac.len() as u64, FLAC_SAMPLES);
        let mut expected = flac;
        expected.extend_from_slice(&wav);

        let next_file = std::fs::File::open(format!("{FIXTURES}/tone_24_96.wav")).expect("wav");
        let preloaded = TrackDecoder::new(Box::new(next_file), Some("wav")).expect("preload");

        // A small ring keeps the decode thread blocked on the consumer, so `Next` is always
        // queued well before the first track reaches EOF.
        let (producer, mut consumer) = rtrb::RingBuffer::<i32>::new(4096);
        let stats = Arc::new(RingStats::new());
        let (cmd_tx, cmd_rx) = mpsc::channel::<DecoderCmd>();
        let (msg_tx, msg_rx) = mpsc::channel::<Msg>();
        let handle = thread::spawn({
            let opener = fixture_opener();
            move || {
                decode_main(
                    1,
                    DecoderSource::Open(track("tone_24_96.flac")),
                    opener,
                    msg_tx,
                    cmd_rx,
                )
            }
        });
        cmd_tx
            .send(DecoderCmd::Start {
                producer,
                stats: Arc::clone(&stats),
            })
            .expect("send start");
        cmd_tx
            .send(DecoderCmd::Next {
                entry_id: 2,
                decoder: Box::new(preloaded),
            })
            .expect("send next");

        // Drain on this thread until the decode thread reports its end (or fails).
        let deadline = Instant::now() + DEADLINE;
        let mut samples: Vec<i32> = Vec::new();
        let mut msgs: Vec<DecoderMsg> = Vec::new();
        loop {
            while let Ok(s) = consumer.pop() {
                samples.push(s);
            }
            while let Ok(Msg::Dec(m)) = msg_rx.try_recv() {
                msgs.push(m);
            }
            if msgs
                .iter()
                .any(|m| matches!(m, DecoderMsg::Finished { .. } | DecoderMsg::Failed { .. }))
            {
                break;
            }
            assert!(Instant::now() < deadline, "decode did not finish");
            thread::sleep(Duration::from_millis(1));
        }
        // Finished is sent after the last push, so everything is in the ring by now.
        while let Ok(s) = consumer.pop() {
            samples.push(s);
        }
        cmd_tx.send(DecoderCmd::Stop).expect("send stop");
        handle.join().expect("decode thread");

        assert_eq!(
            msg_names(&msgs),
            vec!["Opened", "Advanced", "Finished"],
            "unexpected decode messages"
        );
        let advanced: Vec<(EntryId, u64)> = msgs
            .iter()
            .filter_map(|m| match m {
                DecoderMsg::Advanced {
                    entry_id,
                    boundary_samples,
                    ..
                } => Some((*entry_id, *boundary_samples)),
                _ => None,
            })
            .collect();
        assert_eq!(advanced, vec![(2, FLAC_SAMPLES)]);
        assert!(
            samples == expected,
            "decoded {} samples, expected {}",
            samples.len(),
            expected.len()
        );
    }

    #[test]
    fn seek_for_a_track_already_switched_away_from_is_rejected() {
        let next_file = std::fs::File::open(format!("{FIXTURES}/tone_24_96.wav")).expect("wav");
        let preloaded = TrackDecoder::new(Box::new(next_file), Some("wav")).expect("preload");
        let (producer, mut consumer) = rtrb::RingBuffer::<i32>::new(1 << 17);
        let (cmd_tx, cmd_rx) = mpsc::channel::<DecoderCmd>();
        let (msg_tx, msg_rx) = mpsc::channel::<Msg>();
        let handle = thread::spawn({
            let opener = fixture_opener();
            move || {
                let source = DecoderSource::Open(track("tone_24_96.flac"));
                decode_main(1, source, opener, msg_tx, cmd_rx)
            }
        });
        let stats = Arc::new(RingStats::new());
        cmd_tx
            .send(DecoderCmd::Next {
                entry_id: 2,
                decoder: Box::new(preloaded),
            })
            .expect("send next");
        cmd_tx
            .send(DecoderCmd::Start { producer, stats })
            .expect("send start");

        let wait_for =
            |name: &str, msgs: &mut Vec<DecoderMsg>, consumer: &mut rtrb::Consumer<i32>| {
                let deadline = Instant::now() + DEADLINE;
                while !msg_names(msgs).contains(&name) {
                    while consumer.pop().is_ok() {}
                    while let Ok(Msg::Dec(m)) = msg_rx.try_recv() {
                        msgs.push(m);
                    }
                    assert!(
                        Instant::now() < deadline,
                        "no {name}: {:?}",
                        msg_names(msgs)
                    );
                    thread::sleep(Duration::from_millis(1));
                }
            };
        let mut msgs = Vec::new();
        wait_for("Finished", &mut msgs, &mut consumer);

        // The controller has not seen the switch yet (track_seq 0): the seek must be refused.
        let (stale_producer, _stale_consumer) = rtrb::RingBuffer::<i32>::new(4096);
        cmd_tx
            .send(DecoderCmd::Seek {
                to: Duration::from_millis(100),
                seq: 7,
                track_seq: 0,
                producer: stale_producer,
                stats: Arc::new(RingStats::new()),
            })
            .expect("send seek");
        wait_for("SeekRejected", &mut msgs, &mut consumer);
        assert!(
            msgs.iter()
                .any(|m| matches!(m, DecoderMsg::SeekRejected { seq: 7, .. })),
            "{:?}",
            msg_names(&msgs)
        );

        // With the switch acknowledged (track_seq 1) the same seek is applied to the new track.
        let (fresh_producer, mut fresh_consumer) = rtrb::RingBuffer::<i32>::new(4096);
        cmd_tx
            .send(DecoderCmd::Seek {
                to: Duration::from_millis(100),
                seq: 8,
                track_seq: 1,
                producer: fresh_producer,
                stats: Arc::new(RingStats::new()),
            })
            .expect("send seek");
        wait_for("Seeked", &mut msgs, &mut fresh_consumer);
        cmd_tx.send(DecoderCmd::Stop).expect("send stop");
        handle.join().expect("decode thread");
    }

    /// Output that captures every sample it consumes instead of discarding them. It consumes
    /// only while playing, and never zero-fills.
    struct CaptureSink {
        captured: Arc<Mutex<Vec<i32>>>,
        quit: Arc<AtomicBool>,
        playing: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl CaptureSink {
        fn new(captured: Arc<Mutex<Vec<i32>>>) -> Self {
            Self {
                captured,
                quit: Arc::new(AtomicBool::new(false)),
                playing: Arc::new(AtomicBool::new(false)),
                thread: None,
            }
        }

        fn halt(&mut self) {
            self.quit.store(true, Ordering::Release);
            self.playing.store(false, Ordering::Release);
            if let Some(handle) = self.thread.take() {
                let _ = handle.join();
            }
        }
    }

    impl Drop for CaptureSink {
        fn drop(&mut self) {
            self.halt();
        }
    }

    impl OutputSink for CaptureSink {
        fn start(
            &mut self,
            _format: OutputFormat,
            mut consumer: rtrb::Consumer<i32>,
            stats: Arc<RingStats>,
        ) -> Result<()> {
            self.halt();
            self.quit = Arc::new(AtomicBool::new(false));
            self.playing = Arc::new(AtomicBool::new(false));
            let quit = Arc::clone(&self.quit);
            let playing = Arc::clone(&self.playing);
            let captured = Arc::clone(&self.captured);
            let handle = thread::Builder::new()
                .name("sonorus-capture".to_owned())
                .spawn(move || {
                    while !quit.load(Ordering::Acquire) {
                        let slots = consumer.slots();
                        if playing.load(Ordering::Acquire)
                            && slots > 0
                            && let Ok(chunk) = consumer.read_chunk(slots)
                        {
                            let (a, b) = chunk.as_slices();
                            let n = a.len() + b.len();
                            if let Ok(mut out) = captured.lock() {
                                out.extend_from_slice(a);
                                out.extend_from_slice(b);
                            }
                            chunk.commit_all();
                            stats.samples_played.fetch_add(n as u64, Ordering::Release);
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                })
                .expect("spawn capture thread");
            self.thread = Some(handle);
            Ok(())
        }

        fn play(&mut self) -> Result<()> {
            self.playing.store(true, Ordering::Release);
            Ok(())
        }

        fn pause(&mut self) -> Result<()> {
            self.playing.store(false, Ordering::Release);
            Ok(())
        }

        fn stop(&mut self) {
            self.halt();
        }

        fn latency(&self) -> Duration {
            Duration::from_millis(1)
        }

        fn name(&self) -> &str {
            "capture"
        }
    }

    #[test]
    fn flac_to_wav_is_gapless_and_bit_exact() {
        let mut expected = decode_fixture("tone_24_96.flac", "flac");
        expected.extend(decode_fixture("tone_24_96.wav", "wav"));

        let captured = Arc::new(Mutex::new(Vec::<i32>::new()));
        let config = PlayerConfig {
            output_kind: SampleKind::I32,
            ..test_config()
        };
        let sink = Box::new(CaptureSink::new(Arc::clone(&captured)));
        let (player, rx) = Player::new(sink, fixture_opener(), config);
        player.set_queue(
            vec![track("tone_24_96.flac"), track("tone_24_96.wav")],
            0,
            true,
        );
        let events = collect_until(&rx, ends_idle);
        assert!(ends_idle(&events), "not idle at end: {events:#?}");
        assert_eq!(changed_ids(&events), vec![1, 2], "{events:#?}");

        // The controller stops the sink before going idle, so nothing is appended after this.
        let got = captured.lock().expect("capture lock").clone();
        assert!(
            got == expected,
            "captured {} samples, expected {}; first difference at {:?}",
            got.len(),
            expected.len(),
            got.iter().zip(&expected).position(|(a, b)| a != b)
        );

        let first_end = events
            .iter()
            .position(|e| matches!(e, PlayerEvent::TrackEnded { .. }))
            .expect("first TrackEnded");
        let before = &events[..first_end];
        let buffering = before
            .iter()
            .filter(|e| is_state(e, PlayerState::Buffering))
            .count();
        let playing = before
            .iter()
            .filter(|e| is_state(e, PlayerState::Playing))
            .count();
        assert_eq!((buffering, playing), (1, 1), "{events:#?}");

        let second_change = events
            .iter()
            .enumerate()
            .skip(first_end)
            .find(|(_, e)| matches!(e, PlayerEvent::TrackChanged { .. }))
            .map(|(i, _)| i)
            .expect("second TrackChanged");
        let gap = &events[first_end..second_change];
        assert!(
            !gap.iter()
                .any(|e| is_state(e, PlayerState::Buffering) || is_state(e, PlayerState::Loading)),
            "state change between tracks: {gap:#?}"
        );
    }

    #[test]
    fn format_change_cold_starts_at_new_rate() {
        let (player, rx) = player_with(fixture_opener(), 4.0);
        player.set_queue(
            vec![track("tone_24_96.flac"), track("tone_16_44.flac")],
            0,
            true,
        );
        let events = collect_until(&rx, ends_idle);
        assert!(ends_idle(&events), "not idle at end: {events:#?}");

        let ended = events
            .iter()
            .filter(|e| matches!(e, PlayerEvent::TrackEnded { .. }))
            .count();
        assert_eq!(ended, 2, "{events:#?}");
        let rates: Vec<u32> = events
            .iter()
            .filter_map(|e| match e {
                PlayerEvent::TrackLoaded { info, .. } => Some(info.sample_rate),
                _ => None,
            })
            .collect();
        assert_eq!(rates, vec![96_000, 44_100], "{events:#?}");
    }

    #[test]
    fn repeat_one_replays_the_same_entry() {
        let (player, rx) = player_with(fixture_opener(), 4.0);
        player.set_queue(vec![track("tone_24_96.flac")], 0, true);
        player.set_repeat(RepeatMode::One);
        let events = collect_until(&rx, |e| changed_ids(e).len() >= 2);
        player.stop();

        let ids = changed_ids(&events);
        assert!(ids.len() >= 2, "fewer than two starts: {events:#?}");
        assert!(ids.iter().all(|&id| id == 1), "{ids:?}");
    }

    #[test]
    fn transport_and_queue_views_follow_the_queue() {
        let (player, rx) = player_with(fixture_opener(), 4.0);
        player.set_queue(
            vec![
                track("tone_24_96.flac"),
                track("tone_16_44.flac"),
                track("tone_24_96.wav"),
            ],
            0,
            false,
        );
        let _ = collect_until(&rx, |e| has_state(e, PlayerState::Paused));

        player.next();
        assert_eq!(next_track_changed(&rx), Some(2));
        player.previous();
        assert_eq!(next_track_changed(&rx), Some(1));
        player.jump_to(3);
        assert_eq!(next_track_changed(&rx), Some(3));

        player.set_shuffle(true);
        let events = collect_until(&rx, |e| queue_snapshots(e).last().is_some_and(|s| s.2));
        let (ids, current, shuffle, repeat) = queue_snapshots(&events)
            .last()
            .cloned()
            .expect("shuffle snapshot");
        assert!(shuffle, "{events:#?}");
        assert_eq!(current, Some(3));
        assert_eq!(
            ids.first(),
            Some(&3),
            "current entry leads the shuffled order"
        );
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![1, 2, 3]);
        assert_eq!(repeat, RepeatMode::Off);

        player.set_repeat(RepeatMode::All);
        let events = collect_until(&rx, |e| {
            queue_snapshots(e)
                .last()
                .is_some_and(|s| s.3 == RepeatMode::All)
        });
        let (_, current, shuffle, repeat) = queue_snapshots(&events)
            .last()
            .cloned()
            .expect("repeat snapshot");
        assert_eq!((current, shuffle, repeat), (Some(3), true, RepeatMode::All));
        player.stop();
    }

    #[test]
    fn removed_upcoming_entry_is_never_played() {
        let (player, rx) = player_with(fixture_opener(), 4.0);
        player.set_queue(
            vec![
                track("tone_24_96.flac"),
                track("tone_24_96.wav"),
                track("tone_24_96.flac"),
            ],
            0,
            true,
        );
        // Remove the upcoming wav (entry 2) once the first track plays. Its hand-off has
        // normally happened by then; either way it must never become the current entry.
        let mut events = collect_until(&rx, |e| has_state(e, PlayerState::Playing));
        player.remove(2);
        events.extend(collect_until(&rx, ends_idle));
        assert!(ends_idle(&events), "not idle at end: {events:#?}");
        assert_eq!(changed_ids(&events), vec![1, 3], "{events:#?}");
    }
}

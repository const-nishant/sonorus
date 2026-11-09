//! Event delivery to the Android shell.
//!
//! The player's controller thread sends [`PlayerEvent`]s over a channel. The "sonorus-events"
//! thread (the pump) receives them, does the play-time accounting that feeds the listening
//! history, converts each event to a [`CoreEvent`] and hands it to the foreign [`EventListener`].
//! Scan events are emitted from the thread that runs the scan.
//!
//! Listeners are called synchronously on those threads. Implementations must return quickly and
//! hop to their own thread. A listener must not call back into [`crate::SonorusCore`] in a way
//! that waits for the event thread, because the event thread may be the one running the listener.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use sonorus_engine::player::{PlayerEvent, PlayerState, SleepTimer};
use sonorus_library::Library;

use crate::types::{CoreEvent, SleepTimerInfo, duration_ms, parse_track_id, player_event};

/// Name of the thread that receives player events.
const EVENT_THREAD: &str = "sonorus-events";

/// Listened time needed to count a play, when the track is at least one minute long.
pub(crate) const MIN_PLAY_MS: u64 = 30_000;
/// Largest jump between two position reports that still counts as listening. Bigger jumps are
/// seeks and are not counted.
pub(crate) const MAX_POSITION_STEP_MS: u64 = 2_000;

/// Receives events from the core.
///
/// Called on the "sonorus-events" thread, or on the thread running a scan. Implementations must
/// return quickly and hand the event to their own thread. A panic inside `on_event` is caught
/// and logged; it does not stop the core.
#[uniffi::export(foreign)]
pub trait EventListener: Send + Sync {
    /// Delivers one event.
    fn on_event(&self, event: CoreEvent);
}

/// The listener registered with the core, shared between the core and the event thread.
#[derive(Clone, Default)]
pub(crate) struct ListenerSlot(Arc<RwLock<Option<Arc<dyn EventListener>>>>);

impl ListenerSlot {
    /// Registers `listener`, replacing any previous one.
    pub(crate) fn set(&self, listener: Arc<dyn EventListener>) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = Some(listener);
    }

    /// Removes the registered listener.
    pub(crate) fn clear(&self) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Delivers `event` to the registered listener, if any. The lock is released before the
    /// listener runs, so a listener may register or clear listeners without deadlocking.
    pub(crate) fn emit(&self, event: CoreEvent) {
        let listener = self
            .0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(listener) = listener else {
            return;
        };
        if catch_unwind(AssertUnwindSafe(|| listener.on_event(event))).is_err() {
            log::error!("event listener panicked; the event was dropped");
        }
    }
}

/// Player state the core keeps for its status getters: the gain of the current track and the
/// sleep timer.
///
/// The event pump updates it from every player event before the event is forwarded. The sleep
/// timer is also set by a request, before the request reaches the player; the player's next
/// `SleepTimerChanged` event then confirms or replaces it, so the pump keeps it in step with the
/// player.
#[derive(Debug)]
pub(crate) struct PlayerStatus {
    /// Gain of the last `GainApplied`, in dB. `None` when no gain is applied.
    replay_gain_db: Mutex<Option<f32>>,
    /// The sleep timer. `At` holds the deadline.
    sleep: Mutex<SleepTimer>,
}

impl Default for PlayerStatus {
    fn default() -> Self {
        Self {
            replay_gain_db: Mutex::new(None),
            sleep: Mutex::new(SleepTimer::Off),
        }
    }
}

impl PlayerStatus {
    /// Updates the state from one player event. `now` is when the event was received, and anchors
    /// the deadline of a timed sleep.
    pub(crate) fn observe(&self, event: &PlayerEvent, now: Instant) {
        match event {
            PlayerEvent::GainApplied { db } => self.set_replay_gain_db(*db),
            PlayerEvent::SleepTimerChanged {
                remaining,
                end_of_track,
            } => self.set_sleep(timer_from_event(*remaining, *end_of_track, now)),
            _ => {}
        }
    }

    /// The gain of the current track in dB, `None` when no gain is applied.
    pub(crate) fn replay_gain_db(&self) -> Option<f32> {
        *lock(&self.replay_gain_db)
    }

    /// Sets the gain of the current track, `None` for no gain.
    pub(crate) fn set_replay_gain_db(&self, db: Option<f32>) {
        *lock(&self.replay_gain_db) = db;
    }

    /// Records a sleep timer request. The caller sends the same timer to the player.
    pub(crate) fn request_sleep_timer(&self, timer: SleepTimer) {
        self.set_sleep(timer);
    }

    /// The sleep timer a new player starts with. A timed sleep whose deadline has passed is
    /// dropped (no player was running to pause when it passed) and reported as off from now on.
    pub(crate) fn sleep_timer_for_new_player(&self, now: Instant) -> SleepTimer {
        let mut timer = lock(&self.sleep);
        *timer = live_timer(*timer, now);
        *timer
    }

    /// The sleep timer for `SonorusCore::sleep_timer`. An expired timed sleep is dropped, as in
    /// `sleep_timer_for_new_player`.
    pub(crate) fn sleep_timer_info(&self, now: Instant) -> SleepTimerInfo {
        let mut timer = lock(&self.sleep);
        *timer = live_timer(*timer, now);
        sleep_info(*timer, now)
    }

    fn set_sleep(&self, timer: SleepTimer) {
        *lock(&self.sleep) = timer;
    }
}

/// Locks `m`, recovering the value from a poisoned lock: the state is plain data.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The timer a player event describes: a timed sleep ends at `now` plus the remaining time.
fn timer_from_event(remaining: Option<Duration>, end_of_track: bool, now: Instant) -> SleepTimer {
    match (remaining, end_of_track) {
        (Some(left), _) => SleepTimer::At(now + left),
        (None, true) => SleepTimer::EndOfTrack,
        (None, false) => SleepTimer::Off,
    }
}

/// `timer`, or `Off` when it is a timed sleep whose deadline is at or before `now`.
fn live_timer(timer: SleepTimer, now: Instant) -> SleepTimer {
    match timer {
        SleepTimer::At(deadline) if deadline <= now => SleepTimer::Off,
        other => other,
    }
}

/// The status view of `timer` at `now`: the time left on a timed sleep, and whether an
/// end-of-track sleep is set.
fn sleep_info(timer: SleepTimer, now: Instant) -> SleepTimerInfo {
    SleepTimerInfo {
        remaining_ms: match timer {
            SleepTimer::At(deadline) => Some(duration_ms(deadline.saturating_duration_since(now))),
            SleepTimer::Off | SleepTimer::EndOfTrack => None,
        },
        end_of_track: timer == SleepTimer::EndOfTrack,
    }
}

/// Play-time accounting for the track currently playing.
///
/// Position reports are cumulative. Each step between two consecutive reports of the same track
/// adds its length to the listened time, unless it is zero, backwards, or longer than
/// [`MAX_POSITION_STEP_MS`] (a seek). A play counts when the listened time reaches
/// `min(MIN_PLAY_MS, duration / 2)`.
#[derive(Debug, Default)]
pub(crate) struct PlayTracker {
    current: Option<Current>,
}

#[derive(Debug)]
struct Current {
    track_id: i64,
    listened_ms: u64,
    last_position_ms: Option<u64>,
    duration_ms: Option<u64>,
}

impl PlayTracker {
    /// Starts tracking `track_id` from position 0 (a new queue entry always starts at the
    /// beginning, so the time up to the first position report counts). Any track still tracked
    /// is dropped, so call [`PlayTracker::finish`] first to account for it.
    pub(crate) fn start(&mut self, track_id: i64) {
        self.start_at(track_id, Some(0));
    }

    fn start_at(&mut self, track_id: i64, last_position_ms: Option<u64>) {
        self.current = Some(Current {
            track_id,
            listened_ms: 0,
            last_position_ms,
            duration_ms: None,
        });
    }

    /// Records a position report for `track_id`. Reports for other tracks are ignored. When no
    /// track is tracked, `track_id` starts being tracked.
    pub(crate) fn position(&mut self, track_id: i64, pos_ms: u64, duration_ms: Option<u64>) {
        if self.current.is_none() {
            // Joined mid-track (no TrackChanged seen): the start position is unknown.
            self.start_at(track_id, None);
        }
        let Some(current) = self.current.as_mut().filter(|c| c.track_id == track_id) else {
            return;
        };
        if let Some(last) = current.last_position_ms
            && pos_ms > last
        {
            let step = pos_ms - last;
            if step <= MAX_POSITION_STEP_MS {
                current.listened_ms = current.listened_ms.saturating_add(step);
            }
        }
        current.last_position_ms = Some(pos_ms);
        current.duration_ms = duration_ms;
    }

    /// Stops tracking the current track. Returns `(track_id, listened_ms)` when it reached the
    /// play threshold, `None` otherwise or when no track was tracked.
    pub(crate) fn finish(&mut self) -> Option<(i64, u64)> {
        let current = self.current.take()?;
        let threshold = match current.duration_ms {
            Some(duration) => (duration / 2).min(MIN_PLAY_MS),
            None => MIN_PLAY_MS,
        };
        (current.listened_ms >= threshold).then_some((current.track_id, current.listened_ms))
    }
}

/// Writes a finished play to the history. Failures are logged and otherwise ignored.
fn record_finished_play(library: &Library, finished: Option<(i64, u64)>) {
    let Some((track_id, listened_ms)) = finished else {
        return;
    };
    match library.track(track_id) {
        Ok(track) => {
            if let Err(e) = library.record_play(&track.key, listened_ms) {
                log::warn!("recording play of track {track_id} failed: {e}");
            }
        }
        Err(e) => log::warn!("play of track {track_id} not recorded: {e}"),
    }
}

/// Starts the "sonorus-events" thread. It runs until `rx` disconnects, which happens when the
/// [`sonorus_engine::player::Player`] is dropped.
pub(crate) fn spawn_pump(
    rx: Receiver<PlayerEvent>,
    library: Arc<Library>,
    listener: ListenerSlot,
    status: Arc<PlayerStatus>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name(EVENT_THREAD.to_owned())
        .spawn(move || {
            let mut tracker = PlayTracker::default();
            for event in rx {
                status.observe(&event, Instant::now());
                match &event {
                    PlayerEvent::TrackChanged { track_id, .. } => {
                        record_finished_play(&library, tracker.finish());
                        if let Some(id) = parse_track_id(track_id) {
                            tracker.start(id);
                        }
                    }
                    PlayerEvent::Position {
                        track_id,
                        position,
                        duration,
                    } => {
                        if let Some(id) = parse_track_id(track_id) {
                            tracker.position(id, duration_ms(*position), duration.map(duration_ms));
                        }
                    }
                    PlayerEvent::TrackEnded { .. } => {
                        record_finished_play(&library, tracker.finish());
                    }
                    PlayerEvent::StateChanged(PlayerState::Idle) => {
                        record_finished_play(&library, tracker.finish());
                    }
                    _ => {}
                }
                if let Some(core_event) = player_event(event, None, &library) {
                    listener.emit(core_event);
                }
            }
            record_finished_play(&library, tracker.finish());
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn tracker_accumulates_small_steps() {
        let mut t = PlayTracker::default();
        t.start(1);
        t.position(1, 0, Some(100_000));
        for pos in (250..=40_000).step_by(250) {
            t.position(1, pos, Some(100_000));
        }
        assert_eq!(t.finish(), Some((1, 40_000)));
    }

    #[test]
    fn tracker_counts_from_track_start() {
        // A 300 ms track with a single report before it ends still counts.
        let mut t = PlayTracker::default();
        t.start(1);
        t.position(1, 240, Some(300));
        assert_eq!(t.finish(), Some((1, 240)));
    }

    #[test]
    fn tracker_joined_mid_track_has_unknown_origin() {
        let mut t = PlayTracker::default();
        t.position(1, 1_000, Some(1_000_000));
        assert_eq!(
            listened(&t),
            0,
            "no TrackChanged seen: first report only sets the origin"
        );
        t.position(1, 1_250, Some(1_000_000));
        assert_eq!(listened(&t), 250);
    }

    #[test]
    fn tracker_first_position_has_no_predecessor() {
        let mut t = PlayTracker::default();
        t.start(1);
        t.position(1, 5_000, Some(100_000));
        assert_eq!(
            t.finish(),
            None,
            "a 5 s first report is a seek, not listening"
        );
    }

    /// Listened time of the current track, for asserting what was not counted.
    fn listened(t: &PlayTracker) -> u64 {
        t.current.as_ref().map_or(0, |c| c.listened_ms)
    }

    #[test]
    fn tracker_ignores_seek_jumps() {
        let mut t = PlayTracker::default();
        t.start(1);
        t.position(1, 0, Some(60_000));
        t.position(1, 1_000, Some(60_000));
        t.position(1, 2_000, Some(60_000));
        t.position(1, 3_000, Some(60_000));
        assert_eq!(listened(&t), 3_000);
        // A jump to 60 s is a seek: the 57 s step is not counted.
        t.position(1, 60_000, Some(60_000));
        assert_eq!(listened(&t), 3_000);
        t.position(1, 60_250, Some(60_000));
        assert_eq!(listened(&t), 3_250);
        // Exactly 2 s is still a normal step.
        t.position(1, 62_250, Some(60_000));
        assert_eq!(listened(&t), 5_250);
    }

    #[test]
    fn tracker_ignores_backwards_and_repeated_positions() {
        let mut t = PlayTracker::default();
        t.start(1);
        t.position(1, 1_000, None);
        assert_eq!(listened(&t), 1_000, "counted from the start at 0");
        t.position(1, 1_000, None);
        assert_eq!(listened(&t), 1_000, "repeat adds nothing");
        t.position(1, 500, None);
        assert_eq!(listened(&t), 1_000, "backwards adds nothing");
        t.position(1, 1_500, None);
        assert_eq!(listened(&t), 2_000, "counted from the last report, 500");
        assert_eq!(t.finish(), None);

        t.start(1);
        t.position(1, 0, Some(1_000));
        t.position(1, 900, Some(1_000));
        assert_eq!(t.finish(), Some((1, 900)));
    }

    #[test]
    fn tracker_ignores_other_track_ids() {
        let mut t = PlayTracker::default();
        t.start(1);
        play_to(&mut t, 1, 29_000, Some(100_000));
        // Reports for track 2 are neither counted nor allowed to move the position of track 1.
        t.position(2, 5_000, Some(100_000));
        t.position(2, 29_500, Some(100_000));
        assert_eq!(listened(&t), 29_000);
        t.position(1, 30_250, Some(100_000));
        assert_eq!(listened(&t), 30_250);
        assert_eq!(t.finish(), Some((1, 30_250)));
    }

    /// Reports positions from 0 to `end_ms` in steps of at most one second, so that no step is
    /// mistaken for a seek.
    fn play_to(t: &mut PlayTracker, id: i64, end_ms: u64, duration: Option<u64>) {
        t.position(id, 0, duration);
        let mut pos = 0;
        while pos < end_ms {
            pos = (pos + 1_000).min(end_ms);
            t.position(id, pos, duration);
        }
    }

    #[test]
    fn tracker_threshold_is_half_duration_up_to_thirty_seconds() {
        // 300 ms track: threshold 150 ms.
        let mut t = PlayTracker::default();
        t.start(1);
        play_to(&mut t, 1, 149, Some(300));
        assert_eq!(t.finish(), None);
        t.start(1);
        play_to(&mut t, 1, 150, Some(300));
        assert_eq!(t.finish(), Some((1, 150)));

        // 4 min track: threshold capped at 30 s.
        let long = Some(240_000);
        let mut t = PlayTracker::default();
        t.start(2);
        play_to(&mut t, 2, 29_750, long);
        assert_eq!(t.finish(), None);
        t.start(2);
        play_to(&mut t, 2, 30_000, long);
        assert_eq!(t.finish(), Some((2, 30_000)));
    }

    #[test]
    fn tracker_unknown_duration_needs_thirty_seconds() {
        let mut t = PlayTracker::default();
        t.start(3);
        play_to(&mut t, 3, 29_999, None);
        assert_eq!(t.finish(), None);
        t.start(3);
        play_to(&mut t, 3, 30_000, None);
        assert_eq!(t.finish(), Some((3, 30_000)));
    }

    #[test]
    fn tracker_finish_resets() {
        let mut t = PlayTracker::default();
        assert_eq!(t.finish(), None, "nothing tracked");
        t.start(1);
        t.position(1, 0, Some(1_000));
        t.position(1, 900, Some(1_000));
        assert_eq!(t.finish(), Some((1, 900)));
        assert_eq!(t.finish(), None, "finish takes the track");
        // A position with no current track starts a new one from scratch.
        t.position(4, 0, Some(1_000));
        t.position(4, 900, Some(1_000));
        assert_eq!(t.finish(), Some((4, 900)));
    }

    #[test]
    fn tracker_start_discards_previous_track() {
        let mut t = PlayTracker::default();
        t.start(1);
        t.position(1, 0, Some(1_000));
        t.position(1, 900, Some(1_000));
        t.start(2);
        assert_eq!(t.finish(), None, "track 1 was dropped without accounting");
    }

    #[test]
    fn sleep_timer_from_events_counts_down_and_clears() {
        let status = PlayerStatus::default();
        let t0 = Instant::now();
        // The player reports a 60 s timed sleep: the deadline is 60 s after the report.
        status.observe(
            &PlayerEvent::SleepTimerChanged {
                remaining: Some(Duration::from_secs(60)),
                end_of_track: false,
            },
            t0,
        );
        let at = |secs: u64| status.sleep_timer_info(t0 + Duration::from_secs(secs));
        assert_eq!(at(0).remaining_ms, Some(60_000));
        assert_eq!(at(20).remaining_ms, Some(40_000));
        assert!(!at(20).end_of_track);
        assert_eq!(at(59).remaining_ms, Some(1_000));
        // Past the deadline the timer is gone, and stays gone.
        assert_eq!(at(61).remaining_ms, None);
        assert_eq!(at(10).remaining_ms, None, "expired timer is not revived");

        // The player fired a timer: nothing left to show.
        status.observe(
            &PlayerEvent::SleepTimerChanged {
                remaining: None,
                end_of_track: false,
            },
            t0,
        );
        assert_eq!(
            at(0),
            SleepTimerInfo {
                remaining_ms: None,
                end_of_track: false
            }
        );
    }

    #[test]
    fn end_of_track_sleep_is_reported_until_cleared() {
        let status = PlayerStatus::default();
        let now = Instant::now();
        status.observe(
            &PlayerEvent::SleepTimerChanged {
                remaining: None,
                end_of_track: true,
            },
            now,
        );
        assert_eq!(
            status.sleep_timer_info(now),
            SleepTimerInfo {
                remaining_ms: None,
                end_of_track: true,
            }
        );
        // The end-of-track timer fired: the event clears it.
        status.observe(
            &PlayerEvent::SleepTimerChanged {
                remaining: None,
                end_of_track: false,
            },
            now,
        );
        assert!(!status.sleep_timer_info(now).end_of_track);
    }

    #[test]
    fn request_then_player_confirmation_keeps_timer() {
        let status = PlayerStatus::default();
        let now = Instant::now();
        // A request is recorded before the player's confirmation arrives.
        status.request_sleep_timer(SleepTimer::At(now + Duration::from_secs(120)));
        assert_eq!(status.sleep_timer_info(now).remaining_ms, Some(120_000));
        // A stale clear from an earlier timer is followed by the confirmation of the new one.
        status.observe(
            &PlayerEvent::SleepTimerChanged {
                remaining: None,
                end_of_track: false,
            },
            now,
        );
        status.observe(
            &PlayerEvent::SleepTimerChanged {
                remaining: Some(Duration::from_secs(120)),
                end_of_track: false,
            },
            now,
        );
        assert_eq!(status.sleep_timer_info(now).remaining_ms, Some(120_000));
    }

    #[test]
    fn new_player_drops_an_expired_timed_sleep() {
        let status = PlayerStatus::default();
        let now = Instant::now();
        status.request_sleep_timer(SleepTimer::At(now + Duration::from_secs(5)));
        assert_eq!(
            status.sleep_timer_for_new_player(now),
            SleepTimer::At(now + Duration::from_secs(5)),
            "a live timer is handed to the new player"
        );
        let later = now + Duration::from_secs(6);
        assert_eq!(status.sleep_timer_for_new_player(later), SleepTimer::Off);
        assert_eq!(status.sleep_timer_info(later).remaining_ms, None);
    }

    #[test]
    fn replay_gain_status_follows_gain_events() {
        let status = PlayerStatus::default();
        let now = Instant::now();
        assert_eq!(status.replay_gain_db(), None);
        status.observe(&PlayerEvent::GainApplied { db: Some(-4.5) }, now);
        assert_eq!(status.replay_gain_db(), Some(-4.5));
        status.observe(&PlayerEvent::GainApplied { db: None }, now);
        assert_eq!(
            status.replay_gain_db(),
            None,
            "a track with no gain clears it"
        );
        status.set_replay_gain_db(Some(1.0));
        status.set_replay_gain_db(None);
        assert_eq!(status.replay_gain_db(), None);
    }

    /// Counts the events it receives.
    struct Counter(Mutex<usize>);

    impl EventListener for Counter {
        fn on_event(&self, _event: CoreEvent) {
            *self.0.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        }
    }

    /// Panics on every event.
    struct Panicker;

    impl EventListener for Panicker {
        fn on_event(&self, _event: CoreEvent) {
            panic!("listener bug");
        }
    }

    fn state_event() -> CoreEvent {
        CoreEvent::StateChanged {
            state: crate::types::PlaybackState::Idle,
        }
    }

    #[test]
    fn listener_slot_survives_panicking_listener() {
        let slot = ListenerSlot::default();
        // Nothing registered: emit is a no-op.
        slot.emit(state_event());

        slot.set(Arc::new(Panicker));
        slot.emit(state_event()); // must not unwind into the caller

        let good = Arc::new(Counter(Mutex::new(0)));
        slot.set(good.clone());
        slot.emit(state_event());
        slot.emit(state_event());
        assert_eq!(*good.0.lock().unwrap_or_else(PoisonError::into_inner), 2);

        slot.clear();
        slot.emit(state_event());
        assert_eq!(*good.0.lock().unwrap_or_else(PoisonError::into_inner), 2);
    }

    #[test]
    fn listener_slot_clones_share_registration() {
        let slot = ListenerSlot::default();
        let other = slot.clone();
        let good = Arc::new(Counter(Mutex::new(0)));
        other.set(good.clone());
        slot.emit(state_event());
        assert_eq!(*good.0.lock().unwrap_or_else(PoisonError::into_inner), 1);
    }
}

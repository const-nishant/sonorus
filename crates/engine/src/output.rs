//! Output sinks: the boundary between the ring buffer and the audio device.
//!
//! [`OutputSink`] is the trait the player drives. [`NullSink`] is the desktop/test
//! implementation: a paced thread that drains the ring in real time (times `speed`)
//! and discards the samples.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::error::{EngineError, Result};
use crate::render::{self, RingStats};

/// Sample representation expected by the output device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    /// Float, volume applied.
    F32,
    /// Left-justified 32-bit integers, bit-perfect, no volume.
    I32,
    /// 24-bit little-endian packed integers, 3 bytes per sample (bit-perfect path for 24-bit DACs).
    I24Packed,
}

/// Format the output is opened with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub sample: SampleKind,
}

/// A destination that consumes an rtrb ring of left-justified `i32` samples.
pub trait OutputSink: Send {
    /// Open/prepare the output for `format`, taking ownership of the ring consumer. Starts PAUSED (silent,
    /// not consuming). Calling start while started = stop() first.
    fn start(
        &mut self,
        format: OutputFormat,
        consumer: rtrb::Consumer<i32>,
        stats: Arc<RingStats>,
    ) -> Result<()>;
    /// Begin consuming.
    fn play(&mut self) -> Result<()>;
    /// Stop consuming, keep consumer and position.
    fn pause(&mut self) -> Result<()>;
    /// Close; drops the consumer. Idempotent.
    fn stop(&mut self);
    /// Best estimate of output latency (NullSink: period).
    fn latency(&self) -> Duration;
    fn name(&self) -> &str;
}

/// Pacing period of the NullSink render loop.
const PERIOD: Duration = Duration::from_millis(10);

/// If the thread falls behind its deadline by more than this, the deadline is reset to now.
const RESYNC_THRESHOLD: Duration = Duration::from_millis(100);

const THREAD_NAME: &str = "sonorus-nullsink";

/// Desktop/test sink: a thread that, while playing, every `period` (10 ms) renders
/// sample_rate * period frames via render_f32/render_i32 into a buffer preallocated in start(),
/// paced by an absolute deadline (next += period; sleep until next; if behind by > 100 ms, resync) so
/// it consumes in real time × `speed`. Controls via Arc<AtomicBool> playing + Arc<AtomicBool> quit;
/// stop() sets quit and joins the thread.
pub struct NullSink {
    speed: f32,
    period: Duration,
    playing: Arc<AtomicBool>,
    quit: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    format: Option<OutputFormat>,
}

impl NullSink {
    /// Real-time sink (speed 1.0).
    pub fn new() -> Self {
        Self::with_speed(1.0)
    }

    /// Sink consuming `speed` times real time. Speeds that are not finite or <= 0 become 1.0.
    pub fn with_speed(speed: f32) -> Self {
        // Capped so the preallocated render buffer stays small (64x real time is plenty for tests).
        let speed = if speed.is_finite() && speed > 0.0 {
            speed.min(64.0)
        } else {
            1.0
        };
        Self {
            speed,
            period: PERIOD,
            playing: Arc::new(AtomicBool::new(false)),
            quit: Arc::new(AtomicBool::new(false)),
            thread: None,
            format: None,
        }
    }
}

impl Default for NullSink {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for NullSink {
    fn drop(&mut self) {
        self.stop();
    }
}

impl OutputSink for NullSink {
    fn start(
        &mut self,
        format: OutputFormat,
        consumer: rtrb::Consumer<i32>,
        stats: Arc<RingStats>,
    ) -> Result<()> {
        self.stop();
        self.quit.store(false, Ordering::Release);
        self.playing.store(false, Ordering::Release);

        if format.channels == 0 || format.sample_rate == 0 {
            return Err(EngineError::Unsupported(
                "invalid output format".to_string(),
            ));
        }

        let frames = frames_per_period(format.sample_rate, self.period, self.speed);
        let handle = spawn_pacer(
            format,
            consumer,
            stats,
            Arc::clone(&self.quit),
            Arc::clone(&self.playing),
            self.period,
            frames,
        )?;
        self.thread = Some(handle);
        self.format = Some(format);
        Ok(())
    }

    fn play(&mut self) -> Result<()> {
        if self.thread.is_none() || self.format.is_none() {
            return Err(EngineError::Unsupported("sink not started".to_string()));
        }
        self.playing.store(true, Ordering::Release);
        Ok(())
    }

    fn pause(&mut self) -> Result<()> {
        self.playing.store(false, Ordering::Release);
        Ok(())
    }

    fn stop(&mut self) {
        self.quit.store(true, Ordering::Release);
        self.playing.store(false, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            // A panicked render thread has nothing left to clean up; ignore the join error.
            let _ = handle.join();
        }
        self.format = None;
    }

    fn latency(&self) -> Duration {
        self.period
    }

    fn name(&self) -> &str {
        "null"
    }
}

/// Frames rendered per pacing period: `sample_rate * period * speed`, rounded, at least 1.
fn frames_per_period(sample_rate: u32, period: Duration, speed: f32) -> usize {
    let frames = sample_rate as f64 * period.as_secs_f64() * speed as f64;
    (frames.round() as usize).max(1)
}

/// Spawns the "sonorus-nullsink" thread. The render buffer is allocated here (one variant only,
/// by sample kind) and moved into the thread before its loop starts.
fn spawn_pacer(
    format: OutputFormat,
    mut consumer: rtrb::Consumer<i32>,
    stats: Arc<RingStats>,
    quit: Arc<AtomicBool>,
    playing: Arc<AtomicBool>,
    period: Duration,
    frames_per_period: usize,
) -> std::io::Result<JoinHandle<()>> {
    let channels = format.channels as usize;
    let samples = frames_per_period.saturating_mul(channels);
    thread::Builder::new()
        .name(THREAD_NAME.to_owned())
        .spawn(move || match format.sample {
            SampleKind::F32 => {
                let mut buf = vec![0.0f32; samples];
                pace(&quit, &playing, period, || {
                    render::render_f32(&mut consumer, &mut buf, channels, &stats);
                });
            }
            SampleKind::I32 => {
                let mut buf = vec![0i32; samples];
                pace(&quit, &playing, period, || {
                    render::render_i32(&mut consumer, &mut buf, channels, &stats);
                });
            }
            SampleKind::I24Packed => {
                let mut buf = vec![0u8; samples * 3];
                pace(&quit, &playing, period, || {
                    render::render_i24_packed(&mut consumer, &mut buf, channels, &stats);
                });
            }
        })
}

/// Deadline-paced loop. While `playing`, calls `render_period` once per period; while paused it
/// only keeps the deadline at "now" so that resuming does not burst. Exits when `quit` is set.
fn pace(
    quit: &AtomicBool,
    playing: &AtomicBool,
    period: Duration,
    mut render_period: impl FnMut(),
) {
    let mut next = Instant::now();
    loop {
        if quit.load(Ordering::Acquire) {
            break;
        }
        if playing.load(Ordering::Acquire) {
            render_period();
        } else {
            next = Instant::now();
        }
        next += period;
        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        } else if now.duration_since(next) > RESYNC_THRESHOLD {
            next = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const CH: u16 = 2;

    fn fmt(sample: SampleKind) -> OutputFormat {
        OutputFormat {
            sample_rate: RATE,
            channels: CH,
            sample,
        }
    }

    /// Ring holding 1 s of stereo silence (96 000 samples), plus its stats.
    fn preloaded() -> (rtrb::Consumer<i32>, Arc<RingStats>) {
        let stats = Arc::new(RingStats::new());
        let (mut p, c) = render::new_ring(RATE, CH);
        let silence = vec![0i32; RATE as usize * CH as usize];
        assert_eq!(
            render::push_frames(&mut p, &silence, CH as usize, &stats),
            silence.len()
        );
        (c, stats)
    }

    fn played(stats: &RingStats) -> u64 {
        stats.samples_played.load(Ordering::Acquire)
    }

    #[test]
    fn frames_per_period_rounds_and_has_minimum() {
        assert_eq!(frames_per_period(48_000, PERIOD, 1.0), 480);
        assert_eq!(frames_per_period(48_000, PERIOD, 4.0), 1920);
        assert_eq!(frames_per_period(100, PERIOD, 1.0), 1);
    }

    #[test]
    fn with_speed_sanitizes_invalid_values() {
        assert_eq!(NullSink::new().speed, 1.0);
        assert_eq!(NullSink::with_speed(4.0).speed, 4.0);
        for bad in [0.0, -2.0, f32::NAN, f32::INFINITY] {
            assert_eq!(NullSink::with_speed(bad).speed, 1.0);
        }
    }

    #[test]
    fn name_and_latency() {
        let sink = NullSink::new();
        assert_eq!(sink.name(), "null");
        assert_eq!(sink.latency(), Duration::from_millis(10));
    }

    #[test]
    fn play_before_start_is_error() {
        let mut sink = NullSink::new();
        assert!(sink.play().is_err());
        // pause is fine even when not started.
        assert!(sink.pause().is_ok());
    }

    #[test]
    fn invalid_format_is_rejected() {
        let mut sink = NullSink::new();
        let (c, s) = preloaded();
        let bad_rate = OutputFormat {
            sample_rate: 0,
            channels: CH,
            sample: SampleKind::F32,
        };
        assert!(matches!(
            sink.start(bad_rate, c, s),
            Err(EngineError::Unsupported(_))
        ));
        let (c, s) = preloaded();
        let bad_ch = OutputFormat {
            sample_rate: RATE,
            channels: 0,
            sample: SampleKind::F32,
        };
        assert!(matches!(
            sink.start(bad_ch, c, s),
            Err(EngineError::Unsupported(_))
        ));
        assert!(sink.play().is_err());
    }

    #[test]
    fn play_consumes_in_real_time() {
        let (c, stats) = preloaded();
        let mut sink = NullSink::new();
        sink.start(fmt(SampleKind::F32), c, Arc::clone(&stats))
            .unwrap();
        sink.play().unwrap();
        thread::sleep(Duration::from_millis(250));
        sink.pause().unwrap();
        sink.stop();

        // 0.25 s * 48 000 frames/s * 2 channels = 24 000 samples; accept 20 %.
        let expected = u64::from(RATE) * u64::from(CH) / 4;
        let lo = expected * 8 / 10;
        let hi = expected * 12 / 10;
        let got = played(&stats);
        assert!(
            (lo..=hi).contains(&got),
            "played {got} samples, expected {lo}..={hi}"
        );
    }

    #[test]
    fn i24_packed_format_consumes() {
        let (c, stats) = preloaded();
        let mut sink = NullSink::new();
        sink.start(fmt(SampleKind::I24Packed), c, Arc::clone(&stats))
            .unwrap();
        sink.play().unwrap();
        thread::sleep(Duration::from_millis(60));
        sink.stop();
        assert!(played(&stats) > 0);
    }

    #[test]
    fn i32_format_consumes() {
        let (c, stats) = preloaded();
        let mut sink = NullSink::new();
        sink.start(fmt(SampleKind::I32), c, Arc::clone(&stats))
            .unwrap();
        sink.play().unwrap();
        thread::sleep(Duration::from_millis(60));
        sink.stop();
        assert!(played(&stats) > 0);
    }

    #[test]
    fn pause_stops_consumption() {
        let (c, stats) = preloaded();
        let mut sink = NullSink::new();
        sink.start(fmt(SampleKind::F32), c, Arc::clone(&stats))
            .unwrap();
        sink.play().unwrap();
        thread::sleep(Duration::from_millis(50));
        sink.pause().unwrap();
        // Let an in-flight period finish before sampling.
        thread::sleep(Duration::from_millis(20));
        let before = played(&stats);
        thread::sleep(Duration::from_millis(100));
        let after = played(&stats);
        sink.stop();
        assert_eq!(before, after);
    }

    #[test]
    fn started_sink_is_paused_until_play() {
        let (c, stats) = preloaded();
        let mut sink = NullSink::new();
        sink.start(fmt(SampleKind::F32), c, Arc::clone(&stats))
            .unwrap();
        thread::sleep(Duration::from_millis(30));
        assert_eq!(played(&stats), 0);
        sink.play().unwrap();
        thread::sleep(Duration::from_millis(30));
        sink.stop();
        assert!(played(&stats) > 0);
    }

    #[test]
    fn stop_is_idempotent() {
        let mut sink = NullSink::new();
        sink.stop();
        sink.stop();
        let (c, stats) = preloaded();
        sink.start(fmt(SampleKind::F32), c, stats).unwrap();
        sink.stop();
        sink.stop();
        assert!(sink.play().is_err());
    }

    #[test]
    fn start_twice_replaces_previous_session() {
        let (c1, s1) = preloaded();
        let (c2, s2) = preloaded();
        let mut sink = NullSink::new();
        sink.start(fmt(SampleKind::F32), c1, s1).unwrap();
        sink.play().unwrap();
        thread::sleep(Duration::from_millis(20));

        // Second start is paused and consumes from the new ring only.
        sink.start(fmt(SampleKind::I32), c2, Arc::clone(&s2))
            .unwrap();
        thread::sleep(Duration::from_millis(30));
        assert_eq!(played(&s2), 0);
        sink.play().unwrap();
        thread::sleep(Duration::from_millis(30));
        sink.stop();
        assert!(played(&s2) > 0);
    }

    #[test]
    fn drop_stops_thread() {
        let (c, stats) = preloaded();
        let mut sink = NullSink::new();
        sink.start(fmt(SampleKind::F32), c, Arc::clone(&stats))
            .unwrap();
        sink.play().unwrap();
        thread::sleep(Duration::from_millis(20));
        drop(sink);
        // Drop joins the thread, so no further consumption may happen afterwards.
        let before = played(&stats);
        thread::sleep(Duration::from_millis(30));
        assert_eq!(before, played(&stats));
    }
}

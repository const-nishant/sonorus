//! Playback ring buffer: shared stats, ring construction, the audio-callback renderers
//! and the decode-thread push helper.
//!
//! Audio-thread rules (`render_f32`, `render_i32`, `push_frames`): no allocation, no locks,
//! no logging, no panicking operations (no unchecked indexing, unwrap or expect), no
//! unchecked integer arithmetic.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Ring capacity in seconds of audio (see [`new_ring`]).
pub const RING_SECONDS: usize = 2;

/// Minimum ring capacity in samples, regardless of format.
const MIN_RING_SAMPLES: usize = 4096;

/// `1 / 2^31`: maps a left-justified `i32` sample to the `[-1.0, 1.0)` float range.
const I32_TO_F32_SCALE: f32 = 1.0 / 2_147_483_648.0;

/// Shared between decode thread (producer side), callback (consumer side) and controller (reader).
#[derive(Debug)]
pub struct RingStats {
    /// Decode thread: `fetch_add` after each committed write.
    pub samples_pushed: AtomicU64,
    /// Callback: `fetch_add` after each read commit.
    pub samples_played: AtomicU64,
    /// Callback: +1 per callback that had to zero-fill while `!decoder_done`.
    pub underruns: AtomicU64,
    /// Decode thread: true after the last sample is pushed (EOF).
    pub decoder_done: AtomicBool,
    /// f32 bits, default 1.0.
    volume_bits: AtomicU32,
}

impl RingStats {
    /// All counters zero, `decoder_done` false, volume 1.0.
    pub fn new() -> Self {
        Self {
            samples_pushed: AtomicU64::new(0),
            samples_played: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            decoder_done: AtomicBool::new(false),
            volume_bits: AtomicU32::new(1.0f32.to_bits()),
        }
    }

    /// Clamps `v` to `0.0..=1.0`; NaN becomes 1.0.
    pub fn set_volume(&self, v: f32) {
        let v = if v.is_nan() { 1.0 } else { v.clamp(0.0, 1.0) };
        self.volume_bits.store(v.to_bits(), Ordering::Relaxed);
    }

    /// Current volume in `0.0..=1.0`.
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume_bits.load(Ordering::Relaxed))
    }

    /// Samples pushed but not yet played.
    pub fn buffered_samples(&self) -> u64 {
        // Load `played` before `pushed`: `pushed` is then at least as new as `played`,
        // so the difference is not clipped by saturation in normal operation.
        let played = self.samples_played.load(Ordering::Acquire);
        let pushed = self.samples_pushed.load(Ordering::Acquire);
        pushed.saturating_sub(played)
    }
}

impl Default for RingStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Capacity = sample_rate * channels * RING_SECONDS (min 4096).
pub fn new_ring(sample_rate: u32, channels: u16) -> (rtrb::Producer<i32>, rtrb::Consumer<i32>) {
    let capacity = (sample_rate as usize)
        .saturating_mul(channels as usize)
        .saturating_mul(RING_SECONDS)
        .max(MIN_RING_SAMPLES);
    rtrb::RingBuffer::<i32>::new(capacity)
}

/// Callback renderer for float outputs. `out` is interleaved with `channels` channels.
///
/// Renders `n = min(out.len() / channels, consumer.slots() / channels)` whole frames, applying
/// the current volume, and zero-fills the rest of `out` (including any leftover partial frame).
/// Counts one underrun if fewer frames than requested were available and the decoder is not done.
/// `channels == 0` zero-fills `out` and returns 0.
#[deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]
pub fn render_f32(
    consumer: &mut rtrb::Consumer<i32>,
    out: &mut [f32],
    channels: usize,
    stats: &RingStats,
) -> usize {
    if channels == 0 {
        out.iter_mut().for_each(|x| *x = 0.0);
        return 0;
    }
    let frames_wanted = out.len() / channels;
    let frames_avail = consumer.slots() / channels;
    let n = frames_wanted.min(frames_avail);

    // Volume applied once per callback; `I32_TO_F32_SCALE` is a power of two, so folding it
    // into the gain does not change rounding.
    let gain = stats.volume() * I32_TO_F32_SCALE;

    let mut frames = 0;
    if n > 0
        && let Ok(chunk) = consumer.read_chunk(n.saturating_mul(channels))
    {
        let (a, b) = chunk.as_slices();
        for (dst, &s) in out.iter_mut().zip(a.iter().chain(b.iter())) {
            *dst = (s as f32) * gain;
        }
        chunk.commit_all();
        frames = n;
    }

    let written = frames.saturating_mul(channels);
    if let Some(tail) = out.get_mut(written..) {
        tail.iter_mut().for_each(|x| *x = 0.0);
    }
    if written > 0 {
        stats
            .samples_played
            .fetch_add(written as u64, Ordering::Release);
    }
    if frames < frames_wanted && !stats.decoder_done.load(Ordering::Acquire) {
        stats.underruns.fetch_add(1, Ordering::Relaxed);
    }
    frames
}

/// Callback renderer for integer outputs. Same contract as [`render_f32`], except that samples
/// are copied verbatim (bit-perfect path) and volume is not applied.
#[deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]
pub fn render_i32(
    consumer: &mut rtrb::Consumer<i32>,
    out: &mut [i32],
    channels: usize,
    stats: &RingStats,
) -> usize {
    if channels == 0 {
        out.iter_mut().for_each(|x| *x = 0);
        return 0;
    }
    let frames_wanted = out.len() / channels;
    let frames_avail = consumer.slots() / channels;
    let n = frames_wanted.min(frames_avail);

    let mut frames = 0;
    if n > 0
        && let Ok(chunk) = consumer.read_chunk(n.saturating_mul(channels))
    {
        let (a, b) = chunk.as_slices();
        for (dst, &s) in out.iter_mut().zip(a.iter().chain(b.iter())) {
            *dst = s;
        }
        chunk.commit_all();
        frames = n;
    }

    let written = frames.saturating_mul(channels);
    if let Some(tail) = out.get_mut(written..) {
        tail.iter_mut().for_each(|x| *x = 0);
    }
    if written > 0 {
        stats
            .samples_played
            .fetch_add(written as u64, Ordering::Release);
    }
    if frames < frames_wanted && !stats.decoder_done.load(Ordering::Acquire) {
        stats.underruns.fetch_add(1, Ordering::Relaxed);
    }
    frames
}

/// Decode-thread helper: writes as much of `samples` as fits, returns samples written (may be 0).
///
/// Writes only whole frames: `n = min(samples.len(), producer.slots())` rounded down to a
/// multiple of `channels`. `channels == 0` writes nothing and returns 0.
#[deny(clippy::indexing_slicing, clippy::unwrap_used, clippy::expect_used)]
pub fn push_frames(
    producer: &mut rtrb::Producer<i32>,
    samples: &[i32],
    channels: usize,
    stats: &RingStats,
) -> usize {
    if channels == 0 {
        return 0;
    }
    let limit = samples.len().min(producer.slots());
    let n = limit - limit % channels;
    if n == 0 {
        return 0;
    }
    let Ok(chunk) = producer.write_chunk_uninit(n) else {
        return 0;
    };
    let written = chunk.fill_from_iter(samples.iter().take(n).copied());
    if written > 0 {
        stats
            .samples_pushed
            .fetch_add(written as u64, Ordering::Release);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    const CH: usize = 1;

    fn ring(capacity: usize) -> (rtrb::Producer<i32>, rtrb::Consumer<i32>) {
        rtrb::RingBuffer::<i32>::new(capacity)
    }

    #[test]
    fn push_then_render_f32_converts_exactly() {
        let stats = RingStats::new();
        let (mut p, mut c) = ring(16);
        let pushed = push_frames(&mut p, &[i32::MIN, 1 << 30], CH, &stats);
        assert_eq!(pushed, 2);
        let mut out = [9.0f32; 2];
        let frames = render_f32(&mut c, &mut out, CH, &stats);
        assert_eq!(frames, 2);
        assert_eq!(out[0], -1.0);
        assert_eq!(out[1], 0.5);
        assert_eq!(stats.underruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn volume_half_halves_output() {
        let stats = RingStats::new();
        stats.set_volume(0.5);
        let (mut p, mut c) = ring(16);
        push_frames(&mut p, &[i32::MIN, 1 << 30], CH, &stats);
        let mut out = [0.0f32; 2];
        render_f32(&mut c, &mut out, CH, &stats);
        assert_eq!(out[0], -0.5);
        assert_eq!(out[1], 0.25);
    }

    #[test]
    fn set_volume_clamps_and_nan_is_one() {
        let stats = RingStats::new();
        assert_eq!(stats.volume(), 1.0);
        stats.set_volume(2.0);
        assert_eq!(stats.volume(), 1.0);
        stats.set_volume(-1.0);
        assert_eq!(stats.volume(), 0.0);
        stats.set_volume(f32::NAN);
        assert_eq!(stats.volume(), 1.0);
    }

    #[test]
    fn render_i32_is_bit_exact() {
        let stats = RingStats::new();
        stats.set_volume(0.1); // must be ignored on the integer path
        let (mut p, mut c) = ring(16);
        let src = [i32::MIN, -1, 0, 1, i32::MAX, 123_456_789];
        assert_eq!(push_frames(&mut p, &src, CH, &stats), src.len());
        let mut out = [0i32; 6];
        assert_eq!(render_i32(&mut c, &mut out, CH, &stats), 6);
        assert_eq!(out, src);
    }

    #[test]
    fn partial_availability_zero_fills_and_counts_one_underrun() {
        let stats = RingStats::new();
        let (mut p, mut c) = ring(16);
        push_frames(&mut p, &[1 << 30, 1 << 30], CH, &stats);
        let mut out = [7.0f32; 4];
        let frames = render_f32(&mut c, &mut out, CH, &stats);
        assert_eq!(frames, 2);
        assert_eq!(out, [0.5, 0.5, 0.0, 0.0]);
        assert_eq!(stats.underruns.load(Ordering::Relaxed), 1);
        assert_eq!(stats.samples_played.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn partial_availability_i32_zero_fills_and_counts_one_underrun() {
        let stats = RingStats::new();
        let (mut p, mut c) = ring(16);
        push_frames(&mut p, &[5, 6], CH, &stats);
        let mut out = [7i32; 4];
        assert_eq!(render_i32(&mut c, &mut out, CH, &stats), 2);
        assert_eq!(out, [5, 6, 0, 0]);
        assert_eq!(stats.underruns.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn decoder_done_zero_fill_counts_no_underrun() {
        let stats = RingStats::new();
        let (mut p, mut c) = ring(16);
        push_frames(&mut p, &[1, 2], CH, &stats);
        stats.decoder_done.store(true, Ordering::Release);
        let mut out = [0i32; 4];
        assert_eq!(render_i32(&mut c, &mut out, CH, &stats), 2);
        assert_eq!(out, [1, 2, 0, 0]);
        assert_eq!(stats.underruns.load(Ordering::Relaxed), 0);

        let mut out_f = [0.0f32; 2];
        assert_eq!(render_f32(&mut c, &mut out_f, CH, &stats), 0);
        assert_eq!(out_f, [0.0, 0.0]);
        assert_eq!(stats.underruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn wrap_around_stays_in_order() {
        // Small capacity so that push/render cycles wrap the ring many times.
        let stats = RingStats::new();
        let (mut p, mut c) = ring(5);
        let mut next: i32 = 0;
        let mut expected: i32 = 0;
        for _ in 0..100 {
            let batch: Vec<i32> = (0..3).map(|i| next + i).collect();
            let pushed = push_frames(&mut p, &batch, CH, &stats);
            next += pushed as i32;

            let mut out = [0i32; 3];
            let frames = render_i32(&mut c, &mut out, CH, &stats);
            for &v in out.iter().take(frames) {
                assert_eq!(v, expected);
                expected += 1;
            }
        }
        assert!(expected > 200);
        assert_eq!(stats.samples_pushed.load(Ordering::Relaxed), next as u64);
        assert_eq!(
            stats.samples_played.load(Ordering::Relaxed),
            expected as u64
        );
    }

    #[test]
    fn push_frames_never_writes_partial_frame() {
        let stats = RingStats::new();
        // Capacity 5 with 2 channels: only 4 slots usable in whole frames.
        let (mut p, c) = ring(5);
        let src = [1, 2, 3, 4, 5, 6];
        let written = push_frames(&mut p, &src, 2, &stats);
        assert_eq!(written, 4);
        assert_eq!(stats.samples_pushed.load(Ordering::Relaxed), 4);
        assert_eq!(c.slots(), 4);

        // Odd sample count: the trailing sample of an incomplete frame is not written.
        let (mut p2, mut c2) = ring(16);
        let written2 = push_frames(&mut p2, &[1, 2, 3], 2, &stats);
        assert_eq!(written2, 2);
        let mut out = [0i32; 2];
        assert_eq!(render_i32(&mut c2, &mut out, 2, &stats), 1);
        assert_eq!(out, [1, 2]);
    }

    #[test]
    fn buffered_samples_tracks_push_minus_play() {
        let stats = RingStats::new();
        let (mut p, mut c) = ring(16);
        assert_eq!(stats.buffered_samples(), 0);
        push_frames(&mut p, &[1, 2, 3, 4], CH, &stats);
        assert_eq!(stats.buffered_samples(), 4);
        let mut out = [0i32; 3];
        render_i32(&mut c, &mut out, CH, &stats);
        assert_eq!(stats.buffered_samples(), 1);
        let mut out2 = [0i32; 4];
        render_i32(&mut c, &mut out2, CH, &stats);
        assert_eq!(stats.buffered_samples(), 0);
    }

    #[test]
    fn channels_zero_is_safe() {
        let stats = RingStats::new();
        let (mut p, mut c) = ring(16);
        assert_eq!(push_frames(&mut p, &[1, 2], 0, &stats), 0);
        let mut out = [1.0f32; 2];
        assert_eq!(render_f32(&mut c, &mut out, 0, &stats), 0);
        assert_eq!(out, [0.0, 0.0]);
    }

    #[test]
    fn new_ring_capacity_has_minimum() {
        let (p, _c) = new_ring(100, 1);
        assert_eq!(p.slots(), MIN_RING_SAMPLES);
        let (p2, _c2) = new_ring(48_000, 2);
        assert_eq!(p2.slots(), 48_000 * 2 * RING_SECONDS);
    }
}

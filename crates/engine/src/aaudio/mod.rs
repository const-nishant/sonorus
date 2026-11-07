//! AAudio output: configuration, negotiated stream info and pure helpers.
//!
//! Everything here is host-testable and free of FFI. The Android-only pieces are the raw
//! wrapper ([`stream`]) and the [`OutputSink`](crate::output::OutputSink) implementation
//! ([`sink`]); [`AAudioSink`] and [`AAudioHandle`] are re-exported on Android only.

use std::time::Duration;

use crate::output::{OutputFormat, SampleKind};

#[cfg(target_os = "android")]
mod sink;
#[cfg(target_os = "android")]
mod stream;

#[cfg(target_os = "android")]
pub use sink::{AAudioHandle, AAudioSink};

/// How the stream shares the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sharing {
    /// Shared mixer (always available, higher latency).
    #[default]
    Shared,
    /// Exclusive device access (MMAP path, lowest latency; requires low-latency performance).
    Exclusive,
}

/// AAudio performance mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Performance {
    /// No performance hint.
    None,
    /// Favour battery life over latency.
    #[default]
    PowerSaving,
    /// Favour low latency.
    LowLatency,
}

/// User-facing AAudio configuration. Applied by the next stream open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AAudioConfig {
    /// Requested sharing mode.
    pub sharing: Sharing,
    /// Requested performance mode (see [`AAudioConfig::effective_performance`]).
    pub performance: Performance,
    /// Device to open, or `None` for the default device.
    pub device_id: Option<i32>,
}

impl AAudioConfig {
    /// Performance mode actually requested from AAudio: exclusive streams are forced to
    /// [`Performance::LowLatency`] (the MMAP path needs it); otherwise the configured mode.
    pub fn effective_performance(&self) -> Performance {
        match self.sharing {
            Sharing::Exclusive => Performance::LowLatency,
            Sharing::Shared => self.performance,
        }
    }
}

/// What a stream was actually opened with. AAudio may change the sharing mode, device, burst
/// size or buffer size relative to the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    /// Negotiated sample rate in Hz.
    pub sample_rate: u32,
    /// Negotiated channel count.
    pub channels: u16,
    /// Negotiated sample representation.
    pub sample: SampleKind,
    /// Actual sharing mode.
    pub sharing: Sharing,
    /// Actual performance mode.
    pub performance: Performance,
    /// Device the stream is attached to.
    pub device_id: i32,
    /// Frames per hardware burst.
    pub frames_per_burst: u32,
    /// Buffer size in frames.
    pub buffer_frames: u32,
}

/// Checks that the device opened with what was requested. Returns the mismatch as a message.
///
/// `actual_sample` is `None` when AAudio reports a format code this crate does not know.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn check_negotiated(
    requested: OutputFormat,
    actual_rate: u32,
    actual_channels: u16,
    actual_sample: Option<SampleKind>,
) -> Result<(), String> {
    if actual_rate == requested.sample_rate
        && actual_channels == requested.channels
        && actual_sample == Some(requested.sample)
    {
        return Ok(());
    }
    let actual_sample = match actual_sample {
        Some(kind) => format!("{kind:?}"),
        None => "unknown format".to_owned(),
    };
    Err(format!(
        "device opened {actual_rate} Hz/{actual_channels} ch/{actual_sample} instead of {} Hz/{} ch/{:?}",
        requested.sample_rate, requested.channels, requested.sample
    ))
}

/// Output latency of one buffer: `buffer_frames / sample_rate`. A zero rate gives zero.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn latency_for(buffer_frames: u32, sample_rate: u32) -> Duration {
    if sample_rate == 0 {
        return Duration::ZERO;
    }
    Duration::from_secs_f64(f64::from(buffer_frames) / f64::from(sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requested() -> OutputFormat {
        OutputFormat {
            sample_rate: 96_000,
            channels: 2,
            sample: SampleKind::I32,
        }
    }

    #[test]
    fn config_defaults() {
        let c = AAudioConfig::default();
        assert_eq!(c.sharing, Sharing::Shared);
        assert_eq!(c.performance, Performance::PowerSaving);
        assert_eq!(c.device_id, None);
        assert_eq!(Sharing::default(), Sharing::Shared);
        assert_eq!(Performance::default(), Performance::PowerSaving);
    }

    #[test]
    fn effective_performance_forces_low_latency_for_exclusive() {
        let shared = AAudioConfig {
            sharing: Sharing::Shared,
            performance: Performance::None,
            device_id: None,
        };
        assert_eq!(shared.effective_performance(), Performance::None);

        let exclusive = AAudioConfig {
            sharing: Sharing::Exclusive,
            performance: Performance::PowerSaving,
            device_id: Some(7),
        };
        assert_eq!(exclusive.effective_performance(), Performance::LowLatency);
    }

    #[test]
    fn check_negotiated_accepts_exact_match() {
        assert!(check_negotiated(requested(), 96_000, 2, Some(SampleKind::I32)).is_ok());
    }

    #[test]
    fn check_negotiated_reports_rate_mismatch() {
        let err = check_negotiated(requested(), 48_000, 2, Some(SampleKind::F32)).unwrap_err();
        assert_eq!(
            err,
            "device opened 48000 Hz/2 ch/F32 instead of 96000 Hz/2 ch/I32"
        );
    }

    #[test]
    fn check_negotiated_reports_channel_and_format_mismatch() {
        let ch = check_negotiated(requested(), 96_000, 1, Some(SampleKind::I32)).unwrap_err();
        assert!(ch.contains("1 ch"), "{ch}");
        let fmt =
            check_negotiated(requested(), 96_000, 2, Some(SampleKind::I24Packed)).unwrap_err();
        assert!(fmt.contains("I24Packed"), "{fmt}");
    }

    #[test]
    fn check_negotiated_unknown_format_is_mismatch() {
        let err = check_negotiated(requested(), 96_000, 2, None).unwrap_err();
        assert_eq!(
            err,
            "device opened 96000 Hz/2 ch/unknown format instead of 96000 Hz/2 ch/I32"
        );
    }

    #[test]
    fn latency_for_computes_and_handles_zero_rate() {
        assert_eq!(latency_for(480, 48_000), Duration::from_millis(10));
        assert_eq!(latency_for(256, 0), Duration::ZERO);
        assert_eq!(latency_for(0, 48_000), Duration::ZERO);
    }
}

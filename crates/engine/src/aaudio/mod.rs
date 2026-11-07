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

/// What kind of path the audio takes to the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathMode {
    /// Integer samples copied unchanged to the device (see [`path_mode`]).
    BitPerfect,
    /// Exclusive device access, but not bit-perfect.
    Exclusive,
    /// Shared mixer.
    Mixer,
}

/// Asked before each stream open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteRequest {
    /// Sample rate of the source (the rate the stream is opened at).
    pub sample_rate: u32,
    /// Channel count of the source.
    pub channels: u16,
    /// Bit depth of the decoded source, when known.
    pub source_bits: Option<u32>,
    /// Sample kind the player asks for.
    pub requested: SampleKind,
    /// The user's AAudio configuration.
    pub config: AAudioConfig,
}

/// How to open the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutePlan {
    /// Device to open, or `None` for the default device.
    pub device_id: Option<i32>,
    /// Sample representation to open the stream with.
    pub sample: SampleKind,
    /// Sharing mode to request.
    pub sharing: Sharing,
    /// Performance mode to request (already adjusted for the sharing mode).
    pub performance: Performance,
    /// Whether the route is meant to be bit-perfect.
    pub bit_perfect: bool,
    /// Open the stream but do not restart it automatically after a disconnect reopen; an explicit
    /// play still starts it. Set by a router while the output device has just been removed and
    /// the app is pausing.
    pub hold: bool,
}

/// What actually happened, reported after each open attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalPath {
    /// Path the audio takes (`Mixer` when the open failed).
    pub mode: PathMode,
    /// The plan the open was attempted with.
    pub plan: RoutePlan,
    /// Negotiated stream, `None` when the open failed.
    pub stream: Option<StreamInfo>,
    /// Open error message, `None` on success.
    pub error: Option<String>,
    /// Sample rate of the source.
    pub source_rate: u32,
    /// Bit depth of the source, when known.
    pub source_bits: Option<u32>,
}

/// Chooses the route for each open. Called off the audio thread, without the sink lock held; may
/// block briefly (binder calls).
pub trait RoutePlanner: Send + Sync {
    /// Plans the open for `request`.
    fn plan(&self, request: &RouteRequest) -> RoutePlan;
    /// Reports the outcome of an open attempt made with the planner's plan.
    fn opened(&self, path: &SignalPath);
}

/// The planner used when none is set: the user's config and requested sample kind, no device
/// preference.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultPlanner;

impl RoutePlanner for DefaultPlanner {
    fn plan(&self, request: &RouteRequest) -> RoutePlan {
        RoutePlan {
            device_id: request.config.device_id,
            sample: request.requested,
            sharing: request.config.sharing,
            performance: request.config.effective_performance(),
            bit_perfect: false,
            hold: false,
        }
    }

    fn opened(&self, _path: &SignalPath) {}
}

/// Classifies an opened stream. `BitPerfect` requires `plan.bit_perfect`, an integer sample kind
/// (not `F32`) and that the device got exactly the planned kind; otherwise `Exclusive` when the
/// stream is exclusive, else `Mixer`.
pub fn path_mode(plan: &RoutePlan, stream: &StreamInfo) -> PathMode {
    if plan.bit_perfect && stream.sample != SampleKind::F32 && stream.sample == plan.sample {
        PathMode::BitPerfect
    } else if stream.sharing == Sharing::Exclusive {
        PathMode::Exclusive
    } else {
        PathMode::Mixer
    }
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
            source_bits: None,
        }
    }

    fn stream(sample: SampleKind, sharing: Sharing) -> StreamInfo {
        StreamInfo {
            sample_rate: 96_000,
            channels: 2,
            sample,
            sharing,
            performance: Performance::LowLatency,
            device_id: 7,
            frames_per_burst: 192,
            buffer_frames: 768,
        }
    }

    fn plan(sample: SampleKind, bit_perfect: bool) -> RoutePlan {
        RoutePlan {
            device_id: None,
            sample,
            sharing: Sharing::Shared,
            performance: Performance::None,
            bit_perfect,
            hold: false,
        }
    }

    fn request(config: AAudioConfig) -> RouteRequest {
        RouteRequest {
            sample_rate: 96_000,
            channels: 2,
            source_bits: Some(24),
            requested: SampleKind::I24Packed,
            config,
        }
    }

    #[test]
    fn default_planner_uses_config_and_requested_kind() {
        let config = AAudioConfig {
            sharing: Sharing::Shared,
            performance: Performance::None,
            device_id: Some(3),
        };
        let p = DefaultPlanner.plan(&request(config));
        assert_eq!(
            p,
            RoutePlan {
                device_id: Some(3),
                sample: SampleKind::I24Packed,
                sharing: Sharing::Shared,
                performance: Performance::None,
                bit_perfect: false,
                hold: false,
            }
        );
    }

    #[test]
    fn default_planner_never_holds() {
        let config = AAudioConfig {
            sharing: Sharing::Exclusive,
            performance: Performance::PowerSaving,
            device_id: Some(5),
        };
        assert!(!DefaultPlanner.plan(&request(config)).hold);
    }

    #[test]
    fn default_planner_applies_effective_performance() {
        let config = AAudioConfig {
            sharing: Sharing::Exclusive,
            performance: Performance::PowerSaving,
            device_id: None,
        };
        let p = DefaultPlanner.plan(&request(config));
        assert_eq!(p.sharing, Sharing::Exclusive);
        assert_eq!(p.performance, Performance::LowLatency);
        assert!(!p.bit_perfect);
        assert_eq!(p.device_id, None);
    }

    #[test]
    fn default_planner_opened_is_a_no_op() {
        let path = SignalPath {
            mode: PathMode::Mixer,
            plan: plan(SampleKind::F32, false),
            stream: None,
            error: Some("gone".to_owned()),
            source_rate: 44_100,
            source_bits: None,
        };
        DefaultPlanner.opened(&path);
    }

    #[test]
    fn path_mode_bit_perfect_needs_integer_kind_matching_plan() {
        let s = stream(SampleKind::I32, Sharing::Shared);
        assert_eq!(
            path_mode(&plan(SampleKind::I32, true), &s),
            PathMode::BitPerfect
        );
        // Bit-perfect wins over exclusive.
        let ex = stream(SampleKind::I24Packed, Sharing::Exclusive);
        assert_eq!(
            path_mode(&plan(SampleKind::I24Packed, true), &ex),
            PathMode::BitPerfect
        );
    }

    #[test]
    fn path_mode_float_is_never_bit_perfect() {
        let s = stream(SampleKind::F32, Sharing::Shared);
        assert_eq!(path_mode(&plan(SampleKind::F32, true), &s), PathMode::Mixer);
        let ex = stream(SampleKind::F32, Sharing::Exclusive);
        assert_eq!(
            path_mode(&plan(SampleKind::F32, true), &ex),
            PathMode::Exclusive
        );
    }

    #[test]
    fn path_mode_kind_mismatch_falls_through() {
        // Device gave I32 although I16 was planned: not bit-perfect.
        let s = stream(SampleKind::I32, Sharing::Shared);
        assert_eq!(path_mode(&plan(SampleKind::I16, true), &s), PathMode::Mixer);
        let ex = stream(SampleKind::I32, Sharing::Exclusive);
        assert_eq!(
            path_mode(&plan(SampleKind::I16, true), &ex),
            PathMode::Exclusive
        );
    }

    #[test]
    fn path_mode_without_bit_perfect_flag() {
        let shared = stream(SampleKind::I32, Sharing::Shared);
        assert_eq!(
            path_mode(&plan(SampleKind::I32, false), &shared),
            PathMode::Mixer
        );
        let ex = stream(SampleKind::I32, Sharing::Exclusive);
        assert_eq!(
            path_mode(&plan(SampleKind::I32, false), &ex),
            PathMode::Exclusive
        );
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

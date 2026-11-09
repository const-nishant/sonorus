//! ReplayGain: the linear gain a track's tags call for, given the user's settings.
//!
//! The gain is applied on the decode side (see `player`) and only on the float output path.
//! Integer (bit-perfect) output is never scaled.

use crate::opener::ReplayGain;

/// Largest linear gain ever applied (+12.04 dB).
pub(crate) const MAX_GAIN: f32 = 4.0;

/// Which ReplayGain value to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplayGainMode {
    /// No gain is applied.
    #[default]
    Off,
    /// Track gain, falling back to album gain.
    Track,
    /// Album gain, falling back to track gain.
    Album,
}

/// User settings for ReplayGain. Changes apply from the next track (see `Player::set_replay_gain`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ReplayGainSettings {
    /// Which tag values to use.
    pub mode: ReplayGainMode,
    /// Offset added to the tag gain, in dB.
    pub preamp_db: f32,
}

/// Linear gain for a track with ReplayGain tags `rg` under `settings`. Returns 1.0 when nothing
/// applies: mode `Off`, no tags, or neither gain value present.
///
/// The gain is `10^((gain_db + preamp_db) / 20)`. The peak paired with the chosen gain (the other
/// peak when that one is missing) caps the result at `1 / peak`, so the peak cannot exceed full
/// scale. Non-finite or non-positive peaks are ignored. The result is clamped to `0.0..=4.0`, and
/// a non-finite result is 1.0.
pub fn linear_gain(rg: Option<&ReplayGain>, settings: &ReplayGainSettings) -> f32 {
    let Some(rg) = rg else {
        return 1.0;
    };
    // (gain, peak) pairs in preference order.
    let (chosen, other) = match settings.mode {
        ReplayGainMode::Off => return 1.0,
        ReplayGainMode::Track => (
            (rg.track_gain_db, rg.track_peak),
            (rg.album_gain_db, rg.album_peak),
        ),
        ReplayGainMode::Album => (
            (rg.album_gain_db, rg.album_peak),
            (rg.track_gain_db, rg.track_peak),
        ),
    };
    let (gain_db, peak) = match chosen {
        (Some(db), peak) => (db, peak.or(other.1)),
        (None, _) => match other {
            (Some(db), peak) => (db, peak.or(chosen.1)),
            (None, _) => return 1.0,
        },
    };

    let db = f64::from(gain_db) + f64::from(settings.preamp_db);
    let mut g = 10f64.powf(db / 20.0);
    if !g.is_finite() {
        return 1.0;
    }
    if let Some(peak) = peak.filter(|p| p.is_finite() && *p > 0.0) {
        g = g.min(1.0 / f64::from(peak));
    }
    g.clamp(0.0, f64::from(MAX_GAIN)) as f32
}

/// Scales left-justified samples by `gain`, saturating at the `i32` range. A gain of exactly 1.0
/// (or a non-finite one) returns at once, so unscaled output stays bit-exact.
pub(crate) fn scale_samples(samples: &mut [i32], gain: f32) {
    if gain == 1.0 || !gain.is_finite() {
        return;
    }
    let g = f64::from(gain);
    let (lo, hi) = (f64::from(i32::MIN), f64::from(i32::MAX));
    for s in samples.iter_mut() {
        *s = (f64::from(*s) * g).clamp(lo, hi) as i32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(mode: ReplayGainMode, preamp_db: f32) -> ReplayGainSettings {
        ReplayGainSettings { mode, preamp_db }
    }

    fn track_only(db: f32, peak: Option<f32>) -> ReplayGain {
        ReplayGain {
            track_gain_db: Some(db),
            track_peak: peak,
            ..ReplayGain::default()
        }
    }

    fn album_only(db: f32, peak: Option<f32>) -> ReplayGain {
        ReplayGain {
            album_gain_db: Some(db),
            album_peak: peak,
            ..ReplayGain::default()
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn minus_six_db_is_about_half() {
        let rg = track_only(-6.02, None);
        let g = linear_gain(Some(&rg), &settings(ReplayGainMode::Track, 0.0));
        assert!((g - 0.5).abs() < 0.001, "{g}");
    }

    #[test]
    fn plus_six_db_is_limited_by_peak() {
        let rg = track_only(6.0, Some(0.9));
        let g = linear_gain(Some(&rg), &settings(ReplayGainMode::Track, 0.0));
        assert!(close(g, 1.0 / 0.9), "{g}");
    }

    #[test]
    fn peak_only_limits_when_it_is_the_binding_constraint() {
        // 10^(-6/20) = 0.501 is below 1/0.9: the peak does not change the result.
        let rg = track_only(-6.0, Some(0.9));
        let g = linear_gain(Some(&rg), &settings(ReplayGainMode::Track, 0.0));
        assert!((g - 10f32.powf(-6.0 / 20.0)).abs() < 1e-4, "{g}");
    }

    #[test]
    fn preamp_is_added_to_the_gain() {
        let rg = track_only(-6.02, None);
        let g = linear_gain(Some(&rg), &settings(ReplayGainMode::Track, 6.02));
        assert!((g - 1.0).abs() < 0.001, "{g}");
    }

    #[test]
    fn off_and_untagged_are_unity() {
        let rg = track_only(6.0, Some(0.5));
        assert_eq!(
            linear_gain(Some(&rg), &settings(ReplayGainMode::Off, 0.0)),
            1.0
        );
        assert_eq!(
            linear_gain(None, &settings(ReplayGainMode::Track, 0.0)),
            1.0
        );
        assert_eq!(
            linear_gain(None, &settings(ReplayGainMode::Album, 3.0)),
            1.0
        );
    }

    #[test]
    fn track_mode_falls_back_to_album_gain() {
        let rg = album_only(-6.02, None);
        let g = linear_gain(Some(&rg), &settings(ReplayGainMode::Track, 0.0));
        assert!((g - 0.5).abs() < 0.001, "{g}");
    }

    #[test]
    fn album_mode_falls_back_to_track_gain() {
        let rg = track_only(-6.02, None);
        let g = linear_gain(Some(&rg), &settings(ReplayGainMode::Album, 0.0));
        assert!((g - 0.5).abs() < 0.001, "{g}");
    }

    #[test]
    fn each_mode_prefers_its_own_gain() {
        let rg = ReplayGain {
            track_gain_db: Some(-6.02),
            album_gain_db: Some(0.0),
            ..ReplayGain::default()
        };
        let album = linear_gain(Some(&rg), &settings(ReplayGainMode::Album, 0.0));
        assert_eq!(album, 1.0);
        let track = linear_gain(Some(&rg), &settings(ReplayGainMode::Track, 0.0));
        assert!((track - 0.5).abs() < 0.001, "{track}");
    }

    #[test]
    fn no_gain_values_is_unity_even_with_peaks() {
        let rg = ReplayGain {
            track_peak: Some(0.5),
            album_peak: Some(0.5),
            ..ReplayGain::default()
        };
        assert_eq!(
            linear_gain(Some(&rg), &settings(ReplayGainMode::Track, 0.0)),
            1.0
        );
        assert_eq!(
            linear_gain(Some(&rg), &settings(ReplayGainMode::Album, 0.0)),
            1.0
        );
    }

    #[test]
    fn missing_matching_peak_falls_back_to_the_other_peak() {
        // Track gain without a track peak: the album peak still caps the gain.
        let rg = ReplayGain {
            track_gain_db: Some(6.0),
            album_peak: Some(0.9),
            ..ReplayGain::default()
        };
        let g = linear_gain(Some(&rg), &settings(ReplayGainMode::Track, 0.0));
        assert!(close(g, 1.0 / 0.9), "{g}");
    }

    #[test]
    fn non_finite_or_zero_peak_is_ignored() {
        let inf = track_only(6.0, Some(f32::INFINITY));
        let g = linear_gain(Some(&inf), &settings(ReplayGainMode::Track, 0.0));
        assert!((g - 10f32.powf(6.0 / 20.0)).abs() < 1e-3, "{g}");
        let zero = track_only(6.0, Some(0.0));
        let g0 = linear_gain(Some(&zero), &settings(ReplayGainMode::Track, 0.0));
        assert!((g0 - 10f32.powf(6.0 / 20.0)).abs() < 1e-3, "{g0}");
    }

    #[test]
    fn nan_is_unity() {
        let nan_gain = track_only(f32::NAN, None);
        assert_eq!(
            linear_gain(Some(&nan_gain), &settings(ReplayGainMode::Track, 0.0)),
            1.0
        );
        let rg = track_only(-6.0, None);
        assert_eq!(
            linear_gain(Some(&rg), &settings(ReplayGainMode::Track, f32::NAN)),
            1.0
        );
    }

    #[test]
    fn result_is_clamped_to_four() {
        let rg = track_only(40.0, None);
        assert_eq!(
            linear_gain(Some(&rg), &settings(ReplayGainMode::Track, 0.0)),
            4.0
        );
        let quiet = track_only(-200.0, None);
        let g = linear_gain(Some(&quiet), &settings(ReplayGainMode::Track, 0.0));
        assert!((0.0..1e-6).contains(&g), "{g}");
    }

    #[test]
    fn scale_samples_unity_is_untouched() {
        let mut s = [i32::MIN, -1, 0, 1, i32::MAX, 123_456_789];
        let before = s;
        scale_samples(&mut s, 1.0);
        assert_eq!(s, before);
    }

    #[test]
    fn scale_samples_halves_and_saturates() {
        let mut s = [i32::MAX, i32::MIN, 1000, -1000];
        scale_samples(&mut s, 0.5);
        assert_eq!(s, [1_073_741_823, -1_073_741_824, 500, -500]);

        let mut big = [i32::MAX, i32::MIN, 1000, -1000];
        scale_samples(&mut big, 4.0);
        assert_eq!(big, [i32::MAX, i32::MIN, 4000, -4000]);
    }
}

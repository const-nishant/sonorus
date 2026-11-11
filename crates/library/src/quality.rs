//! Audio quality classes shared by search filters and smart playlists.
//!
//! The rule lives in two forms that must agree: `matches_quality` (Rust) and `quality_condition`
//! (the SQL predicate on `tracks t`). Both are built from `LOSSLESS_CODECS` and the thresholds
//! below. The test `sql_and_rust_classifiers_agree` checks them against each other.

/// Codecs, as the scanner stores them in `tracks.codec`, that count as lossless. Matching is
/// case-insensitive.
pub const LOSSLESS_CODECS: &[&str] = &[
    "FLAC", "ALAC", "WAV", "AIFF", "PCM", "WAVPACK", "APE", "TTA",
];

/// A lossless track is hi-res when it has more than this many bits per sample...
const HI_RES_BITS: u8 = 16;
/// ...or a sample rate above this many Hz.
const HI_RES_RATE: u32 = 48_000;

/// Which tracks a search or a browse keeps, by audio quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QualityFilter {
    /// Every track.
    #[default]
    Any,
    /// Lossless, and above 16 bits per sample or above 48 kHz.
    HiRes,
    /// Any lossless codec, at any resolution.
    Lossless,
    /// Every track that is not lossless, including tracks with no codec recorded.
    Lossy,
}

/// Whether `codec` is one of `LOSSLESS_CODECS`. `None` (no codec recorded) is not lossless.
pub fn is_lossless(codec: Option<&str>) -> bool {
    codec.is_some_and(|c| LOSSLESS_CODECS.iter().any(|l| l.eq_ignore_ascii_case(c)))
}

/// Whether a track with these properties matches `filter`. This is the single Rust definition of
/// the quality classes; `quality_condition` is its SQL form.
pub fn matches_quality(
    filter: QualityFilter,
    codec: Option<&str>,
    bits_per_sample: Option<u8>,
    sample_rate: Option<u32>,
) -> bool {
    let lossless = is_lossless(codec);
    match filter {
        QualityFilter::Any => true,
        QualityFilter::Lossless => lossless,
        QualityFilter::Lossy => !lossless,
        QualityFilter::HiRes => {
            lossless
                && (bits_per_sample.is_some_and(|b| b > HI_RES_BITS)
                    || sample_rate.is_some_and(|r| r > HI_RES_RATE))
        }
    }
}

/// SQL predicate on `tracks t` that keeps the tracks matching `filter`. `Any` is `1 = 1`.
///
/// The lossless test is wrapped in `COALESCE(..., 0)` so it is never NULL: a NULL codec is then
/// lossy under `NOT`, as `matches_quality` says.
pub(crate) fn quality_condition(filter: QualityFilter) -> String {
    let codecs = LOSSLESS_CODECS
        .iter()
        .map(|c| format!("'{c}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let lossless = format!("COALESCE(UPPER(t.codec) IN ({codecs}), 0)");
    match filter {
        QualityFilter::Any => "1 = 1".to_owned(),
        QualityFilter::Lossless => lossless,
        QualityFilter::Lossy => format!("NOT {lossless}"),
        QualityFilter::HiRes => format!(
            "({lossless} AND (t.bits_per_sample > {HI_RES_BITS} OR t.sample_rate > {HI_RES_RATE}))"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Library;
    use crate::db::test_support::{TempDir, stamp};
    use crate::model::TrackMeta;

    #[test]
    fn classifier_table() {
        use QualityFilter::*;
        /// Codec, bits per sample, sample rate, then the expected HiRes, Lossless and Lossy answers.
        type Case = (
            Option<&'static str>,
            Option<u8>,
            Option<u32>,
            bool,
            bool,
            bool,
        );
        let cases: [Case; 11] = [
            // 16/44.1 FLAC: lossless, not hi-res.
            (Some("FLAC"), Some(16), Some(44_100), false, true, false),
            // 24-bit FLAC: hi-res by bit depth.
            (Some("FLAC"), Some(24), Some(44_100), true, true, false),
            // 16-bit at 96 kHz: hi-res by rate.
            (Some("WAV"), Some(16), Some(96_000), true, true, false),
            // Exactly 48 kHz at 16 bits is not hi-res.
            (Some("ALAC"), Some(16), Some(48_000), false, true, false),
            // Lossless codec with no properties recorded: lossless, not hi-res.
            (Some("AIFF"), None, None, false, true, false),
            // Lossy codecs, even with high rates: never hi-res.
            (Some("MPEG"), None, Some(96_000), false, false, true),
            (Some("AAC"), Some(16), Some(96_000), false, false, true),
            (Some("OPUS"), None, Some(48_000), false, false, true),
            (Some("VORBIS"), None, None, false, false, true),
            // No codec recorded: lossy.
            (None, Some(24), Some(96_000), false, false, true),
            // Codec names are case-insensitive.
            (Some("flac"), Some(24), None, true, true, false),
        ];
        for (codec, bits, rate, hi, lossless, lossy) in cases {
            let at = |f| matches_quality(f, codec, bits, rate);
            assert_eq!(at(HiRes), hi, "HiRes for {codec:?} {bits:?} {rate:?}");
            assert_eq!(at(Lossless), lossless, "Lossless for {codec:?}");
            assert_eq!(at(Lossy), lossy, "Lossy for {codec:?}");
            assert!(at(Any), "Any keeps everything");
        }
    }

    #[test]
    fn lossless_list_is_the_documented_set() {
        assert_eq!(
            LOSSLESS_CODECS,
            &[
                "FLAC", "ALAC", "WAV", "AIFF", "PCM", "WAVPACK", "APE", "TTA"
            ]
        );
    }

    #[test]
    fn sql_and_rust_classifiers_agree() {
        let dir = TempDir::new();
        let lib = Library::open_in_memory(&dir.path().join("covers")).unwrap();
        // Every codec the scanner can store, plus unknown and missing ones, at several
        // resolutions. Each row is one track, so the SQL and Rust answers are compared per track.
        let codecs = [
            Some("FLAC"),
            Some("ALAC"),
            Some("WAV"),
            Some("AIFF"),
            Some("PCM"),
            Some("WAVPACK"),
            Some("APE"),
            Some("TTA"),
            Some("MPEG"),
            Some("AAC"),
            Some("OPUS"),
            Some("VORBIS"),
            Some("MPC"),
            Some("SPEEX"),
            Some("CUSTOM(\"X\")"),
            None,
        ];
        let depths = [None, Some(16u8), Some(24u8)];
        let rates = [None, Some(44_100u32), Some(48_000u32), Some(96_000u32)];
        let mut n = 0;
        for codec in codecs {
            for bits in depths {
                for rate in rates {
                    let key = format!("k/{n}.bin");
                    let meta = TrackMeta {
                        title: Some(format!("t{n}")),
                        artist: Some("A".into()),
                        album: Some("B".into()),
                        codec: codec.map(str::to_owned),
                        bits_per_sample: bits,
                        sample_rate: rate,
                        ..Default::default()
                    };
                    lib.upsert_track(&stamp(&key, 1), &meta, None).unwrap();
                    n += 1;
                }
            }
        }
        // Every stored track, checked in Rust against the rule.
        let tracks: Vec<_> = (0..n)
            .map(|i| lib.track_by_key(&format!("k/{i}.bin")).unwrap().unwrap())
            .collect();

        for filter in [
            QualityFilter::HiRes,
            QualityFilter::Lossless,
            QualityFilter::Lossy,
        ] {
            let sql_keys: std::collections::HashSet<String> = lib
                .search_filtered("", 1_000, filter)
                .unwrap()
                .tracks
                .into_iter()
                .map(|t| t.key)
                .collect();
            let rust_keys: std::collections::HashSet<String> = tracks
                .iter()
                .filter(|t| {
                    matches_quality(filter, t.codec.as_deref(), t.bits_per_sample, t.sample_rate)
                })
                .map(|t| t.key.clone())
                .collect();
            assert_eq!(sql_keys, rust_keys, "filter {filter:?}");
            assert!(!sql_keys.is_empty(), "filter {filter:?} matched nothing");
        }
    }
}

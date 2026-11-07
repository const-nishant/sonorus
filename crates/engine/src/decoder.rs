//! Symphonia-backed track decoder: probe, decode to interleaved samples, accurate seek.
//!
//! Output sample formats are chosen by the caller through the generic parameter of
//! [`TrackDecoder::next_block`]. Integer output is Symphonia's own conversion (lossless, no
//! dither or scaling here).

use std::time::Duration;

use symphonia::core::audio::conv::ConvertibleSample;
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

use crate::error::{EngineError, Result};

/// Number of consecutive undecodable packets tolerated before giving up.
const MAX_CONSECUTIVE_BAD: u32 = 64;

/// Static description of an opened audio stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    /// "FLAC" | "ALAC" | "WAV" | "AIFF" | "PCM" | uppercase short_name otherwise.
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: Option<u32>,
    pub duration: Option<Duration>,
    pub total_frames: Option<u64>,
}

impl StreamInfo {
    /// "FLAC 24/96", "WAV 16/44.1", "ALAC 24/192", "FLAC 24/352.8"; bits None → "FLAC 96 kHz".
    pub fn label(&self) -> String {
        let khz = format_khz(self.sample_rate);
        match self.bits_per_sample {
            Some(bits) => format!("{} {}/{}", self.codec, bits, khz),
            None => format!("{} {} kHz", self.codec, khz),
        }
    }
}

/// 44100 → "44.1", 96000 → "96", 88200 → "88.2", 352800 → "352.8", 11025 → "11.025".
pub fn format_khz(rate: u32) -> String {
    let whole = rate / 1000;
    let frac = rate % 1000;
    if frac == 0 {
        return whole.to_string();
    }
    format!("{whole}.{frac:03}")
        .trim_end_matches('0')
        .to_string()
}

/// Decodes the default audio track of one container/stream.
pub struct TrackDecoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    /// Kept so the decoder can be recreated on `ResetRequired`.
    codec_params: AudioCodecParameters,
    time_base: Option<TimeBase>,
    info: StreamInfo,
    /// Frames consumed from the stream (including skipped blocks), in frames.
    position: u64,
    /// Frames before this position are dropped after a seek, in frames.
    discard_until: u64,
    skipped_packets: u64,
    consecutive_bad: u32,
    /// Set when a seek was clamped to the exact end of the stream; `next_block` then yields `None`.
    at_end: bool,
}

impl TrackDecoder {
    /// Probes `source` and creates a gapless decoder for its default audio track.
    /// `ext_hint` is a file extension used as a probe hint (case-insensitive).
    pub fn new(source: Box<dyn MediaSource>, ext_hint: Option<&str>) -> Result<Self> {
        let ext = ext_hint.map(str::to_ascii_lowercase);
        let mut hint = Hint::new();
        if let Some(ext) = ext.as_deref() {
            hint.with_extension(ext);
        }

        let mss = MediaSourceStream::new(source, Default::default());
        let format = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| EngineError::Unsupported(e.to_string()))?;

        let (track_id, codec_params, time_base, num_frames, duration_units) = {
            let track = format
                .default_track(TrackType::Audio)
                .ok_or(EngineError::NoAudioTrack)?;
            let params = track
                .codec_params
                .as_ref()
                .and_then(|p| p.audio())
                .ok_or_else(|| EngineError::Unsupported("no audio codec parameters".to_string()))?;
            (
                track.id,
                params.clone(),
                track.time_base,
                track.num_frames,
                track.duration,
            )
        };

        let sample_rate = codec_params
            .sample_rate
            .filter(|&r| r > 0)
            .ok_or_else(|| EngineError::Unsupported("unknown sample rate".to_string()))?;
        let channels = codec_params
            .channels
            .as_ref()
            .map(|c| c.count())
            .and_then(|c| u16::try_from(c).ok())
            .filter(|&c| c > 0)
            .ok_or_else(|| EngineError::Unsupported("unknown channel layout".to_string()))?;

        let decoder = make_decoder(&codec_params)?;
        let codec = codec_label(decoder.codec_info().short_name, ext.as_deref());

        let duration = match num_frames {
            Some(frames) => secs_to_duration(frames as f64 / f64::from(sample_rate)),
            None => time_base
                .zip(duration_units)
                .and_then(|(tb, d)| tb.calc_duration(d))
                .and_then(|t| secs_to_duration(t.as_secs_f64())),
        };

        let is_alac = codec == "ALAC";
        let info = StreamInfo {
            codec,
            sample_rate,
            channels,
            bits_per_sample: codec_params.bits_per_sample.or_else(|| {
                // symphonia-codec-alac 0.6.1 never fills bits_per_sample; read it from the cookie.
                is_alac
                    .then(|| codec_params.extra_data.as_deref().and_then(alac_bit_depth))
                    .flatten()
            }),
            duration,
            total_frames: num_frames,
        };

        Ok(Self {
            format,
            decoder,
            track_id,
            codec_params,
            time_base,
            info,
            position: 0,
            discard_until: 0,
            skipped_packets: 0,
            consecutive_bad: 0,
            at_end: false,
        })
    }

    /// Static information about the stream.
    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    /// Decodes the next block into `out` (cleared first), interleaved.
    /// Returns `Ok(Some(frames))` with `frames > 0`, or `Ok(None)` at end of stream.
    pub fn next_block<S: ConvertibleSample>(&mut self, out: &mut Vec<S>) -> Result<Option<usize>> {
        loop {
            if self.at_end {
                return Ok(None);
            }
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return Ok(None),
                Err(SymError::ResetRequired) => {
                    self.decoder = make_decoder(&self.codec_params)?;
                    continue;
                }
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(None);
                }
                // Real I/O failures (e.g. a RangeSource read timeout) must reach the caller.
                Err(SymError::IoError(e)) => return Err(EngineError::Io(e)),
                Err(e) => return Err(EngineError::Decode(e.to_string())),
            };
            if packet.track_id != self.track_id {
                continue;
            }

            let buf = match self.decoder.decode(&packet) {
                Ok(buf) => buf,
                Err(err @ (SymError::DecodeError(_) | SymError::IoError(_))) => {
                    log::warn!("skipping undecodable packet: {err}");
                    self.skipped_packets = self.skipped_packets.saturating_add(1);
                    self.consecutive_bad = self.consecutive_bad.saturating_add(1);
                    if self.consecutive_bad > MAX_CONSECUTIVE_BAD {
                        return Err(EngineError::Decode("too many corrupt packets".to_string()));
                    }
                    continue;
                }
                Err(e) => return Err(EngineError::Decode(e.to_string())),
            };
            self.consecutive_bad = 0;

            let frames = buf.frames();
            if frames == 0 {
                continue;
            }
            let frames_u64 = frames as u64;
            if self.position.saturating_add(frames_u64) <= self.discard_until {
                // Whole block lies before the seek target: drop it without copying.
                self.position = self.position.saturating_add(frames_u64);
                continue;
            }

            out.clear();
            buf.copy_to_vec_interleaved(out);
            let channels = usize::from(self.info.channels);
            let mut frames = frames;
            if self.position < self.discard_until {
                // Partial block: the first `skip` frames precede the target and are dropped.
                // Since position + frames > discard_until, skip < frames.
                let skip = (self.discard_until - self.position).min(frames_u64) as usize;
                let n = skip.saturating_mul(channels);
                out.drain(..n.min(out.len()));
                frames = frames.saturating_sub(skip);
                self.position = self.discard_until;
            }
            if frames == 0 {
                continue;
            }
            self.position = self.position.saturating_add(frames as u64);
            return Ok(Some(frames));
        }
    }

    /// Accurate seek. Returns the position actually reached (== requested unless clamped to the
    /// duration). Seeking past a known duration clamps to the duration, after which
    /// `next_block` returns `Ok(None)`.
    pub fn seek(&mut self, to: Duration) -> Result<Duration> {
        let clamped_to_end = matches!(self.info.duration, Some(d) if to >= d);
        let to = match self.info.duration {
            Some(d) if to > d => d,
            _ => to,
        };
        let out_of_range = || EngineError::Seek("seek position out of range".to_string());
        let secs = i64::try_from(to.as_secs()).map_err(|_| out_of_range())?;
        let time = Time::try_new(secs, to.subsec_nanos()).ok_or_else(out_of_range)?;

        let seeked = match self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        ) {
            Ok(seeked) => seeked,
            // Symphonia reports UnexpectedEof when asked for the exact end of the stream.
            // Only the explicit end-clamp is mapped to end-of-stream; other I/O errors propagate.
            Err(SymError::IoError(e))
                if clamped_to_end && e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                let end_frames = (to.as_secs_f64() * f64::from(self.info.sample_rate))
                    .round()
                    .max(0.0) as u64;
                self.decoder.reset();
                self.position = end_frames;
                self.discard_until = end_frames;
                self.at_end = true;
                return Ok(to);
            }
            Err(e) => return Err(EngineError::Seek(e.to_string())),
        };
        self.decoder.reset();
        self.at_end = false;

        let required = self.ts_to_frames(seeked.required_ts);
        let actual = self.ts_to_frames(seeked.actual_ts);
        self.position = actual;
        self.discard_until = required;

        // `info.sample_rate` is non-zero (checked in `new`), so this division is finite.
        let reached = secs_to_duration(required as f64 / f64::from(self.info.sample_rate))
            .unwrap_or_default();
        Ok(reached)
    }

    /// Frames delivered so far (after a seek: the seek target), i.e. playback position in frames.
    pub fn position_frames(&self) -> u64 {
        self.position.max(self.discard_until)
    }

    /// Number of packets dropped because they failed to decode.
    pub fn skipped_packets(&self) -> u64 {
        self.skipped_packets
    }

    /// Converts a container timestamp to frames at the stream sample rate.
    fn ts_to_frames(&self, ts: Timestamp) -> u64 {
        match self.time_base {
            Some(tb) => {
                let secs = tb.calc_time_saturating(ts).as_secs_f64();
                (secs * f64::from(self.info.sample_rate)).round().max(0.0) as u64
            }
            None => u64::try_from(ts.get()).unwrap_or(0),
        }
    }
}

fn make_decoder(params: &AudioCodecParameters) -> Result<Box<dyn AudioDecoder>> {
    let opts = AudioDecoderOptions::default().gapless(true);
    symphonia::default::get_codecs()
        .make_audio_decoder(params, &opts)
        .map_err(|e| EngineError::Unsupported(e.to_string()))
}

/// `ext` must already be lowercased.
fn codec_label(short_name: &str, ext: Option<&str>) -> String {
    match short_name {
        "flac" => "FLAC".to_string(),
        "alac" => "ALAC".to_string(),
        name if name.starts_with("pcm") => match ext {
            Some("wav") => "WAV".to_string(),
            Some("aif") | Some("aiff") => "AIFF".to_string(),
            _ => "PCM".to_string(),
        },
        name => name.to_uppercase(),
    }
}

/// Bit depth from an ALAC magic cookie (ALACSpecificConfig byte 5), skipping optional leading
/// `frma` and `alac` atoms exactly like `symphonia_common::apple::audio::alac::MagicCookie::read`.
fn alac_bit_depth(mut cookie: &[u8]) -> Option<u32> {
    for atom in [b"frma", b"alac"] {
        if cookie.get(4..8) == Some(&atom[..]) {
            cookie = cookie.get(12..)?;
        }
    }
    if cookie.len() != 24 && cookie.len() != 48 {
        return None;
    }
    cookie.get(5).map(|&b| u32::from(b)).filter(|&b| b > 0)
}

/// Returns None for negative, NaN or overflowing values.
fn secs_to_duration(secs: f64) -> Option<Duration> {
    Duration::try_from_secs_f64(secs).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("read fixture")
    }

    fn open(bytes: Vec<u8>, ext: &str) -> TrackDecoder {
        TrackDecoder::new(Box::new(Cursor::new(bytes)), Some(ext)).expect("open fixture")
    }

    fn decode_all_i32(dec: &mut TrackDecoder) -> Vec<i32> {
        let mut all = Vec::new();
        let mut block = Vec::<i32>::new();
        while dec.next_block(&mut block).expect("decode").is_some() {
            all.extend_from_slice(&block);
        }
        all
    }

    fn full_i32(name: &str, ext: &str) -> Vec<i32> {
        decode_all_i32(&mut open(fixture(name), ext))
    }

    // 1. info + labels
    #[test]
    fn info_and_labels() {
        let flac = open(fixture("tone_24_96.flac"), "flac");
        let info = flac.info();
        assert_eq!(info.codec, "FLAC");
        assert_eq!(info.sample_rate, 96_000);
        assert_eq!(info.channels, 2);
        assert_eq!(info.bits_per_sample, Some(24));
        assert_eq!(info.total_frames, Some(28_800));
        assert_eq!(info.label(), "FLAC 24/96");
        let d = info.duration.expect("duration known");
        assert!((d.as_secs_f64() - 0.3).abs() < 0.001, "duration {d:?}");

        let wav = open(fixture("tone_24_96.wav"), "wav");
        assert_eq!(wav.info().label(), "WAV 24/96");

        let m4a = open(fixture("tone_24_96.m4a"), "m4a");
        assert_eq!(m4a.info().label(), "ALAC 24/96");

        let flac16 = open(fixture("tone_16_44.flac"), "flac");
        assert_eq!(flac16.info().label(), "FLAC 16/44.1");
        assert_eq!(flac16.info().total_frames, Some(13_230));
        let d16 = flac16.info().duration.expect("duration known");
        assert!((d16.as_secs_f64() - 0.3).abs() < 0.001, "duration {d16:?}");
    }

    // 2. bit-exact across containers
    #[test]
    fn bit_exact_across_containers() {
        let wav = full_i32("tone_24_96.wav", "wav");
        let flac = full_i32("tone_24_96.flac", "flac");
        let alac = full_i32("tone_24_96.m4a", "m4a");
        assert_eq!(wav.len(), 28_800 * 2);
        assert_eq!(flac, wav);
        assert_eq!(alac, wav);
        assert!(
            wav.iter().all(|s| s & 0xFF == 0),
            "24-bit low byte must be zero"
        );

        let wav16 = full_i32("tone_16_44.wav", "wav");
        let flac16 = full_i32("tone_16_44.flac", "flac");
        assert_eq!(wav16.len(), 13_230 * 2);
        assert_eq!(flac16, wav16);
    }

    // 3. f32 path
    #[test]
    fn f32_path_matches_i32_scaling() {
        let mut dec = open(fixture("tone_24_96.wav"), "wav");
        let mut f: Vec<f32> = Vec::new();
        let mut block = Vec::<f32>::new();
        let mut total = 0usize;
        while let Some(n) = dec.next_block(&mut block).expect("decode f32") {
            total += n;
            f.extend_from_slice(&block);
        }
        assert_eq!(total, 28_800);

        let reference = full_i32("tone_24_96.wav", "wav");
        assert!(f.len() >= 1000);
        for (a, b) in f.iter().zip(reference.iter()).take(1000) {
            assert_eq!(*a, (f64::from(*b) / 2f64.powi(31)) as f32);
        }
    }

    // 4. accurate seek is sample-exact against a full decode
    fn check_seek(bytes: Vec<u8>, ext: &str, full: &[i32]) {
        let mut dec = open(bytes, ext);
        let target = Duration::from_millis(150);
        let reached = dec.seek(target).expect("seek");
        let diff = reached.abs_diff(target);
        assert!(diff <= Duration::from_micros(1), "reached {reached:?}");
        assert_eq!(dec.position_frames(), 14_400);

        let mut block = Vec::<i32>::new();
        let n = dec
            .next_block(&mut block)
            .expect("decode after seek")
            .expect("block after seek");
        assert!(n > 0);
        assert_eq!(block.len(), n * 2);
        assert_eq!(dec.position_frames(), 14_400 + n as u64);

        let start = 14_400 * 2;
        let expected = full
            .get(start..start + block.len())
            .expect("full decode covers block");
        assert_eq!(block.as_slice(), expected);
    }

    #[test]
    fn accurate_seek_is_sample_exact() {
        let flac = full_i32("tone_24_96.flac", "flac");
        check_seek(fixture("tone_24_96.flac"), "flac", &flac);

        let wav = full_i32("tone_24_96.wav", "wav");
        check_seek(fixture("tone_24_96.wav"), "wav", &wav);
    }

    // 5. seek beyond end
    #[test]
    fn seek_beyond_end_reaches_eof() {
        let mut dec = open(fixture("tone_24_96.flac"), "flac");
        dec.seek(Duration::from_secs(10)).expect("seek past end");
        let mut block = Vec::<i32>::new();
        let mut ended = false;
        for _ in 0..16 {
            if dec.next_block(&mut block).expect("decode").is_none() {
                ended = true;
                break;
            }
        }
        assert!(ended);
    }

    // 6. corrupt input never panics
    #[test]
    fn corrupt_input_does_not_panic() {
        let good = fixture("tone_24_96.flac");
        let mut corrupt = Vec::with_capacity(good.len() + 4096);
        corrupt.extend_from_slice(&good[..4096]);
        corrupt.extend(std::iter::repeat_n(0x5A_u8, 4096));
        corrupt.extend_from_slice(&good[4096..]);

        match TrackDecoder::new(Box::new(Cursor::new(corrupt)), Some("flac")) {
            Err(_) => {}
            Ok(mut dec) => {
                let mut block = Vec::<i32>::new();
                let mut finished = false;
                for _ in 0..10_000 {
                    match dec.next_block(&mut block) {
                        Ok(Some(_)) => {}
                        Ok(None) | Err(_) => {
                            finished = true;
                            break;
                        }
                    }
                }
                assert!(finished, "decoder did not terminate on corrupt input");
            }
        }

        let garbage = vec![0x5A_u8; 8192];
        let result = TrackDecoder::new(Box::new(Cursor::new(garbage)), Some("flac"));
        assert!(matches!(result, Err(EngineError::Unsupported(_))));
    }

    // 7. format_khz
    #[test]
    fn format_khz_cases() {
        assert_eq!(format_khz(44_100), "44.1");
        assert_eq!(format_khz(96_000), "96");
        assert_eq!(format_khz(88_200), "88.2");
        assert_eq!(format_khz(352_800), "352.8");
        assert_eq!(format_khz(11_025), "11.025");
    }
}

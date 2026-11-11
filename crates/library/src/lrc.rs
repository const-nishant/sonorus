//! LRC parser for synced lyrics. Text with timestamped lines becomes a list of timed lines; text
//! without any is plain lyrics and yields `None`.
//!
//! Rules:
//! - Timestamps `[mm:ss]`, `[mm:ss.x]`, `[mm:ss.xx]` and `[mm:ss.xxx]`. Minutes may have any
//!   number of digits, and the decimal separator may be a comma.
//! - One line may carry several leading timestamps; each produces its own timed line.
//! - `[offset:N]` (milliseconds) shifts every line. A positive value moves lyrics earlier, as the
//!   LRC convention says. Times are clamped at 0.
//! - Other `[key:value]` tags (`[ar:]`, `[ti:]`, ...) are ignored.
//! - Enhanced word timestamps `<mm:ss.xx>` in the text are removed. Other `<...>` text is kept.
//! - Text is trimmed. Timed lines with empty text are kept: they are instrumental gaps.
//! - Lines without a timestamp are dropped when the text has timed lines.
//! - A leading byte order mark is ignored. CRLF line endings are accepted.

/// One timed line of synced lyrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricLine {
    /// Start time from the beginning of the track, in milliseconds.
    pub time_ms: u64,
    /// The line text, trimmed. Empty for an instrumental gap.
    pub text: String,
}

/// Parses LRC text into timed lines, sorted by time (stable for equal times).
///
/// Returns `None` when the text has no timestamped line, which means it is plain lyrics.
pub fn parse_lrc(text: &str) -> Option<Vec<LyricLine>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let offset = find_offset(text);
    let mut lines = Vec::new();
    for line in text.lines() {
        let (times, body) = leading_timestamps(line);
        if times.is_empty() {
            continue;
        }
        let body = strip_word_stamps(body);
        let body = body.trim();
        for raw in times {
            lines.push(LyricLine {
                time_ms: shift(raw, offset),
                text: body.to_owned(),
            });
        }
    }
    if lines.is_empty() {
        return None;
    }
    lines.sort_by_key(|l| l.time_ms);
    Some(lines)
}

/// The `[offset:N]` tag in milliseconds. The first valid tag wins; 0 when there is none.
fn find_offset(text: &str) -> i64 {
    text.lines()
        .find_map(|line| {
            let inner = line.trim().strip_prefix('[')?.split(']').next()?;
            let (key, value) = inner.split_once(':')?;
            if key.trim().eq_ignore_ascii_case("offset") {
                value.trim().parse::<i64>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
}

/// Applies the offset to a timestamp, clamping the result to `0..=u64::MAX`.
fn shift(time_ms: u64, offset_ms: i64) -> u64 {
    let shifted = i128::from(time_ms) - i128::from(offset_ms);
    u64::try_from(shifted.max(0)).unwrap_or(u64::MAX)
}

/// The timestamps at the front of a line, and the text after them.
///
/// Bracket groups that are not timestamps but look like `key:value` tags are skipped. Any other
/// bracket group ends the run and stays in the text.
fn leading_timestamps(mut line: &str) -> (Vec<u64>, &str) {
    let mut times = Vec::new();
    while let Some(inner) = line.strip_prefix('[') {
        let Some(close) = inner.find(']') else {
            break;
        };
        let group = &inner[..close];
        if let Some(time) = parse_timestamp(group) {
            times.push(time);
        } else if !group.contains(':') {
            break;
        }
        line = &inner[close + 1..];
    }
    (times, line)
}

/// Parses the inside of a timestamp (`mm:ss`, `mm:ss.x`, `mm:ss.xx`, `mm:ss.xxx`, comma allowed)
/// into milliseconds. `None` for anything else, including seconds of 60 or more and values that
/// overflow.
fn parse_timestamp(group: &str) -> Option<u64> {
    let (minutes, rest) = group.split_once(':')?;
    let minutes = digits(minutes)?;
    let (secs, fraction) = match rest.find(['.', ',']) {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    if secs.len() != 2 {
        return None;
    }
    let secs = digits(secs)?;
    if secs >= 60 {
        return None;
    }
    let millis = match fraction {
        None => 0,
        Some(f) => {
            // One digit is tenths, two hundredths, three thousandths.
            let scale = match f.len() {
                1 => 100,
                2 => 10,
                3 => 1,
                _ => return None,
            };
            digits(f)? * scale
        }
    };
    minutes
        .checked_mul(60_000)?
        .checked_add(secs * 1000 + millis)
}

/// Parses a non-empty run of ASCII digits. `None` on anything else or on overflow.
fn digits(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Removes enhanced-LRC word timestamps `<mm:ss.xx>`. Other `<...>` text is kept as it is.
fn strip_word_stamps(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('>') {
            Some(close) if parse_timestamp(&after[..close]).is_some() => {
                rest = &after[close + 1..];
            }
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shorthand for an expected line.
    fn line(time_ms: u64, text: &str) -> LyricLine {
        LyricLine {
            time_ms,
            text: text.to_owned(),
        }
    }

    fn times_and_texts(text: &str) -> Vec<(u64, String)> {
        parse_lrc(text)
            .expect("synced")
            .into_iter()
            .map(|l| (l.time_ms, l.text))
            .collect()
    }

    #[test]
    fn basic_line_is_timed_and_trimmed() {
        assert_eq!(
            parse_lrc("[00:12.34]  Hello world  "),
            Some(vec![line(12_340, "Hello world")])
        );
    }

    #[test]
    fn fraction_forms_scale_to_milliseconds() {
        assert_eq!(parse_lrc("[00:01.5]a").unwrap()[0].time_ms, 1_500);
        assert_eq!(parse_lrc("[00:01.50]a").unwrap()[0].time_ms, 1_500);
        assert_eq!(parse_lrc("[00:01.500]a").unwrap()[0].time_ms, 1_500);
        assert_eq!(parse_lrc("[00:01.05]a").unwrap()[0].time_ms, 1_050);
        assert_eq!(parse_lrc("[00:01.005]a").unwrap()[0].time_ms, 1_005);
    }

    #[test]
    fn timestamp_without_fraction_is_accepted() {
        assert_eq!(parse_lrc("[00:01]a").unwrap()[0].time_ms, 1_000);
    }

    #[test]
    fn comma_is_a_decimal_separator() {
        assert_eq!(parse_lrc("[00:01,25]a").unwrap()[0].time_ms, 1_250);
    }

    #[test]
    fn short_minutes_and_long_minutes() {
        assert_eq!(parse_lrc("[1:02.00]a").unwrap()[0].time_ms, 62_000);
        assert_eq!(parse_lrc("[123:00.00]a").unwrap()[0].time_ms, 7_380_000);
        assert_eq!(parse_lrc("[0005:00.00]a").unwrap()[0].time_ms, 300_000);
    }

    #[test]
    fn invalid_timestamps_are_not_timed() {
        assert_eq!(parse_lrc("[00:60.00]a"), None, "seconds must be below 60");
        assert_eq!(parse_lrc("[00:5.00]a"), None, "seconds need two digits");
        assert_eq!(parse_lrc("[00:01.]a"), None, "empty fraction");
        assert_eq!(parse_lrc("[00:01.0000]a"), None, "four fraction digits");
        assert_eq!(
            parse_lrc("[ 00:01.00 ]a"),
            None,
            "no spaces inside brackets"
        );
        assert_eq!(parse_lrc("[99999999999999999999:00.00]a"), None, "overflow");
    }

    #[test]
    fn several_timestamps_on_one_line_each_produce_a_line() {
        assert_eq!(
            times_and_texts("[00:12.00][01:30.50]chorus"),
            vec![(12_000, "chorus".to_owned()), (90_500, "chorus".to_owned())]
        );
    }

    #[test]
    fn offset_shifts_earlier_for_positive_values() {
        let text = "[offset:+500]\n[00:01.00]a\n[00:03.00]b";
        assert_eq!(
            times_and_texts(text),
            vec![(500, "a".into()), (2_500, "b".into())]
        );
    }

    #[test]
    fn offset_shifts_later_for_negative_values() {
        let text = "[offset:-250]\n[00:01.00]a";
        assert_eq!(times_and_texts(text), vec![(1_250, "a".into())]);
    }

    #[test]
    fn offset_saturates_at_zero() {
        let text = "[offset:1000]\n[00:00.20]early\n[00:00.90]later";
        assert_eq!(
            times_and_texts(text),
            vec![(0, "early".into()), (0, "later".into())]
        );
    }

    #[test]
    fn invalid_offset_is_ignored() {
        assert_eq!(times_and_texts("[offset:soon]\n[00:01.00]a")[0].0, 1_000);
    }

    #[test]
    fn metadata_tags_are_ignored() {
        let text = "[ar:Artist]\n[ti:Title]\n[al:Album]\n[by:Me]\n[length:03:45]\n[re:app]\n\
                    [ve:1.0]\n[xx:whatever]\n[00:01.00]line";
        assert_eq!(times_and_texts(text), vec![(1_000, "line".into())]);
    }

    #[test]
    fn metadata_tag_before_timestamps_on_the_same_line_is_skipped() {
        assert_eq!(
            times_and_texts("[ar:Someone][00:04.00]text"),
            vec![(4_000, "text".into())]
        );
    }

    #[test]
    fn enhanced_word_timestamps_are_stripped() {
        assert_eq!(
            parse_lrc("[00:01.00]<00:01.00>Hel<00:01.50>lo <00:02.00>world").unwrap()[0].text,
            "Hello world"
        );
    }

    #[test]
    fn angle_brackets_that_are_not_timestamps_are_kept() {
        assert_eq!(
            parse_lrc("[00:01.00]a <3 b <tag>").unwrap()[0].text,
            "a <3 b <tag>"
        );
    }

    #[test]
    fn empty_timed_lines_are_kept_as_gaps() {
        assert_eq!(
            parse_lrc("[00:05.00]\n[00:10.00]   \n[00:12.00]x"),
            Some(vec![line(5_000, ""), line(10_000, ""), line(12_000, "x")])
        );
    }

    #[test]
    fn sort_is_by_time_and_stable() {
        let text = "[00:05.00]b\n[00:01.00]a\n[00:05.00]c";
        assert_eq!(
            times_and_texts(text),
            vec![
                (1_000, "a".into()),
                (5_000, "b".into()),
                (5_000, "c".into())
            ]
        );
    }

    #[test]
    fn bom_and_crlf_are_handled() {
        let text = "\u{feff}[00:01.00]a\r\n[00:02.00]b\r\n";
        assert_eq!(
            parse_lrc(text),
            Some(vec![line(1_000, "a"), line(2_000, "b")])
        );
    }

    #[test]
    fn lines_without_timestamps_are_dropped_when_timed_lines_exist() {
        let text = "intro words\n[00:01.00]x\nmore words";
        assert_eq!(parse_lrc(text), Some(vec![line(1_000, "x")]));
    }

    #[test]
    fn bracket_that_is_not_a_tag_ends_the_timestamp_run() {
        assert_eq!(
            parse_lrc("[00:01.00][bracketed]text"),
            Some(vec![line(1_000, "[bracketed]text")])
        );
    }

    #[test]
    fn plain_text_is_not_synced() {
        assert_eq!(parse_lrc("just words\nno stamps at all"), None);
        assert_eq!(parse_lrc("[ar:Only metadata]\n[ti:Nothing else]"), None);
    }

    #[test]
    fn empty_and_garbage_input_is_not_synced() {
        assert_eq!(parse_lrc(""), None);
        assert_eq!(parse_lrc("   \n\n\t"), None);
        assert_eq!(parse_lrc("\u{feff}"), None);
        assert_eq!(parse_lrc("[[[]]]<<>>[:]"), None);
        assert_eq!(parse_lrc("[00:01.00"), None, "unclosed bracket");
        assert_eq!(parse_lrc("\u{0}\u{1}\u{2}[]"), None);
    }
}

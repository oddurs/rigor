//! Small helpers: epoch time, ISO-8601 parsing, human-scale formatting, width-aware truncation.

use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthStr;

#[cfg(test)]
thread_local! {
    static FROZEN: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

/// Pin the clock for the current test thread, so rendered ages and durations
/// are identical on every run — a snapshot must not depend on which second it
/// happened to be taken in.
#[cfg(test)]
pub fn freeze_time(at: i64) {
    FROZEN.with(|f| f.set(Some(at)));
}

/// A configured duration in seconds as a signed epoch offset, saturating: an
/// absurd config value means "never", not an overflow.
pub fn secs(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// A count or width as a terminal coordinate, saturating at the largest one.
pub fn cols(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

pub fn now_secs() -> i64 {
    #[cfg(test)]
    if let Some(at) = FROZEN.with(std::cell::Cell::get) {
        return at;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| secs(d.as_secs()))
}

/// Parse an RFC 3339 timestamp to epoch seconds: `2026-09-09T20:13:41Z` as
/// GitHub writes it, or with fractional seconds, or with a numeric offset,
/// which is applied. Anything else is not a timestamp — a date that does not
/// exist, a time with no zone, text after the end. Nothing is read by taking
/// the first nineteen characters and ignoring the rest.
pub fn parse_iso8601(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    // ASCII throughout, so every index below is a character boundary.
    if bytes.len() < 20 || !text.is_ascii() {
        return None;
    }
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    // Digits only: `parse` alone would take a sign.
    let num = |from: usize, to: usize| -> Option<i64> {
        let digits = &text[from..to];
        digits
            .bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| digits.parse::<i64>().ok())
            .flatten()
    };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    // 60 is a leap second, which RFC 3339 allows.
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) || h > 23 || mi > 59 || sec > 60
    {
        return None;
    }

    let mut zone = &text[19..];
    if let Some(fraction) = zone.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        zone = &fraction[digits..];
    }
    let offset = if matches!(zone, "Z" | "z") {
        0
    } else {
        // `+hh:mm` or `-hh:mm`, and nothing after it.
        let offset = zone.as_bytes();
        if offset.len() != 6 || offset[3] != b':' {
            return None;
        }
        let sign = match offset[0] {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let start = text.len() - 6;
        let (hours, minutes) = (num(start + 1, start + 3)?, num(start + 4, start + 6)?);
        if hours > 23 || minutes > 59 {
            return None;
        }
        sign * (hours * 3600 + minutes * 60)
    };
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + sec - offset)
}

const fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        4 | 6 | 9 | 11 => 30,
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's civil-date algorithm).
const fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Compact age: `12s`, `4m`, `3h`, `2d`, `5w`.
pub fn rel_time(ts: i64, now: i64) -> String {
    let d = (now - ts).max(0);
    if d < 60 {
        format!("{d}s")
    } else if d < 3600 {
        format!("{}m", d / 60)
    } else if d < 86400 {
        format!("{}h", d / 3600)
    } else if d < 86400 * 14 {
        format!("{}d", d / 86400)
    } else {
        format!("{}w", d / (86400 * 7))
    }
}

/// Compact elapsed duration for a check run: `42s`, `1m42s`, `1h04m`.
pub fn dur_short(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate to `max` display columns, appending `…` when it had to cut.
pub fn truncate(s: &str, max: usize) -> String {
    // Fit first: zero-width text (a lone combining mark) fits in zero columns
    // and must come back unchanged, not emptied by the zero-budget case.
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = UnicodeWidthStr::width(ch.to_string().as_str());
        if w + cw > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

/// Pad to exactly `n` display columns (truncating when too long).
pub fn pad(s: &str, n: usize) -> String {
    let t = truncate(s, n);
    let w = width(&t);
    format!("{t}{}", " ".repeat(n.saturating_sub(w)))
}

/// A column followed by more text: truncated one short of `n` so it always
/// keeps a gutter. `pad` alone lets a full-width value run into its neighbor.
pub fn cell(s: &str, n: usize) -> String {
    pad(&truncate(s, n.saturating_sub(1)), n)
}

/// Right-aligned in `n` columns — for numbers read down a column, like ages.
pub fn right(s: &str, n: usize) -> String {
    let t = truncate(s, n);
    format!("{}{t}", " ".repeat(n.saturating_sub(width(&t))))
}

/// Break `text` into lines of at most `max` columns, at spaces where it can.
/// The detail pane clips rather than wraps, so prose is wrapped before it gets
/// there. A line that already fits is kept as written, indentation and all.
pub fn wrap(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.lines() {
        let para = para.trim_end().replace('\t', "  ");
        if width(&para) <= max {
            out.push(para);
            continue;
        }
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && width(&line) + 1 + width(word) > max {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            // A word longer than a whole line is cut wherever the line ends.
            for ch in word.chars() {
                if !line.is_empty() && width(&line) + width(ch.encode_utf8(&mut [0; 4])) > max {
                    out.push(std::mem::take(&mut line));
                }
                line.push(ch);
            }
        }
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Text as it arrives from GitHub: ASCII, accents, CJK and emoji (two
    /// columns wide), and combining marks (zero columns).
    fn text() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-zA-Z0-9 ._/#-]{0,12}[é漢字🙂\\u{301}]{0,4}[a-z ]{0,12}")
            .unwrap()
    }

    /// A timestamp is read whole or not at all. The old reading took the
    /// first nineteen characters and ignored the rest, so an offset was
    /// dropped and an impossible date was a number.
    #[test]
    fn timestamps_are_rfc_3339_or_nothing() {
        let at = parse_iso8601("2027-01-15T08:00:00Z").unwrap();
        assert_eq!(at, 1_800_000_000);
        for same in [
            "2027-01-15t08:00:00z",
            "2027-01-15T08:00:00.000Z",
            "2027-01-15T08:00:00.987654321Z",
            "2027-01-15T10:00:00+02:00",
            "2027-01-15T02:30:00-05:30",
            "2027-01-15T08:00:00+00:00",
            "2027-01-14T23:30:00.5-08:30",
        ] {
            assert_eq!(parse_iso8601(same), Some(at), "{same}");
        }
        assert_eq!(
            parse_iso8601("2028-02-29T23:59:60Z"),
            Some(parse_iso8601("2028-03-01T00:00:00Z").unwrap()),
            "a leap second in a leap year"
        );
        for not in [
            "",
            "2027-01-15",
            "2027-01-15T08:00:00",
            "2027-01-15T08:00:00.Z",
            "2027-01-15 08:00:00Z",
            "2027-01-15T08:00:00Zjunk",
            "2027-01-15T08:00:00+0200",
            "2027-01-15T08:00:00+02:00Z",
            "2027-01-15T08:00:00+00:+02:00",
            "2027-01-15T08:00:00.5",
            "2027-01-15T08:00:00 +02:00",
            "2027-01-15T08:00:00+24:00",
            "2027-01-15T08:00:00+02:60",
            "2027-13-15T08:00:00Z",
            "2027-00-15T08:00:00Z",
            "2027-02-29T08:00:00Z",
            "2027-04-31T08:00:00Z",
            "2027-01-00T08:00:00Z",
            "2027-01-15T24:00:00Z",
            "2027-01-15T08:60:00Z",
            "2027-01-15T08:00:61Z",
            "+027-01-15T08:00:00Z",
            "2027-01-15T+8:00:00Z",
            "2027-01-15T08:00:00\u{ff3a}",
            "yesterday at about eight",
        ] {
            assert_eq!(parse_iso8601(not), None, "{not:?}");
        }
    }

    proptest! {
        /// The guarantee every truncated column relies on: exactly `n` columns,
        /// and the last one blank, whatever went in.
        #[test]
        fn a_cell_is_exactly_its_width_and_ends_in_a_gutter(s in text(), n in 1usize..40) {
            let c = cell(&s, n);
            prop_assert_eq!(width(&c), n, "{:?} -> {:?}", s, c);
            prop_assert!(c.ends_with(' '), "{:?} -> {:?}", s, c);
        }

        #[test]
        fn truncate_never_exceeds_its_budget(s in text(), n in 0usize..40) {
            let t = truncate(&s, n);
            prop_assert!(width(&t) <= n, "{:?} -> {:?}", s, t);
            if width(&s) <= n {
                prop_assert_eq!(t, s, "text that fits must be left alone");
            }
        }

        #[test]
        fn pad_and_right_fill_exactly(s in text(), n in 0usize..40) {
            prop_assert_eq!(width(&pad(&s, n)), n);
            prop_assert_eq!(width(&right(&s, n)), n);
        }

        /// Wrapped prose never runs past the pane, and loses no words.
        #[test]
        fn wrap_fits_every_line_and_keeps_every_word(s in text(), n in 2usize..40) {
            let lines = wrap(&s, n);
            for l in &lines {
                prop_assert!(width(l) <= n, "{:?} -> {:?}", s, lines);
            }
            let squash = |t: &str| t.chars().filter(|c| !c.is_whitespace()).collect::<String>();
            prop_assert_eq!(squash(&lines.join(" ")), squash(&s));
        }

        /// Timestamps from GitHub round-trip through the hand-written civil
        /// date arithmetic, across leap years and centuries.
        #[test]
        fn iso8601_round_trips(ts in 0i64..4_102_444_800) {
            prop_assert_eq!(parse_iso8601(&format_iso8601(ts)), Some(ts));
        }

        #[test]
        fn rel_time_never_panics_and_stays_short(a in any::<i32>(), b in any::<i32>()) {
            prop_assert!(rel_time(i64::from(a), i64::from(b)).len() <= 8);
        }
    }

    /// The inverse of `parse_iso8601`, for the round-trip property only.
    fn format_iso8601(ts: i64) -> String {
        let (days, secs) = (ts.div_euclid(86400), ts.rem_euclid(86400));
        // Howard Hinnant's civil_from_days.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    }
}

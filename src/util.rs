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

/// Parse the exact shape GitHub returns: `2026-09-09T20:13:41Z`.
pub fn parse_iso8601(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { text.get(from..to)?.parse::<i64>().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + sec)
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

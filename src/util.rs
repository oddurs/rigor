//! Small helpers: epoch time, ISO-8601 parsing, human-scale formatting, width-aware truncation.

use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthStr;

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parse the exact shape GitHub returns: `2026-09-09T20:13:41Z`.
pub fn parse_iso8601(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let num = |a: usize, z: usize| -> Option<i64> { s.get(a..z)?.parse::<i64>().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + sec)
}

/// Days since 1970-01-01 (Howard Hinnant's civil-date algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
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
    if max == 0 {
        return String::new();
    }
    if width(s) <= max {
        return s.to_string();
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
/// keeps a gutter. `pad` alone lets a full-width value run into its neighbour.
pub fn cell(s: &str, n: usize) -> String {
    pad(&truncate(s, n.saturating_sub(1)), n)
}

/// Right-aligned in `n` columns — for numbers read down a column, like ages.
pub fn right(s: &str, n: usize) -> String {
    let t = truncate(s, n);
    format!("{}{t}", " ".repeat(n.saturating_sub(width(&t))))
}

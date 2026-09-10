//! Ask the terminal which colors it is actually drawing with.
//!
//! OSC 10 and OSC 11 report the default foreground and background. With those,
//! the selection band and the hairlines can be mixed from the user's real theme
//! instead of pinned to a palette slot — a fixed slot is what made the old
//! highlight look off against a tinted background.
//!
//! A DA1 query goes last because every terminal answers it. Replies arrive in
//! order, so once DA1's reply is in, a terminal that has not answered OSC 10/11
//! never will: unsupported terminals cost a round trip, not a timeout.

use std::time::Duration;

pub type Rgb = (u8, u8, u8);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Probed {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
}

/// Must run in raw mode, before anything else reads stdin, so the replies are
/// neither echoed nor handed to the event loop as keystrokes.
#[cfg(unix)]
pub fn query(timeout: Duration) -> Probed {
    use std::io::Write;
    use std::time::Instant;

    // Only meaningful when both ends are a terminal.
    // SAFETY: isatty(3) takes a plain descriptor number and touches no memory.
    if unsafe { libc::isatty(0) == 0 || libc::isatty(1) == 0 } {
        return Probed::default();
    }
    let mut out = std::io::stdout();
    if out
        .write_all(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c")
        .and_then(|()| out.flush())
        .is_err()
    {
        return Probed::default();
    }

    let deadline = Instant::now() + timeout;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 256];
    while !has_da1(&buf) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut pfd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = i32::try_from(left.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: `pfd` is a live, initialized pollfd and the count is 1, so
        // poll(2) reads and writes exactly that one struct.
        if unsafe { libc::poll(&raw mut pfd, 1, ms) } <= 0 {
            break;
        }
        // SAFETY: the pointer and length describe `chunk`, a live buffer that
        // read(2) may fill; it never writes past `chunk.len()` bytes.
        let read = unsafe { libc::read(0, chunk.as_mut_ptr().cast(), chunk.len()) };
        // Negative is an error, zero is end of input: both end the probe.
        let Ok(n) = usize::try_from(read) else { break };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    parse(&buf)
}

#[cfg(not(unix))]
pub fn query(_timeout: Duration) -> Probed {
    Probed::default()
}

/// `ESC [ ? <digits and semicolons> c` — the Primary Device Attributes reply.
fn has_da1(buf: &[u8]) -> bool {
    let mut from = 0;
    while let Some(p) = buf[from..].windows(3).position(|w| w == b"\x1b[?") {
        let mut i = from + p + 3;
        while i < buf.len() && (buf[i].is_ascii_digit() || buf[i] == b';') {
            i += 1;
        }
        if buf.get(i) == Some(&b'c') {
            return true;
        }
        from += p + 3;
    }
    false
}

pub fn parse(buf: &[u8]) -> Probed {
    let s = String::from_utf8_lossy(buf);
    Probed {
        fg: reply(&s, "\x1b]10;"),
        bg: reply(&s, "\x1b]11;"),
    }
}

/// The color in an OSC reply, terminated by BEL or by ST (`ESC \`).
fn reply(s: &str, prefix: &str) -> Option<Rgb> {
    let rest = &s[s.find(prefix)? + prefix.len()..];
    let end = rest.find(['\x07', '\x1b']).unwrap_or(rest.len());
    parse_rgb(&rest[..end])
}

/// `rgb:RRRR/GGGG/BBBB`, one to four hex digits per channel (X11 color spec).
fn parse_rgb(spec: &str) -> Option<Rgb> {
    let mut channels = spec.strip_prefix("rgb:")?.split('/');
    let mut next = || -> Option<u8> {
        let hex = channels.next()?;
        if hex.is_empty() || hex.len() > 4 {
            return None;
        }
        let v = u32::from_str_radix(hex, 16).ok()?;
        let max = (1u32 << (4 * hex.len())) - 1;
        u8::try_from((v * 255 + max / 2) / max).ok()
    };
    Some((next()?, next()?, next()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_colors_terminated_by_st() {
        let r =
            parse(b"\x1b]10;rgb:d4d4/d8d8/dede\x1b\\\x1b]11;rgb:1616/1818/1c1c\x1b\\\x1b[?62;22c");
        assert_eq!(r.fg, Some((0xd4, 0xd8, 0xde)));
        assert_eq!(r.bg, Some((0x16, 0x18, 0x1c)));
    }

    #[test]
    fn reads_bel_terminated_and_short_channels() {
        let r = parse(b"\x1b]11;rgb:fb/fb/fa\x07\x1b[?1;2c");
        assert_eq!(r.bg, Some((0xfb, 0xfb, 0xfa)));
        assert_eq!(r.fg, None);
        // one hex digit per channel scales across the full range
        assert_eq!(parse_rgb("rgb:f/8/0"), Some((255, 136, 0)));
    }

    #[test]
    fn a_terminal_that_only_answers_da1_yields_nothing() {
        let buf = b"\x1b[?62;4c";
        assert!(has_da1(buf));
        assert_eq!(parse(buf), Probed::default());
    }

    #[test]
    fn da1_is_not_mistaken_for_a_partial_reply() {
        assert!(!has_da1(b"\x1b]11;rgb:16/18/1c\x1b\\"));
        assert!(!has_da1(b"\x1b[?62;4"), "incomplete: no final `c` yet");
    }

    proptest::proptest! {
        /// Whatever a terminal (or anything pretending to be one) sends back,
        /// parsing it must not panic: this runs before the UI exists.
        #[test]
        fn arbitrary_replies_never_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let _ = parse(&bytes);
            let _ = has_da1(&bytes);
        }
    }

    #[test]
    fn malformed_specs_are_rejected_not_guessed() {
        assert_eq!(parse_rgb("rgb:12345/00/00"), None);
        assert_eq!(parse_rgb("rgb:zz/00/00"), None);
        assert_eq!(parse_rgb("#161818"), None);
    }
}

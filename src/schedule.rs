//! When to refresh, and how hard to try.
//!
//! Bookkeeping only: nothing here does I/O or knows about the UI. The event
//! loop asks whether something is due, fetch results report how they went, and
//! everything about backoff, focus and the shared GitHub budget is decided here.

use crate::git;
use crate::github::Budget;
use crate::util::secs;

/// The longest rigor ever waits between refreshes, however it is backing off.
pub const MAX_WAIT: i64 = 900;

#[derive(Debug, Clone)]
pub struct Schedule {
    /// False once the terminal reports it lost focus. Terminals that never
    /// report focus leave it true, which keeps their behavior unchanged.
    pub focused: bool,
    /// Consecutive failed fetches; each one doubles the wait, up to `MAX_WAIT`.
    pub failures: u32,
    /// When the last PR fetch started, in epoch seconds; 0 before the first.
    pub last_refresh: i64,
    /// When the next PR refresh is due.
    pub next_refresh: i64,
    /// When the next full worktree scan is due.
    pub next_full_scan: i64,
    /// Set while the shared GitHub budget is low: no automatic fetch until then.
    pub paused_until: Option<i64>,
    /// Makes the next worktree listing scan everything, not just what moved.
    pub pending_full: bool,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            focused: true,
            failures: 0,
            last_refresh: 0,
            next_refresh: 0,
            next_full_scan: 0,
            paused_until: None,
            pending_full: true,
        }
    }
}

impl Schedule {
    /// `r`: the user asked, so forget any backoff or pause — one fetch costs a
    /// few points of budget — and make the next listing a full scan.
    pub const fn reset(&mut self) {
        self.failures = 0;
        self.paused_until = None;
        self.pending_full = true;
    }

    pub const fn refresh_due(&self, now: i64) -> bool {
        now >= self.next_refresh
    }

    /// Full sweeps pick up unstaged edits in desks whose fingerprint did not
    /// move. Nobody needs that while the terminal is in the background.
    pub const fn full_scan_due(&self, now: i64, scan_secs: u64) -> bool {
        self.focused && scan_secs > 0 && now >= self.next_full_scan
    }

    /// A full scan is starting: book the next one.
    pub fn full_scan_started(&mut self, now: i64, scan_secs: u64) {
        self.next_full_scan = if scan_secs > 0 {
            now.saturating_add(secs(scan_secs))
        } else {
            i64::MAX
        };
    }

    /// A fetch succeeded. Leave most of the shared budget to everything else
    /// using `gh`: below 5%, pause automatic fetches until the window resets.
    pub fn succeeded(&mut self, budget: Option<Budget>, refresh_secs: u64) {
        self.failures = 0;
        self.paused_until =
            budget.and_then(|b| (b.remaining < (b.limit / 20).max(50)).then_some(b.reset_at));
        self.schedule_from(self.last_refresh, refresh_secs);
    }

    /// A fetch failed at `now`. The wait counts from the failure, not from when
    /// the attempt began: a call that took 45s to time out would otherwise be
    /// retried at once, hammering a dead connection back to back.
    pub fn failed(&mut self, error: &str, refresh_secs: u64, now: i64) {
        self.failures = self.failures.saturating_add(1);
        if error.to_lowercase().contains("rate limit") {
            self.paused_until = Some(now.saturating_add(MAX_WAIT));
        }
        self.schedule_from(now, refresh_secs);
    }

    /// The terminal reported a focus change. Rescheduling from the last refresh
    /// is the whole mechanism: losing focus stretches the wait, and on regaining
    /// it a stale screen has a due time already in the past, so the next tick
    /// catches up at once.
    pub fn set_focus(&mut self, focused: bool, refresh_secs: u64) {
        if self.focused != focused {
            self.focused = focused;
            self.schedule_from(self.last_refresh, refresh_secs);
        }
    }

    /// The configured interval, stretched fourfold while unfocused, doubled per
    /// consecutive failure, capped at `MAX_WAIT`, and never before a pause ends.
    /// `refresh_secs = 0` turns automatic refresh off.
    fn schedule_from(&mut self, base: i64, refresh_secs: u64) {
        let every = secs(refresh_secs);
        if every == 0 {
            self.next_refresh = i64::MAX;
            return;
        }
        let mut wait = every;
        if !self.focused {
            wait = wait.saturating_mul(4).min(MAX_WAIT);
        }
        if self.failures > 0 {
            wait = wait.saturating_mul(1 << self.failures.min(4)).min(MAX_WAIT);
        }
        let mut next = base.saturating_add(wait);
        if let Some(until) = self.paused_until {
            next = next.max(until);
        }
        self.next_refresh = next;
    }
}

/// Whether a worktree needs `git status` this round. Idle desks are the common
/// case — fifty of them cost ten CPU-seconds to rescan — so a desk is only
/// rescanned when its fingerprint moved, when it is new, when it is the one
/// being looked at, or when it is a candidate for "removable". That last one is
/// always rechecked: the safety claim must never rest on stale data.
pub fn needs_scan(
    full: bool,
    previous: Option<&git::Signature>,
    current: &git::Signature,
    removable_candidate: bool,
    selected: bool,
) -> bool {
    full || previous != Some(current) || removable_candidate || selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    const EVERY: u64 = 90;

    fn after_start(at: i64) -> Schedule {
        Schedule {
            last_refresh: at,
            ..Schedule::default()
        }
    }

    /// A failure must never stop refreshing: it waits longer each time,
    /// doubling up to fifteen minutes, and a success resets it.
    #[test]
    fn failures_back_off_exponentially_and_recover() {
        let mut s = after_start(1_000);
        let waits: Vec<i64> = (0..6)
            .map(|_| {
                s.failed("gh api graphql: timed out after 45s", EVERY, 2_000);
                s.next_refresh - 2_000
            })
            .collect();
        assert_eq!(waits, vec![180, 360, 720, 900, 900, 900]);

        s.succeeded(None, EVERY);
        assert_eq!(s.failures, 0);
        assert_eq!(
            s.next_refresh,
            1_000 + 90,
            "a success counts from the start"
        );
    }

    /// rigor shares the GraphQL budget with every other `gh` call, agents
    /// included. Below 5% it stops fetching until the window resets.
    #[test]
    fn a_low_shared_budget_pauses_until_reset() {
        let mut s = after_start(1_000);
        let low = Budget {
            remaining: 120,
            limit: 5000,
            reset_at: 5_000,
        };
        s.succeeded(Some(low), EVERY);
        assert_eq!((s.paused_until, s.next_refresh), (Some(5_000), 5_000));

        let healthy = Budget {
            remaining: 4_500,
            ..low
        };
        s.succeeded(Some(healthy), EVERY);
        assert_eq!((s.paused_until, s.next_refresh), (None, 1_090));
    }

    #[test]
    fn a_rate_limit_error_pauses_rather_than_retrying_at_once() {
        let mut s = after_start(1_000);
        s.failed(
            "GitHub returned errors: API rate limit exceeded",
            EVERY,
            2_000,
        );
        assert_eq!(s.paused_until, Some(2_000 + MAX_WAIT));
        assert!(s.next_refresh >= 2_000 + MAX_WAIT);
    }

    /// In the background the interval stretches fourfold; back in focus, a
    /// stale screen is due immediately.
    #[test]
    fn focus_stretches_the_interval_and_catches_up_on_return() {
        let mut s = after_start(1_000);
        s.set_focus(false, EVERY);
        assert_eq!(s.next_refresh, 1_000 + 360);
        s.set_focus(true, EVERY);
        assert_eq!(s.next_refresh, 1_000 + 90);
        assert!(s.refresh_due(1_200), "stale by now, so due at once");
    }

    #[test]
    fn zero_turns_automatic_refresh_off() {
        let mut s = after_start(1_000);
        s.succeeded(None, 0);
        assert_eq!(s.next_refresh, i64::MAX);
        s.full_scan_started(1_000, 0);
        assert!(!s.full_scan_due(i64::MAX - 1, 0));
    }

    /// Arithmetic on configured intervals saturates instead of overflowing.
    #[test]
    fn huge_intervals_saturate_rather_than_overflow() {
        let mut s = after_start(i64::MAX - 10);
        s.failed("boom", u64::MAX, i64::MAX - 10);
        assert_eq!(s.next_refresh, i64::MAX);
    }

    #[test]
    fn reset_clears_backoff_and_pause() {
        let mut s = Schedule {
            failures: 3,
            paused_until: Some(9_999),
            pending_full: false,
            ..Schedule::default()
        };
        s.reset();
        assert_eq!(
            (s.failures, s.paused_until, s.pending_full),
            (0, None, true)
        );
    }

    /// The efficiency win rests on this: an idle desk is skipped, a desk that
    /// moved is rescanned, and a removable candidate or the selected desk is
    /// always rescanned.
    #[test]
    fn only_desks_that_moved_are_rescanned() {
        let t = |s| Some(UNIX_EPOCH + Duration::from_secs(s));
        let seen = (t(10), t(20));
        assert!(
            !needs_scan(false, Some(&seen), &seen, false, false),
            "idle desk"
        );
        assert!(
            needs_scan(false, Some(&seen), &(t(10), t(21)), false, false),
            "index moved"
        );
        assert!(
            needs_scan(false, Some(&seen), &(t(11), t(20)), false, false),
            "HEAD moved"
        );
        assert!(
            needs_scan(false, None, &seen, false, false),
            "never scanned"
        );
        assert!(
            needs_scan(false, Some(&seen), &seen, true, false),
            "removable candidate"
        );
        assert!(
            needs_scan(false, Some(&seen), &seen, false, true),
            "selected"
        );
        assert!(
            needs_scan(true, Some(&seen), &seen, false, false),
            "full sweep"
        );
    }
}

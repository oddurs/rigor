//! Application state: what is loaded, what is selected, and what keys and
//! clicks do to it.

use ratatui::layout::Rect;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::config::{Settings, View};
use crate::git;
use crate::github;
use crate::model::{
    CheckState, Mergeable, MergedPr, Pr, RepoInfo, ReviewDecision, Worktree, WorktreeStatus,
};
use crate::theme::Theme;
use crate::util::now_secs;

pub enum Msg {
    Prs(Result<github::Fetched, String>),
    Worktrees(Result<Vec<Worktree>, String>),
    Statuses(Result<Vec<(PathBuf, git::Signature, WorktreeStatus)>, String>),
}

/// The longest rigor ever waits between refreshes, however it is backing off.
const MAX_WAIT: i64 = 900;

/// Run `work` off the UI thread and always answer. A panic inside becomes an
/// error message rather than a reply that never comes — which would leave a
/// loading flag set and refreshes stopped for good.
fn spawn_reply<T: Send + 'static>(
    tx: Sender<Msg>,
    wrap: impl FnOnce(Result<T, String>) -> Msg + Send + 'static,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) {
    std::thread::spawn(move || {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
            .unwrap_or_else(|_| Err("internal error: a background task panicked".into()));
        let _ = tx.send(wrap(res));
    });
}

/// Whether a worktree needs `git status` this round. Idle desks are the
/// common case — fifty of them cost ten CPU-seconds to rescan — so a desk is
/// only rescanned when its fingerprint moved, when it is new, when it is the
/// one being looked at, or when it is a candidate for "removable". That last
/// one is always rechecked: the safety claim must never rest on stale data.
pub fn needs_scan(
    full: bool,
    previous: Option<&git::Signature>,
    current: &git::Signature,
    removable_candidate: bool,
    selected: bool,
) -> bool {
    full || previous != Some(current) || removable_candidate || selected
}

/// A visible line in the list: either a pull request or a worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Pr(usize),
    Wt(usize),
}

/// What a worktree is good for right now — the question the worktree view is
/// really answering: is there work here, or can I collect the desk?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WtState {
    /// Uncommitted or unpushed work lives here. Never suggest removing it.
    Working,
    /// Its branch is merged and nothing local is at risk.
    Removable,
    /// Everything is pushed and there is nothing merged to collect.
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Recent,
    Attention,
}

impl Sort {
    pub fn label(self) -> &'static str {
        match self {
            Sort::Recent => "recent",
            Sort::Attention => "attention",
        }
    }
}

/// Screen regions recorded during the last draw so clicks can be resolved.
#[derive(Default)]
pub struct Hits {
    pub tabs: Vec<(Rect, View)>,
    pub rows: Vec<(u16, usize)>,
    pub checks: Vec<(u16, usize)>,
    pub list: Rect,
    pub detail: Rect,
}

pub struct App {
    pub repo: RepoInfo,
    pub settings: Settings,
    pub theme: Theme,
    pub viewer: String,
    pub prs: Vec<Pr>,
    pub worktrees: Vec<Worktree>,
    pub wt_by_branch: HashMap<String, usize>,
    pub merged: Vec<MergedPr>,
    pub merged_by_branch: HashMap<String, usize>,
    pub view: View,
    pub sort: Sort,
    pub rows: Vec<Row>,
    pub selected: usize,
    /// Set once the user picks a row, so late-arriving data may re-sort the
    /// list without dragging an untouched selection down the page.
    pub user_selected: bool,
    pub offset: usize,
    pub detail_scroll: u16,
    pub filter: String,
    pub filter_mode: bool,
    pub loading_prs: bool,
    pub loading_wts: bool,
    pub scanning: bool,
    pub error: Option<String>,
    /// False once the terminal reports it lost focus. Terminals that never
    /// report focus leave it true, which keeps their behaviour unchanged.
    pub focused: bool,
    /// Consecutive failed fetches; each one doubles the wait, up to MAX_WAIT.
    pub failures: u32,
    /// When the next PR refresh is due, in epoch seconds.
    pub next_refresh: i64,
    /// When the next full worktree scan is due.
    pub next_full_scan: i64,
    /// Set while the shared GitHub budget is low: no automatic fetch until then.
    pub paused_until: Option<i64>,
    /// Each worktree's fingerprint at its last scan.
    scanned: HashMap<PathBuf, git::Signature>,
    /// Makes the next worktree listing scan everything, not just what moved.
    pending_full: bool,
    pub notice: Option<(String, i64)>,
    pub last_refresh: i64,
    pub show_help: bool,
    pub quit: bool,
    pub spinner: usize,
    pub home: Option<String>,
    pub hits: Hits,
    tx: Sender<Msg>,
}

impl App {
    pub fn new(repo: RepoInfo, settings: Settings, theme: Theme) -> (Self, Receiver<Msg>) {
        let (tx, rx) = channel();
        let app = Self {
            view: settings.default_view,
            repo,
            settings,
            theme,
            viewer: String::new(),
            prs: Vec::new(),
            worktrees: Vec::new(),
            wt_by_branch: HashMap::new(),
            merged: Vec::new(),
            merged_by_branch: HashMap::new(),
            sort: Sort::Recent,
            rows: Vec::new(),
            selected: 0,
            user_selected: false,
            offset: 0,
            detail_scroll: 0,
            filter: String::new(),
            filter_mode: false,
            loading_prs: false,
            loading_wts: false,
            scanning: false,
            error: None,
            focused: true,
            failures: 0,
            next_refresh: 0,
            next_full_scan: 0,
            paused_until: None,
            scanned: HashMap::new(),
            pending_full: true,
            notice: None,
            last_refresh: 0,
            show_help: false,
            quit: false,
            spinner: 0,
            home: std::env::var("HOME").ok(),
            hits: Hits::default(),
            tx,
        };
        (app, rx)
    }

    // ---------------------------------------------------------------- loading

    /// `r`: everything, now. Clears any backoff and any rate-limit pause —
    /// the user asked, and one fetch costs a few points of budget.
    pub fn refresh(&mut self) {
        self.failures = 0;
        self.paused_until = None;
        self.pending_full = true;
        self.refresh_prs();
    }

    /// Called every turn of the event loop. Starts whatever is due; never waits.
    pub fn tick(&mut self) {
        let now = now_secs();
        if !self.loading_prs && now >= self.next_refresh {
            self.refresh_prs();
        }
        // Full sweeps pick up unstaged edits in desks whose fingerprint did not
        // move. Nobody needs that while the terminal is in the background.
        let every = self.settings.worktree_scan_secs as i64;
        if self.focused
            && every > 0
            && !self.loading_wts
            && !self.scanning
            && now >= self.next_full_scan
        {
            self.pending_full = true;
            self.refresh_worktrees();
        }
    }

    pub fn refresh_prs(&mut self) {
        if self.loading_prs {
            return;
        }
        self.loading_prs = true;
        self.last_refresh = now_secs();
        let (owner, name, max) = (
            self.repo.owner.clone(),
            self.repo.name.clone(),
            self.settings.max_prs,
        );
        spawn_reply(self.tx.clone(), Msg::Prs, move || {
            github::fetch_prs(&owner, &name, max).map_err(|e| format!("{e:#}"))
        });
        // Every PR refresh also relists worktrees and rescans the ones that moved.
        self.refresh_worktrees();
    }

    pub fn refresh_worktrees(&mut self) {
        if self.loading_wts {
            return;
        }
        self.loading_wts = true;
        let root = self.repo.root.clone();
        spawn_reply(self.tx.clone(), Msg::Worktrees, move || {
            git::list_worktrees(&root).map_err(|e| format!("{e:#}"))
        });
    }

    /// Set the next automatic refresh from the start of the last one: the
    /// configured interval, stretched fourfold while unfocused, doubled per
    /// consecutive failure, capped at MAX_WAIT, and never before a rate-limit
    /// pause ends. `refresh_secs = 0` turns automatic refresh off.
    fn schedule(&mut self) {
        self.schedule_from(self.last_refresh);
    }

    /// As `schedule`, counting the wait from `base`. A success counts from when
    /// the fetch started, which keeps a steady cadence; a failure counts from
    /// when it failed, or a call that took 45s to time out would be retried at
    /// once and a dead connection hammered back to back.
    fn schedule_from(&mut self, base: i64) {
        let every = self.settings.refresh_secs as i64;
        if every == 0 {
            self.next_refresh = i64::MAX;
            return;
        }
        let mut wait = every;
        if !self.focused {
            wait = (wait * 4).min(MAX_WAIT);
        }
        if self.failures > 0 {
            wait = (wait << self.failures.min(4)).min(MAX_WAIT);
        }
        let mut next = base + wait;
        if let Some(until) = self.paused_until {
            next = next.max(until);
        }
        self.next_refresh = next;
    }

    /// The terminal reported a focus change. Rescheduling from the last
    /// refresh is the whole mechanism: losing focus stretches the wait, and on
    /// regaining it a stale screen has a due time already in the past, so the
    /// next tick catches up at once.
    pub fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.schedule();
        }
    }

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Prs(Ok(f)) => {
                self.loading_prs = false;
                self.error = None;
                self.failures = 0;
                // Leave most of the shared budget for everything else using gh:
                // pause automatic fetches below 5% until the window resets.
                self.paused_until = f
                    .budget
                    .and_then(|b| (b.remaining < (b.limit / 20).max(50)).then_some(b.reset_at));
                self.viewer = f.viewer;
                self.repo.default_branch = f.default_branch;
                self.prs = f.prs;
                self.merged_by_branch = f
                    .merged
                    .iter()
                    .enumerate()
                    .map(|(i, m)| (m.head_ref.clone(), i))
                    .collect();
                self.merged = f.merged;
                self.schedule();
                self.rebuild();
            }
            Msg::Prs(Err(e)) => {
                self.loading_prs = false;
                self.failures += 1;
                if e.to_lowercase().contains("rate limit") {
                    self.paused_until = Some(now_secs() + MAX_WAIT);
                }
                self.error = Some(e);
                self.schedule_from(now_secs());
            }
            Msg::Worktrees(Ok(mut wts)) => {
                self.loading_wts = false;
                wts.sort_by_key(|a| a.name());
                // Carry each desk's last known status forward: most are not
                // rescanned this round, and blanking them would flicker.
                let known: HashMap<PathBuf, WorktreeStatus> = self
                    .worktrees
                    .iter()
                    .filter_map(|w| w.status.clone().map(|st| (w.path.clone(), st)))
                    .collect();
                for w in &mut wts {
                    if w.status.is_none() {
                        w.status = known.get(&w.path).cloned();
                    }
                }
                self.worktrees = wts;
                self.index_worktrees();
                self.rebuild();

                let full = std::mem::take(&mut self.pending_full);
                if full {
                    let every = self.settings.worktree_scan_secs as i64;
                    self.next_full_scan = if every > 0 {
                        now_secs() + every
                    } else {
                        i64::MAX
                    };
                }
                if self.settings.worktree_status && !self.scanning {
                    let which = self.scan_set(full);
                    if !which.is_empty() {
                        self.scanning = true;
                        let (wts, branch) =
                            (self.worktrees.clone(), self.repo.default_branch.clone());
                        spawn_reply(self.tx.clone(), Msg::Statuses, move || {
                            Ok(git::scan(&wts, &which, &branch))
                        });
                    }
                }
            }
            Msg::Worktrees(Err(e)) => {
                self.loading_wts = false;
                self.error = Some(e);
            }
            Msg::Statuses(Ok(results)) => {
                self.scanning = false;
                for (path, sig, st) in results {
                    // Adopt only for worktrees still present.
                    if let Some(w) = self.worktrees.iter_mut().find(|w| w.path == path) {
                        w.status = Some(st);
                        self.scanned.insert(path, sig);
                    }
                }
                self.rebuild();
            }
            Msg::Statuses(Err(e)) => {
                self.scanning = false;
                self.error = Some(e);
            }
        }
    }

    /// Which worktrees to rescan this round; see `needs_scan`.
    fn scan_set(&self, full: bool) -> Vec<usize> {
        let selected = self.selected_worktree().map(|w| w.path.clone());
        self.worktrees
            .iter()
            .enumerate()
            .filter(|(_, w)| {
                let candidate = !w.is_main
                    && w.branch
                        .as_deref()
                        .is_some_and(|b| self.merged_for_branch(b).is_some());
                needs_scan(
                    full,
                    self.scanned.get(&w.path),
                    &git::signature(&w.path),
                    candidate,
                    selected.as_ref() == Some(&w.path),
                )
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn index_worktrees(&mut self) {
        self.wt_by_branch = self
            .worktrees
            .iter()
            .enumerate()
            .filter_map(|(i, w)| w.branch.clone().map(|b| (b, i)))
            .collect();
    }

    pub fn worktree_for(&self, pr: &Pr) -> Option<&Worktree> {
        self.wt_by_branch
            .get(&pr.head_ref)
            .map(|&i| &self.worktrees[i])
    }

    pub fn pr_for_branch(&self, branch: &str) -> Option<&Pr> {
        self.prs.iter().find(|p| p.head_ref == branch)
    }

    /// The merged PR this worktree's branch landed as, if it is in the recent
    /// window GitHub gave us.
    pub fn merged_for_branch(&self, branch: &str) -> Option<&MergedPr> {
        self.merged_by_branch.get(branch).map(|&i| &self.merged[i])
    }

    /// A worktree is only collectable when nothing local would be lost and its
    /// branch has actually landed. The main worktree is never collectable, and
    /// a detached one has no branch to match, so both stay Idle.
    pub fn wt_state(&self, w: &Worktree) -> WtState {
        let st = w.status.clone().unwrap_or_default();
        if st.dirty > 0 || st.unpushed > 0 {
            return WtState::Working;
        }
        if w.is_main {
            return WtState::Idle;
        }
        match w.branch.as_deref() {
            Some(b) if self.pr_for_branch(b).is_none() && self.merged_for_branch(b).is_some() => {
                WtState::Removable
            }
            _ => WtState::Idle,
        }
    }

    pub fn removable_count(&self) -> usize {
        self.worktrees
            .iter()
            .filter(|w| self.wt_state(w) == WtState::Removable)
            .count()
    }

    // ------------------------------------------------------------- row set

    /// Recompute the visible rows for the current view, filter and sort,
    /// keeping the previously selected item selected where possible.
    pub fn rebuild(&mut self) {
        let keep = if self.user_selected {
            self.rows.get(self.selected).copied()
        } else {
            None
        };
        let q = self.filter.to_lowercase();

        self.rows = match self.view {
            View::Worktrees => {
                let mut idx: Vec<usize> = (0..self.worktrees.len())
                    .filter(|&i| self.wt_matches(i, &q))
                    .collect();
                idx.sort_by(|&a, &b| {
                    let ka = self.worktrees[a]
                        .status
                        .as_ref()
                        .and_then(|s| s.last_commit_at);
                    let kb = self.worktrees[b]
                        .status
                        .as_ref()
                        .and_then(|s| s.last_commit_at);
                    kb.cmp(&ka)
                        .then_with(|| self.worktrees[a].name().cmp(&self.worktrees[b].name()))
                });
                idx.into_iter().map(Row::Wt).collect()
            }
            v => {
                let viewer = self.viewer.clone();
                let mut idx: Vec<usize> = (0..self.prs.len())
                    .filter(|&i| {
                        let p = &self.prs[i];
                        if !self.settings.show_drafts && p.is_draft {
                            return false;
                        }
                        let in_view = match v {
                            View::Ready => p.is_ready(),
                            View::Mine => p.is_mine(&viewer),
                            View::Review => p.wants_my_review(&viewer),
                            View::Assigned => p.assigned_to_me(&viewer),
                            View::Blocked => p.is_blocked(),
                            _ => true,
                        };
                        in_view && self.pr_matches(i, &q)
                    })
                    .collect();
                if self.sort == Sort::Attention {
                    idx.sort_by_key(|&i| (attention_rank(&self.prs[i]), -self.prs[i].updated_at));
                } else {
                    idx.sort_by_key(|&i| -self.prs[i].updated_at);
                }
                idx.into_iter().map(Row::Pr).collect()
            }
        };

        self.selected = keep
            .and_then(|k| self.rows.iter().position(|r| *r == k))
            .unwrap_or(0)
            .min(self.rows.len().saturating_sub(1));
        self.detail_scroll = 0;
    }

    fn pr_matches(&self, i: usize, q: &str) -> bool {
        if q.is_empty() {
            return true;
        }
        let p = &self.prs[i];
        p.title.to_lowercase().contains(q)
            || p.head_ref.to_lowercase().contains(q)
            || p.author.to_lowercase().contains(q)
            || p.number.to_string().contains(q)
            || p.labels.iter().any(|l| l.to_lowercase().contains(q))
    }

    fn wt_matches(&self, i: usize, q: &str) -> bool {
        if q.is_empty() {
            return true;
        }
        let w = &self.worktrees[i];
        w.name().to_lowercase().contains(q)
            || w.branch.as_deref().unwrap_or("").to_lowercase().contains(q)
            || w.path.to_string_lossy().to_lowercase().contains(q)
    }

    pub fn count_for(&self, v: View) -> usize {
        match v {
            View::Worktrees => self.worktrees.len(),
            View::All => self.prs.len(),
            View::Ready => self.prs.iter().filter(|p| p.is_ready()).count(),
            View::Blocked => self.prs.iter().filter(|p| p.is_blocked()).count(),
            View::Mine => self.prs.iter().filter(|p| p.is_mine(&self.viewer)).count(),
            View::Review => self
                .prs
                .iter()
                .filter(|p| p.wants_my_review(&self.viewer))
                .count(),
            View::Assigned => self
                .prs
                .iter()
                .filter(|p| p.assigned_to_me(&self.viewer))
                .count(),
        }
    }

    pub fn selected_row(&self) -> Option<Row> {
        self.rows.get(self.selected).copied()
    }

    pub fn selected_pr(&self) -> Option<&Pr> {
        match self.selected_row()? {
            Row::Pr(i) => self.prs.get(i),
            Row::Wt(i) => {
                let b = self.worktrees.get(i)?.branch.as_deref()?;
                self.pr_for_branch(b)
            }
        }
    }

    pub fn selected_worktree(&self) -> Option<&Worktree> {
        match self.selected_row()? {
            Row::Wt(i) => self.worktrees.get(i),
            Row::Pr(i) => self.worktree_for(self.prs.get(i)?),
        }
    }

    // ------------------------------------------------------------ navigation

    pub fn move_sel(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        self.user_selected = true;
        let n = self.rows.len() as isize;
        self.selected = (self.selected as isize + delta).clamp(0, n - 1) as usize;
        self.detail_scroll = 0;
    }

    pub fn select(&mut self, i: usize) {
        if i < self.rows.len() {
            self.user_selected = true;
            self.selected = i;
            self.detail_scroll = 0;
        }
    }

    pub fn set_view(&mut self, v: View) {
        if self.view != v {
            self.view = v;
            self.selected = 0;
            self.user_selected = false;
            self.offset = 0;
            self.rows.clear();
            self.rebuild();
        }
    }

    pub fn cycle_view(&mut self, delta: isize) {
        let views = self.settings.views.clone();
        let cur = views.iter().position(|v| *v == self.view).unwrap_or(0) as isize;
        let n = views.len() as isize;
        self.set_view(views[(cur + delta).rem_euclid(n) as usize]);
    }

    /// Number keys index the configured tab bar, not a fixed list of views.
    pub fn view_at(&self, i: usize) -> Option<View> {
        self.settings.views.get(i).copied()
    }

    /// Keep the selection inside the window of `height` visible rows.
    pub fn clamp_scroll(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + height {
            self.offset = self.selected + 1 - height;
        }
        let max_off = self.rows.len().saturating_sub(height);
        self.offset = self.offset.min(max_off);
    }

    // --------------------------------------------------------------- actions

    pub fn note(&mut self, s: impl Into<String>) {
        self.notice = Some((s.into(), now_secs()));
    }

    pub fn open_url(&mut self, url: &str) {
        let cmd = self.settings.open_command.clone();
        let mut parts = cmd.split_whitespace();
        let Some(bin) = parts.next() else { return };
        let args: Vec<&str> = parts.collect();
        match Command::new(bin)
            .args(&args)
            .arg(url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_) => self.note(format!("opened {url}")),
            Err(e) => self.note(format!("could not run `{bin}`: {e}")),
        }
    }

    pub fn open_selected(&mut self) {
        if let Some(url) = self.selected_pr().map(|p| p.url.clone()) {
            self.open_url(&url);
            return;
        }
        // A collected desk still has a story: open the PR its branch landed as.
        let landed = self
            .selected_worktree()
            .and_then(|w| w.branch.clone())
            .and_then(|b| self.merged_for_branch(&b).map(|m| m.url.clone()));
        match landed {
            Some(url) => self.open_url(&url),
            None => {
                if let Some(w) = self.selected_worktree() {
                    let name = w.name();
                    self.note(format!("{name} has no PR, open or merged"));
                }
            }
        }
    }

    pub fn open_checks(&mut self) {
        if let Some(url) = self.selected_pr().map(|p| p.checks_url()) {
            self.open_url(&url);
        }
    }

    pub fn open_check(&mut self, i: usize) {
        let url = self
            .selected_pr()
            .and_then(|p| p.checks.get(i))
            .and_then(|c| c.url.clone());
        match url {
            Some(u) => self.open_url(&u),
            None => self.note("that check has no link"),
        }
    }

    pub fn copy_url(&mut self) {
        let Some(url) = self.selected_pr().map(|p| p.url.clone()) else {
            return;
        };
        let cmd = self.settings.copy_command.clone();
        let mut parts = cmd.split_whitespace();
        let Some(bin) = parts.next() else { return };
        let args: Vec<&str> = parts.collect();
        let child = Command::new(bin)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match child {
            Ok(mut c) => {
                if let Some(mut si) = c.stdin.take() {
                    let _ = si.write_all(url.as_bytes());
                }
                let _ = c.wait();
                self.note(format!("copied {url}"));
            }
            Err(e) => self.note(format!("could not run `{bin}`: {e}")),
        }
    }
}

/// Ordering for the "attention" sort. Mergeable PRs come first because merging
/// them is the point of the dashboard; then whatever is blocking the rest.
fn attention_rank(p: &Pr) -> i32 {
    if p.is_ready() {
        return 0;
    }
    if p.rollup == CheckState::Failure {
        return 1;
    }
    if p.review_decision == Some(ReviewDecision::ChangesRequested) {
        return 2;
    }
    if p.mergeable == Mergeable::Conflicting {
        return 3;
    }
    if p.rollup == CheckState::Pending {
        return 4;
    }
    if p.is_draft {
        return 6;
    }
    5
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    use crate::github::{Budget, Fetched};
    use crate::model::RepoInfo;
    use std::time::{Duration, UNIX_EPOCH};

    fn app() -> App {
        let repo = RepoInfo {
            owner: "acme".into(),
            name: "widget".into(),
            default_branch: "main".into(),
            root: "/tmp/widget".into(),
            current_branch: None,
        };
        let settings = Settings {
            refresh_secs: 90,
            worktree_status: false,
            ..Settings::default()
        };
        App::new(repo, settings, Theme::default()).0
    }

    fn fetched(budget: Option<Budget>) -> Fetched {
        Fetched {
            budget,
            viewer: "octocat".into(),
            default_branch: "main".into(),
            prs: vec![],
            merged: vec![],
        }
    }

    /// A failure must never stop refreshing — it waits longer each time,
    /// doubling up to fifteen minutes, and a success resets it.
    #[test]
    fn failures_back_off_exponentially_and_recover() {
        let mut a = app();
        a.last_refresh = 1_000;
        let mut waits = Vec::new();
        for _ in 0..6 {
            a.loading_prs = true;
            a.on_msg(Msg::Prs(Err("gh api graphql: timed out after 45s".into())));
            // Counted from the failure, not from when the attempt began.
            waits.push(a.next_refresh - now_secs());
        }
        let want = [180, 360, 720, 900, 900, 900];
        assert!(
            waits.iter().zip(want).all(|(g, w)| (g - w).abs() <= 1),
            "{waits:?}"
        );
        assert!(a.error.is_some());

        a.loading_prs = true;
        a.on_msg(Msg::Prs(Ok(fetched(None))));
        assert_eq!(a.failures, 0);
        assert_eq!(a.next_refresh - a.last_refresh, 90);
        assert!(a.error.is_none());
    }

    /// rigor shares the GraphQL budget with every other gh call, agents
    /// included. Below 5% it stops fetching until the window resets.
    #[test]
    fn a_low_shared_budget_pauses_until_reset() {
        let mut a = app();
        a.last_refresh = 1_000;
        let low = Budget {
            remaining: 120,
            limit: 5000,
            reset_at: 5_000,
        };
        a.loading_prs = true;
        a.on_msg(Msg::Prs(Ok(fetched(Some(low)))));
        assert_eq!(a.paused_until, Some(5_000));
        assert_eq!(a.next_refresh, 5_000);

        let healthy = Budget {
            remaining: 4_500,
            limit: 5000,
            reset_at: 5_000,
        };
        a.loading_prs = true;
        a.on_msg(Msg::Prs(Ok(fetched(Some(healthy)))));
        assert_eq!(a.paused_until, None);
        assert_eq!(a.next_refresh, 1_090);
    }

    #[test]
    fn a_rate_limit_error_pauses_rather_than_retrying_at_once() {
        let mut a = app();
        a.loading_prs = true;
        a.on_msg(Msg::Prs(Err(
            "GitHub returned errors: API rate limit exceeded".into(),
        )));
        assert!(a.paused_until.is_some_and(|p| p > now_secs()));
        assert!(a.next_refresh >= a.paused_until.unwrap());
    }

    /// In the background the interval stretches fourfold; back in focus, a
    /// stale screen is due immediately.
    #[test]
    fn focus_stretches_the_interval_and_catches_up_on_return() {
        let mut a = app();
        a.last_refresh = now_secs() - 200;
        a.schedule();
        a.set_focus(false);
        assert_eq!(a.next_refresh - a.last_refresh, 360);
        a.set_focus(true);
        assert!(
            a.next_refresh <= now_secs(),
            "a stale screen should refresh at once"
        );
    }

    #[test]
    fn manual_refresh_clears_backoff_and_pause() {
        let mut a = app();
        a.failures = 3;
        a.paused_until = Some(now_secs() + 600);
        a.refresh();
        assert_eq!(a.failures, 0);
        assert!(a.paused_until.is_none());
        assert!(a.loading_prs);
    }

    /// The whole efficiency win rests on this: an idle desk is skipped, a
    /// desk that moved is rescanned, and a removable candidate or the selected
    /// desk is always rescanned.
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

    /// A panicking worker still answers, so its loading flag always clears.
    #[test]
    fn a_panicking_worker_still_reports_back() {
        let (tx, rx) = channel();
        spawn_reply::<()>(
            tx,
            |r| Msg::Worktrees(r.map(|_| Vec::new())),
            || panic!("boom"),
        );
        match rx.recv_timeout(Duration::from_secs(5)).expect("no reply") {
            Msg::Worktrees(Err(e)) => assert!(e.contains("panicked"), "{e}"),
            _ => panic!("unexpected message"),
        }
    }
}

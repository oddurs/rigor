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
use crate::schedule::{self, Schedule};
use crate::theme::Theme;
use crate::util::now_secs;

pub enum Msg {
    Prs(Result<github::Fetched, String>),
    Worktrees(Result<Vec<Worktree>, String>),
    Statuses(Result<Vec<(PathBuf, git::Signature, WorktreeStatus)>, String>),
}

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

/// What the keyboard is doing: browsing, typing a filter, or reading help.
/// One at a time — the type makes "typing a filter with help open" impossible,
/// where two booleans made it merely unlikely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Browse,
    Filter,
    Help,
}

/// Background work running right now.
#[derive(Debug, Clone, Copy, Default)]
pub struct InFlight {
    pub prs: bool,
    pub worktrees: bool,
    pub scan: bool,
}

impl InFlight {
    /// A fetch or listing the user is waiting on, which is what the spinner
    /// shows. A scan is not: it runs at background priority and nobody waits.
    pub const fn syncing(self) -> bool {
        self.prs || self.worktrees
    }
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
    pub const fn label(self) -> &'static str {
        match self {
            Self::Recent => "recent",
            Self::Attention => "attention",
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
    pub mode: Mode,
    pub in_flight: InFlight,
    pub schedule: Schedule,
    pub error: Option<String>,
    /// Each worktree's fingerprint at its last scan.
    scanned: HashMap<PathBuf, git::Signature>,
    pub notice: Option<(String, i64)>,
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
            mode: Mode::Browse,
            in_flight: InFlight::default(),
            schedule: Schedule::default(),
            error: None,
            scanned: HashMap::new(),
            notice: None,
            quit: false,
            spinner: 0,
            home: std::env::var("HOME").ok(),
            hits: Hits::default(),
            tx,
        };
        (app, rx)
    }

    // ---------------------------------------------------------------- loading

    /// `r`: everything, now, whatever the backoff or pause says.
    pub fn refresh(&mut self) {
        self.schedule.reset();
        self.refresh_prs();
    }

    /// Called every turn of the event loop. Starts whatever is due; never waits.
    pub fn tick(&mut self) {
        let now = now_secs();
        if !self.in_flight.prs && self.schedule.refresh_due(now) {
            self.refresh_prs();
        }
        if !self.in_flight.worktrees
            && !self.in_flight.scan
            && self
                .schedule
                .full_scan_due(now, self.settings.worktree_scan_secs)
        {
            self.schedule.pending_full = true;
            self.refresh_worktrees();
        }
    }

    pub fn refresh_prs(&mut self) {
        if self.in_flight.prs {
            return;
        }
        self.in_flight.prs = true;
        self.schedule.last_refresh = now_secs();
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
        if self.in_flight.worktrees {
            return;
        }
        self.in_flight.worktrees = true;
        let root = self.repo.root.clone();
        spawn_reply(self.tx.clone(), Msg::Worktrees, move || {
            git::list_worktrees(&root).map_err(|e| format!("{e:#}"))
        });
    }

    pub fn set_focus(&mut self, focused: bool) {
        self.schedule.set_focus(focused, self.settings.refresh_secs);
    }

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Prs(Ok(f)) => {
                self.in_flight.prs = false;
                self.error = None;
                self.schedule
                    .succeeded(f.budget, self.settings.refresh_secs);
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
                self.rebuild();
            }
            Msg::Prs(Err(e)) => {
                self.in_flight.prs = false;
                self.schedule
                    .failed(&e, self.settings.refresh_secs, now_secs());
                self.error = Some(e);
            }
            Msg::Worktrees(Ok(mut wts)) => {
                self.in_flight.worktrees = false;
                wts.sort_by_key(Worktree::name);
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

                let full = std::mem::take(&mut self.schedule.pending_full);
                if full {
                    self.schedule
                        .full_scan_started(now_secs(), self.settings.worktree_scan_secs);
                }
                if self.settings.worktree_status && !self.in_flight.scan {
                    let which = self.scan_set(full);
                    if !which.is_empty() {
                        self.in_flight.scan = true;
                        let (wts, branch) =
                            (self.worktrees.clone(), self.repo.default_branch.clone());
                        spawn_reply(self.tx.clone(), Msg::Statuses, move || {
                            Ok(git::scan(&wts, &which, &branch))
                        });
                    }
                }
            }
            Msg::Worktrees(Err(e)) => {
                self.in_flight.worktrees = false;
                self.error = Some(e);
            }
            Msg::Statuses(Ok(results)) => {
                self.in_flight.scan = false;
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
                self.in_flight.scan = false;
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
                schedule::needs_scan(
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
        // Unknown status counts as clean here, as it always has: `Working`
        // needs evidence of local work, and "removable" also needs the branch
        // to have landed, which is checked below.
        if w.status
            .as_ref()
            .is_some_and(|st| st.dirty > 0 || st.unpushed > 0)
        {
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
        let last = self.rows.len() - 1;
        self.selected = self.selected.saturating_add_signed(delta).min(last);
        self.detail_scroll = 0;
    }

    pub const fn select(&mut self, i: usize) {
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
        let views = &self.settings.views;
        let n = views.len();
        if n == 0 {
            return;
        }
        let cur = views.iter().position(|v| *v == self.view).unwrap_or(0);
        let step = delta.unsigned_abs() % n;
        let next = if delta >= 0 {
            (cur + step) % n
        } else {
            (cur + n - step) % n
        };
        let view = views[next];
        self.set_view(view);
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
        if let Some(url) = self.selected_pr().map(Pr::checks_url) {
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
    use std::time::Duration;

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

    /// Fetch results reach the schedule: a failure is shown and backs off, a
    /// success clears it and takes the budget into account.
    #[test]
    fn fetch_results_drive_the_schedule() {
        let mut a = app();
        a.in_flight.prs = true;
        a.on_msg(Msg::Prs(Err("gh api graphql: timed out after 45s".into())));
        assert!(!a.in_flight.prs, "the flag always clears");
        assert_eq!(a.schedule.failures, 1);
        assert!(a.error.is_some());
        assert!(a.schedule.next_refresh > now_secs());

        a.schedule.last_refresh = 1_000;
        a.in_flight.prs = true;
        let low = Budget {
            remaining: 1,
            limit: 5000,
            reset_at: 5_000,
        };
        a.on_msg(Msg::Prs(Ok(fetched(Some(low)))));
        assert_eq!(a.schedule.failures, 0);
        assert!(a.error.is_none());
        assert_eq!(a.schedule.paused_until, Some(5_000));
    }

    #[test]
    fn manual_refresh_clears_backoff_and_pause_and_fetches() {
        let mut a = app();
        a.schedule.failures = 3;
        a.schedule.paused_until = Some(now_secs() + 600);
        a.refresh();
        assert_eq!(a.schedule.failures, 0);
        assert!(a.schedule.paused_until.is_none());
        assert!(a.in_flight.prs);
    }

    /// A panicking worker still answers, so its loading flag always clears.
    #[test]
    fn a_panicking_worker_still_reports_back() {
        let (tx, rx) = channel();
        spawn_reply::<()>(
            tx,
            |r| Msg::Worktrees(r.map(|()| Vec::new())),
            || panic!("boom"),
        );
        match rx.recv_timeout(Duration::from_secs(5)).expect("no reply") {
            Msg::Worktrees(Err(e)) => assert!(e.contains("panicked"), "{e}"),
            _ => panic!("unexpected message"),
        }
    }
}

//! Application state: what is loaded, what is selected, and what keys and
//! clicks do to it.

use ratatui::layout::Rect;
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::config::{Settings, View};
use crate::git;
use crate::github;
use crate::model::{CheckState, Mergeable, MergedPr, Pr, RepoInfo, ReviewDecision, Worktree};
use crate::theme::Theme;
use crate::util::now_secs;

pub enum Msg {
    Prs(Result<github::Fetched, String>),
    Worktrees(Result<Vec<Worktree>, String>),
    Statuses(Vec<Worktree>),
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
    pub error: Option<String>,
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
            error: None,
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

    pub fn refresh(&mut self) {
        self.refresh_prs();
        self.refresh_worktrees();
    }

    pub fn refresh_prs(&mut self) {
        if self.loading_prs {
            return;
        }
        self.loading_prs = true;
        self.last_refresh = now_secs();
        let (tx, owner, name, max) = (
            self.tx.clone(),
            self.repo.owner.clone(),
            self.repo.name.clone(),
            self.settings.max_prs,
        );
        std::thread::spawn(move || {
            let res = github::fetch_prs(&owner, &name, max).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Prs(res));
        });
    }

    pub fn refresh_worktrees(&mut self) {
        if self.loading_wts {
            return;
        }
        self.loading_wts = true;
        let (tx, root) = (self.tx.clone(), self.repo.root.clone());
        std::thread::spawn(move || {
            let res = git::list_worktrees(&root).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Worktrees(res));
        });
    }

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Prs(Ok(f)) => {
                self.loading_prs = false;
                self.error = None;
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
                self.loading_prs = false;
                self.error = Some(e);
            }
            Msg::Worktrees(Ok(mut wts)) => {
                self.loading_wts = false;
                wts.sort_by_key(|a| a.name());
                self.worktrees = wts;
                self.index_worktrees();
                self.rebuild();
                if self.settings.worktree_status {
                    let (tx, mut copy, branch) = (
                        self.tx.clone(),
                        self.worktrees.clone(),
                        self.repo.default_branch.clone(),
                    );
                    std::thread::spawn(move || {
                        git::fill_statuses(&mut copy, &branch);
                        let _ = tx.send(Msg::Statuses(copy));
                    });
                }
            }
            Msg::Worktrees(Err(e)) => {
                self.loading_wts = false;
                self.error = Some(e);
            }
            Msg::Statuses(wts) => {
                // Only adopt statuses whose worktree is still present.
                let by_path: HashMap<_, _> = wts
                    .into_iter()
                    .map(|w| (w.path.clone(), w.status))
                    .collect();
                for w in &mut self.worktrees {
                    if let Some(st) = by_path.get(&w.path) {
                        w.status = st.clone();
                    }
                }
                self.rebuild();
            }
        }
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

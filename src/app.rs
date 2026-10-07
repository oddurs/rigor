//! Application state: what is loaded, what is selected, and what keys and
//! clicks do to it.

use ratatui::layout::Rect;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::config::{Settings, View};
use crate::drafts;
use crate::git;
use crate::github;
use crate::model::{
    CheckState, Mergeable, MergedPr, Pr, RepoInfo, ReviewDecision, Worktree, WorktreeStatus,
};
use crate::schedule::{self, Schedule};
use crate::theme::Theme;
use crate::util::{format_iso8601, now_secs};

pub enum Msg {
    Prs(Result<github::Fetched, String>),
    Worktrees(Result<Vec<Worktree>, String>),
    Statuses(Result<Vec<git::Scanned>, String>),
    /// How the review in `App::posting` went.
    Posted(Result<(), String>),
}

/// Sends one review: owner, repository, pull request number, request body.
pub type Poster = Arc<dyn Fn(&str, &str, u64, &str) -> Result<(), String> + Send + Sync>;

/// A post waiting on its second `a`: which draft, the file as it read when
/// the prompt went up, and what the prompt says would be sent. Once the
/// selected draft stops matching, the question on screen is no longer the one
/// a keypress would be answering, and it is withdrawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Armed {
    pub number: u64,
    raw: String,
    pub what: String,
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

/// A visible line in the list: a pull request, a worktree, or a review draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Pr(usize),
    Wt(usize),
    Draft(usize),
}

/// What the last fetch knows about the pull request a draft was written for.
#[derive(Clone, Copy)]
pub enum DraftPr<'a> {
    Open(&'a Pr),
    Merged(&'a MergedPr),
    /// Not among the open or recently merged pull requests rigor fetched.
    Unknown,
}

impl DraftPr<'_> {
    /// The pull request's head commit, when the fetch carried one.
    pub const fn head(&self) -> Option<&str> {
        match self {
            Self::Open(p) => Some(p.head_oid.as_str()),
            Self::Merged(m) => Some(m.head_oid.as_str()),
            Self::Unknown => None,
        }
    }

    pub const fn url(&self) -> Option<&str> {
        match self {
            Self::Open(p) => Some(p.url.as_str()),
            Self::Merged(m) => Some(m.url.as_str()),
            Self::Unknown => None,
        }
    }

    /// When the pull request last moved, for ordering and the age column.
    pub const fn touched_at(&self) -> Option<i64> {
        match self {
            Self::Open(p) => Some(p.updated_at),
            Self::Merged(m) => Some(m.merged_at),
            Self::Unknown => None,
        }
    }
}

/// What a worktree is good for right now — the question the worktree view is
/// really answering: is there work here, or can I collect the desk?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WtState {
    /// Uncommitted or unpushed work lives here. Never suggest removing it.
    Working,
    /// Its work landed in a merged PR and nothing local is at risk.
    Removable,
    /// Nothing to act on: work in progress with a PR, nothing merged to
    /// collect, or a status not yet known.
    Idle,
}

/// Everything the worktree view says about one desk, worked out in one place
/// so the list, the detail pane and the counts cannot disagree.
pub struct Desk<'a> {
    pub wt: &'a Worktree,
    /// The open PR for its branch.
    pub pr: Option<&'a Pr>,
    /// The merged PR its branch name last landed as.
    pub merged: Option<&'a MergedPr>,
    /// HEAD is that merged PR's final commit, so everything here reached
    /// GitHub. Compared by commit because branch names are reused, and
    /// because after a squash merge whose remote branch was deleted, git alone
    /// counts every commit on the branch as unpushed.
    pub landed: bool,
    pub state: WtState,
}

impl Desk<'_> {
    /// The desk's status, with commits that landed counted as pushed. `None`
    /// until a scan has read it: unknown is not clean.
    pub fn status(&self) -> Option<WorktreeStatus> {
        let mut st = self.wt.status.clone()?;
        if self.landed {
            st.unpushed = 0;
            st.published = true;
        }
        Some(st)
    }
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
    /// Where review drafts are read from. `main` sets it; an `App` built
    /// without it reads none, so a test never picks up the developer's own.
    pub drafts_dir: Option<PathBuf>,
    pub drafts: Vec<drafts::Entry>,
    pub armed: Option<Armed>,
    /// The pull request a review is being sent to right now, and the draft
    /// file it came from. One at a time.
    pub posting: Option<(u64, PathBuf)>,
    /// Pull requests whose review went out but whose draft file could not be
    /// marked posted. The file still says `drafted`; these are never offered
    /// again this session.
    unrecorded: HashSet<u64>,
    /// Why the last post failed, until the next keypress. Kept apart from
    /// `error`, which is the sync's and is cleared by the next good fetch.
    pub post_error: Option<String>,
    /// How a review is sent. Inert until `main` wires in `gh`, so an `App`
    /// built in a test cannot reach GitHub.
    pub poster: Poster,
    /// A draft file the user asked to edit. The event loop owns the terminal,
    /// so it is the one to run the editor.
    pub edit_request: Option<PathBuf>,
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
            drafts_dir: None,
            drafts: Vec::new(),
            armed: None,
            posting: None,
            unrecorded: HashSet::new(),
            post_error: None,
            poster: Arc::new(|_, _, _, _| Err("posting is not wired up".into())),
            edit_request: None,
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
        // Read here rather than on a worker: a handful of small local files,
        // and a draft written since the last look should not wait on GitHub.
        self.reload_drafts();
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
                        // A status git could not read is forgotten, not kept
                        // stale, and the desk is scanned again next round.
                        if st.is_some() {
                            self.scanned.insert(path, sig);
                        } else {
                            self.scanned.remove(&path);
                        }
                        w.status = st;
                    }
                }
                self.rebuild();
            }
            Msg::Statuses(Err(e)) => {
                self.in_flight.scan = false;
                self.error = Some(e);
            }
            Msg::Posted(result) => self.posted(result),
        }
    }

    /// The review in `posting` came back. Only one that went out changes the
    /// draft's file; a failure leaves it exactly as it was.
    fn posted(&mut self, result: Result<(), String>) {
        let Some((number, path)) = self.posting.take() else {
            return;
        };
        match result {
            Ok(()) => {
                let at = format_iso8601(now_secs());
                let slug = self.repo.slug();
                match drafts::set_status(&path, &slug, drafts::Status::Posted, Some(at)) {
                    Ok(()) => self.note(format!("posted to #{number}")),
                    // The review is on GitHub whatever the file says.
                    Err(e) => {
                        self.unrecorded.insert(number);
                        self.post_error = Some(format!(
                            "the review was posted to #{number}, but its draft file could not be updated: {e}"
                        ));
                    }
                }
            }
            Err(e) => self.post_error = Some(format!("posting to #{number} failed: {e}")),
        }
        self.reload_drafts();
    }

    /// Which worktrees to rescan this round; see `needs_scan`.
    fn scan_set(&self, full: bool) -> Vec<usize> {
        let selected = self.selected_worktree().map(|w| w.path.clone());
        self.worktrees
            .iter()
            .enumerate()
            .filter(|(_, w)| {
                let candidate = !w.is_main && self.desk(w).landed;
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

    /// A worktree is only collectable when its work landed and a scan found
    /// nothing local that would be lost. The main worktree is never
    /// collectable, and a detached one has no branch to match.
    pub fn desk<'a>(&'a self, wt: &'a Worktree) -> Desk<'a> {
        let branch = wt.branch.as_deref();
        let pr = branch.and_then(|b| self.pr_for_branch(b));
        let merged = branch.and_then(|b| self.merged_for_branch(b));
        let landed = merged.is_some_and(|m| !m.head_oid.is_empty() && m.head_oid == wt.head);
        let state = match &wt.status {
            Some(st) if st.dirty > 0 || (st.unpushed > 0 && !landed) => WtState::Working,
            Some(_) if landed && !wt.is_main && pr.is_none() => WtState::Removable,
            // Includes a desk not scanned yet, or with status off: with no
            // evidence either way, it is neither working nor removable.
            _ => WtState::Idle,
        };
        Desk {
            wt,
            pr,
            merged,
            landed,
            state,
        }
    }

    pub fn wt_state(&self, w: &Worktree) -> WtState {
        self.desk(w).state
    }

    pub fn removable_count(&self) -> usize {
        self.worktrees
            .iter()
            .filter(|w| self.wt_state(w) == WtState::Removable)
            .count()
    }

    // ---------------------------------------------------------------- drafts

    /// Read this repository's drafts again. Rows hold positions in the list,
    /// so the Drafts view is rebuilt with it.
    pub fn reload_drafts(&mut self) {
        let Some(dir) = &self.drafts_dir else {
            return;
        };
        // Taken before the list is replaced: after it, the same position in
        // the list may be another draft.
        let cursor = self.selected_draft().map(|e| e.path.clone());
        match drafts::load(dir, &self.repo.owner, &self.repo.name) {
            Ok(found) => self.drafts = found,
            Err(e) => self.note(format!("could not read review drafts: {e}")),
        }
        if self.view == View::Drafts {
            self.rebuild_around(cursor);
        }
    }

    /// The pull request a draft is for, open or recently merged, as of the
    /// last fetch.
    pub fn draft_pr(&self, entry: &drafts::Entry) -> DraftPr<'_> {
        let Some(number) = entry.number else {
            return DraftPr::Unknown;
        };
        if let Some(p) = self.prs.iter().find(|p| p.number == number) {
            return DraftPr::Open(p);
        }
        self.merged
            .iter()
            .find(|m| m.number == number)
            .map_or(DraftPr::Unknown, DraftPr::Merged)
    }

    pub fn draft_state(&self, entry: &drafts::Entry) -> drafts::State {
        drafts::state(&entry.draft, self.draft_pr(entry).head())
    }

    /// Drafts still waiting on a decision: neither posted nor skipped.
    pub fn drafted_count(&self) -> usize {
        self.drafts
            .iter()
            .filter(|e| {
                e.draft
                    .as_ref()
                    .is_ok_and(|d| d.status == drafts::Status::Drafted)
            })
            .count()
    }

    pub fn selected_draft(&self) -> Option<&drafts::Entry> {
        match self.selected_row()? {
            Row::Draft(i) => self.drafts.get(i),
            Row::Pr(_) | Row::Wt(_) => None,
        }
    }

    /// `a`. Posting is public and under the user's name, so one keypress only
    /// asks: the first `a` puts up a prompt naming what would be sent, and
    /// only a second `a` on the same, unchanged draft sends it. `armed` is
    /// the prompt that was up when this key arrived, if there was one.
    pub fn post_key(&mut self, armed: Option<&Armed>) {
        let Some(entry) = self.selected_draft().cloned() else {
            return;
        };
        let label = entry.label();
        let state = self.draft_state(&entry);
        let (Some(number), Ok(draft), drafts::State::Ready) = (entry.number, &entry.draft, state)
        else {
            let why = match state {
                drafts::State::Posted => "is already posted",
                drafts::State::Skipped => "is skipped; x brings it back",
                drafts::State::Stale => {
                    "is stale: the pull request has moved on since this draft was written"
                }
                drafts::State::Unverified => {
                    "is unverified: its pull request was not fetched, so its head is not known"
                }
                drafts::State::Invalid | drafts::State::Ready => {
                    "is invalid and cannot be posted; e opens it in your editor"
                }
            };
            self.note(format!("{label} {why}"));
            return;
        };
        if self.posting.is_some() {
            self.note("a review is already being posted");
            return;
        }
        if self.unrecorded.contains(&number) {
            self.note(format!(
                "#{number} was posted this session; its draft file could not be updated"
            ));
            return;
        }
        let ask = Armed {
            number,
            raw: entry.raw.clone(),
            what: draft.describe(),
        };
        if armed != Some(&ask) {
            self.armed = Some(ask);
            return;
        }

        // The file is the draft. If it changed after the prompt went up, what
        // would go out is not what was confirmed.
        let on_disk = std::fs::read_to_string(&entry.path).ok();
        if on_disk.as_deref() != Some(entry.raw.as_str()) {
            self.reload_drafts();
            self.note(format!("{label} changed on disk; nothing was posted"));
            return;
        }
        let body = match drafts::payload(draft) {
            Ok(body) => body,
            Err(e) => {
                self.note(format!("could not build the review for {label}: {e}"));
                return;
            }
        };
        self.posting = Some((number, entry.path.clone()));
        self.note(format!("posting {} to {label}…", ask.what));
        let (owner, name) = (self.repo.owner.clone(), self.repo.name.clone());
        let post = Arc::clone(&self.poster);
        spawn_reply(self.tx.clone(), Msg::Posted, move || {
            post(&owner, &name, number, &body)
        });
    }

    /// `x`: set a draft aside, or bring a skipped one back.
    pub fn skip_key(&mut self) {
        let Some(entry) = self.selected_draft().cloned() else {
            return;
        };
        let label = entry.label();
        let Ok(draft) = &entry.draft else {
            self.note(format!("{label} is invalid; e opens it in your editor"));
            return;
        };
        if self.posting.as_ref().map(|(n, _)| *n) == entry.number {
            self.note(format!("{label} is being posted"));
            return;
        }
        let to = match draft.status {
            drafts::Status::Drafted => drafts::Status::Skipped,
            drafts::Status::Skipped => drafts::Status::Drafted,
            drafts::Status::Posted => {
                self.note(format!("{label} is already posted"));
                return;
            }
        };
        match drafts::set_status(&entry.path, &self.repo.slug(), to, None) {
            Ok(()) if to == drafts::Status::Skipped => self.note(format!("skipped {label}")),
            Ok(()) => self.note(format!("{label} is a draft again")),
            Err(e) => self.note(format!("could not update {label}: {e}")),
        }
        self.reload_drafts();
    }

    /// `e`: ask for the selected draft's file to be opened in an editor.
    pub fn edit_key(&mut self) {
        if let Some(entry) = self.selected_draft() {
            self.edit_request = Some(entry.path.clone());
        }
    }

    fn draft_matches(&self, i: usize, q: &str) -> bool {
        if q.is_empty() {
            return true;
        }
        let entry = &self.drafts[i];
        let about_pr = match self.draft_pr(entry) {
            DraftPr::Open(p) => {
                p.title.to_lowercase().contains(q)
                    || p.head_ref.to_lowercase().contains(q)
                    || p.author.to_lowercase().contains(q)
            }
            DraftPr::Merged(m) => {
                m.title.to_lowercase().contains(q) || m.head_ref.to_lowercase().contains(q)
            }
            DraftPr::Unknown => false,
        };
        about_pr || entry.label().to_lowercase().contains(q)
    }

    // ------------------------------------------------------------- row set

    /// Recompute the visible rows for the current view, filter and sort,
    /// keeping the previously selected item selected where possible.
    pub fn rebuild(&mut self) {
        let cursor = self.selected_draft().map(|e| e.path.clone());
        self.rebuild_around(cursor);
    }

    /// `rebuild`, told which draft the selection was on — by file, since a
    /// row's position and a draft's place in the list both change as drafts
    /// come, go and re-sort. A draft is what a keypress may post, so in the
    /// Drafts view the selection follows the draft and nothing else.
    fn rebuild_around(&mut self, cursor: Option<PathBuf>) {
        let was = self.selected;
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
            View::Drafts => {
                let mut idx: Vec<usize> = (0..self.drafts.len())
                    .filter(|&i| self.draft_matches(i, &q))
                    .collect();
                // Newest pull request first, as in the other views; a draft
                // whose pull request was not fetched has no age and goes last.
                let touched =
                    |i: usize| std::cmp::Reverse(self.draft_pr(&self.drafts[i]).touched_at());
                if self.sort == Sort::Attention {
                    idx.sort_by_key(|&i| {
                        (draft_rank(self.draft_state(&self.drafts[i])), touched(i))
                    });
                } else {
                    idx.sort_by_key(|&i| touched(i));
                }
                idx.into_iter().map(Row::Draft).collect()
            }
            v => {
                let mut idx: Vec<usize> = (0..self.prs.len())
                    .filter(|&i| self.in_view(&self.prs[i], v) && self.pr_matches(i, &q))
                    .collect();
                if self.sort == Sort::Attention {
                    idx.sort_by_key(|&i| (attention_rank(&self.prs[i]), -self.prs[i].updated_at));
                } else {
                    idx.sort_by_key(|&i| -self.prs[i].updated_at);
                }
                idx.into_iter().map(Row::Pr).collect()
            }
        };

        let last = self.rows.len().saturating_sub(1);
        if self.view == View::Drafts {
            let same = cursor.and_then(|path| {
                self.rows
                    .iter()
                    .position(|r| matches!(r, Row::Draft(i) if self.drafts[*i].path == path))
            });
            // Still on its draft: nothing the reader sees has to move, the
            // detail pane's scroll included. Gone: the nearest row, not the
            // top of the list.
            if same.is_none() {
                self.detail_scroll = 0;
            }
            self.selected = same.unwrap_or(was).min(last);
            return;
        }
        self.selected = keep
            .and_then(|k| self.rows.iter().position(|r| *r == k))
            .unwrap_or(0)
            .min(last);
        self.detail_scroll = 0;

        // A pending post is only as good as the row it names. If a refresh
        // moved the selection, rewrote the file or made the draft unpostable,
        // the question is withdrawn.
        if let Some(armed) = &self.armed {
            let still = self.selected_draft().is_some_and(|e| {
                e.number == Some(armed.number)
                    && e.raw == armed.raw
                    && self.draft_state(e) == drafts::State::Ready
            });
            if !still {
                self.armed = None;
            }
        }
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

    /// Whether `p` belongs on the view `v`, before any filter. Hidden drafts
    /// are out of every view.
    fn in_view(&self, p: &Pr, v: View) -> bool {
        if p.is_draft && !self.settings.show_drafts {
            return false;
        }
        match v {
            View::Ready => p.is_ready(),
            View::Mine => p.is_mine(&self.viewer),
            View::Review => p.wants_my_review(&self.viewer),
            View::Assigned => p.assigned_to_me(&self.viewer),
            View::Blocked => p.is_blocked(),
            View::All => true,
            View::Worktrees | View::Drafts => false,
        }
    }

    /// The size of a view before filtering: what its tab counts, and the
    /// "of N" while a filter is typed. It must agree with the list, so it
    /// shares `in_view` with `rebuild`.
    pub fn count_for(&self, v: View) -> usize {
        match v {
            View::Worktrees => self.worktrees.len(),
            View::Drafts => self.drafts.len(),
            _ => self.prs.iter().filter(|p| self.in_view(p, v)).count(),
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
            Row::Draft(i) => match self.draft_pr(self.drafts.get(i)?) {
                DraftPr::Open(p) => Some(p),
                DraftPr::Merged(_) | DraftPr::Unknown => None,
            },
        }
    }

    pub fn selected_worktree(&self) -> Option<&Worktree> {
        match self.selected_row()? {
            Row::Wt(i) => self.worktrees.get(i),
            Row::Pr(i) => self.worktree_for(self.prs.get(i)?),
            Row::Draft(_) => self.worktree_for(self.selected_pr()?),
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
        match launch(&self.settings.open_command, url) {
            Ok(_) => self.note(format!("opened {url}")),
            Err(e) => self.note(format!(
                "could not run `{}`: {e}",
                self.settings.open_command
            )),
        }
    }

    /// The selected row's pull request on GitHub. A draft's may have merged
    /// since it was written.
    fn selected_url(&self) -> Option<String> {
        if let Some(entry) = self.selected_draft() {
            return self.draft_pr(entry).url().map(String::from);
        }
        self.selected_pr().map(|p| p.url.clone())
    }

    pub fn open_selected(&mut self) {
        if let Some(url) = self.selected_url() {
            self.open_url(&url);
            return;
        }
        if let Some(entry) = self.selected_draft() {
            let label = entry.label();
            self.note(format!("{label} is not among the fetched pull requests"));
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
        let Some(url) = self.selected_url() else {
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
                match c.wait() {
                    Ok(status) if status.success() => self.note(format!("copied {url}")),
                    Ok(status) => self.note(format!("`{bin}` failed ({status}); nothing copied")),
                    Err(e) => self.note(format!("could not run `{bin}`: {e}")),
                }
            }
            Err(e) => self.note(format!("could not run `{bin}`: {e}")),
        }
    }
}

/// Run a configured command with `url` as its last argument and return its
/// pid. The command gets no stdin, since the terminal belongs to rigor, and
/// is reaped off-thread, so opening PRs all session leaves no zombies behind.
fn launch(command: &str, url: &str) -> std::io::Result<u32> {
    let mut parts = command.split_whitespace();
    let bin = parts
        .next()
        .ok_or_else(|| std::io::Error::other("the command is empty"))?;
    let mut child = Command::new(bin)
        .args(parts)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
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

/// Ordering for the "attention" sort in the Drafts view: what can be posted
/// now, then what needs a look before it can, then what is already decided.
const fn draft_rank(state: drafts::State) -> u8 {
    match state {
        drafts::State::Ready => 0,
        drafts::State::Stale => 1,
        drafts::State::Unverified => 2,
        drafts::State::Invalid => 3,
        drafts::State::Skipped => 4,
        drafts::State::Posted => 5,
    }
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

    /// Opening a PR must not leave a zombie process for the rest of the session.
    #[cfg(unix)]
    #[test]
    fn a_launched_opener_is_reaped() {
        let pid = libc::pid_t::try_from(launch("true", "https://example.com").unwrap()).unwrap();
        let gone = (0..200).any(|_| {
            std::thread::sleep(Duration::from_millis(10));
            // SAFETY: kill(2) with signal 0 sends nothing; it only reports
            // whether the process still exists, and a zombie does.
            unsafe { libc::kill(pid, 0) != 0 }
        });
        assert!(gone, "the opener was never reaped");
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

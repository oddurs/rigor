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
use crate::util::{format_iso8601, now_millis, now_secs};

pub enum Msg {
    Prs(Result<github::Fetched, String>),
    Worktrees(Result<Vec<Worktree>, String>),
    Statuses(Result<Vec<git::Scanned>, String>),
    /// How the attempt in `App::posting` ended.
    Posted(Result<drafts::Outcome, String>),
}

/// The remote an `App` starts with: none. Every call fails, so nothing is
/// ever sent until `main` wires in `gh`.
struct Unwired;

impl drafts::Remote for Unwired {
    fn pull(&self, _: u64) -> Result<drafts::Live, String> {
        Err("GitHub is not wired up".into())
    }
    fn reviews(&self, _: u64, _: usize) -> Result<Vec<drafts::Review>, String> {
        Err("GitHub is not wired up".into())
    }
    fn comments(&self, _: u64, _: usize) -> Result<Vec<drafts::Placed>, String> {
        Err("GitHub is not wired up".into())
    }
    fn post(&self, _: u64, _: &str) -> Result<drafts::Review, drafts::PostFailure> {
        Err(drafts::PostFailure::Declined(
            "GitHub is not wired up".into(),
        ))
    }
    fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(1)
    }
}

/// The answer to the question counts only once the question has been on
/// screen this long. Keys typed ahead, or struck in the same breath as the
/// `a` that asked, are not an answer to something nobody has yet read.
pub const ANSWER_FLOOR_MS: i64 = 400;

/// A question nobody answers or reads withdraws itself after this long, so
/// that a `Y` typed much later is not about it.
pub const ARMED_FOR_MS: i64 = 30_000;

/// A post waiting to be confirmed: a question on screen, and the overlay that
/// shows what it is about.
///
/// It names its draft by file, and holds the draft as it was parsed when the
/// question went up. That one value is what the overlay is drawn from and
/// what the request is built from, so what was read is what is sent. Once the
/// selected draft stops being that file with that text in that state, the
/// question on screen is no longer the one a keypress would be answering, and
/// it is withdrawn.
///
/// The question is asked with `a` and answered with `Y`: a different key, so
/// that neither holding `a` down nor striking it twice can be the answer, and
/// a capital, so that no ordinary word typed into the wrong window is.
#[derive(Debug, Clone)]
pub struct Armed {
    pub path: PathBuf,
    pub number: u64,
    raw: String,
    pub draft: drafts::Draft,
    /// `Ready`, or `Unverified` for a pull request the last fetch did not
    /// include. The prompt says which.
    pub state: drafts::State,
    /// When the question went up or was last read, on `now_millis`'s clock,
    /// which stops while the machine sleeps —
    at: i64,
    /// — and on the wall clock, which does not: a question asked before the
    /// lid closed is gone when it opens.
    at_wall: i64,
    /// When the overlay was first on screen. `None` until a frame has drawn
    /// it; the floor is counted from here, not from the key that asked.
    pub drawn_at: Option<i64>,
    /// How far down the overlay is scrolled. Clamped as it is drawn.
    pub scroll: u16,
    /// Which rows of the overlay have been on screen since it opened, at the
    /// width they were last laid out for. Nothing is posted until every one
    /// has: reaching the end is not the same as having passed the middle.
    pub seen: Vec<bool>,
    /// The width `seen` was laid out for. At another width the rows are
    /// other rows, and what was read of these says nothing about those.
    pub seen_width: u16,
    /// The window is too small for the overlay to show its rows whole.
    /// Nothing is posted from a window that cannot show it.
    pub cramped: bool,
    /// A `Y` was turned away for either reason, so the overlay says it louder.
    pub nudged: bool,
}

impl Armed {
    /// How many rows of what would be posted have not been on screen yet.
    pub fn unread(&self) -> usize {
        self.seen.iter().filter(|seen| !**seen).count()
    }

    /// Whether all of it has been shown: the overlay has been drawn, in a
    /// window large enough, and no row is still unread.
    pub fn shown(&self) -> bool {
        self.drawn_at.is_some() && !self.cramped && self.unread() == 0
    }

    /// Note that the question is being read: it has that much longer.
    fn read_on(&mut self) {
        self.at = now_millis();
        self.at_wall = now_secs();
    }

    fn expired(&self) -> bool {
        let wall = now_secs() - self.at_wall;
        now_millis() - self.at >= ARMED_FOR_MS || wall >= ARMED_FOR_MS / 1000
    }
}

/// An attempt in flight: which draft, and exactly what was read from it when
/// it was confirmed. The file may change before the answer comes back; this
/// is what the answer is about.
#[derive(Debug, Clone)]
pub struct Posting {
    pub number: u64,
    pub path: PathBuf,
    raw: String,
    draft: drafts::Draft,
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

impl<'a> DraftPr<'a> {
    /// The pull request's head commit, when the fetch carried one.
    pub const fn head(&self) -> Option<&'a str> {
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
    /// The attempt to post that is in flight. One at a time.
    pub posting: Option<Posting>,
    /// Drafts whose review was sent and never answered for, by file. Whether
    /// they are on GitHub is not known, and their row says so until an
    /// attempt gets far enough to find out.
    unsettled: HashSet<PathBuf>,
    /// Quit was asked for while a post was in flight. It is being let finish.
    pub quitting: bool,
    /// Head commits GitHub reported at the moment of a post, for pull
    /// requests whose draft turned out to be behind. Newer than the last
    /// fetch, so it wins until the next fetch covers the pull request.
    live_heads: HashMap<u64, String>,
    /// Why the last attempt to post sent nothing, or what is not known about
    /// it. It stays until `esc` or the next attempt: a keypress made for some
    /// other reason must not sweep it away unread. Kept apart from `error`,
    /// which is the sync's and is cleared by the next good fetch.
    pub post_error: Option<String>,
    /// GitHub, as posting needs it. Inert until `main` wires in `gh`, so an
    /// `App` built in a test cannot reach GitHub.
    pub remote: Arc<dyn drafts::Remote>,
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
            unsettled: HashSet::new(),
            quitting: false,
            live_heads: HashMap::new(),
            post_error: None,
            remote: Arc::new(Unwired),
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
        if self.armed.as_ref().is_some_and(Armed::expired) {
            self.armed = None;
            self.note("the question timed out; nothing was posted");
        }
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
                // What this fetch covers it knows better than an older read.
                self.live_heads.retain(|n, _| {
                    !self.prs.iter().any(|p| p.number == *n)
                        && !self.merged.iter().any(|m| m.number == *n)
                });
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

    /// The attempt in `posting` came back. The draft's file changes only when
    /// its review is on GitHub — sent just now, or found already there; every
    /// refusal leaves it exactly as it was.
    fn posted(&mut self, result: Result<drafts::Outcome, String>) {
        use drafts::Outcome;
        let Some(sent) = self.posting.take() else {
            return;
        };
        let number = sent.number;
        // What is recorded as posted is what was sent, not whatever the file
        // has come to say while the answer was on its way.
        let record = |at: String| {
            let at = if at.is_empty() {
                format_iso8601(now_secs())
            } else {
                at
            };
            drafts::record_posted(&sent.path, &sent.raw, &sent.draft, at)
        };
        let kept = |changed: bool| {
            if changed {
                "; the file changed while posting, and what was sent was kept"
            } else {
                ""
            }
        };
        // An attempt that read the reviews knows whether an earlier,
        // unanswered post landed. One that stopped short of them does not.
        let mut settles = true;
        let (done, failure) = match result {
            Ok(Outcome::Sent) => match record(String::new()) {
                Ok(changed) => (Some(format!("posted to #{number}{}", kept(changed))), None),
                // The review is on GitHub whatever the file says. The next
                // attempt will find it there and send nothing.
                Err(e) => (
                    None,
                    Some(format!(
                        "the review was posted to #{number}, but its draft file could not be updated: {e}"
                    )),
                ),
            },
            Ok(Outcome::AlreadySent(at)) => match record(at) {
                Ok(changed) => (
                    Some(format!(
                        "#{number} already has this review from you; marked posted, nothing sent{}",
                        kept(changed)
                    )),
                    None,
                ),
                Err(e) => (
                    None,
                    Some(format!(
                        "#{number} already has this review from you, but its draft file could not be updated: {e}"
                    )),
                ),
            },
            Ok(Outcome::Unknown(e)) => {
                settles = false;
                self.unsettled.insert(sent.path.clone());
                (
                    None,
                    Some(format!(
                        "#{number}: the review was sent, and whether it was posted is not known ({e}). a asks again, and GitHub is checked before anything is sent"
                    )),
                )
            }
            Ok(Outcome::Refused(why)) => {
                let (message, reached_the_reviews) = self.refusal(number, why);
                settles = reached_the_reviews;
                (None, Some(message))
            }
            Err(e) => {
                settles = false;
                (None, Some(format!("posting to #{number} failed: {e}")))
            }
        };
        if settles {
            self.unsettled.remove(&sent.path);
        }
        if let Some(done) = done {
            self.note(done);
        }
        self.post_error = failure;
        self.reload_drafts();
    }

    /// Why nothing was sent, for the screen, and whether the attempt got as
    /// far as reading the reviews — which is what it takes to know if an
    /// earlier, unanswered post landed.
    fn refusal(&mut self, number: u64, why: drafts::Refusal) -> (String, bool) {
        use drafts::Refusal;
        match why {
            Refusal::Closed => (
                format!("nothing posted: #{number} was closed without being merged"),
                false,
            ),
            Refusal::Moved(head) => {
                let short = head.get(..7).unwrap_or(&head).to_string();
                self.live_heads.insert(number, head);
                (
                    format!(
                        "nothing posted: #{number} has moved on to {short} since this draft was written"
                    ),
                    false,
                )
            }
            Refusal::ReviewedSince => (
                format!(
                    "nothing posted: you have reviewed #{number} since this draft was written; x skips it, or review in the browser"
                ),
                true,
            ),
            Refusal::Pending => (
                format!(
                    "nothing posted: you have a review of #{number} started and not submitted; submit or discard it in the browser first"
                ),
                true,
            ),
            Refusal::NoViewer => (
                "nothing posted: your GitHub login is not known yet; r refreshes".to_string(),
                false,
            ),
            Refusal::Unchecked(e) => (
                format!("nothing posted: could not check #{number} on GitHub: {e}"),
                false,
            ),
            Refusal::Busy => (
                format!("nothing posted: another rigor window is posting #{number}"),
                false,
            ),
            Refusal::Rejected(e) => (format!("posting to #{number} failed: {e}"), true),
        }
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

    /// The pull request's head commit as far as rigor knows: what GitHub said
    /// at the last attempt to post, else what the last fetch carried.
    pub fn draft_head(&self, entry: &drafts::Entry) -> Option<&str> {
        entry
            .number
            .and_then(|n| self.live_heads.get(&n))
            .map(String::as_str)
            .or_else(|| self.draft_pr(entry).head())
    }

    pub fn draft_state(&self, entry: &drafts::Entry) -> drafts::State {
        drafts::state(&entry.draft, self.draft_head(entry))
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

    /// `a` with no question up. Posting is public and under the user's name,
    /// so this only asks: it opens the overlay showing exactly what would be
    /// sent. Sending takes a different key, `Y` (`confirm`).
    pub fn post_key(&mut self) {
        use drafts::State;
        let Some(entry) = self.selected_draft().cloned() else {
            return;
        };
        let label = entry.label();
        let state = self.draft_state(&entry);
        // An unverified draft may be asked about too: the list cannot vouch
        // for its pull request's head, but the check before sending can.
        let (Some(number), Ok(draft), State::Ready | State::Unverified) =
            (entry.number, entry.draft, state)
        else {
            let why = match state {
                State::Posted => "is already posted",
                State::Skipped => "is skipped; x brings it back",
                State::Stale => {
                    "is stale: the pull request has moved on since this draft was written"
                }
                State::Invalid | State::Ready | State::Unverified => {
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
        self.armed = Some(Armed {
            path: entry.path,
            number,
            raw: entry.raw,
            draft,
            state,
            at: now_millis(),
            at_wall: now_secs(),
            drawn_at: None,
            scroll: 0,
            seen: Vec::new(),
            seen_width: 0,
            cramped: false,
            nudged: false,
        });
    }

    /// Scroll the overlay. It does not withdraw the question, and it gives it
    /// longer: reading what would be sent is the point of asking.
    pub fn scroll_confirm(&mut self, delta: i32) {
        if let Some(armed) = &mut self.armed {
            let to = i32::from(armed.scroll).saturating_add(delta).max(0);
            armed.scroll = u16::try_from(to).unwrap_or(u16::MAX);
            armed.read_on();
        }
    }

    /// `Y`, with the question up. Goes ahead only once all of what would be
    /// sent has been on screen, in a window that can show it, for long enough
    /// to have been seen — and then only if GitHub, asked there and then,
    /// still agrees (`drafts::deliver`).
    pub fn confirm(&mut self) {
        let Some(armed) = &mut self.armed else {
            return;
        };
        // Too soon after the overlay first drew — or before it has drawn at
        // all — to be an answer to it. Not a reason to take it down either.
        if armed
            .drawn_at
            .is_none_or(|drawn| now_millis() - drawn < ANSWER_FLOOR_MS)
        {
            return;
        }
        if !armed.shown() {
            armed.nudged = true;
            return;
        }
        let Some(armed) = self.armed.take() else {
            return;
        };

        // The file is the draft. If it changed after the question went up,
        // what was shown is no longer what it says. Read the way the list
        // reads it: only ever a regular file of ordinary size.
        let on_disk = drafts::read_text(&armed.path).ok();
        if on_disk.as_deref() != Some(armed.raw.as_str()) {
            self.reload_drafts();
            self.note(format!(
                "#{} changed on disk; nothing was posted",
                armed.number
            ));
            return;
        }
        let number = armed.number;
        self.post_error = None;
        self.note(format!("checking #{number} on GitHub, then posting…"));
        let (remote, viewer) = (Arc::clone(&self.remote), self.viewer.clone());
        let (path, draft) = (armed.path.clone(), armed.draft.clone());
        self.posting = Some(Posting {
            number,
            path: armed.path,
            raw: armed.raw,
            draft: armed.draft,
        });
        spawn_reply(self.tx.clone(), Msg::Posted, move || {
            Ok(drafts::deliver_exclusively(
                remote.as_ref(),
                &viewer,
                number,
                &path,
                &draft,
            ))
        });
    }

    /// The one way out of the event loop. `ended` is how the loop ended, and
    /// is handed back untouched; whatever it is — a key, a signal, an error
    /// from a terminal that went away — a post in flight is let finish first
    /// (`finish_post`). Taking the loop's own result is the point: there is
    /// no ending the loop this is not called for.
    ///
    /// `show` draws the screen once, to say what is being waited for. It is
    /// only used when the loop ended in good order: otherwise the terminal
    /// may be what failed, and nothing here may depend on it.
    pub fn leave<E>(
        &mut self,
        ended: Result<(), E>,
        replies: &Receiver<Msg>,
        limit: std::time::Duration,
        show: impl FnOnce(&mut Self),
    ) -> (Result<(), E>, Option<String>) {
        if self.posting.is_none() {
            return (ended, None);
        }
        self.quitting = true;
        if ended.is_ok() {
            show(self);
        }
        let said = self.finish_post(replies, limit);
        (ended, said)
    }

    /// Rigor is on its way out with an attempt in flight. Let it finish:
    /// `gh` cut off mid-request leaves it unknown whether the review landed,
    /// with nobody left to find out. The answer is taken from `replies` and
    /// recorded as it would be at any other time.
    ///
    /// The wait is bounded by `limit`, once, for the whole of what is left —
    /// not by each call's own deadline one after another. It reads nothing
    /// from the terminal, which may be gone. Returns what to tell the user;
    /// `None` when nothing was in flight.
    pub fn finish_post(
        &mut self,
        replies: &Receiver<Msg>,
        limit: std::time::Duration,
    ) -> Option<String> {
        let number = self.posting.as_ref()?.number;
        self.quitting = true;
        let deadline = std::time::Instant::now() + limit;
        while self.posting.is_some() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match replies.recv_timeout(left) {
                Ok(msg) => self.on_msg(msg),
                Err(_) => break,
            }
        }
        if let Some(unfinished) = self.posting.take() {
            // Out of time. What is known is that it was started: say that,
            // and leave the file saying what is true of it, a draft.
            self.unsettled.insert(unfinished.path);
            return Some(format!(
                "the post to #{number} did not finish in {}s. It may or may not be posted; open rigor and press a on it, and GitHub is asked before anything is sent again",
                limit.as_secs()
            ));
        }
        self.post_error
            .clone()
            .or_else(|| self.notice.as_ref().map(|(text, _)| text.clone()))
    }

    /// Whether `entry` is the draft an attempt is in flight for.
    pub fn is_posting(&self, entry: &drafts::Entry) -> bool {
        self.posting.as_ref().is_some_and(|p| p.path == entry.path)
    }

    /// Whether `entry`'s review was sent and never answered for.
    pub fn is_unsettled(&self, entry: &drafts::Entry) -> bool {
        self.unsettled.contains(&entry.path)
    }

    /// `x`: set a draft aside, or bring a skipped one back.
    pub fn skip_key(&mut self) {
        let Some(entry) = self.selected_draft().cloned() else {
            return;
        };
        let label = entry.label();
        // The answer on its way is about the file as it was. It is left
        // alone until the answer is in.
        if self.is_posting(&entry) {
            self.note(format!(
                "{label} is being posted; x waits until that is done"
            ));
            return;
        }
        let Ok(draft) = &entry.draft else {
            self.note(format!("{label} is invalid; e opens it in your editor"));
            return;
        };
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
        let Some(entry) = self.selected_draft() else {
            return;
        };
        if self.is_posting(entry) {
            let label = entry.label();
            self.note(format!(
                "{label} is being posted; e waits until that is done"
            ));
            return;
        }
        self.edit_request = Some(entry.path.clone());
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
        } else {
            self.selected = keep
                .and_then(|k| self.rows.iter().position(|r| *r == k))
                .unwrap_or(0)
                .min(last);
            self.detail_scroll = 0;
        }

        // A question is only as good as the draft it names. If the selection
        // is no longer on that file, or the file no longer reads the same,
        // or the draft can no longer be posted as it could, it is withdrawn.
        if let Some(armed) = &self.armed {
            let still = self.selected_draft().is_some_and(|e| {
                e.path == armed.path && e.raw == armed.raw && self.draft_state(e) == armed.state
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

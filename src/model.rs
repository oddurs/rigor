//! Domain types shared by the fetchers and the UI.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Success,
    Failure,
    Pending,
    Cancelled,
    Skipped,
    Neutral,
    None,
}

impl CheckState {
    /// A `CheckRun` carries `status` (`QUEUED/IN_PROGRESS/COMPLETED`) plus a `conclusion` once done.
    pub fn from_check_run(status: &str, conclusion: Option<&str>) -> Self {
        match status {
            "COMPLETED" => match conclusion.unwrap_or("") {
                "SUCCESS" => Self::Success,
                "FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED" => Self::Failure,
                "CANCELLED" => Self::Cancelled,
                "SKIPPED" => Self::Skipped,
                // NEUTRAL, STALE, and anything GitHub adds later: neither a
                // pass nor a failure.
                _ => Self::Neutral,
            },
            "QUEUED" | "IN_PROGRESS" | "WAITING" | "PENDING" | "REQUESTED" => Self::Pending,
            _ => Self::None,
        }
    }

    /// Legacy commit statuses (and the rollup) use a flat state enum.
    pub fn from_status(state: &str) -> Self {
        match state {
            "SUCCESS" => Self::Success,
            "FAILURE" | "ERROR" => Self::Failure,
            "PENDING" | "EXPECTED" => Self::Pending,
            _ => Self::None,
        }
    }

    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Success => "✓",
            Self::Failure => "✗",
            Self::Pending => "◐",
            Self::Cancelled => "⊘",
            Self::Skipped => "–",
            Self::Neutral => "·",
            Self::None => " ",
        }
    }

    pub const fn is_bad(self) -> bool {
        matches!(self, Self::Failure | Self::Cancelled)
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub state: CheckState,
    pub url: Option<String>,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

impl Check {
    /// Wall-clock the check has taken, still counting up while it runs.
    pub fn elapsed(&self, now: i64) -> Option<i64> {
        let start = self.started_at?;
        Some(self.completed_at.unwrap_or(now) - start)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

impl ReviewDecision {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "APPROVED" => Some(Self::Approved),
            "CHANGES_REQUESTED" => Some(Self::ChangesRequested),
            "REVIEW_REQUIRED" => Some(Self::ReviewRequired),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::ChangesRequested => "changes requested",
            Self::ReviewRequired => "review required",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mergeable {
    Clean,
    Conflicting,
    Unknown,
}

impl Mergeable {
    pub fn parse(s: &str) -> Self {
        match s {
            "MERGEABLE" => Self::Clean,
            "CONFLICTING" => Self::Conflicting,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Pr {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub is_draft: bool,
    pub updated_at: i64,
    pub author: String,
    pub head_ref: String,
    pub base_ref: String,
    pub additions: u32,
    pub deletions: u32,
    pub changed_files: u32,
    pub mergeable: Mergeable,
    pub review_decision: Option<ReviewDecision>,
    pub assignees: Vec<String>,
    pub review_requests: Vec<String>,
    pub labels: Vec<String>,
    pub comments: u32,
    pub checks: Vec<Check>,
    pub rollup: CheckState,
}

/// Counts across a PR's checks. A skipped job is not a passing one — a repo
/// with path filters skips most of its matrix on most PRs, and folding those
/// into "passed" makes every PR look better tested than it is.
#[derive(Debug, Clone, Copy, Default)]
pub struct CheckTally {
    pub passed: usize,
    pub failed: usize,
    pub running: usize,
    pub skipped: usize,
    pub total: usize,
}

impl Pr {
    pub fn tally(&self) -> CheckTally {
        let mut t = CheckTally::default();
        for c in &self.checks {
            t.total += 1;
            match c.state {
                CheckState::Success => t.passed += 1,
                CheckState::Failure | CheckState::Cancelled => t.failed += 1,
                CheckState::Pending => t.running += 1,
                CheckState::Skipped | CheckState::Neutral => t.skipped += 1,
                CheckState::None => {}
            }
        }
        t
    }

    pub fn checks_url(&self) -> String {
        format!("{}/checks", self.url)
    }

    pub fn is_mine(&self, viewer: &str) -> bool {
        self.author == viewer
    }

    pub fn wants_my_review(&self, viewer: &str) -> bool {
        self.review_requests.iter().any(|r| r == viewer)
    }

    pub fn assigned_to_me(&self, viewer: &str) -> bool {
        self.assignees.iter().any(|a| a == viewer)
    }

    /// Nothing stands between this PR and the merge button: green (or unchecked),
    /// no conflicts, and either approved or not requiring review at all.
    pub fn is_ready(&self) -> bool {
        !self.is_draft
            && self.mergeable == Mergeable::Clean
            && matches!(self.rollup, CheckState::Success | CheckState::None)
            && !matches!(
                self.review_decision,
                Some(ReviewDecision::ChangesRequested | ReviewDecision::ReviewRequired)
            )
    }

    /// Something concrete is in the way: red CI, requested changes, or conflicts.
    /// Waiting on a first review is not "blocked" — that is just the queue.
    pub fn is_blocked(&self) -> bool {
        !self.is_draft
            && (self.rollup == CheckState::Failure
                || self.review_decision == Some(ReviewDecision::ChangesRequested)
                || self.mergeable == Mergeable::Conflicting)
    }
}

/// A merged PR, kept only so a worktree can be told that its branch has landed.
#[derive(Debug, Clone)]
pub struct MergedPr {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub head_ref: String,
    /// The PR's final commit. A worktree still at this commit has nothing that
    /// did not land; the branch name alone cannot say that, since names are
    /// reused.
    pub head_oid: String,
    pub merged_at: i64,
}

/// One `git worktree` entry — in practice, one agent's workspace.
#[derive(Debug, Clone)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub is_main: bool,
    pub detached: bool,
    pub status: Option<WorktreeStatus>,
}

impl Worktree {
    pub fn name(&self) -> String {
        self.path.file_name().map_or_else(
            || self.path.to_string_lossy().to_string(),
            |s| s.to_string_lossy().to_string(),
        )
    }

    /// The last two path components — enough to tell two agent desks apart
    /// without spending a whole pane on the path.
    pub fn label(&self) -> String {
        let mut it = self.path.iter().rev().take(2).collect::<Vec<_>>();
        it.reverse();
        it.iter()
            .map(|s| s.to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// `~`-shortened path for display.
    pub fn short_path(&self, home: Option<&str>) -> String {
        let p = self.path.to_string_lossy().to_string();
        match home {
            Some(h) if p.starts_with(h) => format!("~{}", &p[h.len()..]),
            _ => p,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WorktreeStatus {
    /// Files with uncommitted changes.
    pub dirty: usize,
    /// Commits sitting here that the remote has not seen.
    pub unpushed: usize,
    /// Whether the branch has an upstream at all.
    pub published: bool,
    pub last_commit_at: Option<i64>,
    pub last_subject: String,
    pub last_author: String,
}

impl WorktreeStatus {
    /// One short cell answering "is there work here that has not gone anywhere?"
    pub fn drift(&self) -> String {
        match (self.published, self.unpushed) {
            (true, 0) => String::new(),
            (true, n) => format!("⇡{n}"),
            (false, 0) => "local".into(),
            (false, n) => format!("⇡{n} local"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RepoInfo {
    pub owner: String,
    pub name: String,
    pub default_branch: String,
    pub root: PathBuf,
    pub current_branch: Option<String>,
}

impl RepoInfo {
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

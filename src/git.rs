//! Local git: which repo are we in, and which worktrees does it have.
//!
//! Each worktree is treated as one workspace — in this setup, one agent's desk.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::proc::{self, Priority};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::model::{RepoInfo, Worktree, WorktreeStatus};

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    // `--no-optional-locks`: a plain `git status` may take index.lock to
    // refresh its stat cache, and with an agent committing in the same
    // worktree that collision fails the agent's commit. Background readers
    // are exactly who this flag is for.
    let out = proc::run(
        Command::new("git")
            .arg("--no-optional-locks")
            .arg("-C")
            .arg(dir)
            .args(args),
        Duration::from_secs(20),
        Priority::Background,
    )
    .context("running git")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

fn git_ok(dir: &Path, args: &[&str]) -> Option<String> {
    git(dir, args).ok()
}

/// Resolve the repo we are sitting in, including its `owner/name` on GitHub.
pub fn discover(start: &Path, repo_override: Option<&str>) -> Result<RepoInfo> {
    let root =
        git(start, &["rev-parse", "--show-toplevel"]).context("not inside a git repository")?;
    let root = PathBuf::from(root);

    let current_branch = git_ok(&root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .filter(|b| b != "HEAD" && !b.is_empty());

    let (owner, name) = match repo_override {
        Some(s) => split_slug(s).context("--repo must look like owner/name")?,
        None => resolve_slug(&root)?,
    };

    Ok(RepoInfo {
        owner,
        name,
        default_branch: "main".into(), // replaced by the value GitHub reports
        root,
        current_branch,
    })
}

/// Prefer `gh`'s own resolution (it honours `remote.origin.gh-resolved`), fall
/// back to parsing the origin URL so we still work offline-ish.
fn resolve_slug(root: &Path) -> Result<(String, String)> {
    if let Ok(out) = proc::run(
        Command::new("gh")
            .args([
                "repo",
                "view",
                "--json",
                "nameWithOwner",
                "-q",
                ".nameWithOwner",
            ])
            .current_dir(root),
        Duration::from_secs(20),
        Priority::Normal,
    ) && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Some(p) = split_slug(&s) {
            return Ok(p);
        }
    }

    let url = git(root, &["remote", "get-url", "origin"])
        .context("no origin remote, and gh could not identify the repo")?;
    parse_remote(&url).context(format!("could not parse a GitHub repo out of {url}"))
}

fn split_slug(s: &str) -> Option<(String, String)> {
    let (o, n) = s.trim().split_once('/')?;
    if o.is_empty() || n.is_empty() {
        return None;
    }
    Some((o.to_string(), n.trim_end_matches(".git").to_string()))
}

/// Handles `git@host:owner/name.git`, `ssh://git@host/owner/name`, `https://host/owner/name.git`.
fn parse_remote(url: &str) -> Option<(String, String)> {
    let s = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let tail = if let Some((_, rest)) = s.split_once("://") {
        rest.split_once('/').map(|(_, t)| t)?
    } else if let Some((_, rest)) = s.split_once(':') {
        rest
    } else {
        s
    };
    let mut parts = tail.rsplitn(2, '/');
    let name = parts.next()?;
    let owner = parts.next()?.rsplit('/').next()?;
    split_slug(&format!("{owner}/{name}"))
}

/// Enumerate every worktree attached to this repo, newest-checkout first is not
/// guaranteed — the caller sorts.
pub fn list_worktrees(root: &Path) -> Result<Vec<Worktree>> {
    let out = git(root, &["worktree", "list", "--porcelain"])?;
    let mut wts = Vec::new();
    let mut cur: Option<Worktree> = None;

    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(w) = cur.take() {
                wts.push(w);
            }
            cur = Some(Worktree {
                path: PathBuf::from(p),
                branch: None,
                head: String::new(),
                is_main: wts.is_empty(),
                detached: false,
                status: None,
            });
        } else if let Some(h) = line.strip_prefix("HEAD ") {
            if let Some(w) = cur.as_mut() {
                w.head = h.to_string();
            }
        } else if let Some(b) = line.strip_prefix("branch ") {
            if let Some(w) = cur.as_mut() {
                w.branch = Some(b.trim_start_matches("refs/heads/").to_string());
            }
        } else if line == "detached" {
            if let Some(w) = cur.as_mut() {
                w.detached = true;
            }
        } else if line == "bare" {
            cur = None;
        }
    }
    if let Some(w) = cur.take() {
        wts.push(w);
    }
    Ok(wts)
}

/// Working-tree state for one worktree: uncommitted files, drift from upstream,
/// and the tip commit. Each call shells out a few times, so callers run it off-thread.
pub fn worktree_status(wt: &Worktree, default_branch: &str) -> WorktreeStatus {
    let p = &wt.path;
    let mut st = WorktreeStatus::default();

    if let Some(s) = git_ok(p, &["status", "--porcelain"]) {
        st.dirty = s.lines().filter(|l| !l.is_empty()).count();
    }

    // Unpushed work is the signal that matters on an agent's desk. Against an
    // upstream that is exactly what `@{upstream}..HEAD` counts; with no upstream
    // the branch has never been published, so everything since it left the
    // default branch is unpushed.
    if let Some(c) = git_ok(p, &["rev-list", "--count", "@{upstream}..HEAD"]) {
        st.published = true;
        st.unpushed = c.trim().parse().unwrap_or(0);
    } else {
        st.published = false;
        st.unpushed = git_ok(
            p,
            &[
                "rev-list",
                "--count",
                &format!("origin/{default_branch}..HEAD"),
            ],
        )
        .and_then(|c| c.trim().parse().ok())
        .unwrap_or(0);
    }

    if let Some(l) = git_ok(p, &["log", "-1", "--format=%ct%x00%s%x00%an"]) {
        let mut it = l.split('\0');
        st.last_commit_at = it.next().and_then(|x| x.parse().ok());
        st.last_subject = it.next().unwrap_or("").to_string();
        st.last_author = it.next().unwrap_or("").to_string();
    }

    st
}

/// A cheap fingerprint of a worktree's git state: when its HEAD file and index
/// were last written. Two `stat` calls, no subprocess. Commits, staging,
/// checkouts and rebases all move one of them, so an unchanged fingerprint
/// means `git status` has nothing new to say — except about unstaged edits,
/// which the periodic full scan picks up.
pub type Signature = (Option<SystemTime>, Option<SystemTime>);

pub fn signature(worktree: &Path) -> Signature {
    let Some(gitdir) = gitdir(worktree) else {
        return (None, None);
    };
    let mtime = |f: &str| {
        std::fs::metadata(gitdir.join(f))
            .and_then(|m| m.modified())
            .ok()
    };
    (mtime("HEAD"), mtime("index"))
}

/// A linked worktree's `.git` is a file pointing at its private git dir; the
/// main worktree's is the directory itself.
fn gitdir(worktree: &Path) -> Option<PathBuf> {
    let dot = worktree.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let target = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
    Some(if target.is_absolute() {
        target
    } else {
        worktree.join(target)
    })
}

/// Status for the worktrees at `which`, a few at a time and at background
/// priority. Returns each scanned worktree's path, fingerprint and status; the
/// fingerprint is taken before the scan, so a change made mid-scan is seen as a
/// change next time rather than missed.
pub fn scan(
    wts: &[Worktree],
    which: &[usize],
    default_branch: &str,
) -> Vec<(PathBuf, Signature, WorktreeStatus)> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(PathBuf, Signature, WorktreeStatus)>> = Mutex::new(Vec::new());
    // Four at a time: enough to finish promptly, few enough that a scan of a
    // large repository never saturates the disk while agents are building.
    let workers = std::thread::available_parallelism()
        .map_or(2, |n| n.get().min(4))
        .min(which.len().max(1));

    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&i) = which.get(k) else { break };
                    let w = &wts[i];
                    let sig = signature(&w.path);
                    let st = worktree_status(w, default_branch);
                    // A panicked worker cannot corrupt a Vec push; keep going.
                    results
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((w.path.clone(), sig, st));
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

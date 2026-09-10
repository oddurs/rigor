//! The git hooks and `scripts/agent`, exercised in throwaway repositories.
//!
//! Every rule here was broken at least once before it was tested: a `set -e`
//! footgun that made `start` exit silently, `commit` exiting zero on an empty
//! index, a BSD-only `tr` failure. These keep them fixed.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A command with every `GIT_*` variable removed. The pre-push hook runs this
/// suite, and git can hand hooks GIT_DIR or GIT_INDEX_FILE; inherited, they
/// would point these throwaway commands at the real repository.
fn cmd(program: &str, cwd: &Path) -> Command {
    let mut c = Command::new(program);
    c.current_dir(cwd);
    for (k, _) in std::env::vars() {
        if k.starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    // Modern git starts background maintenance after writes (commit, fetch,
    // worktree add) and it can outlive the command that started it — a
    // process still running after the test ends. Tests turn it off.
    for (k, v) in quiet_git() {
        c.env(k, v);
    }
    c
}

/// git configuration, passed through the environment so it reaches every git
/// a test starts — including those started by scripts/agent and the hooks.
fn quiet_git() -> [(&'static str, &'static str); 5] {
    [
        ("GIT_CONFIG_COUNT", "2"),
        ("GIT_CONFIG_KEY_0", "maintenance.auto"),
        ("GIT_CONFIG_VALUE_0", "false"),
        ("GIT_CONFIG_KEY_1", "gc.auto"),
        ("GIT_CONFIG_VALUE_1", "0"),
    ]
}

fn git(cwd: &Path, args: &[&str]) -> Output {
    cmd("git", cwd).args(args).output().unwrap()
}

fn ok(o: &Output) -> bool {
    o.status.success()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

/// A repository with one commit on main, pushed to a bare origin, with
/// origin/HEAD set — enough for the hooks and the agent to resolve `main`.
struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    fn new() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let r = Repo { dir };
        let (work, origin) = (r.work(), r.dir.path().join("origin.git"));
        std::fs::create_dir_all(&work).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            assert!(ok(&git(&work, &args)));
        }
        std::fs::write(work.join("README.md"), "x\n").unwrap();
        assert!(ok(&git(&work, &["add", "."])));
        assert!(ok(&git(&work, &["commit", "-q", "-m", "chore: init"])));
        assert!(ok(&git(
            r.dir.path(),
            &["init", "-q", "--bare", origin.to_str().unwrap()]
        )));
        assert!(ok(&git(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()]
        )));
        assert!(ok(&git(&work, &["push", "-q", "-u", "origin", "main"])));
        assert!(ok(&git(&work, &["remote", "set-head", "origin", "main"])));
        r
    }

    fn work(&self) -> PathBuf {
        self.dir.path().join("widget")
    }

    /// Point git at this repository's real hooks, with a stub `scripts/task`
    /// standing in for the project's checks: every target passes except the
    /// ones named in `failing`.
    fn with_hooks(self, failing: &[&str]) -> Repo {
        let hooks = root().join(".githooks");
        assert!(ok(&git(
            &self.work(),
            &["config", "core.hooksPath", hooks.to_str().unwrap()]
        )));
        let scripts = self.work().join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        let task = scripts.join("task");
        let fails = if failing.is_empty() {
            "__none__".to_string()
        } else {
            failing.join("|")
        };
        std::fs::write(
            &task,
            format!("#!/bin/sh\ncase \"$1\" in {fails}) echo \"$1 failed\" >&2; exit 1 ;; esac\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&task, std::fs::Permissions::from_mode(0o755)).unwrap();
        // The stub is local scaffolding, not part of any commit under test.
        std::fs::write(self.work().join(".git/info/exclude"), "scripts/\n").unwrap();
        self
    }

    fn commit(&self, file: &str) {
        std::fs::write(self.work().join(file), file).unwrap();
        assert!(ok(&git(&self.work(), &["add", file])));
        let o = git(
            &self.work(),
            &["commit", "-q", "-m", &format!("feat: add {file}")],
        );
        assert!(ok(&o), "{}", stderr(&o));
    }

    fn agent(&self, args: &[&str], cwd: &Path) -> Output {
        cmd("sh", cwd)
            .arg(root().join("scripts/agent"))
            .args(args)
            .output()
            .unwrap()
    }
}

// ---------------------------------------------------------------- commit-msg

fn commit_msg(message: &str) -> Output {
    let f = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(f.path(), message).unwrap();
    cmd("sh", &root())
        .arg(root().join(".githooks/commit-msg"))
        .arg(f.path())
        .output()
        .unwrap()
}

#[test]
fn commit_msg_accepts_conventional_commits() {
    for m in [
        "feat: add the ready view",
        "fix(ui): keep a gutter after every column",
        "perf!: rescan only moved worktrees",
        "chore(deps): bump ratatui",
        "docs: explain the seam\n\nLonger body explaining why.",
        "feat: a subject\n# a comment git strips\n",
        "Merge branch 'feature'",
        "Revert \"feat: something\"",
    ] {
        let o = commit_msg(m);
        assert!(ok(&o), "rejected {m:?}: {}", stderr(&o));
    }
}

#[test]
fn commit_msg_rejects_everything_else() {
    let long = format!("feat: {}", "x".repeat(70));
    for m in [
        "add a thing",
        "Feat: capitalised type",
        "feat:no space",
        "feat: trailing period.",
        "wip: not a type",
        long.as_str(),
    ] {
        assert!(!ok(&commit_msg(m)), "accepted {m:?}");
    }
}

/// Work here is published under its author's name; attribution to an
/// assistant never reaches a git object.
#[test]
fn commit_msg_rejects_assistant_attribution() {
    for trailer in [
        "Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>",
        "Co-authored-by: GitHub Copilot <copilot@github.com>",
        "🤖 Generated with some assistant",
        "Claude-Session: https://example.com/session",
    ] {
        let o = commit_msg(&format!("feat: a real change\n\n{trailer}"));
        assert!(!ok(&o), "accepted attribution {trailer:?}");
        assert!(stderr(&o).contains("attribution"), "{}", stderr(&o));
    }
}

// ------------------------------------------------------------------ pre-push

#[test]
fn pre_push_refuses_the_default_branch() {
    let r = Repo::new().with_hooks(&[]);
    r.commit("a.txt");
    let o = git(&r.work(), &["push", "origin", "main"]);
    assert!(!ok(&o), "a push to main went through");
    assert!(
        stderr(&o).contains("refusing to push directly to main"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn pre_push_allows_a_green_feature_branch_and_blocks_a_red_one() {
    let green = Repo::new().with_hooks(&[]);
    assert!(ok(&git(&green.work(), &["checkout", "-q", "-b", "feat/x"])));
    green.commit("a.txt");
    let o = git(&green.work(), &["push", "-q", "origin", "feat/x"]);
    assert!(ok(&o), "{}", stderr(&o));

    let red = Repo::new().with_hooks(&["check"]);
    assert!(ok(&git(&red.work(), &["checkout", "-q", "-b", "feat/x"])));
    red.commit("a.txt");
    assert!(
        !ok(&git(&red.work(), &["push", "-q", "origin", "feat/x"])),
        "a red branch was pushed"
    );
}

/// pre-commit is the fast gate: formatting and lint, before a commit exists.
#[test]
fn pre_commit_blocks_formatting_drift_and_lint_failures() {
    for (target, message) in [("fmt:check", "formatting drift"), ("lint", "lint failed")] {
        let r = Repo::new().with_hooks(&[target]);
        std::fs::write(r.work().join("a.txt"), "a").unwrap();
        assert!(ok(&git(&r.work(), &["add", "a.txt"])));
        let o = git(&r.work(), &["commit", "-q", "-m", "feat: add a"]);
        assert!(!ok(&o), "committed through a failing {target}");
        assert!(stderr(&o).contains(message), "{target}: {}", stderr(&o));
    }
}

/// The one documented exemption: publishing the default branch to a brand new
/// remote, where there is nothing yet to advance.
#[test]
fn pre_push_allows_the_first_publish_of_a_new_repository() {
    let r = Repo::new().with_hooks(&[]);
    let fresh = r.dir.path().join("fresh.git");
    assert!(ok(&git(
        r.dir.path(),
        &["init", "-q", "--bare", fresh.to_str().unwrap()]
    )));
    let o = git(&r.work(), &["push", "-q", fresh.to_str().unwrap(), "main"]);
    assert!(ok(&o), "{}", stderr(&o));
}

// ------------------------------------------------------------- scripts/agent

#[test]
fn agent_start_validates_names_and_makes_one_worktree_per_branch() {
    let r = Repo::new();
    let bad = r.agent(&["start", "Feature/Bad_Name"], &r.work());
    assert!(!ok(&bad));
    assert!(
        stderr(&bad).contains("branch name must match"),
        "{}",
        stderr(&bad)
    );

    let good = r.agent(&["start", "feat/ready-view"], &r.work());
    assert!(ok(&good), "{}", stderr(&good));
    let path = r.dir.path().join(".worktrees/widget/feat/ready-view");
    assert!(
        path.join("README.md").exists(),
        "worktree not created at {}",
        path.display()
    );
    assert!(
        String::from_utf8_lossy(&good.stdout).contains(path.to_str().unwrap()),
        "prints the cd path"
    );

    let again = r.agent(&["start", "feat/ready-view"], &r.work());
    assert!(!ok(&again), "a second worktree for one branch");
    assert!(
        stderr(&again).contains("already exists"),
        "{}",
        stderr(&again)
    );
}

#[test]
fn agent_commit_refuses_an_empty_index_and_a_bad_message() {
    let r = Repo::new();
    let empty = r.agent(&["commit", "feat: nothing staged"], &r.work());
    assert!(!ok(&empty), "exited zero with nothing staged");
    assert!(
        stderr(&empty).contains("nothing staged"),
        "{}",
        stderr(&empty)
    );

    std::fs::write(r.work().join("b.txt"), "b").unwrap();
    assert!(ok(&git(&r.work(), &["add", "b.txt"])));
    let bad = r.agent(&["commit", "added b"], &r.work());
    assert!(!ok(&bad));
    assert!(
        stderr(&bad).contains("not a Conventional Commit"),
        "{}",
        stderr(&bad)
    );

    let good = r.agent(&["commit", "feat: add b"], &r.work());
    assert!(ok(&good), "{}", stderr(&good));
}

#[test]
fn agent_done_refuses_to_delete_the_worktree_it_runs_in() {
    let r = Repo::new();
    assert!(ok(&r.agent(&["start", "fix/thing"], &r.work())));
    let inside = r.dir.path().join(".worktrees/widget/fix/thing");
    let o = r.agent(&["done"], &inside);
    assert!(!ok(&o));
    assert!(
        stderr(&o).contains("run this from the primary checkout"),
        "{}",
        stderr(&o)
    );
    assert!(inside.exists(), "the worktree was removed anyway");
}

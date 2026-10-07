//! End to end: the real binary, in a real pseudo-terminal, against a fake `gh`
//! and a throwaway git repository with real worktrees.
//!
//! These are the checks that were once run by hand — a stalled `gh`, quitting
//! mid-call, SIGTERM, the color probe, lock-free git, focus — turned into tests
//! so they keep holding. The screen is rebuilt with a real terminal emulator
//! (vt100), so what is asserted is what a user would see.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "in a test an unwrap is an assertion: a failed setup step is a failed test"
)]

mod common;

use common::{QUIET_GIT, inherited_git_vars, isolated, write_exe};

use portable_pty::{Child, CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FIXTURE: &str = include_str!("fixtures/graphql_page.json");
/// The final commit the fixture gives the merged PR for `landed-branch`. Each
/// run's repository has its own commit ids, so the fake `gh` answers with the
/// landed desk's real HEAD in its place.
const FIXTURE_LANDED_OID: &str = "5d6c7b8a9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b";
/// The commit the fixture gives open PR #101 as its head.
const FIXTURE_HEAD_101: &str = "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b";
const ROWS: u16 = 30;
const COLS: u16 = 160;

// ------------------------------------------------------------------ the world

/// A repository with a bare origin and two worktrees, a fake `gh` that answers
/// from a fixture (or hangs, fails, or reports a rate limit), serves what a
/// test says about single pull requests, and records every call in order, and
/// a `git` shim that logs every call rigor makes.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        install_fakes(dir.path());
        Self { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn gh(&self, mode: &str) {
        std::fs::write(self.root().join("gh.mode"), mode).unwrap();
    }

    /// Leave a review draft for `acme/widget#number`, the way whatever writes
    /// them would: valid and written against #101's head, until `edit` says
    /// otherwise.
    fn draft(&self, number: u64, edit: impl FnOnce(&mut serde_json::Value)) -> PathBuf {
        let mut v = serde_json::json!({
            "version": 1,
            "repo": "acme/widget",
            "number": number,
            "head_sha": FIXTURE_HEAD_101,
            "verdict": "approve",
            "body": "Reads well.",
            "comments": [
                { "path": "src/x.rs", "line": 84, "side": "RIGHT", "body": "Worth a test." }
            ],
            "note": "PRIVATE-NOTE",
            "picked": "PRIVATE-PICKED",
            // Well in the past, whenever this runs: rigor here is on the real
            // clock, and a draft is written before it is read.
            "created_at": "2026-01-15T09:00:00Z",
            "status": "drafted",
            "posted_at": null
        });
        edit(&mut v);
        let dir = self.root().join("drafts/acme/widget");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{number}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        path
    }

    /// rigor on the Drafts view, reading this world's drafts.
    fn drafts_view(&self) -> Term {
        self.drafts_view_with(&[])
    }

    fn drafts_view_with(&self, env: &[(&str, &str)]) -> Term {
        let dir = self.root().join("drafts");
        let mut env = env.to_vec();
        env.push(("RIGOR_DRAFTS_DIR", dir.to_str().unwrap()));
        Term::start(
            self,
            Opts {
                args: &["-v", "drafts"],
                env: &env,
                ..Opts::default()
            },
        )
    }

    /// Every call the fake `gh` took, in the order it took them: its
    /// arguments, one call per line. The fetch is `api graphql`.
    fn calls(&self) -> Vec<String> {
        self.log("gh.calls")
    }

    /// The exact bytes the `n`th write was given on stdin, counting from 1.
    fn post_body(&self, n: usize) -> String {
        std::fs::read_to_string(self.root().join(format!("gh.post.{n}"))).unwrap()
    }

    /// What GitHub says about pull request `number` when asked for it alone.
    /// Until a test says, there is no such pull request.
    fn pull(&self, number: u64, state: &str, merged: bool, head: &str) {
        let v = serde_json::json!({
            "number": number, "state": state, "merged": merged, "head": { "sha": head }
        });
        std::fs::write(
            self.root().join(format!("pull.{number}.json")),
            v.to_string(),
        )
        .unwrap();
    }

    /// One page of the reviews on pull request `number`. Until a test says,
    /// every page is empty.
    fn reviews(&self, number: u64, page: usize, reviews: &serde_json::Value) {
        std::fs::write(
            self.root().join(format!("reviews.{number}.{page}.json")),
            reviews.to_string(),
        )
        .unwrap();
    }

    /// The first page of the inline comments on pull request `number`,
    /// whichever review each belongs to. Until a test says, there are none.
    fn comments(&self, number: u64, comments: &serde_json::Value) {
        std::fs::write(
            self.root().join(format!("comments.{number}.1.json")),
            comments.to_string(),
        )
        .unwrap();
    }

    /// How many writes the fake `gh` was asked to make.
    fn posts(&self) -> usize {
        self.log("gh.posts").len()
    }

    fn log(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.root().join(name))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }
}

fn git(args: &[&str], cwd: &Path) {
    let out = isolated("git", cwd).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository pushed to a bare origin, with two worktrees: a desk holding
/// uncommitted work, and one whose branch has landed.
fn init_repo(root: &Path) {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&["init", "-q", "-b", "main"], &repo);
    git(&["config", "user.email", "test@example.com"], &repo);
    git(&["config", "user.name", "Test"], &repo);
    git(&["config", "commit.gpgsign", "false"], &repo);
    std::fs::write(repo.join("README.md"), "widget\n").unwrap();
    git(&["add", "."], &repo);
    git(&["commit", "-q", "-m", "init"], &repo);
    let origin = root.join("origin.git");
    git(&["init", "-q", "--bare", origin.to_str().unwrap()], root);
    git(
        &["remote", "add", "origin", origin.to_str().unwrap()],
        &repo,
    );
    git(&["push", "-q", "-u", "origin", "main"], &repo);
    git(&["remote", "set-head", "origin", "main"], &repo);

    git(
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "retry-budget",
            "../wt/retry-budget",
        ],
        &repo,
    );
    std::fs::write(root.join("wt/retry-budget/scratch.txt"), "wip\n").unwrap();
    git(
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "landed-branch",
            "../wt/landed",
        ],
        &repo,
    );
}

/// The fake `gh`. It answers from files a test leaves beside it, so what it
/// says is the test's to decide, and it writes down every call it takes.
const FAKE_GH: &str = r#"#!/bin/sh
dir="$(cd "$(dirname "$0")/.." && pwd)"
# Every call, in the order it came. The fetch's query spans many lines, so it
# is logged by name; the rest by their arguments.
case "$2" in
  graphql) echo "api graphql" >> "$dir/gh.calls" ;;
  *) echo "$*" >> "$dir/gh.calls" ;;
esac
# A write: the body it was given on stdin is kept, and it is accepted unless a
# test left a refusal for it, which is answered the way gh answers an HTTP
# error — the response body on stdout, a one-line summary on stderr.
case "$*" in
  *--method*)
    echo "$*" >> "$dir/gh.posts"
    cat > "$dir/gh.post.$(wc -l < "$dir/gh.posts" | tr -d ' ')"
    # A write GitHub never answers, and one it takes its time over.
    if [ -f "$dir/post.hang" ]; then exec sleep 600; fi
    if [ -f "$dir/post.delay" ]; then sleep "$(cat "$dir/post.delay")"; fi
    if [ -f "$dir/post.refusal.json" ]; then
      cat "$dir/post.refusal.json"
      echo "gh: Unprocessable Entity (HTTP 422)" >&2
      exit 1
    fi
    # What GitHub answers a write with: the review it created.
    cat "$dir/post.response.json"
    exit 0 ;;
esac
# The reads made before a write: one pull request, and its reviews and its
# inline comments a page at a time.
case "$2" in
  repos/*/pulls/*/comments*)
    n="${2#*/pulls/}"; n="${n%%/*}"; page="${2##*page=}"
    if [ -f "$dir/comments.$n.$page.json" ]; then
      cat "$dir/comments.$n.$page.json"
    else
      printf '[]'
    fi
    exit 0 ;;
  repos/*/pulls/*/reviews*)
    n="${2#*/pulls/}"; n="${n%%/*}"; page="${2##*page=}"
    if [ -f "$dir/reviews.$n.err" ]; then
      cat "$dir/reviews.$n.err" >&2
      exit 1
    fi
    if [ -f "$dir/reviews.$n.$page.json" ]; then
      cat "$dir/reviews.$n.$page.json"
    else
      printf '[]'
    fi
    exit 0 ;;
  repos/*/pulls/*)
    n="${2##*/}"
    if [ -f "$dir/pull.$n.json" ]; then
      cat "$dir/pull.$n.json"
      exit 0
    fi
    printf '%s' '{"message":"Not Found","status":"404"}'
    echo "gh: Not Found (HTTP 404)" >&2
    exit 1 ;;
esac
# One line per call. Not "$*": the query argument spans many lines, and
# counting lines would count the query, not the calls.
echo "$1 $2" >> "$dir/gh.log"
case "$(cat "$dir/gh.mode")" in
  hang) echo $$ > "$dir/gh.pid"; exec sleep 600 ;;
  fail) echo "HTTP 502: Bad Gateway" >&2; exit 1 ;;
  ratelimit) printf '%s' '{"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded for user ID 1."}]}' ;;
  *) cat "$dir/fixture.json" ;;
esac
"#;

/// A fake `gh` answering from the fixture (or hanging, failing, or reporting a
/// rate limit), a `git` shim logging every call, and an opener that records the
/// URL it was handed.
fn install_fakes(root: &Path) {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let landed = isolated("git", &root.join("wt/landed"))
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let landed = String::from_utf8(landed.stdout).unwrap();
    assert!(FIXTURE.contains(FIXTURE_LANDED_OID));
    std::fs::write(
        root.join("fixture.json"),
        FIXTURE.replace(FIXTURE_LANDED_OID, landed.trim()),
    )
    .unwrap();
    std::fs::write(root.join("gh.mode"), "ok").unwrap();
    write_exe(&bin.join("gh"), FAKE_GH);
    let real_git = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    write_exe(
        &bin.join("git"),
        &format!(
            "#!/bin/sh\necho \"$*\" >> \"$(cd \"$(dirname \"$0\")/..\" && pwd)/git.log\"\nexec {} \"$@\"\n",
            real_git.trim()
        ),
    );
    write_exe(
        &bin.join("open-log"),
        "#!/bin/sh\necho \"$1\" >> \"$(cd \"$(dirname \"$0\")/..\" && pwd)/opened.log\"\n",
    );
    // Nothing a test presses reaches the real clipboard.
    std::fs::write(
        root.join("config.toml"),
        format!(
            "open_command = \"{}\"\ncopy_command = \"true\"\n",
            bin.join("open-log").display()
        ),
    )
    .unwrap();
    // What the fake `gh` answers a write with, until a test says otherwise:
    // the review `World::draft` describes, created, by the fixture's viewer.
    std::fs::write(
        root.join("post.response.json"),
        serde_json::json!({
            "id": 900,
            "user": { "login": "octocat" },
            "state": "APPROVED",
            "commit_id": FIXTURE_HEAD_101,
            "body": "Reads well.",
            "submitted_at": "2026-01-15T10:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
}

// --------------------------------------------------------------- the terminal

struct Term {
    screen: Arc<Mutex<vt100::Parser>>,
    raw: Arc<Mutex<Vec<u8>>>,
    input: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

#[derive(Default, Clone, Copy)]
struct Opts<'a> {
    args: &'a [&'a str],
    env: &'a [(&'a str, &'a str)],
    /// Answer OSC 10/11 like a terminal that reports its colors.
    colors: bool,
}

impl Term {
    fn start(world: &World, opts: Opts<'_>) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_rigor"));
        cmd.args(["-R", "acme/widget"]);
        // Start on All unless the test picks a view: the fixture has nothing
        // ready to merge, so the default Ready view would be empty.
        if !opts.args.contains(&"-v") {
            cmd.args(["-v", "all"]);
        }
        cmd.args(opts.args);
        cmd.cwd(world.root().join("repo"));
        // Hermetic: the fake tools first on PATH, and none of the developer's
        // own config or theme leaking in.
        let path = format!(
            "{}:{}",
            world.root().join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        cmd.env("PATH", path);
        cmd.env("HOME", world.root());
        cmd.env("RIGOR_CONFIG", world.root().join("config.toml"));
        cmd.env("TERM", "xterm-256color");
        for k in [
            "RIGOR_THEME",
            "HERDR_THEME_FILE",
            "NO_COLOR",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "RIGOR_DRAFTS_DIR",
            "VISUAL",
            "EDITOR",
        ] {
            cmd.env_remove(k);
        }
        for k in inherited_git_vars() {
            cmd.env_remove(k);
        }
        for (k, v) in QUIET_GIT {
            cmd.env(k, v);
        }
        for (k, v) in opts.env {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).unwrap();
        drop(pair.slave);

        let screen = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
        let raw = Arc::new(Mutex::new(Vec::new()));
        let input: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
        let mut reader = pair.master.try_clone_reader().unwrap();
        let (screen2, raw2, input2, colors) =
            (screen.clone(), raw.clone(), input.clone(), opts.colors);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut answered = false;
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                screen2.lock().unwrap().process(&buf[..n]);
                let mut raw = raw2.lock().unwrap();
                raw.extend_from_slice(&buf[..n]);
                // Behave like a terminal: every terminal answers DA1; only some
                // report their colors.
                if !answered && raw.windows(3).any(|w| w == b"\x1b[c") {
                    answered = true;
                    let mut reply = Vec::new();
                    if colors {
                        reply.extend_from_slice(
                            b"\x1b]10;rgb:d4d4/d8d8/dede\x1b\\\x1b]11;rgb:1616/1818/1c1c\x1b\\",
                        );
                    }
                    reply.extend_from_slice(b"\x1b[?62;22c");
                    let _ = input2.lock().unwrap().write_all(&reply);
                }
            }
        });
        Self {
            screen,
            raw,
            input,
            child,
            _master: pair.master,
        }
    }

    fn text(&self) -> String {
        self.screen.lock().unwrap().screen().contents()
    }

    fn row(&self, r: u16) -> String {
        self.screen
            .lock()
            .unwrap()
            .screen()
            .rows(0, COLS)
            .nth(r as usize)
            .unwrap_or_default()
    }

    /// Wait until `f` holds for the screen text; the screen at the deadline is
    /// in the failure message, so a failing test shows what was on screen.
    fn wait(&self, what: &str, timeout: Duration, f: impl Fn(&str) -> bool) {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if f(&self.text()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("timed out waiting for {what}. Screen:\n{}", self.text());
    }

    /// Wait until some line of the screen satisfies `f`, and return it.
    fn wait_line(&self, what: &str, timeout: Duration, f: impl Fn(&str) -> bool) -> String {
        self.wait(what, timeout, |t| t.lines().any(&f));
        self.text().lines().find(|l| f(l)).unwrap().to_string()
    }

    fn wait_for(&self, needle: &str, timeout: Duration) {
        self.wait(&format!("{needle:?}"), timeout, |t| t.contains(needle));
    }

    fn send(&self, bytes: &[u8]) {
        self.input.lock().unwrap().write_all(bytes).unwrap();
    }

    fn pid(&self) -> i32 {
        i32::try_from(self.child.process_id().unwrap()).unwrap()
    }

    fn wait_exit(&mut self, timeout: Duration) -> portable_pty::ExitStatus {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("rigor did not exit within {timeout:?}");
    }

    fn raw(&self) -> Vec<u8> {
        self.raw.lock().unwrap().clone()
    }
}

impl Drop for Term {
    /// Quit the way a user would and wait for it, so rigor cleans up its own
    /// children before the test ends. Only a rigor that will not quit is killed.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.input.lock().map(|mut i| i.write_all(b"q"));
            let end = Instant::now() + Duration::from_secs(3);
            while Instant::now() < end {
                if let Ok(Some(_)) = self.child.try_wait() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: kill(2) with signal 0 delivers nothing; it only asks whether the
    // process exists. It takes plain integers and touches no memory.
    unsafe { libc::kill(pid, 0) == 0 }
}

const LOAD: Duration = Duration::from_secs(15);

// --------------------------------------------------------------------- tests

#[test]
fn shows_pull_requests_and_the_worktrees_behind_them() {
    let w = World::new();
    let t = Term::start(
        &w,
        Opts {
            args: &["-v", "all"],
            ..Opts::default()
        },
    );
    t.wait_for("#101", LOAD);
    let text = t.text();
    for needle in [
        "#102",
        "#103",
        "Give the read path a retry budget",
        "acme/widget",
    ] {
        assert!(text.contains(needle), "missing {needle:?}:\n{text}");
    }

    t.send(b"6"); // Worktrees is the sixth tab
    // Wait on something only this view draws: the main worktree's own row.
    // "retry-budget" and "removable" are already on the All view, in the
    // detail pane and the subnav, and would pass before the key landed.
    t.wait_for("repo (main)", LOAD);
    // The landed branch's desk is clean, so it is offered for removal; the
    // desk with a scratch file is marked as holding work and is not.
    t.wait_line("the landed desk marked removable", LOAD, |l| {
        l.contains(" landed ") && l.contains("removable")
    });
    let dirty = t.wait_line("the dirty desk", LOAD, |l| {
        l.contains("retry-budget") && l.contains('●')
    });
    assert!(!dirty.contains("removable"), "{dirty:?}");
}

#[test]
fn open_hands_the_selected_pull_request_to_the_browser() {
    let w = World::new();
    let t = Term::start(
        &w,
        Opts {
            args: &["-v", "all"],
            ..Opts::default()
        },
    );
    t.wait_for("#101", LOAD);
    t.send(b"o");
    let end = Instant::now() + Duration::from_secs(5);
    while w.log("opened.log").is_empty() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        w.log("opened.log"),
        vec!["https://github.com/acme/widget/pull/101"]
    );
}

#[test]
fn a_click_on_a_tab_switches_the_view() {
    let w = World::new();
    let t = Term::start(&w, Opts::default());
    t.wait_for("#101", LOAD);
    let tabs = t.row(1);
    let col = u16::try_from(tabs.find("Worktrees").expect("tab bar")).unwrap() + 2;
    // SGR mouse: press and release, 1-based coordinates.
    t.send(format!("\x1b[<0;{col};2M\x1b[<0;{col};2m").as_bytes());
    t.wait_for("removable", LOAD);
}

#[test]
fn a_stalled_gh_times_out_and_the_next_refresh_recovers() {
    let w = World::new();
    w.gh("hang");
    let t = Term::start(
        &w,
        Opts {
            args: &["-i", "1"],
            env: &[("RIGOR_GH_TIMEOUT_SECS", "2")],
            ..Opts::default()
        },
    );
    t.wait_for("sync failed", Duration::from_secs(15));
    assert!(t.text().contains("timed out after 2s"), "{}", t.text());
    w.gh("ok");
    t.wait_for("#101", Duration::from_secs(20));
}

#[test]
fn quitting_mid_call_leaves_nothing_running() {
    let w = World::new();
    w.gh("hang");
    let mut t = Term::start(&w, Opts::default());
    let end = Instant::now() + LOAD;
    while !w.root().join("gh.pid").exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    let pid: i32 = std::fs::read_to_string(w.root().join("gh.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(alive(pid), "the stalled gh should be running");
    t.send(b"q");
    t.wait_exit(Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(200));
    assert!(!alive(pid), "gh {pid} outlived rigor");
}

#[test]
fn sigterm_exits_cleanly_and_restores_the_terminal() {
    let w = World::new();
    let mut t = Term::start(&w, Opts::default());
    t.wait_for("#101", LOAD);
    // SAFETY: kill(2) takes plain integers and touches no memory; the target is
    // the rigor this test started and still owns.
    unsafe { libc::kill(t.pid(), libc::SIGTERM) };
    let status = t.wait_exit(Duration::from_secs(5));
    assert!(status.success(), "{status:?}");
    let raw = t.raw();
    let has = |seq: &[u8]| raw.windows(seq.len()).any(|w| w == seq);
    assert!(has(b"\x1b[?1049l"), "left the alternate screen");
    assert!(has(b"\x1b[?1004l"), "turned focus reporting off");
}

/// The selection band is mixed from the colors the terminal reports; a
/// terminal that reports nothing gets no band.
#[test]
fn the_selection_band_comes_from_the_terminals_own_colors() {
    let band = |colors: bool| {
        let w = World::new();
        let t = Term::start(
            &w,
            Opts {
                args: &["-v", "all"],
                colors,
                ..Opts::default()
            },
        );
        t.wait_for("#101", LOAD);
        // A copy of the screen, so the lock is released at once.
        let s = t.screen.lock().unwrap().screen().clone();
        let row = (0..ROWS)
            .find(|r| s.contents_between(*r, 0, *r, COLS).contains("#101"))
            .unwrap();
        s.cell(row, 10).unwrap().bgcolor()
    };
    assert!(
        matches!(band(true), vt100::Color::Rgb(..)),
        "no band from reported colors"
    );
    assert_eq!(
        band(false),
        vt100::Color::Default,
        "a band without reported colors"
    );
}

#[test]
fn background_git_never_takes_optional_locks() {
    let w = World::new();
    let t = Term::start(&w, Opts::default());
    t.wait_for("#101", LOAD);
    std::thread::sleep(Duration::from_secs(1)); // let the worktree scan finish
    let calls = w.log("git.log");
    assert!(
        calls.iter().any(|c| c.contains(" status ")),
        "no status scan ran: {calls:?}"
    );
    let unlocked: Vec<_> = calls
        .iter()
        .filter(|c| !c.contains("--no-optional-locks"))
        .collect();
    assert!(
        unlocked.is_empty(),
        "git calls that may take index.lock: {unlocked:?}"
    );
}

#[test]
fn losing_focus_slows_refreshing() {
    let w = World::new();
    let t = Term::start(
        &w,
        Opts {
            args: &["-i", "1"],
            ..Opts::default()
        },
    );
    t.wait_for("#101", LOAD);
    let count_over = |secs: u64| {
        let before = w.log("gh.log").len();
        std::thread::sleep(Duration::from_secs(secs));
        w.log("gh.log").len() - before
    };
    let focused = count_over(4);
    t.send(b"\x1b[O"); // the terminal reports focus lost
    std::thread::sleep(Duration::from_millis(300)); // let it land
    let unfocused = count_over(4);
    assert!(
        focused >= 3,
        "expected about one fetch a second while focused, got {focused}"
    );
    assert!(
        unfocused <= 1,
        "expected at most one fetch in 4s unfocused, got {unfocused}"
    );
}

#[test]
fn a_rate_limited_budget_pauses_instead_of_retrying() {
    let w = World::new();
    w.gh("ratelimit");
    let t = Term::start(
        &w,
        Opts {
            args: &["-i", "1"],
            ..Opts::default()
        },
    );
    t.wait_for("rate limited", LOAD);
    let before = w.log("gh.log").len();
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(w.log("gh.log").len(), before, "fetched again while paused");
}

// -------------------------------------------------------------------- drafts

/// Drafts are found through the environment and checked against what GitHub
/// says the pull request's head is, by the real binary.
#[test]
fn drafts_are_read_from_the_drafts_directory_and_checked_against_the_head() {
    let w = World::new();
    w.draft(101, |_| {});
    // #102 is at another commit in the fixture.
    w.draft(102, |_| {});
    w.draft(999, |_| {});
    let t = w.drafts_view();
    // Only once the fetch lands is #101 known to be at the draft's commit;
    // until then every draft is unverified.
    t.wait_line("#101 ready to post", LOAD, |l| {
        l.contains("#101") && l.contains("approve +1")
    });
    t.wait_line("#102 stale", LOAD, |l| {
        l.contains("#102") && l.contains("stale")
    });
    t.wait_line("#999 unverified", LOAD, |l| {
        l.contains("#999") && l.contains("unverified")
    });
    assert!(t.row(1).contains("Drafts 3"), "{:?}", t.row(1));
}

/// With no drafts and the view left off the bar, the screen says nothing
/// about them.
#[test]
fn the_default_screen_does_not_mention_drafts() {
    let w = World::new();
    let t = Term::start(&w, Opts::default());
    t.wait_for("#101", LOAD);
    let text = t.text();
    assert!(
        !text.contains("Drafts") && !text.contains("drafted"),
        "{text}"
    );
}

/// What the fake `gh` logs for each of the calls a post is made of.
const FETCH: &str = "api graphql";

fn pull_call(number: u64) -> String {
    format!("api repos/acme/widget/pulls/{number}")
}

fn reviews_call(number: u64, page: usize) -> String {
    format!("api repos/acme/widget/pulls/{number}/reviews?per_page=100&page={page}")
}

fn comments_call(number: u64) -> String {
    format!("api repos/acme/widget/pulls/{number}/comments?per_page=100&page=1")
}

fn post_call(number: u64) -> String {
    format!("api --method POST repos/acme/widget/pulls/{number}/reviews --input -")
}

/// A review as GitHub lists it: on the commit the drafts are written against,
/// half an hour after they were written.
fn review(id: u64, login: &str, state: &str, body: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "user": { "login": login },
        "state": state,
        "commit_id": FIXTURE_HEAD_101,
        "body": body,
        "submitted_at": "2026-01-15T09:30:00Z"
    })
}

/// The one inline comment `World::draft` writes, as GitHub lists it among a
/// pull request's comments, on review `review` — beside somebody else's.
fn the_drafts_comment_on(review: u64) -> serde_json::Value {
    serde_json::json!([
        { "id": 1, "pull_request_review_id": 3, "path": "src/y.rs", "line": 2,
          "original_line": 2, "side": "RIGHT", "original_side": null,
          "start_line": null, "subject_type": "line", "body": "Elsewhere." },
        { "id": 2, "pull_request_review_id": review, "path": "src/x.rs", "line": 84,
          "original_line": 84, "side": "RIGHT", "original_side": null,
          "start_line": null, "subject_type": "line", "body": "Worth a test." }
    ])
}

const MOVED: &str = "9f8e7d6c5b4a39281706f5e4d3c2b1a098765432";

/// A world with one draft for #101, rigor on the Drafts view, and the fetch
/// landed: the draft is ready, and the viewer is known. Returns the draft's
/// path and text as its author left it.
fn ready_to_post(w: &World) -> (Term, PathBuf, String) {
    ready_to_post_with(w, &[])
}

fn ready_to_post_with(w: &World, env: &[(&str, &str)]) -> (Term, PathBuf, String) {
    let path = w.draft(101, |_| {});
    let before = std::fs::read_to_string(&path).unwrap();
    let t = w.drafts_view_with(env);
    t.wait_line("#101 ready to post", LOAD, |l| {
        l.contains("#101") && l.contains("approve +1")
    });
    (t, path, before)
}

/// The question a first `a` puts up, drawn.
const ASKED: &str = "this is what will be posted";

/// `a`, the overlay read, a pause any person would leave, and capital `Y`.
fn confirm(t: &Term) {
    t.send(b"a");
    t.wait_for(ASKED, LOAD);
    t.wait_for("Y to post, esc to cancel", LOAD);
    std::thread::sleep(Duration::from_millis(600));
    t.send(b"Y");
}

/// The one write rigor makes to GitHub, end to end. The first `a` only asks,
/// showing what would be sent. The second reads the pull request and its
/// reviews, there and then, and only after both sends exactly one request
/// with exactly the review in it; only then is the file marked posted.
#[test]
fn a_confirmed_post_reads_twice_then_sends_exactly_one_review() {
    let w = World::new();
    w.pull(101, "open", false, FIXTURE_HEAD_101);
    let (t, path, before) = ready_to_post(&w);
    let before: serde_json::Value = serde_json::from_str(&before).unwrap();

    t.send(b"a");
    t.wait_for(
        "post approve with 1 inline comment to #101? Y to post, esc to cancel",
        LOAD,
    );
    // What will go out is on screen, under the pull request it is for, and
    // what stays private is not in it.
    t.wait_for(ASKED, LOAD);
    let overlay: Vec<String> = t
        .text()
        .lines()
        .filter(|l| l.contains('│'))
        .map(String::from)
        .collect();
    let shown = overlay.join("\n");
    for needle in [
        "#101 Give the read path a retry budget",
        "verdict   approve",
        "body      Reads well.",
        "comment   src/x.rs:84",
        "Worth a test.",
    ] {
        assert!(shown.contains(needle), "missing {needle:?}:\n{shown}");
    }
    assert_eq!(w.calls(), [FETCH], "one a reached GitHub");

    std::thread::sleep(Duration::from_millis(600));
    t.send(b"Y");
    t.wait_line("#101 posted", LOAD, |l| {
        l.contains("#101") && l.contains("✓ posted")
    });

    assert_eq!(
        w.calls(),
        [
            FETCH.to_string(),
            pull_call(101),
            reviews_call(101, 1),
            post_call(101)
        ]
    );
    let body = w.post_body(1);
    assert_eq!(
        body,
        format!(
            r#"{{"commit_id":"{FIXTURE_HEAD_101}","event":"APPROVE","body":"Reads well.","comments":[{{"path":"src/x.rs","line":84,"side":"RIGHT","body":"Worth a test."}}]}}"#
        )
    );
    assert!(!body.contains("PRIVATE"), "note or picked was sent: {body}");

    // Posted, with a time, and every other field as its author wrote it.
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let at = after["posted_at"].as_str().expect("posted_at is set");
    assert!(
        at.len() == 20 && at.ends_with('Z') && at.as_bytes()[10] == b'T',
        "{at}"
    );
    let mut want = before;
    want["status"] = "posted".into();
    want["posted_at"] = at.into();
    assert_eq!(after, want);
    let left: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|f| f.unwrap().file_name())
        .collect();
    assert_eq!(
        left,
        ["101.json"],
        "the rewrite or the lock left something behind"
    );
}

/// A stray keypress reaches nothing: `a` only asks, a second `a` is not an
/// answer, and anything that is not capital `Y` takes the question down.
#[test]
fn a_alone_sends_nothing_and_a_second_a_cancels() {
    let w = World::new();
    w.pull(101, "open", false, FIXTURE_HEAD_101);
    let (t, path, before) = ready_to_post(&w);
    let asked = |t: &Term| t.wait_for(ASKED, LOAD);
    let withdrawn = |t: &Term| {
        t.wait("the question to go", LOAD, |s| !s.contains(ASKED));
    };

    // However long after: the key that asks is not the key that answers.
    t.send(b"a");
    asked(&t);
    std::thread::sleep(Duration::from_millis(900));
    t.send(b"a");
    withdrawn(&t);

    t.send(b"a");
    asked(&t);
    t.send(b"\x1b");
    withdrawn(&t);
    t.send(b"a");
    asked(&t);
    t.send(b"x");
    withdrawn(&t);
    t.send(b"a");
    asked(&t);
    t.send(b"q");
    withdrawn(&t);

    // Help only opens once every key before it has been handled — and rigor
    // is still here to open it: the `q` took the question down, not rigor.
    t.send(b"?");
    t.wait_for(" keys ", LOAD);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(w.calls(), [FETCH], "not so much as a read");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "the x skipped it"
    );
}

/// A key held down, keys typed ahead, a paste with `aY` in it: none of them
/// is a decision, and none of them posts.
#[test]
fn a_held_a_a_typed_ahead_answer_and_a_paste_do_not_post() {
    let w = World::new();
    w.pull(101, "open", false, FIXTURE_HEAD_101);
    let (t, path, before) = ready_to_post(&w);

    // Held: one press, the keyboard's own delay, then a repeat every few
    // hundredths of a second for a couple of seconds.
    t.send(b"a");
    t.wait_for(ASKED, LOAD);
    std::thread::sleep(Duration::from_millis(500));
    for _ in 0..60 {
        t.send(b"a");
        std::thread::sleep(Duration::from_millis(30));
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(w.calls(), [FETCH], "a held a posted");
    // Each repeat took the question down or put it back up; wherever that
    // left it, take it down.
    t.send(b"\x1b");
    t.wait("the question to go", LOAD, |s| !s.contains(ASKED));

    // Typed ahead: both keys in one write. The `a` asks; the `Y` arrives
    // before the question has been on screen long enough to be an answer.
    t.send(b"aY");
    t.wait_for(ASKED, LOAD);
    std::thread::sleep(Duration::from_millis(900));
    assert_eq!(w.calls(), [FETCH], "a typed-ahead aY posted");
    assert!(t.text().contains(ASKED), "and the question is still up");

    // A paste is not an answer, and takes the question down.
    t.send(b"\x1b[200~Y\x1b[201~");
    t.wait("the question to go", LOAD, |s| !s.contains(ASKED));
    // Nor is it a question.
    t.send(b"\x1b[200~aY\x1b[201~");
    t.send(b"?");
    t.wait_for(" keys ", LOAD);
    let text = t.text();
    assert!(!text.contains(ASKED), "a paste asked to post:\n{text}");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(w.calls(), [FETCH]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

    // The terminal was asked for pastes as pastes.
    let raw = t.raw();
    assert!(
        raw.windows(8).any(|w| w == b"\x1b[?2004h"),
        "bracketed paste was never turned on"
    );
}

/// Stale, invalid, skipped and already posted never arm: `a`, twice, on each
/// asks GitHub nothing, says why, and leaves the file alone.
#[test]
fn drafts_that_never_arm_ask_github_nothing() {
    let w = World::new();
    // The fixture has #102 at another commit and lists #90 as merged.
    let mut files = vec![
        (w.draft(102, |_| {}), "102", "#102 is stale"),
        (w.draft(103, |_| {}), "103", "#103 is invalid"),
        (
            w.draft(101, |v| v["status"] = "skipped".into()),
            "101",
            "#101 is skipped",
        ),
        (
            w.draft(90, |v| {
                v["status"] = "posted".into();
                v["posted_at"] = "2026-01-15T09:30:00Z".into();
            }),
            "90",
            "#90 is already posted",
        ),
    ];
    std::fs::write(&files[1].0, "{ \"version\": 1,").unwrap();
    for n in [90, 101, 102, 103] {
        w.pull(n, "open", false, FIXTURE_HEAD_101);
    }
    let before: Vec<String> = files
        .iter()
        .map(|(p, _, _)| std::fs::read_to_string(p).unwrap())
        .collect();

    let t = w.drafts_view();
    // The fetch has landed once #102's head is known to differ.
    t.wait_line("#102 stale", LOAD, |l| {
        l.contains("#102") && l.contains("stale")
    });
    for (_, number, why) in &files {
        // Narrow the list to the one draft, so the row under `a` is not a
        // matter of ordering. Enter is a carriage return to a terminal in
        // raw mode.
        t.send(format!("/{number}\r").as_bytes());
        t.wait("the filter to apply", LOAD, |s| {
            s.contains(&format!("/{number} ")) && s.contains("Drafts 4")
        });
        t.send(b"a");
        t.wait_for(why, LOAD);
        // No question went up: the screen says why, and shows no overlay.
        assert!(!t.text().contains(ASKED), "#{number} asked to be posted");
        t.send(b"\x1b");
        t.wait("the filter to clear", LOAD, |s| {
            !s.contains(&format!("/{number} "))
        });
    }

    t.send(b"?");
    t.wait_for(" keys ", LOAD);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(w.calls(), [FETCH]);
    for ((path, number, _), text) in files.drain(..).zip(before) {
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            text,
            "#{number} was rewritten"
        );
    }
}

/// The list said #101 was at the draft's commit; GitHub, asked on the
/// confirming keypress, says it has moved. Nothing is sent, the reviews are
/// not even read, and the row is stale from then on.
#[test]
fn a_pull_request_that_moved_since_the_fetch_is_refused_and_goes_stale() {
    let w = World::new();
    w.pull(101, "open", false, MOVED);
    let (t, path, before) = ready_to_post(&w);

    confirm(&t);
    t.wait_for(
        "nothing posted: #101 has moved on to 9f8e7d6 since this draft was written",
        LOAD,
    );
    t.wait_line("#101 stale", LOAD, |l| {
        l.contains("#101") && l.contains("stale")
    });
    assert_eq!(w.calls(), [FETCH.to_string(), pull_call(101)]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn a_pull_request_closed_without_merging_takes_no_review() {
    let w = World::new();
    w.pull(101, "closed", false, FIXTURE_HEAD_101);
    let (t, path, before) = ready_to_post(&w);

    confirm(&t);
    t.wait_for("nothing posted: #101 was closed without being merged", LOAD);
    assert_eq!(w.calls(), [FETCH.to_string(), pull_call(101)]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

/// The same review is already on the pull request — here on the second page
/// of a long list, and with its inline comments read back and found the
/// same — so the draft was sent: it is marked posted, with the time GitHub
/// gives, and nothing more goes out.
#[test]
fn a_review_already_on_github_marks_the_draft_posted_and_sends_nothing() {
    let w = World::new();
    w.pull(101, "open", false, FIXTURE_HEAD_101);
    let others: Vec<_> = (0..100)
        .map(|i| review(i + 1, "hubot", "COMMENTED", "Noted."))
        .collect();
    w.reviews(101, 1, &serde_json::Value::Array(others));
    w.reviews(
        101,
        2,
        &serde_json::json!([review(555, "octocat", "APPROVED", "Reads well.")]),
    );
    w.comments(101, &the_drafts_comment_on(555));
    let (t, path, before) = ready_to_post(&w);
    let before: serde_json::Value = serde_json::from_str(&before).unwrap();

    confirm(&t);
    t.wait_line("#101 posted", LOAD, |l| {
        l.contains("#101") && l.contains("✓ posted")
    });
    assert_eq!(
        w.calls(),
        [
            FETCH.to_string(),
            pull_call(101),
            reviews_call(101, 1),
            reviews_call(101, 2),
            comments_call(101)
        ]
    );
    let mut want = before;
    want["status"] = "posted".into();
    want["posted_at"] = "2026-01-15T09:30:00Z".into();
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(after, want);
}

/// A review with the same verdict, commit and words — but not the draft's
/// inline comment — is another review. The draft is not marked posted on its
/// account, and is not sent on top of it either.
#[test]
fn a_review_that_only_looks_the_same_does_not_mark_the_draft_posted() {
    let w = World::new();
    w.pull(101, "open", false, FIXTURE_HEAD_101);
    w.reviews(
        101,
        1,
        &serde_json::json!([review(555, "octocat", "APPROVED", "Reads well.")]),
    );
    // Review 555 has no inline comments; the draft has one.
    let (t, path, before) = ready_to_post(&w);

    confirm(&t);
    t.wait_for(
        "nothing posted: you have reviewed #101 since this draft was written",
        LOAD,
    );
    assert_eq!(
        w.calls(),
        [
            FETCH.to_string(),
            pull_call(101),
            reviews_call(101, 1),
            comments_call(101)
        ]
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "a draft whose comment never went out was marked posted"
    );
}

/// The viewer has said something else on the pull request since the draft
/// was written. The draft is not sent on top of it, and stays as it is.
#[test]
fn a_different_review_since_the_draft_was_written_blocks_it() {
    let w = World::new();
    w.pull(101, "open", false, FIXTURE_HEAD_101);
    w.reviews(
        101,
        1,
        &serde_json::json!([review(9, "octocat", "CHANGES_REQUESTED", "Not yet.")]),
    );
    let (t, path, before) = ready_to_post(&w);

    confirm(&t);
    t.wait_for(
        "nothing posted: you have reviewed #101 since this draft was written",
        LOAD,
    );
    assert_eq!(
        w.calls(),
        [FETCH.to_string(), pull_call(101), reviews_call(101, 1)]
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    t.wait_line("#101 still a draft", LOAD, |l| {
        l.contains("#101") && l.contains("approve +1")
    });
}

/// If GitHub cannot be asked, nothing is sent on the list's word alone.
#[test]
fn a_read_that_fails_sends_nothing() {
    let w = World::new();
    w.pull(101, "open", false, FIXTURE_HEAD_101);
    std::fs::write(w.root().join("reviews.101.err"), "gh: HTTP 502\n").unwrap();
    let (t, path, before) = ready_to_post(&w);

    confirm(&t);
    t.wait_for(
        "nothing posted: could not check #101 on GitHub: gh: HTTP 502",
        LOAD,
    );
    assert_eq!(
        w.calls(),
        [FETCH.to_string(), pull_call(101), reviews_call(101, 1)]
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

/// GitHub declines the review with a 422. Its own message is what the screen
/// shows; nothing is recorded, and the row is still a draft.
#[test]
fn a_422_from_github_is_shown_in_its_words_and_leaves_the_draft_alone() {
    let w = World::new();
    w.pull(101, "open", false, FIXTURE_HEAD_101);
    std::fs::write(
        w.root().join("post.refusal.json"),
        r#"{"message":"Unprocessable Entity","errors":["Line must be part of the diff"],"status":"422"}"#,
    )
    .unwrap();
    let (t, path, before) = ready_to_post(&w);

    confirm(&t);
    t.wait_for(
        "posting to #101 failed: Unprocessable Entity: Line must be part of the diff (HTTP 422)",
        LOAD,
    );
    assert_eq!(
        w.calls(),
        [
            FETCH.to_string(),
            pull_call(101),
            reviews_call(101, 1),
            post_call(101)
        ]
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    t.wait_line("#101 still a draft", LOAD, |l| {
        l.contains("#101") && l.contains("approve +1")
    });

    // It is not swept away by the next key that happens to be pressed.
    t.send(b"j?");
    t.wait_for(" keys ", LOAD);
    t.send(b"?");
    t.wait("help to close", LOAD, |s| !s.contains(" keys "));
    assert!(t.text().contains("posting to #101 failed"), "{}", t.text());
    t.send(b"\x1b");
    t.wait("the failure to be dismissed", LOAD, |s| {
        !s.contains("posting to #101 failed")
    });
}

/// A write GitHub never answers is not a failure: the review may be there.
/// The screen says so, the row stops reading as a plain draft, and trying
/// again asks GitHub first — which here finds the review, and posts nothing
/// twice.
#[test]
fn a_post_that_times_out_is_unknown_and_is_not_posted_twice() {
    let w = World::new();
    w.pull(101, "open", false, FIXTURE_HEAD_101);
    std::fs::write(w.root().join("post.hang"), "").unwrap();
    // Short enough that the unanswered write does not hold the test up for
    // long, long enough that the fetch before it never comes near it, however
    // busy the machine running the suite is.
    let (t, path, before) = ready_to_post_with(&w, &[("RIGOR_GH_TIMEOUT_SECS", "6")]);

    confirm(&t);
    t.wait_for(
        "whether it was posted is not known",
        Duration::from_secs(30),
    );
    let text = t.text();
    assert!(
        text.contains("the review was sent") && !text.contains("posting to #101 failed"),
        "{text}"
    );
    t.wait_line("#101 no longer a plain draft", LOAD, |l| {
        l.contains("#101") && l.contains("maybe sent")
    });
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

    // It had landed all the same: GitHub lists it, stamped when rigor sent
    // it, which is now.
    std::fs::remove_file(w.root().join("post.hang")).unwrap();
    let now = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .unwrap();
    let mut landed = review(555, "octocat", "APPROVED", "Reads well.");
    landed["submitted_at"] = String::from_utf8(now.stdout).unwrap().trim().into();
    w.reviews(101, 1, &serde_json::Value::Array(vec![landed]));
    w.comments(101, &the_drafts_comment_on(555));
    confirm(&t);
    t.wait_line("#101 posted", LOAD, |l| {
        l.contains("#101") && l.contains("✓ posted")
    });
    assert_eq!(w.posts(), 1, "posted twice: {:?}", w.calls());
    assert_eq!(
        w.calls()[4..],
        [pull_call(101), reviews_call(101, 1), comments_call(101)],
        "the second attempt read, and did not write"
    );
}

/// Two rigor windows on one repository: the one that finds the other's lock
/// beside the draft leaves it to it.
#[test]
fn a_draft_another_window_is_posting_is_not_posted_from_this_one() {
    let w = World::new();
    w.pull(101, "open", false, FIXTURE_HEAD_101);
    let (t, path, before) = ready_to_post(&w);
    std::fs::write(path.with_file_name(".101.json.lock"), "4242\n").unwrap();

    confirm(&t);
    t.wait_for("nothing posted: another rigor window is posting #101", LOAD);
    assert_eq!(w.calls(), [FETCH]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

/// Quitting while a post is in flight does not cut it off: rigor says it is
/// finishing, lets `gh` answer, records the result, and only then goes.
#[test]
fn quitting_mid_post_lets_it_finish_and_records_it() {
    let w = World::new();
    w.pull(101, "open", false, FIXTURE_HEAD_101);
    std::fs::write(w.root().join("post.delay"), "2").unwrap();
    let (mut t, path, _) = ready_to_post(&w);

    confirm(&t);
    let end = Instant::now() + LOAD;
    while w.posts() == 0 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(w.posts(), 1, "the write never started");
    t.send(b"q");
    t.wait_for("finishing the post to #101 before quitting", LOAD);
    let status = t.wait_exit(Duration::from_secs(15));
    assert!(status.success(), "{status:?}");

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        after["status"], "posted",
        "the post was cut off, or its result thrown away"
    );
    let raw = String::from_utf8_lossy(&t.raw()).to_string();
    assert!(
        raw.contains("rigor: posted to #101"),
        "nothing said how it went"
    );
    assert!(
        !path.with_file_name(".101.json.lock").exists(),
        "the lock outlived rigor"
    );
}

/// Words typed into the wrong window, one key at a time, half a second
/// apart: slowly enough that every key gets its frame and the floor is long
/// past. "today" and "okay" each ask, with their `a`, and take the question
/// back, with their `y`. Lowercase `y` is never the answer.
#[test]
fn words_typed_slowly_into_the_wrong_window_post_nothing() {
    let w = World::new();
    w.pull(101, "open", false, FIXTURE_HEAD_101);
    let (t, path, before) = ready_to_post(&w);

    for word in ["today", "okay"] {
        for key in word.bytes() {
            t.send(&[key]);
            if key == b'a' {
                // The question it asks is up, and stays up for the half
                // second before the next key.
                t.wait_for(ASKED, LOAD);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        t.wait("the question to go", LOAD, |s| !s.contains(ASKED));
    }

    // And the plain case: `a`, a long look, lowercase `y`.
    t.send(b"a");
    t.wait_for(ASKED, LOAD);
    t.wait_for("Y to post, esc to cancel", LOAD);
    std::thread::sleep(Duration::from_millis(900));
    t.send(b"y");
    t.wait("the question to go", LOAD, |s| !s.contains(ASKED));

    t.send(b"?");
    t.wait_for(" keys ", LOAD);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(w.calls(), [FETCH], "a word posted a review");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

/// A signal ends rigor the same way a key does, and gets the same care: the
/// post in flight is let finish and is recorded.
#[test]
fn a_signal_mid_post_lets_it_finish_and_records_it() {
    let w = World::new();
    w.pull(101, "open", false, FIXTURE_HEAD_101);
    std::fs::write(w.root().join("post.delay"), "2").unwrap();
    let (mut t, path, _) = ready_to_post(&w);

    confirm(&t);
    let end = Instant::now() + LOAD;
    while w.posts() == 0 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(w.posts(), 1, "the write never started");
    // SAFETY: kill(2) takes plain integers and touches no memory; the target is
    // the rigor this test started and still owns.
    unsafe { libc::kill(t.pid(), libc::SIGTERM) };
    let status = t.wait_exit(Duration::from_secs(15));
    assert!(status.success(), "{status:?}");

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(after["status"], "posted", "the post was cut off");
}

/// `gh` exited cleanly, but what GitHub sent back is not this review: the
/// pull request itself, as a request redirected and followed as a read would
/// bring, or a review that is somebody else's. The draft is not marked
/// posted on an exit status.
#[test]
fn an_answer_that_is_not_the_review_is_not_recorded_as_posted() {
    let answers = [
        serde_json::json!({
            "number": 101, "state": "open", "merged": false,
            "user": { "login": "hubot" }, "head": { "sha": FIXTURE_HEAD_101 }
        }),
        review(901, "hubot", "APPROVED", "Reads well."),
    ];
    for answer in answers {
        let w = World::new();
        w.pull(101, "open", false, FIXTURE_HEAD_101);
        std::fs::write(w.root().join("post.response.json"), answer.to_string()).unwrap();
        let (t, path, before) = ready_to_post(&w);

        confirm(&t);
        t.wait_for("whether it was posted is not known", LOAD);
        t.wait_line("#101 no longer a plain draft", LOAD, |l| {
            l.contains("#101") && l.contains("maybe sent")
        });
        assert_eq!(
            w.calls(),
            [
                FETCH.to_string(),
                pull_call(101),
                reviews_call(101, 1),
                post_call(101)
            ]
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "marked posted on gh's exit status alone: {answer}"
        );
    }
}

/// A draft for a pull request the fetch never returned is not stuck: the
/// prompt says the list cannot vouch for it, and the confirming read finds
/// its head matches and sends it.
#[test]
fn an_unverified_draft_whose_live_head_matches_is_posted() {
    let w = World::new();
    let path = w.draft(999, |_| {});
    w.pull(999, "open", false, FIXTURE_HEAD_101);
    let t = w.drafts_view();
    // The fetch has landed once the viewer is known; #999 is not in it.
    t.wait_for("@octocat", LOAD);
    t.wait_line("#999 unverified", LOAD, |l| {
        l.contains("#999") && l.contains("unverified")
    });

    t.send(b"a");
    t.wait_for(
        "#999? not fetched here, so GitHub is asked for its head first. Y to post",
        LOAD,
    );
    assert_eq!(w.calls(), [FETCH]);

    std::thread::sleep(Duration::from_millis(600));
    t.send(b"Y");
    t.wait_line("#999 posted", LOAD, |l| {
        l.contains("#999") && l.contains("✓ posted")
    });
    assert_eq!(
        w.calls(),
        [
            FETCH.to_string(),
            pull_call(999),
            reviews_call(999, 1),
            post_call(999)
        ]
    );
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(after["status"], "posted");
}

/// `e` steps out of the way for the editor, comes back with the whole screen
/// redrawn, and reads the file again — here, into nonsense.
#[test]
fn editing_a_draft_rereads_it_and_restores_the_screen() {
    let w = World::new();
    let path = w.draft(101, |_| {});
    let editor = w.root().join("bin/scribble");
    write_exe(
        &editor,
        "#!/bin/sh\necho \"$1\" >> \"$(cd \"$(dirname \"$0\")/..\" && pwd)/edited.log\"\necho nonsense > \"$1\"\n",
    );
    let t = w.drafts_view_with(&[("EDITOR", editor.to_str().unwrap())]);
    t.wait_line("#101 ready to post", LOAD, |l| {
        l.contains("#101") && l.contains("approve +1")
    });

    t.send(b"e");
    t.wait_line("#101 invalid", LOAD, |l| {
        l.contains("#101") && l.contains("invalid")
    });
    assert_eq!(w.log("edited.log"), [path.to_str().unwrap()]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "nonsense\n");

    // Back on the alternate screen with everything on it again, not only
    // the cells that changed. Waited for, not read at once: the row above can
    // arrive before the rest of the same frame has.
    t.wait("the whole screen to be redrawn", LOAD, |s| {
        s.contains("acme/widget") && s.contains("Drafts 1") && s.contains("? help")
    });
    let raw = t.raw();
    let count = |seq: &[u8]| raw.windows(seq.len()).filter(|w| *w == seq).count();
    assert_eq!(count(b"\x1b[?1049l"), 1, "left for the editor");
    assert_eq!(count(b"\x1b[?1049h"), 2, "and came back");
    // An invalid draft cannot be posted, and rigor is still listening.
    t.send(b"a");
    t.wait_for("#101 is invalid", LOAD);
    assert_eq!(w.calls(), [FETCH]);
}

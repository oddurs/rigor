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
/// from a fixture (or hangs, fails, or reports a rate limit) and records every
/// write it is asked to make, and a `git` shim that logs every call rigor
/// makes.
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

    /// Every write the fake `gh` was asked to make: its arguments, one call
    /// per line.
    fn posts(&self) -> Vec<String> {
        self.log("gh.posts")
    }

    /// The exact bytes the `n`th write was given on stdin, counting from 1.
    fn post_body(&self, n: usize) -> String {
        std::fs::read_to_string(self.root().join(format!("gh.post.{n}"))).unwrap()
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
    write_exe(
        &bin.join("gh"),
        r#"#!/bin/sh
dir="$(cd "$(dirname "$0")/.." && pwd)"
# A write is recorded whole — its arguments, and the body it was given on
# stdin — and is never answered from the fixture. Kept out of gh.log, which
# counts fetches.
case "$*" in
  *--method*)
    echo "$*" >> "$dir/gh.posts"
    cat > "$dir/gh.post.$(wc -l < "$dir/gh.posts" | tr -d ' ')"
    if [ "$(cat "$dir/gh.mode")" = fail ]; then
      echo "gh: Validation Failed (HTTP 422)" >&2
      exit 1
    fi
    printf '%s' '{"id":1,"state":"APPROVED"}'
    exit 0 ;;
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
"#,
    );
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
    std::fs::write(
        root.join("config.toml"),
        format!("open_command = \"{}\"\n", bin.join("open-log").display()),
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

/// The one write rigor makes to GitHub, end to end: the first `a` only asks,
/// the second sends exactly one request with exactly the review in it, and
/// only then is the file marked posted.
#[test]
fn a_confirmed_post_sends_one_review_and_marks_the_draft_posted() {
    let w = World::new();
    let path = w.draft(101, |_| {});
    let before: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let t = w.drafts_view();
    t.wait_line("#101 ready to post", LOAD, |l| {
        l.contains("#101") && l.contains("approve +1")
    });

    t.send(b"a");
    t.wait_for(
        "post approve with 1 inline comment to #101? a again to confirm, esc to cancel",
        LOAD,
    );
    assert!(w.posts().is_empty(), "one a sent it: {:?}", w.posts());

    t.send(b"a");
    t.wait_line("#101 posted", LOAD, |l| {
        l.contains("#101") && l.contains("✓ posted")
    });

    assert_eq!(
        w.posts(),
        ["api --method POST repos/acme/widget/pulls/101/reviews --input -"]
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
    assert_eq!(left, ["101.json"], "the rewrite left something behind");
}

/// A stray keypress sends nothing: `a` on its own only asks, and anything
/// that is not a second `a` withdraws the question.
#[test]
fn a_single_a_sends_nothing() {
    let w = World::new();
    let path = w.draft(101, |_| {});
    let before = std::fs::read_to_string(&path).unwrap();
    let t = w.drafts_view();
    t.wait_line("#101 ready to post", LOAD, |l| {
        l.contains("#101") && l.contains("approve +1")
    });
    let asked = |t: &Term| t.wait_for("a again to confirm", LOAD);
    let withdrawn = |t: &Term| {
        t.wait("the prompt to go", LOAD, |s| {
            !s.contains("a again to confirm")
        });
    };

    t.send(b"a");
    asked(&t);
    t.send(b"\x1b");
    withdrawn(&t);
    // After a cancel the next `a` is a first `a` again.
    t.send(b"a");
    asked(&t);
    t.send(b"j");
    withdrawn(&t);
    t.send(b"a");
    asked(&t);

    // Help only opens once every key before it has been handled.
    t.send(b"?");
    t.wait_for(" keys ", LOAD);
    std::thread::sleep(Duration::from_millis(300));
    assert!(w.posts().is_empty(), "{:?}", w.posts());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

/// Stale, unverified, invalid, skipped and already posted: `a`, twice, on
/// each sends nothing, says why, and leaves the file alone.
#[test]
fn drafts_that_must_not_be_posted_send_nothing() {
    let w = World::new();
    // The fixture has #102 at another commit, has never heard of #999, and
    // lists #90 as merged.
    let mut files = vec![
        (w.draft(102, |_| {}), "102", "#102 is stale"),
        (w.draft(999, |_| {}), "999", "#999 is unverified"),
        (w.draft(103, |_| {}), "103", "#103 is invalid"),
        (
            w.draft(101, |v| v["status"] = "skipped".into()),
            "101",
            "#101 is skipped",
        ),
        (
            w.draft(90, |v| {
                v["status"] = "posted".into();
                v["posted_at"] = "2027-01-15T09:30:00Z".into();
            }),
            "90",
            "#90 is already posted",
        ),
    ];
    std::fs::write(&files[2].0, "{ \"version\": 1,").unwrap();
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
        // matter of ordering.
        // Enter is a carriage return to a terminal in raw mode.
        t.send(format!("/{number}\r").as_bytes());
        t.wait("the filter to apply", LOAD, |s| {
            s.contains(&format!("/{number} ")) && s.contains("Drafts 5")
        });
        t.send(b"aa");
        t.wait_for(why, LOAD);
        t.send(b"\x1b");
        t.wait("the filter to clear", LOAD, |s| {
            !s.contains(&format!("/{number} "))
        });
    }

    t.send(b"?");
    t.wait_for(" keys ", LOAD);
    std::thread::sleep(Duration::from_millis(300));
    assert!(w.posts().is_empty(), "{:?}", w.posts());
    for ((path, number, _), text) in files.drain(..).zip(before) {
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            text,
            "#{number} was rewritten"
        );
    }
}

/// When `gh` refuses the review nothing went out, so nothing is recorded:
/// the file is byte for byte what it was and the row is still a draft.
#[test]
fn a_failing_gh_leaves_the_draft_untouched() {
    let w = World::new();
    let path = w.draft(101, |_| {});
    let before = std::fs::read_to_string(&path).unwrap();
    let t = w.drafts_view();
    t.wait_line("#101 ready to post", LOAD, |l| {
        l.contains("#101") && l.contains("approve +1")
    });
    w.gh("fail");
    t.send(b"aa");
    t.wait_for(
        "posting to #101 failed: gh api failed: gh: Validation Failed (HTTP 422)",
        LOAD,
    );
    assert_eq!(w.posts().len(), 1, "attempted once: {:?}", w.posts());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    t.wait_line("#101 still a draft", LOAD, |l| {
        l.contains("#101") && l.contains("approve +1")
    });
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
    t.send(b"aa");
    t.wait_for("#101 is invalid", LOAD);
    assert!(w.posts().is_empty());
}

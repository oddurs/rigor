//! End to end: the real binary, in a real pseudo-terminal, against a fake `gh`
//! and a throwaway git repository with real worktrees.
//!
//! These are the checks that were once run by hand — a stalled `gh`, quitting
//! mid-call, SIGTERM, the colour probe, lock-free git, focus — turned into tests
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
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const FIXTURE: &str = include_str!("fixtures/graphql_page.json");
const ROWS: u16 = 30;
const COLS: u16 = 160;

// ------------------------------------------------------------------ the world

/// A repository with a bare origin and two worktrees, a fake `gh` that answers
/// from a fixture (or hangs, fails, or reports a rate limit), and a `git` shim
/// that logs every call rigor makes.
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
    std::fs::write(root.join("fixture.json"), FIXTURE).unwrap();
    std::fs::write(root.join("gh.mode"), "ok").unwrap();
    write_exe(
        &bin.join("gh"),
        r#"#!/bin/sh
dir="$(cd "$(dirname "$0")/.." && pwd)"
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
    /// Answer OSC 10/11 like a terminal that reports its colours.
    colours: bool,
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
        let (screen2, raw2, input2, colours) =
            (screen.clone(), raw.clone(), input.clone(), opts.colours);
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
                // report their colours.
                if !answered && raw.windows(3).any(|w| w == b"\x1b[c") {
                    answered = true;
                    let mut reply = Vec::new();
                    if colours {
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

/// The selection band is mixed from the colours the terminal reports; a
/// terminal that reports nothing gets no band.
#[test]
fn the_selection_band_comes_from_the_terminals_own_colours() {
    let band = |colours: bool| {
        let w = World::new();
        let t = Term::start(
            &w,
            Opts {
                args: &["-v", "all"],
                colours,
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
        "no band from reported colours"
    );
    assert_eq!(
        band(false),
        vt100::Color::Default,
        "a band without reported colours"
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

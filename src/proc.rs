//! Running `git` and `gh` without letting either take rigor down with it.
//!
//! Every subprocess gets a deadline. Before this, a `gh` call that stalled —
//! a connection left half-open across sleep and wake is the usual cause — held
//! the refresh flag forever and the dashboard silently stopped updating. Now a
//! stalled call is killed, reported, and the next refresh goes ahead.

use anyhow::{Context, Result, bail};
use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Process groups of children still running. Each child gets its own group so a
/// timeout can kill everything it spawned — which also means children no longer
/// die with the terminal, so rigor has to take them down itself on exit.
static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Kill every child still in flight. Called on the way out, so quitting in the
/// middle of a stalled fetch leaves nothing running behind.
pub fn kill_all() {
    let live = std::mem::take(&mut *LIVE.lock().unwrap_or_else(|e| e.into_inner()));
    for pgid in live {
        signal_group(pgid);
    }
}

fn signal_group(pgid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pgid as i32), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = pgid;
}

fn forget(pgid: u32) {
    LIVE.lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|p| *p != pgid);
}

/// How a subprocess should be treated while it runs.
#[derive(Clone, Copy)]
pub enum Priority {
    /// Something the user is waiting on.
    Normal,
    /// Background bookkeeping: yields the CPU to the user and their agents.
    Background,
}

pub fn run(cmd: &mut Command, timeout: Duration, priority: Priority) -> Result<Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group, so a timeout can take down anything the child
        // spawned too — `gh` shells out to helpers, and a surviving grandchild
        // would hold the pipes open.
        cmd.process_group(0);
        if matches!(priority, Priority::Background) {
            // setpriority is async-signal-safe, which is what pre_exec requires.
            unsafe {
                cmd.pre_exec(|| {
                    libc::setpriority(libc::PRIO_PROCESS, 0, 10);
                    Ok(())
                });
            }
        }
    }
    #[cfg(not(unix))]
    let _ = priority;

    let program = cmd.get_program().to_string_lossy().to_string();
    let mut child = cmd
        .spawn()
        .with_context(|| format!("could not run `{program}`"))?;
    let pgid = child.id();
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).push(pgid);
    let mut stdout = child.stdout.take().context("no stdout pipe")?;
    let mut stderr = child.stderr.take().context("no stderr pipe")?;
    // Drain both pipes while waiting, so a chatty child never blocks on a full one.
    let out = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stdout.read_to_end(&mut v);
        v
    });
    let err = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stderr.read_to_end(&mut v);
        v
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                forget(pgid);
                return Err(e.into());
            }
        }
        if Instant::now() >= deadline {
            kill_group(&mut child);
            forget(pgid);
            bail!("timed out after {}s", timeout.as_secs().max(1));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    forget(pgid);

    Ok(Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

fn kill_group(child: &mut std::process::Child) {
    signal_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quick_command_returns_its_output() {
        let out = run(
            Command::new("sh").args(["-c", "echo hi; echo oops >&2"]),
            Duration::from_secs(5),
            Priority::Normal,
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\n");
        assert_eq!(String::from_utf8_lossy(&out.stderr), "oops\n");
    }

    #[test]
    fn a_stalled_command_is_killed_at_the_deadline() {
        let start = Instant::now();
        let err = run(
            Command::new("sleep").arg("30"),
            Duration::from_millis(200),
            Priority::Normal,
        )
        .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "took {:?}",
            start.elapsed()
        );
    }

    /// Quitting mid-call must not leave the call running.
    #[test]
    fn kill_all_ends_calls_still_in_flight() {
        let h = std::thread::spawn(|| {
            run(
                Command::new("sh").args(["-c", "sleep 30; true"]),
                Duration::from_secs(60),
                Priority::Background,
            )
        });
        std::thread::sleep(Duration::from_millis(300));
        let start = Instant::now();
        kill_all();
        let res = h.join().unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "call survived kill_all"
        );
        assert!(res.map(|o| !o.status.success()).unwrap_or(true));
    }

    /// The case that matters in practice: the stalled process is a grandchild
    /// holding the pipes open. Killing only the child would leave `run` blocked
    /// on the pipe until the grandchild exits on its own.
    #[test]
    fn a_stalled_grandchild_does_not_outlive_the_deadline() {
        let start = Instant::now();
        let err = run(
            Command::new("sh").args(["-c", "sleep 30; true"]),
            Duration::from_millis(200),
            Priority::Background,
        )
        .unwrap_err();
        assert!(err.to_string().contains("timed out"));
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "took {:?}",
            start.elapsed()
        );
    }
}

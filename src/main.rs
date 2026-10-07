//! rigor — a terminal dashboard for the pull requests you have out on a repo.

mod app;
mod config;
mod drafts;
mod event;
mod git;
mod github;
mod model;
mod probe;
mod proc;
mod schedule;
mod theme;
mod ui;
mod util;

use anyhow::{Context, Result};
use clap::Parser;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyModifiers, poll, read,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use std::ffi::OsString;
use std::io::{Stdout, Write, stdout};
use std::path::{Path, PathBuf};
use std::time::Duration;

use app::App;
use config::View;

#[derive(Parser, Debug)]
#[command(
    name = "rigor",
    about = "Dashboard for the pull requests you have out on a repo",
    version
)]
struct Cli {
    /// Repository directory to inspect (defaults to the working directory)
    path: Option<PathBuf>,

    /// Override repo detection, as owner/name
    #[arg(short = 'R', long)]
    repo: Option<String>,

    /// Start on this view: ready, mine, review, assigned, blocked, all,
    /// worktrees or drafts
    #[arg(short = 'v', long, value_parser = parse_view)]
    view: Option<View>,

    /// Background refresh interval in seconds (0 disables)
    #[arg(short = 'i', long)]
    refresh: Option<u64>,

    /// Read this config file instead of the user one
    #[arg(long)]
    config: Option<PathBuf>,

    /// Theme file to layer on top of the configured colors
    #[arg(long)]
    theme: Option<PathBuf>,

    /// Disable mouse capture (lets the terminal handle selection itself)
    #[arg(long)]
    no_mouse: bool,

    /// Write a starter config file and exit
    #[arg(long)]
    init_config: bool,

    /// Print the resolved configuration and exit
    #[arg(long)]
    print_config: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.init_config {
        return init_config();
    }

    let start = match &cli.path {
        Some(p) => p.clone(),
        None => std::env::current_dir()?,
    };

    let mut repo = git::discover(&start, cli.repo.as_deref())?;
    let mut settings = config::load(&repo.root, cli.config.as_deref())?;

    if let Some(v) = cli.view {
        // Asked for by name, so it gets a tab even if config leaves it off the bar.
        if !settings.views.contains(&v) {
            settings.views.push(v);
        }
        settings.default_view = v;
    }
    if let Some(r) = cli.refresh {
        settings.refresh_secs = r;
    }
    if cli.no_mouse {
        settings.mouse = false;
    }

    // Resolved here so a bad color fails before the terminal is taken over,
    // and so --print-config never writes a query to the tty.
    theme::Theme::resolve(
        &settings.theme,
        cli.theme.as_deref(),
        probe::Probed::default(),
    )?;

    if cli.print_config {
        print_config(&repo, &settings);
        return Ok(());
    }

    // `discover` guesses `main`; the real default branch arrives with the first fetch.
    repo.default_branch = "main".into();

    let mouse = settings.mouse;
    let (mut term, probed) = setup(mouse)?;
    let theme = match theme::Theme::resolve(&settings.theme, cli.theme.as_deref(), probed) {
        Ok(t) => t,
        Err(e) => {
            restore(&mut term, mouse);
            return Err(e);
        }
    };
    let (mut a, rx) = App::new(repo, settings, theme);
    a.drafts_dir = drafts::dir(|key| std::env::var_os(key));
    a.remote = std::sync::Arc::new(github::Gh {
        owner: a.repo.owner.clone(),
        name: a.repo.name.clone(),
    });
    // The loop's result goes straight through `leave`, however it ended, so
    // that a post in flight is let finish before `proc::shutdown` would kill
    // `gh` in the middle of one.
    let ended = run(&mut term, &mut a, &rx);
    let limit = github::gh_timeout() + QUIT_MARGIN;
    let (result, said) = a.leave(ended, &rx, limit, |a| {
        let _ = term.draw(|f| ui::draw(f, a));
    });
    if said.is_some() {
        discard_typed_ahead();
    }
    restore(&mut term, mouse);
    proc::shutdown();
    // The screen that would have said how the post went is gone, so say it
    // where it can still be read. If that cannot be written either, the file
    // already has been.
    if let Some(said) = said {
        let _ = writeln!(std::io::stderr(), "rigor: {said}");
    }
    result
}

/// How much longer than one call to GitHub the way out will wait for a post
/// in flight, all told.
const QUIT_MARGIN: Duration = Duration::from_secs(5);

/// Keys struck while rigor waited on its way out were for rigor, not for the
/// shell it is about to hand the terminal back to. Dropped unread.
fn discard_typed_ahead() {
    #[cfg(unix)]
    // SAFETY: tcflush(3) takes a file descriptor and a constant, touches no
    // memory, and only discards input not yet read.
    unsafe {
        libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH);
    }
}

fn parse_view(name: &str) -> Result<View, String> {
    View::parse(name).ok_or_else(|| format!("not a view; try {}", View::names()))
}

fn run(
    term: &mut Terminal<CrosstermBackend<Stdout>>,
    a: &mut App,
    rx: &std::sync::mpsc::Receiver<app::Msg>,
) -> Result<()> {
    let mut input = event::Input::default();
    let stop = stop_on_signal();
    a.refresh();

    loop {
        term.draw(|f| ui::draw(f, a))?;

        // Short enough that a SIGTERM or SIGHUP is honored well inside the
        // grace period a terminal or supervisor gives before SIGKILL — which
        // would orphan any git or gh call still in flight. Idle, this costs
        // about 0.2% of a core; input returns at once regardless.
        if poll(Duration::from_millis(100))? {
            match read()? {
                Event::Key(k) => {
                    // Ctrl-C always quits, even mid-filter.
                    if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                        a.quit = true;
                    } else {
                        event::key(a, k);
                    }
                }
                Event::Mouse(m) => input.mouse(a, m),
                Event::Paste(text) => event::paste(a, &text),
                Event::FocusGained => a.set_focus(true),
                Event::FocusLost => a.set_focus(false),
                Event::Resize(..) => {}
            }
        }

        if let Some(path) = a.edit_request.take() {
            edit(term, a, &path)?;
        }

        while let Ok(msg) = rx.try_recv() {
            a.on_msg(msg);
        }

        a.spinner = a.spinner.wrapping_add(1);
        a.tick();

        if a.quit || stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
    }
}

/// SIGTERM, SIGHUP or an external SIGINT ends the loop the ordinary way, so the
/// terminal is restored instead of being left in raw mode on the alternate
/// screen. (Ctrl-C inside rigor arrives as a key, not a signal.)
fn stop_on_signal() -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    #[cfg(unix)]
    for sig in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGINT,
    ] {
        let _ = signal_hook::flag::register(sig, std::sync::Arc::clone(&flag));
    }
    flag
}

fn setup(mouse: bool) -> Result<(Terminal<CrosstermBackend<Stdout>>, probe::Probed)> {
    enable_raw_mode()?;
    // In raw mode and before the event loop owns stdin, so the terminal's
    // replies are read here rather than arriving as keystrokes.
    let probed = probe::query(Duration::from_millis(250));
    let mut out = stdout();
    // ratatui's first draw only writes cells that differ from its blank start
    // buffer, so anything already on screen would survive wherever the frame is
    // blank. Clear here rather than with `Terminal::clear`, which first asks the
    // terminal for its cursor position and fails on any host that won't answer.
    // Bracketed paste, so that pasted text arrives as a paste and not as a
    // run of keypresses that could ask to post and then confirm.
    execute!(
        out,
        EnterAlternateScreen,
        Clear(ClearType::All),
        EnableFocusChange,
        EnableBracketedPaste
    )?;
    if mouse {
        execute!(out, EnableMouseCapture)?;
    }

    // Leave the terminal usable if the UI thread panics. Background workers
    // catch their own panics and report them as errors, so a panic there must
    // not tear the screen down — or print over it — while rigor keeps running.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().name() != Some("main") {
            return;
        }
        let mut out = stdout();
        let _ = execute!(
            out,
            DisableBracketedPaste,
            DisableFocusChange,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
        hook(info);
    }));

    Ok((Terminal::new(CrosstermBackend::new(out))?, probed))
}

/// Best effort throughout: after a SIGHUP the terminal may already be gone,
/// and failing to write to it is not an error worth reporting.
fn restore(term: &mut Terminal<CrosstermBackend<Stdout>>, mouse: bool) {
    if mouse {
        let _ = execute!(term.backend_mut(), DisableMouseCapture);
    }
    let _ = execute!(
        term.backend_mut(),
        DisableBracketedPaste,
        DisableFocusChange,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
    let _ = term.show_cursor();
}

/// Take the terminal back after `restore`, for the editor's return.
fn resume(term: &mut Terminal<CrosstermBackend<Stdout>>, mouse: bool) -> Result<()> {
    enable_raw_mode()?;
    execute!(
        term.backend_mut(),
        EnterAlternateScreen,
        Clear(ClearType::All),
        EnableFocusChange,
        EnableBracketedPaste
    )?;
    if mouse {
        execute!(term.backend_mut(), EnableMouseCapture)?;
    }
    term.hide_cursor()?;
    // The editor drew over every cell, and ratatui repaints only what changed
    // since its own last frame. `resize` starts it from a blank one — and,
    // unlike `Terminal::clear`, without asking the terminal for its cursor.
    let size = term.size()?;
    term.resize(size.into())?;
    Ok(())
}

/// `e` on a draft: hand the terminal to the user's editor for that one file,
/// take it back, and read the file again — whatever it now says.
fn edit(term: &mut Terminal<CrosstermBackend<Stdout>>, a: &mut App, path: &Path) -> Result<()> {
    let mouse = a.settings.mouse;
    let command = editor(|key| std::env::var_os(key));
    restore(term, mouse);
    let ran = run_editor(&command, path);
    resume(term, mouse)?;
    match ran {
        Ok(status) if status.success() => {}
        Ok(status) => a.note(format!("`{command}` ended with {status}")),
        Err(e) => a.note(format!("could not run `{command}`: {e}")),
    }
    a.reload_drafts();
    Ok(())
}

/// `$VISUAL`, else `$EDITOR`, else `vi`. A variable set to nothing counts as
/// unset.
fn editor(env: impl Fn(&str) -> Option<OsString>) -> String {
    ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(env)
        .map(|v| v.to_string_lossy().trim().to_string())
        .find(|v| !v.is_empty())
        .unwrap_or_else(|| "vi".into())
}

/// Split into words like `open_command`, with the file as the last argument.
/// The editor gets the terminal itself: rigor has stepped out of the way.
fn run_editor(command: &str, path: &Path) -> std::io::Result<std::process::ExitStatus> {
    let mut parts = command.split_whitespace();
    let bin = parts
        .next()
        .ok_or_else(|| std::io::Error::other("the command is empty"))?;
    std::process::Command::new(bin)
        .args(parts)
        .arg(path)
        .status()
}

fn init_config() -> Result<()> {
    let path = config::user_config_path().context("could not work out a config directory")?;
    if path.exists() {
        println!("{} already exists, leaving it alone", path.display());
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, config::SAMPLE)?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Values are spelled the way a config file spells them, so a line can be
/// copied straight into one.
fn print_config(repo: &model::RepoInfo, s: &config::Settings) {
    let views: Vec<String> = s.views.iter().map(|v| v.name()).collect();
    let mut rows = vec![
        ("repo", repo.slug()),
        ("root", repo.root.display().to_string()),
        ("default_view", s.default_view.name()),
        ("views", views.join(", ")),
        ("refresh_secs", s.refresh_secs.to_string()),
        ("layout", s.layout.name().to_string()),
        ("max_prs", s.max_prs.to_string()),
        ("show_drafts", s.show_drafts.to_string()),
        ("mouse", s.mouse.to_string()),
        ("worktree_status", s.worktree_status.to_string()),
        ("worktree_scan_secs", s.worktree_scan_secs.to_string()),
        ("open_command", s.open_command.clone()),
        ("copy_command", s.copy_command.clone()),
    ];
    if s.sources.is_empty() {
        rows.push(("config file", "(none; using defaults)".into()));
    }
    rows.extend(
        s.sources
            .iter()
            .map(|p| ("config file", p.display().to_string())),
    );
    // A closed pipe (`rigor --print-config | head -1`) is not an error.
    let mut o = stdout().lock();
    for (key, value) in rows {
        let _ = writeln!(o, "{key:<18} {value}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    /// `--view`'s help lists its values by hand; this keeps it honest.
    #[test]
    fn view_help_names_every_view() {
        let help = Cli::command().render_long_help().to_string();
        for v in View::ALL {
            assert!(
                help.contains(&v.name()),
                "--help does not mention {}",
                v.name()
            );
        }
        assert!(parse_view("nope").unwrap_err().contains("worktrees"));
        assert_eq!(parse_view("Ready"), Ok(View::Ready));
    }

    #[test]
    fn the_editor_is_visual_then_editor_then_vi() {
        let pick = |vars: &[(&str, &str)]| {
            editor(|key| {
                vars.iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| OsString::from(v))
            })
        };
        assert_eq!(pick(&[("VISUAL", "hx"), ("EDITOR", "nano")]), "hx");
        assert_eq!(pick(&[("EDITOR", "nano -w")]), "nano -w");
        assert_eq!(pick(&[("VISUAL", " "), ("EDITOR", "nano")]), "nano");
        assert_eq!(pick(&[]), "vi");
    }
}

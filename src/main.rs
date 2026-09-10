//! rigor — a terminal dashboard for the pull requests you have out on a repo.

mod app;
mod config;
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
    DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event, KeyCode,
    KeyModifiers, poll, read,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use std::io::{Stdout, Write, stdout};
use std::path::PathBuf;
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

    /// Start on this view: mine, review, assigned, all, worktrees
    #[arg(short = 'v', long)]
    view: Option<String>,

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

    if let Some(v) = cli.view.as_deref() {
        settings.default_view =
            View::parse(v).context("--view must be mine, review, assigned, all or worktrees")?;
    }
    if let Some(r) = cli.refresh {
        settings.refresh_secs = r;
    }
    if cli.no_mouse {
        settings.mouse = false;
    }

    // Resolved here so a bad colour fails before the terminal is taken over,
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
    let result = run(&mut term, &mut a, &rx);
    restore(&mut term, mouse);
    proc::shutdown();
    result
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

        // Short enough that a SIGTERM or SIGHUP is honoured well inside the
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
                Event::FocusGained => a.set_focus(true),
                Event::FocusLost => a.set_focus(false),
                _ => {}
            }
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
    execute!(
        out,
        EnterAlternateScreen,
        Clear(ClearType::All),
        EnableFocusChange
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
    let _ = execute!(term.backend_mut(), DisableFocusChange, LeaveAlternateScreen);
    let _ = disable_raw_mode();
    let _ = term.show_cursor();
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

fn print_config(repo: &model::RepoInfo, s: &config::Settings) {
    let mut o = stdout();
    let _ = writeln!(o, "repo            {}", repo.slug());
    let _ = writeln!(o, "root            {}", repo.root.display());
    let _ = writeln!(
        o,
        "default_view    {}",
        s.default_view.title().to_lowercase()
    );
    let _ = writeln!(o, "refresh_secs    {}", s.refresh_secs);
    let _ = writeln!(o, "layout          {:?}", s.layout);
    let _ = writeln!(o, "max_prs         {}", s.max_prs);
    let _ = writeln!(o, "show_drafts     {}", s.show_drafts);
    let _ = writeln!(o, "mouse           {}", s.mouse);
    let _ = writeln!(o, "worktree_status {}", s.worktree_status);
    let _ = writeln!(o, "worktree_scan_secs {}", s.worktree_scan_secs);
    let _ = writeln!(o, "open_command    {}", s.open_command);
    let _ = writeln!(o, "copy_command    {}", s.copy_command);
    if s.sources.is_empty() {
        let _ = writeln!(o, "config files    (none; using defaults)");
    } else {
        for p in &s.sources {
            let _ = writeln!(o, "config file     {}", p.display());
        }
    }
}

//! rigor — a terminal dashboard for the pull requests you have out on a repo.

mod app;
mod config;
mod event;
mod git;
mod github;
mod model;
mod theme;
mod ui;
mod util;

use anyhow::{Context, Result};
use clap::Parser;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, poll, read,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use std::io::{Stdout, Write, stdout};
use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use app::App;
use config::View;
use util::now_secs;

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
    if let Some(p) = &cli.theme {
        // A CLI theme is handed to the resolver the same way a parent shell would.
        unsafe { std::env::set_var("RIGOR_THEME", p) };
    }

    let theme = theme::Theme::resolve(&settings.theme)?;

    if cli.print_config {
        print_config(&repo, &settings);
        return Ok(());
    }

    // `discover` guesses `main`; the real default branch arrives with the first fetch.
    repo.default_branch = "main".into();

    let mouse = settings.mouse;
    let (mut a, rx) = App::new(repo, settings, theme);
    let mut term = setup(mouse)?;
    let result = run(&mut term, &mut a, rx);
    restore(&mut term, mouse)?;
    result
}

fn run(
    term: &mut Terminal<CrosstermBackend<Stdout>>,
    a: &mut App,
    rx: std::sync::mpsc::Receiver<app::Msg>,
) -> Result<()> {
    let mut input = event::Input::default();
    a.refresh();

    loop {
        term.draw(|f| ui::draw(f, a))?;

        if poll(Duration::from_millis(120))? {
            match read()? {
                Event::Key(k) => {
                    // Ctrl-C always quits, even mid-filter.
                    if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                        a.quit = true;
                    } else {
                        input.key(a, k);
                    }
                }
                Event::Mouse(m) => input.mouse(a, m),
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        loop {
            match rx.try_recv() {
                Ok(msg) => a.on_msg(msg),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        a.spinner = a.spinner.wrapping_add(1);

        let every = a.settings.refresh_secs;
        if every > 0 && now_secs() - a.last_refresh >= every as i64 {
            a.refresh();
        }

        if a.quit {
            return Ok(());
        }
    }
}

fn setup(mouse: bool) -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    if mouse {
        execute!(out, EnableMouseCapture)?;
    }

    // Leave the terminal usable if we panic mid-draw.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = stdout();
        let _ = execute!(out, DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
        hook(info);
    }));

    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

fn restore(term: &mut Terminal<CrosstermBackend<Stdout>>, mouse: bool) -> Result<()> {
    if mouse {
        execute!(term.backend_mut(), DisableMouseCapture)?;
    }
    execute!(term.backend_mut(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    term.show_cursor()?;
    Ok(())
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

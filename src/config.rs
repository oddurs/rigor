//! Layered configuration: built-in defaults, user config, repo-local config,
//! then environment and CLI overrides.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::theme::ThemeConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Ready,
    Mine,
    Review,
    Assigned,
    Blocked,
    All,
    Worktrees,
}

impl View {
    /// Every view that exists, for config validation and tests.
    pub const ALL: [View; 7] = [
        View::Ready,
        View::Mine,
        View::Review,
        View::Assigned,
        View::Blocked,
        View::All,
        View::Worktrees,
    ];

    /// The tabs shown when config says nothing. Ready leads because merging what
    /// is already green is the job this dashboard exists to do.
    pub const DEFAULT: [View; 6] = [
        View::Ready,
        View::Mine,
        View::Review,
        View::Blocked,
        View::All,
        View::Worktrees,
    ];

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ready" | "mergeable" => Some(View::Ready),
            "mine" | "authored" => Some(View::Mine),
            "review" | "reviews" => Some(View::Review),
            "assigned" => Some(View::Assigned),
            "blocked" | "stuck" => Some(View::Blocked),
            "all" => Some(View::All),
            "worktrees" | "agents" => Some(View::Worktrees),
            _ => None,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            View::Ready => "Ready",
            View::Mine => "Mine",
            View::Review => "Review",
            View::Assigned => "Assigned",
            View::Blocked => "Blocked",
            View::All => "All",
            View::Worktrees => "Worktrees",
        }
    }

    pub fn empty_hint(self) -> &'static str {
        match self {
            View::Ready => "Nothing is ready to merge right now.",
            View::Mine => "No open PRs authored by you in this repo.",
            View::Review => "Nothing is waiting on your review.",
            View::Assigned => "No open PRs are assigned to you.",
            View::Blocked => "Nothing is blocked — no red CI, conflicts or requested changes.",
            View::All => "No open PRs in this repo.",
            View::Worktrees => "No git worktrees found.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Auto,
    Split,
    Stack,
}

impl LayoutMode {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "split" | "side" | "horizontal" => Some(Self::Split),
            "stack" | "vertical" => Some(Self::Stack),
            _ => None,
        }
    }
}

/// The on-disk shape. Everything optional so files can set only what they mean to.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub default_view: Option<String>,
    pub views: Option<Vec<String>>,
    pub refresh_secs: Option<u64>,
    pub layout: Option<String>,
    pub max_prs: Option<usize>,
    pub show_drafts: Option<bool>,
    pub mouse: Option<bool>,
    pub worktree_status: Option<bool>,
    pub worktree_scan_secs: Option<u64>,
    pub open_command: Option<String>,
    pub copy_command: Option<String>,
    pub theme: Option<ThemeConfig>,
}

impl ConfigFile {
    fn merge(&mut self, other: ConfigFile) {
        macro_rules! take {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f; } )* };
        }
        take!(
            default_view,
            views,
            refresh_secs,
            layout,
            max_prs,
            show_drafts,
            mouse,
            worktree_status,
            worktree_scan_secs,
            open_command,
            copy_command
        );
        if let Some(t) = other.theme {
            match self.theme.as_mut() {
                Some(base) => base.merge(t),
                None => self.theme = Some(t),
            }
        }
    }
}

/// Fully resolved settings the app actually runs on.
#[derive(Debug, Clone)]
pub struct Settings {
    pub default_view: View,
    /// The tabs, in the order they are shown. Number keys index this list.
    pub views: Vec<View>,
    pub refresh_secs: u64,
    pub layout: LayoutMode,
    pub max_prs: usize,
    pub show_drafts: bool,
    pub mouse: bool,
    pub worktree_status: bool,
    /// How often every worktree is rescanned for unstaged edits. Between full
    /// scans only desks whose git state moved are rescanned. 0: only on `r`.
    pub worktree_scan_secs: u64,
    pub open_command: String,
    pub copy_command: String,
    pub theme: ThemeConfig,
    pub sources: Vec<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_view: View::Ready,
            views: View::DEFAULT.to_vec(),
            refresh_secs: 90,
            layout: LayoutMode::Auto,
            max_prs: 200,
            show_drafts: true,
            mouse: true,
            worktree_status: true,
            worktree_scan_secs: 300,
            open_command: default_open().into(),
            copy_command: default_copy().into(),
            theme: ThemeConfig::default(),
            sources: Vec::new(),
        }
    }
}

fn default_open() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    }
}

fn default_copy() -> &'static str {
    if cfg!(target_os = "macos") {
        "pbcopy"
    } else {
        "xclip -selection clipboard"
    }
}

/// `$RIGOR_CONFIG`, else `$XDG_CONFIG_HOME/rigor/config.toml`, else `~/.config/rigor/config.toml`.
pub fn user_config_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RIGOR_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("rigor").join("config.toml"))
}

fn read(path: &Path) -> Result<Option<ConfigFile>> {
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: ConfigFile =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(cfg))
}

/// Load user config, then `<repo>/.rigor.toml` on top of it.
pub fn load(repo_root: &Path, explicit: Option<&Path>) -> Result<Settings> {
    let mut merged = ConfigFile::default();
    let mut sources = Vec::new();

    let user_path = match explicit {
        Some(p) => Some(p.to_path_buf()),
        None => user_config_path(),
    };
    if let Some(p) = user_path
        && let Some(c) = read(&p)?
    {
        merged.merge(c);
        sources.push(p);
    }

    let local = repo_root.join(".rigor.toml");
    if let Some(c) = read(&local)? {
        merged.merge(c);
        sources.push(local);
    }

    let mut s = Settings::default();
    if let Some(names) = &merged.views {
        let mut views: Vec<View> = Vec::new();
        for n in names {
            let v = View::parse(n).with_context(|| {
                let names: Vec<&str> = View::ALL.iter().map(|v| v.title()).collect();
                format!(
                    "views: `{n}` is not a view name (try {})",
                    names.join(", ").to_lowercase()
                )
            })?;
            if !views.contains(&v) {
                views.push(v);
            }
        }
        if !views.is_empty() {
            s.views = views;
        }
    }
    if let Some(v) = merged.default_view.as_deref().and_then(View::parse) {
        s.default_view = v;
    }
    // Landing on a tab that is not on the tab bar would leave the bar with
    // nothing highlighted, so fall back to the first configured view.
    if !s.views.contains(&s.default_view) {
        s.default_view = s.views[0];
    }
    if let Some(v) = merged.refresh_secs {
        s.refresh_secs = v;
    }
    if let Some(v) = merged.layout.as_deref().and_then(LayoutMode::parse) {
        s.layout = v;
    }
    if let Some(v) = merged.max_prs {
        s.max_prs = v.clamp(1, 1000);
    }
    if let Some(v) = merged.show_drafts {
        s.show_drafts = v;
    }
    if let Some(v) = merged.mouse {
        s.mouse = v;
    }
    if let Some(v) = merged.worktree_status {
        s.worktree_status = v;
    }
    if let Some(v) = merged.worktree_scan_secs {
        s.worktree_scan_secs = v;
    }
    if let Some(v) = merged.open_command {
        s.open_command = v;
    }
    if let Some(v) = merged.copy_command {
        s.copy_command = v;
    }
    if let Some(t) = merged.theme {
        s.theme.merge(t);
    }
    s.sources = sources;
    Ok(s)
}

/// A commented starter config, written by `rigor --init-config`.
pub const SAMPLE: &str = r##"# rigor — PR dashboard configuration
# Also read from <repo>/.rigor.toml, which overrides this file.

default_view    = "ready"  # any name from `views` below
# The tab bar, in order. Number keys 1-9 select them positionally.
# Available: ready | mine | review | assigned | blocked | all | worktrees
views = ["ready", "mine", "review", "blocked", "all", "worktrees"]
refresh_secs    = 90       # 0 disables background refresh
layout          = "auto"   # auto | split (side-by-side) | stack (list over detail)
max_prs         = 200
show_drafts     = true
mouse           = true
worktree_status = true     # off skips per-worktree git status entirely
# Every refresh rescans only worktrees whose git state moved (a commit, staging,
# a checkout). This is how often all of them are rescanned, to catch unstaged
# edits too. 0 rescans everything only when you press r.
worktree_scan_secs = 300
# open_command  = "open"
# copy_command  = "pbcopy"

# Colors are inherited from the terminal by default, so rigor picks up whatever
# theme its parent (herdr, tmux, the terminal itself) is running. Set any of
# these to override: an ANSI name ("cyan"), a 256-color index ("117"), or a hex
# value ("#7dd3fc"). "inherit" hands the slot back to the terminal.
[theme]
# accent  = "cyan"
# success = "green"
# failure = "red"
# pending = "yellow"
# muted   = "darkgray"
# fg      = "inherit"
# bg      = "inherit"

# The selected row's band and the hairlines are mixed from the colours your
# terminal reports for itself, so they sit inside your theme. Terminals that
# do not report them get no band: an accent bar and a bolder line instead.
# Set either to pin it:
# sel_bg  = "#2b2d31"
# border  = "8"        # ANSI bright-black, i.e. whatever your theme calls it
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    /// A repository's `.rigor.toml` overrides the user config field by field;
    /// anything neither sets keeps its default.
    #[test]
    fn repo_config_overrides_user_config_field_by_field() {
        let d = tempfile::tempdir().unwrap();
        let user = write(
            d.path(),
            "user.toml",
            "refresh_secs = 30\nlayout = \"stack\"\n[theme]\naccent = \"cyan\"\nsuccess = \"green\"\n",
        );
        write(
            d.path(),
            ".rigor.toml",
            "refresh_secs = 120\n[theme]\naccent = \"#7dd3fc\"\n",
        );
        let s = load(d.path(), Some(&user)).unwrap();
        assert_eq!(s.refresh_secs, 120, "the repo wins");
        assert_eq!(s.layout, LayoutMode::Stack, "the user value survives");
        assert_eq!(s.theme.accent.as_deref(), Some("#7dd3fc"));
        assert_eq!(
            s.theme.success.as_deref(),
            Some("green"),
            "theme merges per slot"
        );
        assert_eq!(s.worktree_scan_secs, 300, "untouched fields keep defaults");
        assert_eq!(s.sources.len(), 2);
    }

    #[test]
    fn views_are_validated_and_the_default_view_must_be_on_the_bar() {
        let d = tempfile::tempdir().unwrap();
        let ok = write(
            d.path(),
            "a.toml",
            "views = [\"mine\", \"all\", \"mine\"]\ndefault_view = \"ready\"\n",
        );
        let s = load(d.path(), Some(&ok)).unwrap();
        assert_eq!(
            s.views,
            vec![View::Mine, View::All],
            "deduplicated, in order"
        );
        assert_eq!(s.default_view, View::Mine, "falls back to the first tab");

        let bad = write(d.path(), "b.toml", "views = [\"mine\", \"nope\"]\n");
        let err = load(d.path(), Some(&bad)).unwrap_err().to_string();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn unknown_keys_are_an_error_not_silently_ignored() {
        let d = tempfile::tempdir().unwrap();
        let typo = write(d.path(), "c.toml", "refresh_sec = 30\n");
        assert!(
            load(d.path(), Some(&typo)).is_err(),
            "a typo must not be silently ignored"
        );
    }
}

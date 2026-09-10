# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Six pull request views — `Ready`, `Mine`, `Review`, `Blocked`, `All`,
  `Worktrees` — selected by number key, `Tab`, or a click. The tab bar is
  configurable via `views` in `config.toml`.
- Rolled-up CI state with counts per row, and a detail pane listing every check
  run with its state, duration, and a clickable link to the job. Repeated runs
  of the same check are deduplicated to the newest; skipped jobs are counted
  separately from passing ones.
- Worktree awareness: each worktree is mapped to its branch and pull request,
  with uncommitted and unpushed counts. A worktree whose branch has merged and
  whose tree is clean is reported as removable.
- Theming from the terminal's own ANSI palette, overridable per slot in
  `config.toml`, `.rigor.toml`, `RIGOR_THEME`, or `HERDR_THEME_FILE`. `NO_COLOR`
  is honoured.
- Mouse support: click a tab, a row, or a check run; scroll either pane.
- `site/`: a single-page landing and docs site, built with Next.js and StyleX
  and exported to static HTML. Its hero shows a frame captured from the
  renderer's own test fixtures.

# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- The top of the screen is now a top nav and a subnav. The nav carries a
  `rigor` badge and a `repo › branch` breadcrumb, and sheds the branch, then the
  user, then the status as the terminal narrows. The subnav's tabs show a title
  and a count only — key digits no longer sit beside counts — with Ready and
  Blocked counts coloured when non-zero. A rail beneath underlines the active
  view and joins the detail pane's divider. Number keys are listed in the footer.
- The nav drops its filled badge: `rigor` takes the accent beside a hairline,
  and the repository is the one bold thing on the line. Sync status is a dot —
  green when fresh, amber when stale, red when the last sync failed.
- The tab underline hugs the label, and an active filter shows in the subnav
  instead of the footer.
- The selected row is a full-width band mixed from the terminal's own reported
  colours (OSC 10/11), with a slim accent bar at its edge. Hairlines are mixed
  the same way. Terminals that do not report their colours keep the bar and a
  bolder line, with no band.
- A pass over the rest of the interface. The list drops the review glyph for
  "review required" — the resting state of nearly every PR — keeping only
  approved, changes requested and conflicts; mutes passing CI counts so failures
  stand out; right-aligns ages; hides the author in Mine; and shows your
  position on the rail when the list overflows. The detail pane colours diff
  counts, puts the base branch on the merge row, makes `checks` a label like the
  others with its rows aligned under the values, collapses skipped jobs into one
  line and right-aligns durations. The footer only offers actions that apply to
  the selection and keeps `? help` and `q quit` pinned right. The filter prompt
  and error line drop their filled badges, and the filter shows a live match
  count. The help modal dims what is behind it. Empty views suggest where to go
  next. Truncated columns always keep a gutter, and detached worktrees show a
  plain short hash.

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

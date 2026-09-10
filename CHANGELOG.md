# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `--print-config` lists the tab bar, and spells every value the way a config
  file does, so a line can be copied into one.

- A full testing workflow. nextest runs the suite, with per-test timeouts so a
  hung end-to-end test is killed rather than stalling the run. Snapshot tests
  pin every major screen state. Property tests cover column widths, date
  round-trips, parser robustness, and that strings from a repository or the
  GitHub API never reach the terminal as control or bidi characters.
  End-to-end tests drive the real binary in a pseudo-terminal against a fake
  `gh` and a throwaway repository with worktrees. The git hooks and
  `scripts/agent` are tested in throwaway repositories. The GitHub parser is
  tested against a recorded response and, weekly, against the live API. The
  site's build is checked for classes without CSS. Shell scripts are linted
  with shellcheck. CI runs on macOS and Linux, writes a test report, and
  reports coverage (`scripts/task coverage`).

### Fixed

- A worktree is offered for removal only when it is still at the exact commit
  its pull request merged at. Matching by branch name alone offered a reused
  branch name holding new, unmerged commits. And after a squash merge whose
  remote branch was deleted and pruned, the branch's commits counted as
  unpushed, so the desk was never offered at all.
- A worktree whose status was not yet known — before its first scan, with
  `worktree_status = false`, or when `git status` failed — was shown as `local`
  and `clean`, and could be marked removable. It now makes no claim either way,
  and the detail pane says why.
- With drafts hidden, tab counts still included them, so a tab could promise
  more rows than its list held.
- Side by side, a click in the detail pane selected whichever list row shared
  its screen line.
- With help open, the scroll wheel moved the list behind it.
- A Ctrl or Alt chord typed its letter into the filter.
- A copy command that failed still reported the URL as copied.
- Each opened pull request left a finished opener process unreaped for the rest
  of the session, and the opener inherited the terminal's input.
- A `--config` or `RIGOR_CONFIG` path that did not exist was ignored, and so was
  an unknown `default_view` or `layout` value. Each is now an error.
- `--view` accepted only some view names and could open on a tab missing from
  the tab bar. It takes every view, and adds that view to the bar if needed.

- A `gh` call that stalled — a connection left half-open across sleep and wake
  is the usual cause — stopped all further refreshes for good. Every `gh` and
  `git` call now has a deadline, and a timed-out call's whole process group is
  killed, so nothing it spawned outlives it.
- Background `git status` could take `index.lock` and fail an agent's
  concurrent commit in the same worktree. rigor now runs git with
  `--no-optional-locks`.
- Quitting, or receiving SIGTERM or SIGHUP, while a call was in flight could
  leave that call running. Calls still in flight are killed on the way out, and
  the terminal is restored on those signals.
- Shutdown now also refuses to start new subprocesses, so a worker thread still
  running during exit cannot start a child after the final sweep.
- A SIGTERM or SIGHUP is honored within 100ms, well inside the grace period a
  terminal or supervisor allows before SIGKILL.
- A panic in a background worker could leave a loading flag set forever. Workers
  now always report back, and such a panic no longer tears down the screen.
- A theme color with a sign after the `#`, such as `#+fffff`, was read as a
  different color. Hex colors must now be all hex digits; anything else is
  reported as not a color.
- The pre-push hook refused the first publish of a repository using SHA-256
  object ids, whose missing-ref id is 64 zeros rather than 40.
- CI's `required` gate was skipped, not failed, when a job it depends on
  failed, and branch protection counts a skipped check as passing. It now
  always runs and passes only when every job succeeded.

### Changed

- The code is held to a strict standard, enforced by `scripts/task lint`:
  clippy's pedantic group plus selected nursery and restriction lints, with
  warnings as errors; shellcheck with its optional checks; actionlint for the
  workflows; typos for spelling, in American English. `scripts/task audit`
  checks dependencies against an advisory, license and source policy with
  cargo-deny, and CI runs it on every change. Workflow actions are pinned to a
  commit SHA. The published crate ships only the program and what its unit
  tests read.
- Worktrees are rescanned only when their git state moved — two `stat` calls per
  worktree decide it — plus any removable candidate and the selected one, with a
  full sweep every `worktree_scan_secs` (default 300) to catch unstaged edits.
  Scans run four at a time at background CPU priority. On a 50-worktree
  repository this cut CPU over four refreshes from 87s to 13s.
- Failed refreshes back off exponentially from the failure, capped at fifteen
  minutes, and the status line says when the next attempt is due. rigor pauses
  automatic fetches when the shared GitHub budget drops below 5%, leaving it to
  everything else using `gh`.
- When the terminal reports focus lost, refreshes stretch fourfold and full
  worktree sweeps stop; regaining focus catches up at once if the screen went
  stale.

- The top of the screen is now a top nav and a subnav. The nav carries a
  `rigor` badge and a `repo › branch` breadcrumb, and sheds the branch, then the
  user, then the status as the terminal narrows. The subnav's tabs show a title
  and a count only — key digits no longer sit beside counts — with Ready and
  Blocked counts colored when non-zero. A rail beneath underlines the active
  view and joins the detail pane's divider. Number keys are listed in the footer.
- The nav drops its filled badge: `rigor` takes the accent beside a hairline,
  and the repository is the one bold thing on the line. Sync status is a dot —
  green when fresh, amber when stale, red when the last sync failed.
- The tab underline hugs the label, and an active filter shows in the subnav
  instead of the footer.
- The selected row is a full-width band mixed from the terminal's own reported
  colors (OSC 10/11), with a slim accent bar at its edge. Hairlines are mixed
  the same way. Terminals that do not report their colors keep the bar and a
  bolder line, with no band.
- A pass over the rest of the interface. The list drops the review glyph for
  "review required" — the resting state of nearly every PR — keeping only
  approved, changes requested and conflicts; mutes passing CI counts so failures
  stand out; right-aligns ages; hides the author in Mine; and shows your
  position on the rail when the list overflows. The detail pane colors diff
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
  is honored.
- Mouse support: click a tab, a row, or a check run; scroll either pane.
- `site/`: a single-page landing and docs site, built with Next.js and StyleX
  and exported to static HTML. Its hero shows a frame captured from the
  renderer's own test fixtures.

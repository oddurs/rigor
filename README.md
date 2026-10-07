# rigor

[![CI](https://github.com/oddurs/rigor/actions/workflows/ci.yml/badge.svg)](https://github.com/oddurs/rigor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A terminal dashboard for the pull requests you have out on a repository. Run it
inside a checkout and it shows what is ready to merge, what is blocked, and
which of your worktrees can be thrown away — then hands you off to the browser
to actually merge.

## Why

A repo with a lot of parallel work in flight leaves you with two problems the
GitHub web UI is bad at: knowing at a glance which pull requests are actually
mergeable right now, and knowing which of your local worktrees still matter.
`rigor` answers both in one screen, and stays out of the way of the merge button.

It reads everything through the `gh` CLI, so it inherits your existing
authentication and never asks for a token. The one thing it ever sends to
GitHub is a [review draft](#review-drafts) you chose to post, and only on a
confirmed keypress.

## Install

Requires [`gh`](https://cli.github.com) (authenticated) and a Rust toolchain.

```sh
cargo install --git https://github.com/oddurs/rigor
```

## Quickstart

```sh
cd ~/src/some-repo
rigor
```

Six views, selected with the number keys, Tab, or a click:

| View | Contents |
|---|---|
| `Ready` | Green CI, no conflicts, approved or not requiring review — merge these |
| `Mine` | Open pull requests you authored |
| `Review` | Open pull requests waiting on your review |
| `Blocked` | Red CI, requested changes, or conflicts |
| `All` | Every open pull request |
| `Worktrees` | Every git worktree, its branch, its pull request, and its local state |

An `Assigned` view, for pull requests assigned to you, is off the tab bar by
default; add it with `views` in the config, or open on it with `rigor -v assigned`.
So is `Drafts`, which lists [review drafts](#review-drafts) written for this
repository's pull requests: add `drafts` to `views`, or run `rigor -v drafts`.

Keys: `o`/`Enter` opens the selected pull request in the browser, `c` its checks
page, `y` copies the URL, `/` filters, `s` toggles between recency and
merge-readiness ordering, `d` shows or hides draft pull requests, `r` refreshes,
`?` lists everything. In the `Drafts` view, `a` posts the selected review draft
(it asks first), `e` edits it and `x` skips it.

CI shows as a rolled-up glyph with counts in each row (`✓ 19`, `✗ 1/19`,
`◐ 15/19`); selecting a pull request expands every check run with its state and
duration, and clicking one opens that job.

In the `Worktrees` view, a worktree still at the exact commit its pull request
merged at, with nothing uncommitted, is marked `removable`, with the
`git worktree remove` line in the detail pane. Matching the commit, not just the
branch name, means a reused branch name with new work is never offered, and a
squash-merged branch whose remote was deleted still is. `rigor` never modifies
your repository — it only tells you what is safe to collect.

## Review drafts

A review draft is a file some other tool has written about one pull request: a
verdict, a body, and any inline comments. `rigor` does not write drafts and
does not care what does. It reads them from
`<drafts dir>/<owner>/<repo>/<number>.json`, where the drafts directory is
`$RIGOR_DRAFTS_DIR`, else `$XDG_DATA_HOME/rigor/drafts`, else
`~/.local/share/rigor/drafts`:

```json
{
  "version": 1,
  "repo": "acme/widget",
  "number": 101,
  "head_sha": "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b",
  "verdict": "approve",
  "body": "Reads well.",
  "comments": [
    { "path": "src/x.rs", "line": 84, "side": "RIGHT", "body": "Worth a test." }
  ],
  "note": "For you, not for the pull request.",
  "picked": "Why this pull request.",
  "created_at": "2027-01-15T09:00:00Z",
  "status": "drafted",
  "posted_at": null
}
```

`verdict` is `approve` or `comment`; `status` is `drafted`, `posted` or
`skipped`. `head_sha` is the commit the draft was written against.

The `Drafts` view shows one row per file, with the pull request's title, size
and age beside what the draft would send — `approve`, or `comment +2` when two
inline comments ride along — or the one word for why it cannot be sent:

| State | Meaning |
|---|---|
| `stale` | The pull request has moved on from the commit the draft was written against |
| `unverified` | The pull request is not among those `rigor` fetched, so its head is not known |
| `invalid` | The file is not a draft `rigor` understands; the detail pane says why |
| `✓ posted`, `skipped` | Already decided |

Selecting a draft shows it in the detail pane, with `note` marked as private.
A long draft does not fit: the pane scrolls with `J` / `K` and says when there
is more below. A count of drafts still awaiting a decision sits beside the sort
order in every view.

Every field is required and, as in the config file, an unknown key, value or
`version` is an error, not silently ignored. Timestamps are RFC 3339, with `Z`
or a numeric offset. Only a regular file of at most 256 KB is read; a symbolic
link, or anything else under a draft's name, is listed as `invalid`.

### Posting a draft

Posting is public and under your name, so it takes two keypresses. The first
`a` only asks, naming what would be sent in the footer:

```
post approve to #101? a again to confirm, esc to cancel
```

A second `a` on the same row sends it. Any other key, a click, or a refresh
that changes the row withdraws the question.

- A draft is posted only while the pull request is still at the commit the
  draft was written against, as of the last refresh. `stale`, `unverified`,
  `invalid`, skipped and already posted drafts cannot be posted; `a` says why.
- What goes out is the verdict, the body and the inline comments, pinned to
  that commit, in one `gh api` call. `note` and `picked` never leave your
  machine.
- Once GitHub accepts it, the file's `status` becomes `posted` and `posted_at`
  is set. Those are the only two fields `rigor` ever writes, and it replaces
  the file in one step rather than writing over it. If `gh` reports a failure,
  the file is left exactly as it was.

`e` opens the draft's file in `$VISUAL`, else `$EDITOR`, else `vi`, and reads
it again when the editor exits. `x` skips a draft, or brings a skipped one
back.

This is the one thing `rigor` sends to GitHub. It still never merges anything,
and never touches your git repository.

## Configuration

Colors come from ANSI slots by default, so `rigor` renders in whatever palette
your terminal is themed with. Everything is optional:

```sh
rigor --init-config   # writes a commented ~/.config/rigor/config.toml
```

A repo-local `.rigor.toml` overrides the user config. `RIGOR_CONFIG` (or
`--config`) names a different user config, and `rigor --print-config` shows the
settings in effect and which files they came from. An unknown key or value is an
error, not silently ignored.

`RIGOR_THEME` (or `HERDR_THEME_FILE`) points at a theme file, which lets a parent
shell hand its palette down at launch. `NO_COLOR` is honored.
`RIGOR_GH_TIMEOUT_SECS` changes how long one GitHub call may take (default 45).

The selection band and the hairlines are the exception to palette slots: rigor
asks the terminal for its actual foreground and background (OSC 10/11) and mixes
them from those, so they can never clash with a tinted theme. A terminal that
does not answer gets no band — just an accent bar and a bolder line.

## Development

Requires a Rust toolchain and Node 22+ with pnpm — the landing page and docs in
`site/` are a Next.js app styled with StyleX, and `scripts/task` covers both
stacks.

All automation goes through two scripts. Neither one needs you to know what they
are written in.

```sh
scripts/setup                 # once after cloning: wires git hooks, verifies green
scripts/agent doctor          # check the environment
scripts/agent start feat/x    # branch + worktree, printed path to cd into
scripts/task check            # fmt:check, lint, test, build
scripts/agent pr              # check, push, open the pull request
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full workflow, and
[CLAUDE.md](CLAUDE.md) for the contract agents work under.

### Tests

`scripts/task test` runs unit, property, snapshot, end-to-end and hook tests
under [nextest](https://nexte.st). The end-to-end tests drive the real binary
in a pseudo-terminal against a fake `gh` and a throwaway repository. CI runs
the suite on macOS and Linux, reports coverage, and checks the parser against
GitHub's live API weekly. See [CONTRIBUTING.md](CONTRIBUTING.md#testing).

### The site

`site/` is a single static page — landing and docs — built with Next.js and
StyleX and exported to plain HTML.

```sh
cd site
pnpm install
pnpm dev      # http://localhost:3000
pnpm build    # static export into site/out
```

StyleX compiles through Babel and PostCSS rather than Turbopack, so the `dev`
and `build` scripts pass `--webpack` explicitly.

## License

[MIT](LICENSE) © Oddur Sigurdsson

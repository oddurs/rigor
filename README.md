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
`?` lists everything. In the `Drafts` view, `a` asks to post the selected review
draft and `y` then posts it, `e` edits it and `x` skips it.

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
| `unverified` | The pull request is not among those `rigor` fetched, so its head is not known until you post, when GitHub is asked |
| `invalid` | The file is not a draft `rigor` understands; the detail pane says why |
| `maybe sent` | The review was sent and no answer came back; `a` asks GitHub before sending anything again |
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

Posting is public and under your name, so it takes two different keys, and the
second asks GitHub before it sends anything.

`a` only asks. It opens an overlay showing exactly what would be posted, and
nothing else: the pull request's number and title, the verdict, the body, and
each inline comment at its `path:line`. Nothing in it is cut short — a long
path wraps — and the private `note` and `picked` are not in it, because they
are not sent. The footer names the action:

```
post approve to #101? y to post, esc to cancel
```

`y` goes ahead, under three conditions:

- **It answers a question you have had time to see.** A `y` in the first
  400 ms after the overlay appears is ignored, so keys typed ahead are not an
  answer. Pasted text never asks and never answers. And the key that asks is
  not the key that answers: a second `a`, or an `a` held down, only takes the
  overlay down again.
- **You have been shown all of it.** A draft longer than the overlay scrolls
  inside it with `j` / `k`, and `y` is turned away until every row of it has
  been on screen; jumping to the end does not count for the rows in between.
  In a window too small to show it without cutting something, the overlay asks
  for a larger one and nothing is posted.
- **It is still the question you were asked.** `esc` or any other key takes
  the overlay down and does nothing else; so does a refresh that changes the
  draft or its pull request, and thirty seconds without an answer or a scroll.
  The question follows the draft's file, not its row, so a draft arriving
  above it cannot slide another one under the key.

Going ahead is a few reads, then at most one write:

1. **The pull request, as it is right now.** One closed without being merged
   takes no review. One that has moved on from the commit the draft was
   written against takes none either, and its row turns `stale`. A merged one
   still at that commit is fine.
2. **Its reviews.** If this same review is already there from you — the same
   verdict, commit and body, and the same inline comments, which are read back
   to make sure — then it was sent: a post that timed out, or that you quit in
   the middle of, landed after all. The draft is marked `posted` and nothing
   goes out. If you have submitted some other review since the draft was
   written, or have one started in the browser and not submitted, nothing goes
   out and the draft stays as it is: skip it, or finish in the browser. No key
   sends it anyway.
3. **Only then, the review**: the verdict, the body and the inline comments,
   pinned to that commit, in one `gh api` call.

If any read fails, nothing is sent. The list only decides what is offered: the
window in which a pull request can move under a draft is the moment between
those reads and the write, not the refresh interval. That is also why an
`unverified` draft can be posted — the prompt says the list has not seen its
pull request, and the first read checks it. `stale`, `invalid`, skipped and
already posted drafts are never offered; `a` says why.

What happens next depends on GitHub's answer:

- **Accepted** — GitHub answers with the review it created: this verdict, on
  this commit, under your name. Nothing less counts; `gh` exiting cleanly is
  not evidence. The file's `status` becomes `posted` and `posted_at` is set.
  Those are the only two fields `rigor` ever writes, and it replaces the file
  in one step rather than writing over it. If the file was changed while the
  review was on its way, what was sent is what is recorded, and you are told.
- **Declined** — an approval it will not take, a comment on a line outside the
  diff. GitHub's own message is shown, and stays until `esc` or the next
  attempt. The file is left exactly as it was; edit the draft and it can be
  sent.
- **No answer** — a timeout, a dropped connection, an answer that is not the
  review. The review may or may not be there, and `rigor` says so rather than
  calling it a failure; the row reads `maybe sent`. Ask again with `a` and the
  reads above settle it before anything is sent a second time.

One attempt runs at a time. While it does, `e` and `x` leave that draft alone,
and a second `rigor` on the same repository finds a lock beside the draft and
stands back. However `rigor` is leaving — `q`, a signal, a terminal that went
away — it waits for the answer and records it first, up to one call's timeout
and a few seconds more; past that it goes, and says the outcome is not known.

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

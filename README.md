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
authentication and never asks for a token.

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

Keys: `o`/`Enter` opens the selected pull request in the browser, `c` its checks
page, `y` copies the URL, `/` filters, `s` toggles between recency and
merge-readiness ordering, `r` refreshes, `?` lists everything.

CI shows as a rolled-up glyph with counts in each row (`✓ 19`, `✗ 1/19`,
`◐ 15/19`); selecting a pull request expands every check run with its state and
duration, and clicking one opens that job.

In the `Worktrees` view, a worktree whose branch has landed and whose desk is
clean is marked `removable`, with the exact `git worktree remove` line in the
detail pane. `rigor` never modifies your repository — it only tells you what is
safe to collect.

## Configuration

Colours come from ANSI slots by default, so `rigor` renders in whatever palette
your terminal is themed with. Everything is optional:

```sh
rigor --init-config   # writes a commented ~/.config/rigor/config.toml
```

A repo-local `.rigor.toml` overrides the user config. `RIGOR_THEME` (or
`HERDR_THEME_FILE`) points at a theme file, which lets a parent shell hand its
palette down at launch. `NO_COLOR` is honoured.

## Development

All automation goes through two scripts. Neither one needs you to know it is a
Rust project.

```sh
scripts/setup                 # once after cloning: wires git hooks, verifies green
scripts/agent doctor          # check the environment
scripts/agent start feat/x    # branch + worktree, printed path to cd into
scripts/task check            # fmt:check, lint, test, build
scripts/agent pr              # check, push, open the pull request
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full workflow, and
[CLAUDE.md](CLAUDE.md) for the contract agents work under.

## License

[MIT](LICENSE) © Oddur Sigurdsson

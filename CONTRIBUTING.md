# Contributing

Thanks for helping. The workflow below is enforced by git hooks and by branch
protection, so following it is the path of least resistance.

## Once, after cloning

```sh
scripts/setup
```

This wires `core.hooksPath` to `.githooks/`, installs the pinned Rust toolchain
and the site's dependencies, and verifies the project is green. You need Node
22+ with pnpm as well as Rust, because `site/` is part of `scripts/task check`.

## One unit of work, one worktree, one branch, one pull request

`main` only ever advances through a merged pull request. Never commit to it, and
never share a checkout between two pieces of work.

```sh
scripts/agent start feat/ready-view   # creates the branch and its worktree
cd ../.worktrees/rigor/feat/ready-view
# ... make the change ...
scripts/agent check
scripts/agent commit "feat: add the ready view"
scripts/agent pr
```

After the pull request is squash-merged:

```sh
cd /path/to/rigor            # the primary checkout
scripts/agent done feat/ready-view
```

`scripts/agent list` shows every worktree with its branch and pull request state.
`scripts/agent sync` rebases the current branch onto `origin/main`.

Branch names must match
`^(feat|fix|chore|docs|perf|refactor|test)/[a-z0-9][a-z0-9._-]*$`.

## Commits

[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/):

```
<type>(<optional scope>)!: <subject>
```

Types: `feat`, `fix`, `chore`, `docs`, `perf`, `refactor`, `test`, `build`,
`ci`, `style`. Subject is 72 characters or fewer, imperative, no trailing
period. The `commit-msg` hook rejects anything else.

Commits, pull requests, code comments and docs carry no AI or assistant
attribution — no co-author trailers naming a model, no "generated with"
footers. The hook rejects those too. Work here is published under its author's
name.

## Checks

One command, the same one CI runs:

```sh
scripts/task check     # fmt:check && lint && test && build
```

Individually: `scripts/task fmt`, `fmt:check`, `lint`, `test`, `build`. Lint
runs with warnings denied. `scripts/task audit` checks dependencies; it needs
the network, so it is not part of `check`, but CI runs it on every change. If CI
and your machine ever disagree, the bug is in `scripts/task`, which is the only
place either of them looks.

The hooks are deliberately cheap to reason about:

- `commit-msg` — message format and the attribution ban.
- `pre-commit` — `fmt:check` and `lint`.
- `pre-push` — refuses a push to `main`, then runs the full `check`.

Never use `--no-verify`. If a hook is wrong, fix the hook.

## Standards

Each of these is checked by `scripts/task lint` or `audit`, so none of it is a
matter of review taste.

**Rust.** The lint policy is the `[lints]` table in `Cargo.toml`: rustc's
idiom lints, all of clippy's pedantic group, and a chosen set from nursery and
restriction. Warnings are errors.

- Silence a lint with `#[expect(lint, reason = "...")]`, never `#[allow]`.
  `expect` fails once the lint stops firing, so a stale suppression cannot
  linger, and the reason is required.
- No `unwrap` or `expect` outside tests. Handle the case or propagate it.
- Every `unsafe` block has a `// SAFETY:` comment naming the invariant it
  relies on.
- No `as` cast that can truncate, wrap or lose a sign. Use `try_from`, or the
  saturating `util::secs` and `util::cols`.

**Shell.** POSIX `sh`, checked by shellcheck with the extra checks enabled in
`.shellcheckrc`.

- Assign a command substitution to a variable before using it. `set -e` does
  not see a failure inside `"$(...)"` passed as an argument, so
  `cd "$(git rev-parse --show-toplevel)"` carries on in the wrong directory.
- No `|| true`. Name the fallback: `ref=$(git symbolic-ref ...) || ref=''`.
- Every `case` has a default arm, even an empty one, so an unhandled value
  reads as a decision.

**Workflows.** Checked by actionlint, which also runs shellcheck over every
`run:` block. Actions are pinned to a commit SHA with the version in a comment,
and no job keeps the checkout's credentials.

**Dependencies.** `deny.toml` is the policy: a known advisory or a yanked crate
fails, licenses must be on an explicit allow-list, and crates come only from
crates.io.

**Spelling.** American English, checked by typos (`_typos.toml`).

**Site.** TypeScript runs in strict mode with the stricter flags on as well:
`exactOptionalPropertyTypes`, `noUnusedLocals`, `noImplicitReturns` and the
rest in `site/tsconfig.json`.

## Testing

`scripts/task test` runs every layer below; `scripts/task check` runs it along
with formatting, lint and the build. Beyond Rust and Node, `check` needs
`cargo-nextest`, `shellcheck`, `typos` and `actionlint`, and `audit` needs
`cargo-deny`. `scripts/setup` and `scripts/agent doctor` check for all of them.

| Layer | Where | What it pins |
|---|---|---|
| Unit | next to the code (`#[cfg(test)]`) | behavior of one function or type |
| Property | `proptest!` blocks | invariants over generated input: column widths, date round-trips, parsers never panicking, untrusted strings never reaching the terminal as control characters |
| Snapshot | `src/ui/snapshots/` | the rendered screen in every major state |
| Parser | `src/github.rs` against `tests/fixtures/graphql_page.json` | the GitHub response shape, including the awkward cases |
| End to end | `tests/e2e.rs` | the real binary in a pseudo-terminal, against a fake `gh` and a throwaway repository with worktrees: timeouts, shutdown, signals, the color probe, lock-free git, focus |
| Hooks | `tests/hooks.rs` | `commit-msg`, `pre-commit`, `pre-push` and `scripts/agent`, in throwaway repositories |
| Site | `site/scripts/check-css.mjs`, run by `build` | every class on the page has a CSS rule |
| Live contract | `scripts/task test:live`, weekly in CI | the parser still matches GitHub's real API |

Useful commands:

```sh
cargo nextest run snapshot          # run tests whose name matches
cargo nextest run --test e2e        # one integration suite
scripts/task coverage               # line coverage; HTML in target/llvm-cov/html
scripts/task test:live              # needs gh auth and the network
```

**Snapshots.** When a change alters the screen, the snapshot test fails and
leaves a `.snap.new` beside the old one. Review with `cargo insta review` and
read every diff before accepting it: an accepted snapshot is a claim that the
screen is right. `.snap.new` files are gitignored so a half-reviewed change
cannot be committed. UI tests freeze the clock (`util::freeze_time`) so ages
and durations render identically on every run.

**Property tests** generate new input every run, so they can find a case no
earlier run did. proptest records each failing case under
`proptest-regressions/`; commit those files, so the case is replayed first on
every future run.

**Fixtures** use neutral data — `acme/widget`, `octocat` — never a real
repository, branch or person. The repository is public.

**Test harnesses strip `GIT_*` from the environment** before running git. The
pre-push hook runs this suite, and git can hand hooks `GIT_DIR`; inherited,
it would aim a throwaway test at the real repository.

**Leaks.** nextest flags a test whose child process outlives it. That fails
the run on Linux. On macOS it is reported but does not fail, because macOS
cannot create a pipe that is atomically close-on-exec: under a parallel run one
test can inherit another's output and be blamed for it.

## Review

Required approvals on `main` are set to **0**, because this is currently a
solo-maintained repository and requiring an approval would deadlock the owner.
Everything else is still enforced: a pull request is mandatory, the `required`
status check must pass, the branch must be up to date, and conversations must be
resolved. If the project gains a second maintainer, raise required approvals to
1.

## Reporting problems

Bugs and feature requests go in [Issues](https://github.com/oddurs/rigor/issues)
using the forms provided. Security issues follow [SECURITY.md](SECURITY.md)
instead — please do not open a public issue for those.

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
runs with warnings denied. If CI and your machine ever disagree, the bug is in
`scripts/task`, which is the only place either of them looks.

The hooks are deliberately cheap to reason about:

- `commit-msg` — message format and the attribution ban.
- `pre-commit` — `fmt:check` and `lint`.
- `pre-push` — refuses a push to `main`, then runs the full `check`.

Never use `--no-verify`. If a hook is wrong, fix the hook.

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

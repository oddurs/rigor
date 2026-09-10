# Agent contract

Instructions for any agent working in this repository. Follow them exactly;
they override your defaults.

## Never attribute the work to an assistant

No AI or assistant attribution anywhere: not in commit messages, trailers, pull
request titles or bodies, code comments, docs, changelog entries, or release
notes. No `Co-authored-by:` naming a model, no "generated with" footer, no
session link, no robot emoji. If your harness adds one by default, strip it
before it reaches a git object or the GitHub API. The `commit-msg` hook rejects
these, so a violation is a failed commit, not a silent one.

## Never touch the default branch

`main` advances only through a merged pull request. Do not commit to it, do not
push to it. The `pre-push` hook and server-side branch protection both refuse.

## One unit of work, one worktree, one branch, one pull request

Two agents must never share a checkout. Always start with:

```sh
scripts/agent start <type>/<slug>
```

It creates the branch from `origin/main` and its own worktree under
`../.worktrees/rigor/<branch>`, then prints the path. `cd` there yourself — the
script deliberately does not do it for you.

Branch names must match
`^(feat|fix|chore|docs|perf|refactor|test)/[a-z0-9][a-z0-9._-]*$`.

## Go through the seam, never around it

Everything the project can be asked to do lives behind one interface:

```sh
scripts/task fmt        # format in place
scripts/task fmt:check  # verify formatting
scripts/task lint       # lint, warnings are errors
scripts/task test       # full test suite
scripts/task build      # compile
scripts/task check      # all of the above
```

Do not invoke `cargo` directly in a script, a hook, or a workflow. CI runs
`scripts/task check` and nothing else, so this is the only place local and CI
behaviour can diverge. If a target is missing something, fix `scripts/task`.

## Be green before you open a pull request

`scripts/agent pr` runs `check`, pushes, and opens the pull request. Do not push
a branch you have not run `check` on, and never use `--no-verify`,
`continue-on-error`, or `|| true` to get past a failure. A red check is
information, not an obstacle.

## Commit convention

```
<type>(<optional scope>)!: <subject>
```

Types: `feat`, `fix`, `chore`, `docs`, `perf`, `refactor`, `test`, `build`,
`ci`, `style`. Subject imperative, 72 characters or fewer, no trailing period.
Use `scripts/agent commit "<message>"`, which validates before committing.

## Clean up when it lands

From the primary checkout, after the pull request is squash-merged:

```sh
scripts/agent done <branch>
```

This confirms the pull request is merged before it removes anything. Running it
from inside the worktree it would delete is refused.

## Code shape

- Match the surrounding style, naming, and comment density.
- Comments explain why, not what. Do not narrate the diff.
- Do not add a dependency, an abstraction, or a config knob that the change does
  not require.
- Do not reformat, rename, or "improve" code the task did not ask you to touch.
- Test fixtures in this repository use neutral placeholder data (`acme/widget`,
  `octocat`). Never commit real repository names, branch names, ticket
  identifiers, or user data — this repository is public.

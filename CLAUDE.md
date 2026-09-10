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
scripts/task audit      # dependency policy (needs the network)
```

Do not invoke `cargo` directly in a script, a hook, or a workflow. CI runs
`scripts/task check` and nothing else, so this is the only place local and CI
behavior can diverge. If a target is missing something, fix `scripts/task`.

## Be green before you open a pull request

`scripts/agent pr` runs `check`, pushes, and opens the pull request. Do not push
a branch you have not run `check` on, and never use `--no-verify`,
`continue-on-error`, or `|| true` to get past a failure. A red check is
information, not an obstacle.

## Tests

- Every behavior change comes with a test that fails without it. Run the test
  against the old code, or break the new code on purpose, and watch it fail —
  a test that has never failed has not been shown to test anything.
- `scripts/task test` is the suite. Use `cargo nextest run <filter>` to iterate
  on one test; finish with the full `scripts/task check`.
- A failing snapshot means the screen changed. Read the diff in
  `cargo insta review` and accept it only if the new screen is what you meant.
  Never accept snapshots in bulk, and never commit a `.snap.new`.
- Never add `#[ignore]`, `retries`, or a longer timeout to get past a failing
  or flaky test. Find out why it fails. The `live_` tests are the only ignored
  ones, and only because they need the network.
- End-to-end tests wait on something only the target state draws. Text that is
  already on screen elsewhere makes a wait pass before the action has landed.
- Fixtures use neutral data (`acme/widget`, `octocat`). Strip `GIT_*` from the
  environment before running git in a test.

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
- The standards in CONTRIBUTING.md are enforced, not advisory. In short:
  `#[expect(lint, reason = "...")]`, never `#[allow]`; no `unwrap` or `expect`
  outside tests; a `// SAFETY:` comment on every `unsafe` block; no lossy `as`
  casts; in shell, no `|| true` and no `"$(...)"` used directly as an argument;
  American English.
- Comments explain why, not what. Do not narrate the diff.
- Do not add a dependency, an abstraction, or a config knob that the change does
  not require.
- Do not reformat, rename, or "improve" code the task did not ask you to touch.
- Test fixtures in this repository use neutral placeholder data (`acme/widget`,
  `octocat`). Never commit real repository names, branch names, ticket
  identifiers, or user data — this repository is public.

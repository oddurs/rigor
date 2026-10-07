# Security Policy

## Supported versions

`rigor` is pre-1.0. Only the latest release on `main` receives security fixes.

| Version | Supported |
|---|---|
| latest `main` | yes |
| older tags | no |

## Reporting a vulnerability

Report privately through GitHub Security Advisories:

**<https://github.com/oddurs/rigor/security/advisories/new>**

Please do not open a public issue for a security problem.

Include what you need to make the problem reproducible: the version or commit,
the command you ran, and what happened. If the issue involves a repository's
data, describe the shape of it rather than pasting anything private.

## What to expect

- Acknowledgement within 7 days.
- An assessment, and a fix or a rejection with reasons, within 30 days.
- Credit in the release notes and the advisory, unless you would rather not be
  named.

## Scope notes

`rigor` shells out to `git` and to the `gh` CLI and reads their output. It
stores no credentials and writes nothing to your repository. The one thing it
sends anywhere is a review draft you post from the `Drafts` view: through `gh`,
on a confirmed keypress, after reading the pull request and its reviews back
from GitHub to check that the draft still applies and was not already sent,
and without the draft's private `note` and `picked`.
The interesting attack surface is therefore the handling of untrusted strings
that arrive from a repository, from the GitHub API, or from a draft file —
branch names, pull request titles, check-run names, file paths, and a draft's
text — all of which are rendered into a terminal. Reports about that surface
are especially welcome.

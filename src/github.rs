//! GitHub access, entirely through the `gh` CLI so we inherit its auth.
//!
//! One GraphQL query per refresh pulls every open PR with its author, reviewers,
//! assignees and full check-run rollup — far cheaper than a REST call per PR.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::HashMap;
use std::process::Command;
use std::time::Duration;

use crate::drafts;
use crate::proc::{self, Priority};

use crate::model::{Check, CheckState, Mergeable, MergedPr, Pr, ReviewDecision};
use crate::util::parse_iso8601;

const QUERY: &str = r"
query($owner:String!, $name:String!, $cursor:String) {
  rateLimit { remaining limit resetAt }
  viewer { login }
  repository(owner:$owner, name:$name) {
    defaultBranchRef { name }
    mergedPullRequests: pullRequests(states:MERGED, first:50, orderBy:{field:UPDATED_AT, direction:DESC}) {
      nodes { number title url headRefName headRefOid mergedAt }
    }
    pullRequests(states:OPEN, first:50, orderBy:{field:UPDATED_AT, direction:DESC}, after:$cursor) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number title url isDraft updatedAt
        author { login }
        headRefName headRefOid baseRefName
        additions deletions changedFiles
        mergeable
        reviewDecision
        assignees(first:10){nodes{login}}
        reviewRequests(first:10){nodes{requestedReviewer{ __typename ... on User{login} ... on Team{slug} }}}
        labels(first:8){nodes{name}}
        comments{totalCount}
        commits(last:1){nodes{commit{
          statusCheckRollup{ state contexts(first:60){ nodes{
            __typename
            ... on CheckRun { name status conclusion detailsUrl startedAt completedAt }
            ... on StatusContext { context state targetUrl createdAt }
          }}}
        }}}
      }
    }
  }
}
";

/// Where the GraphQL budget stood after this fetch. rigor shares the budget
/// with every other `gh` call on the machine — agents included — so it backs
/// off well before the budget runs out rather than being what exhausts it.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub remaining: u32,
    pub limit: u32,
    pub reset_at: i64,
}

pub struct Fetched {
    pub budget: Option<Budget>,
    pub viewer: String,
    pub default_branch: String,
    pub prs: Vec<Pr>,
    /// Recently merged, used only to mark worktrees whose branch has landed.
    pub merged: Vec<MergedPr>,
}

/// How long a single `gh api` call may take. Generous, because a large
/// repository paginates; the point is that a stalled connection cannot hold the
/// refresh forever. `RIGOR_GH_TIMEOUT_SECS` overrides it — for very slow links,
/// and so the end-to-end tests can exercise the timeout in seconds, not minutes.
fn gh_timeout() -> Duration {
    let secs = std::env::var("RIGOR_GH_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(45);
    Duration::from_secs(secs)
}

/// Fetch every open PR (up to `max`), paginating the GraphQL connection.
pub fn fetch_prs(owner: &str, name: &str, max: usize) -> Result<Fetched> {
    let mut prs = Vec::new();
    let mut cursor: Option<String> = None;
    let mut first: Option<Page> = None;
    let mut budget: Option<Budget> = None;

    loop {
        let mut cmd = Command::new("gh");
        cmd.args(["api", "graphql", "-F"])
            .arg(format!("owner={owner}"))
            .arg("-F")
            .arg(format!("name={name}"))
            .arg("-f")
            .arg(format!("query={QUERY}"));
        if let Some(c) = &cursor {
            cmd.arg("-F").arg(format!("cursor={c}"));
        }

        let out = proc::run(&mut cmd, gh_timeout(), Priority::Normal).context("gh api graphql")?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            bail!("gh api graphql failed: {}", err.trim());
        }
        let v: Value = serde_json::from_slice(&out.stdout).context("parsing gh JSON response")?;
        let mut page = parse_page(&v)?;

        budget = page.budget.or(budget);
        prs.append(&mut page.prs);
        cursor = page.next_cursor.take();
        // Viewer, default branch and the merged list come from the first page;
        // later pages repeat them.
        first.get_or_insert(page);
        if cursor.is_none() || prs.len() >= max {
            break;
        }
    }

    prs.truncate(max);
    let first = first.context("no page returned")?;
    Ok(Fetched {
        budget,
        viewer: first.viewer,
        default_branch: first.default_branch,
        prs,
        merged: first.merged,
    })
}

/// GitHub's REST API through `gh`, for one repository: what posting a review
/// draft needs, and nothing else.
pub struct Gh {
    pub owner: String,
    pub name: String,
}

/// Reviews are read a page at a time, and all of them: the one that matters
/// may be on the last page.
const REVIEWS_PER_PAGE: usize = 100;
/// More reviews than this on one pull request and the check is abandoned
/// rather than made on part of the list.
const MAX_REVIEW_PAGES: usize = 30;

impl Gh {
    fn pulls(&self, number: u64) -> String {
        format!("repos/{}/{}/pulls/{number}", self.owner, self.name)
    }
}

impl drafts::Remote for Gh {
    fn pull(&self, number: u64) -> Result<drafts::Live, String> {
        get(&self.pulls(number))
            .and_then(|v| parse_live(&v))
            .map_err(|e| format!("{e:#}"))
    }

    fn reviews(&self, number: u64) -> Result<Vec<drafts::Review>, String> {
        let mut all = Vec::new();
        for page in 1..=MAX_REVIEW_PAGES {
            let path = format!(
                "{}/reviews?per_page={REVIEWS_PER_PAGE}&page={page}",
                self.pulls(number)
            );
            let mut batch = get(&path)
                .and_then(|v| parse_reviews(&v))
                .map_err(|e| format!("{e:#}"))?;
            let last = batch.len() < REVIEWS_PER_PAGE;
            all.append(&mut batch);
            if last {
                return Ok(all);
            }
        }
        Err(format!(
            "more than {} reviews; not all of them were read",
            REVIEWS_PER_PAGE * MAX_REVIEW_PAGES
        ))
    }

    /// The only call rigor makes that changes anything on GitHub. `body` is
    /// the request's JSON, from `drafts::payload`.
    fn post(&self, number: u64, body: &str) -> Result<(), String> {
        let mut cmd = Command::new("gh");
        cmd.args(["api", "--method", "POST"])
            .arg(format!("{}/reviews", self.pulls(number)))
            .args(["--input", "-"]);
        let out = proc::run_with_input(&mut cmd, body.as_bytes(), gh_timeout(), Priority::Normal)
            .map_err(|e| format!("gh api: {e:#}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(api_error(&out))
        }
    }
}

/// One REST read.
fn get(path: &str) -> Result<Value> {
    let out = proc::run(
        Command::new("gh").args(["api", path]),
        gh_timeout(),
        Priority::Normal,
    )
    .context("gh api")?;
    if !out.status.success() {
        bail!("{}", api_error(&out));
    }
    serde_json::from_slice(&out.stdout).context("parsing gh JSON response")
}

/// Why a REST call failed, in GitHub's own words where it gave any. On an
/// HTTP error `gh` prints the response body on stdout and a one-line summary,
/// with the status, on stderr; the body is where GitHub says what it actually
/// objected to.
fn api_error(out: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stderr = stderr.trim();
    let body: Option<Value> = serde_json::from_slice(&out.stdout).ok();
    let Some(message) = body
        .as_ref()
        .and_then(|b| b.get("message"))
        .and_then(Value::as_str)
    else {
        return if stderr.is_empty() {
            format!("gh api failed ({})", out.status)
        } else {
            stderr.to_string()
        };
    };
    // Each error is a sentence, or an object carrying one.
    let details: Vec<&str> = body
        .as_ref()
        .and_then(|b| b.get("errors"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| e.as_str().or_else(|| e.get("message")?.as_str()))
        .collect();
    let status = stderr
        .find("(HTTP ")
        .and_then(|at| stderr[at..].split_inclusive(')').next())
        .map(|s| format!(" {s}"))
        .unwrap_or_default();
    if details.is_empty() {
        format!("{message}{status}")
    } else {
        format!("{message}: {}{status}", details.join("; "))
    }
}

fn parse_live(v: &Value) -> Result<drafts::Live> {
    let head = v
        .pointer("/head/sha")
        .and_then(Value::as_str)
        .filter(|sha| !sha.is_empty())
        .context("the pull request has no head commit")?;
    let state = v
        .get("state")
        .and_then(Value::as_str)
        .context("the pull request has no state")?;
    Ok(drafts::Live {
        open: state == "open",
        merged: v.get("merged").and_then(Value::as_bool).unwrap_or(false),
        head: head.to_string(),
    })
}

fn parse_reviews(v: &Value) -> Result<Vec<drafts::Review>> {
    let list = v.as_array().context("the reviews were not a list")?;
    Ok(list
        .iter()
        .map(|r| drafts::Review {
            author: str_at(r, &["user", "login"]).unwrap_or_default(),
            state: string(r, "state"),
            commit: string(r, "commit_id"),
            body: string(r, "body"),
            submitted_at: r
                .get("submitted_at")
                .and_then(Value::as_str)
                .map(String::from),
        })
        .collect())
}

/// One page of the GraphQL response, parsed. Pure, so it is tested against a
/// recorded response rather than the network.
struct Page {
    budget: Option<Budget>,
    viewer: String,
    default_branch: String,
    merged: Vec<MergedPr>,
    prs: Vec<Pr>,
    /// Set when there is another page to fetch.
    next_cursor: Option<String>,
}

fn parse_page(v: &Value) -> Result<Page> {
    if let Some(errs) = v.get("errors").and_then(|e| e.as_array()) {
        let msg: Vec<_> = errs
            .iter()
            .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
            .collect();
        bail!("GitHub returned errors: {}", msg.join("; "));
    }
    let data = v.get("data").context("response had no `data`")?;

    let budget = match (
        data.pointer("/rateLimit/remaining").and_then(Value::as_u64),
        data.pointer("/rateLimit/limit").and_then(Value::as_u64),
        data.pointer("/rateLimit/resetAt")
            .and_then(|x| x.as_str())
            .and_then(parse_iso8601),
    ) {
        (Some(remaining), Some(limit), Some(reset_at)) => Some(Budget {
            remaining: count(Some(remaining)),
            limit: count(Some(limit)),
            reset_at,
        }),
        _ => None,
    };

    let merged = data
        .pointer("/repository/mergedPullRequests/nodes")
        .and_then(|n| n.as_array())
        .into_iter()
        .flatten()
        .map(|n| MergedPr {
            number: n.get("number").and_then(Value::as_u64).unwrap_or(0),
            title: string(n, "title"),
            url: string(n, "url"),
            head_ref: string(n, "headRefName"),
            head_oid: string(n, "headRefOid"),
            merged_at: opt_ts(n, "mergedAt").unwrap_or(0),
        })
        .collect();

    let conn = data
        .pointer("/repository/pullRequests")
        .context("repository not found or not accessible")?;
    let prs = conn
        .pointer("/nodes")
        .and_then(|n| n.as_array())
        .into_iter()
        .flatten()
        .map(parse_pr)
        .collect();
    let has_next = conn
        .pointer("/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let next_cursor = has_next
        .then(|| conn.pointer("/pageInfo/endCursor").and_then(|c| c.as_str()))
        .flatten()
        .map(String::from);

    Ok(Page {
        budget,
        viewer: str_at(data, &["viewer", "login"]).unwrap_or_default(),
        default_branch: str_at(data, &["repository", "defaultBranchRef", "name"])
            .unwrap_or_else(|| "main".into()),
        merged,
        prs,
        next_cursor,
    })
}

fn parse_pr(n: &Value) -> Pr {
    let checks = parse_checks(n);
    let rollup = n
        .pointer("/commits/nodes/0/commit/statusCheckRollup/state")
        .and_then(|s| s.as_str())
        .map_or(CheckState::None, CheckState::from_status);

    Pr {
        number: n.get("number").and_then(Value::as_u64).unwrap_or(0),
        title: string(n, "title"),
        url: string(n, "url"),
        is_draft: n.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        updated_at: opt_ts(n, "updatedAt").unwrap_or(0),
        author: str_at(n, &["author", "login"]).unwrap_or_else(|| "ghost".into()),
        head_ref: string(n, "headRefName"),
        head_oid: string(n, "headRefOid"),
        base_ref: string(n, "baseRefName"),
        additions: num(n, "additions"),
        deletions: num(n, "deletions"),
        changed_files: num(n, "changedFiles"),
        mergeable: n
            .get("mergeable")
            .and_then(|x| x.as_str())
            .map_or(Mergeable::Unknown, Mergeable::parse),
        review_decision: n
            .get("reviewDecision")
            .and_then(|x| x.as_str())
            .and_then(ReviewDecision::parse),
        assignees: logins(n.pointer("/assignees/nodes")),
        review_requests: n
            .pointer("/reviewRequests/nodes")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|r| {
                        let rr = r.get("requestedReviewer")?;
                        rr.get("login")
                            .or_else(|| rr.get("slug"))
                            .and_then(|s| s.as_str())
                            .map(String::from)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        labels: n
            .pointer("/labels/nodes")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|l| l.get("name").and_then(|s| s.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        comments: count(n.pointer("/comments/totalCount").and_then(Value::as_u64)),
        checks,
        rollup,
    }
}

/// A branch that has been pushed repeatedly accumulates a check run per workflow
/// run, all under the same name. Keep only the most recent of each name.
fn parse_checks(n: &Value) -> Vec<Check> {
    let nodes = match n.pointer("/commits/nodes/0/commit/statusCheckRollup/contexts/nodes") {
        Some(v) => v.as_array().cloned().unwrap_or_default(),
        None => return Vec::new(),
    };

    let mut by_name: HashMap<String, Check> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for c in &nodes {
        let check = match c.get("__typename").and_then(|t| t.as_str()) {
            Some("CheckRun") => Check {
                name: string(c, "name"),
                state: CheckState::from_check_run(
                    c.get("status").and_then(|s| s.as_str()).unwrap_or(""),
                    c.get("conclusion").and_then(|s| s.as_str()),
                ),
                url: c
                    .get("detailsUrl")
                    .and_then(|s| s.as_str())
                    .map(String::from),
                started_at: opt_ts(c, "startedAt"),
                completed_at: opt_ts(c, "completedAt"),
            },
            Some("StatusContext") => Check {
                name: string(c, "context"),
                state: c
                    .get("state")
                    .and_then(|s| s.as_str())
                    .map_or(CheckState::None, CheckState::from_status),
                url: c
                    .get("targetUrl")
                    .and_then(|s| s.as_str())
                    .map(String::from),
                started_at: opt_ts(c, "createdAt"),
                completed_at: opt_ts(c, "createdAt"),
            },
            _ => continue,
        };

        match by_name.get(&check.name) {
            Some(prev) if prev.started_at >= check.started_at => {}
            Some(_) => {
                by_name.insert(check.name.clone(), check);
            }
            None => {
                order.push(check.name.clone());
                by_name.insert(check.name.clone(), check);
            }
        }
    }

    let mut checks: Vec<Check> = order
        .into_iter()
        .filter_map(|k| by_name.remove(&k))
        .collect();
    // Failures first, then running, so the interesting rows are at the top.
    checks.sort_by_key(|c| match c.state {
        CheckState::Failure | CheckState::Cancelled => 0,
        CheckState::Pending => 1,
        _ => 2,
    });
    checks
}

fn logins(v: Option<&Value>) -> Vec<String> {
    v.and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("login").and_then(|s| s.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn string(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn num(v: &Value, key: &str) -> u32 {
    count(v.get(key).and_then(Value::as_u64))
}

/// A count from the API as `u32`: absent is zero, and anything too large
/// saturates rather than wrapping around to a small number.
fn count(v: Option<u64>) -> u32 {
    v.map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX))
}

fn opt_ts(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(|x| x.as_str()).and_then(parse_iso8601)
}

fn str_at(v: &Value, path: &[&str]) -> Option<String> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_str().map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CheckState, Mergeable, ReviewDecision};

    const FIXTURE: &str = include_str!("../tests/fixtures/graphql_page.json");

    fn page() -> Page {
        parse_page(&serde_json::from_str(FIXTURE).unwrap()).unwrap()
    }

    #[test]
    fn reads_the_envelope() {
        let p = page();
        assert_eq!(p.viewer, "octocat");
        assert_eq!(p.default_branch, "main");
        let b = p.budget.expect("rateLimit");
        assert_eq!((b.remaining, b.limit), (4812, 5000));
        assert_eq!(b.reset_at, parse_iso8601("2027-01-15T10:00:00Z").unwrap());
        assert_eq!(p.merged.len(), 1);
        assert_eq!(p.merged[0].head_ref, "landed-branch");
        assert_eq!(
            p.merged[0].head_oid,
            "5d6c7b8a9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b"
        );
        assert_eq!(p.next_cursor, None, "hasNextPage is false");
        assert_eq!(p.prs.len(), 3);
    }

    /// A branch pushed twice has two runs of `build`; only the newest counts,
    /// or a fixed failure would keep showing red.
    #[test]
    fn keeps_only_the_newest_run_of_each_check() {
        let pr = &page().prs[0];
        let builds: Vec<_> = pr.checks.iter().filter(|c| c.name == "build").collect();
        assert_eq!(builds.len(), 1);
        assert_eq!(builds[0].state, CheckState::Success);
        assert!(builds[0].url.as_deref().unwrap().ends_with("/runs/2/job/2"));
    }

    #[test]
    fn failures_sort_first_and_legacy_statuses_count() {
        let pr = &page().prs[0];
        assert_eq!(pr.checks[0].name, "test", "the failing check leads");
        assert!(
            pr.checks
                .iter()
                .any(|c| c.name == "ci/legacy" && c.state == CheckState::Success)
        );
        let t = pr.tally();
        assert_eq!((t.passed, t.failed, t.skipped, t.total), (2, 1, 1, 4));
        assert_eq!(pr.rollup, CheckState::Failure);
    }

    #[test]
    fn reads_people_and_metadata() {
        let pr = &page().prs[0];
        assert_eq!(pr.author, "octocat");
        assert_eq!(pr.head_oid, "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b");
        assert_eq!(
            pr.review_requests,
            vec!["hubot", "platform"],
            "users and teams"
        );
        assert_eq!(pr.assignees, vec!["octocat"]);
        assert_eq!(pr.labels, vec!["parser", "backend"]);
        assert_eq!(pr.review_decision, Some(ReviewDecision::ReviewRequired));
        assert_eq!(
            (pr.additions, pr.deletions, pr.changed_files, pr.comments),
            (220, 24, 6, 2)
        );
    }

    #[test]
    fn handles_drafts_conflicts_and_unstarted_checks() {
        let pr = &page().prs[1];
        assert!(pr.is_draft);
        assert_eq!(pr.mergeable, Mergeable::Conflicting);
        assert_eq!(pr.review_decision, None);
        assert_eq!(pr.rollup, CheckState::Pending);
        let queued = pr.checks.iter().find(|c| c.name == "lint").unwrap();
        assert_eq!(queued.state, CheckState::Pending);
        assert_eq!((queued.url.as_deref(), queued.started_at), (None, None));
    }

    /// A deleted account comes back as a null author; a commit with no checks
    /// at all has a null rollup. Neither may drop the PR or crash.
    #[test]
    fn tolerates_nulls_github_really_sends() {
        let pr = &page().prs[2];
        assert_eq!(pr.author, "ghost");
        assert_eq!(pr.rollup, CheckState::None);
        assert!(pr.checks.is_empty());
        assert_eq!(pr.mergeable, Mergeable::Unknown);
        assert!(!pr.is_ready(), "mergeability not yet known is not ready");
    }

    #[test]
    fn graphql_errors_become_one_readable_message() {
        let v = serde_json::json!({ "errors": [
            { "type": "RATE_LIMITED", "message": "API rate limit exceeded for user ID 1." },
            { "message": "Something else." }
        ]});
        let err = parse_page(&v)
            .err()
            .expect("errors must fail the page")
            .to_string();
        assert!(
            err.contains("API rate limit exceeded") && err.contains("Something else"),
            "{err}"
        );
    }

    #[test]
    fn a_missing_repository_is_an_error_not_an_empty_list() {
        let v =
            serde_json::json!({ "data": { "viewer": { "login": "octocat" }, "repository": null } });
        assert!(parse_page(&v).is_err());
    }

    #[cfg(unix)]
    fn failed(stdout: &str, stderr: &str) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    /// A refusal is shown in GitHub's words, not just `gh`'s one-line summary:
    /// the body is where it says what it objected to.
    #[cfg(unix)]
    #[test]
    fn a_refused_call_is_reported_in_githubs_own_words() {
        let cases = [
            (
                r#"{"message":"Unprocessable Entity","errors":["Can not approve your own pull request"],"status":"422"}"#,
                "gh: Unprocessable Entity (HTTP 422)",
                "Unprocessable Entity: Can not approve your own pull request (HTTP 422)",
            ),
            (
                r#"{"message":"Validation Failed","errors":[{"resource":"PullRequestReviewComment","field":"line","message":"line must be part of the diff"},{"message":"path is missing"}]}"#,
                "gh: Validation Failed (HTTP 422)",
                "Validation Failed: line must be part of the diff; path is missing (HTTP 422)",
            ),
            (
                r#"{"message":"Not Found","status":"404"}"#,
                "gh: Not Found (HTTP 404)",
                "Not Found (HTTP 404)",
            ),
            // No body to read: whatever gh said is all there is.
            (
                "",
                "error connecting to api.github.com",
                "error connecting to api.github.com",
            ),
            ("<html>502</html>", "gh: HTTP 502", "gh: HTTP 502"),
        ];
        for (stdout, stderr, want) in cases {
            assert_eq!(api_error(&failed(stdout, stderr)), want);
        }
        assert!(api_error(&failed("", "")).starts_with("gh api failed"));
    }

    #[test]
    fn reads_a_pull_requests_live_state() {
        let live = |v: Value| parse_live(&v);
        let open = live(serde_json::json!({
            "state": "open", "merged": false, "head": { "sha": "1a2b3c4d" }
        }))
        .unwrap();
        assert_eq!(
            (open.open, open.merged, open.head.as_str()),
            (true, false, "1a2b3c4d")
        );

        let merged = live(serde_json::json!({
            "state": "closed", "merged": true, "head": { "sha": "1a2b3c4d" }
        }))
        .unwrap();
        assert!(!merged.open && merged.merged);

        let closed = live(serde_json::json!({
            "state": "closed", "merged": false, "head": { "sha": "1a2b3c4d" }
        }))
        .unwrap();
        assert!(!closed.open && !closed.merged);

        // An answer that does not say where the pull request is, or whether it
        // is open, is not one to post on.
        assert!(live(serde_json::json!({ "state": "open", "head": {} })).is_err());
        assert!(live(serde_json::json!({ "state": "open", "head": { "sha": "" } })).is_err());
        assert!(live(serde_json::json!({ "head": { "sha": "1a2b3c4d" } })).is_err());
    }

    #[test]
    fn reads_reviews_including_a_pending_one() {
        let got = parse_reviews(&serde_json::json!([
            { "user": { "login": "octocat" }, "state": "APPROVED", "commit_id": "1a2b3c4d",
              "body": "Reads well.", "submitted_at": "2027-01-15T09:30:00Z" },
            { "user": { "login": "octocat" }, "state": "PENDING", "commit_id": "1a2b3c4d",
              "body": "" },
            { "user": null, "state": "COMMENTED", "commit_id": null, "body": null,
              "submitted_at": "2027-01-15T09:31:00Z" }
        ]))
        .unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(
            (
                got[0].author.as_str(),
                got[0].state.as_str(),
                got[0].commit.as_str()
            ),
            ("octocat", "APPROVED", "1a2b3c4d")
        );
        assert_eq!(got[0].submitted_at.as_deref(), Some("2027-01-15T09:30:00Z"));
        assert_eq!(got[1].submitted_at, None, "a pending review has no time");
        assert_eq!(
            got[2].author, "",
            "a deleted account is nobody, not the viewer"
        );

        assert!(
            parse_reviews(&serde_json::json!({ "message": "Not Found" })).is_err(),
            "anything but a list is not an empty list"
        );
    }

    /// The contract with the real API: the query still parses against GitHub
    /// as it is today. Needs `gh` authenticated and the network, so it is
    /// ignored locally and run weekly in CI (.github/workflows/contract.yml).
    #[test]
    #[ignore = "live: needs network and gh auth"]
    fn live_query_still_matches_the_github_schema() {
        let f = fetch_prs("cli", "cli", 20).expect("live fetch");
        assert!(!f.viewer.is_empty(), "viewer login");
        assert!(!f.default_branch.is_empty(), "defaultBranchRef");
        assert!(!f.prs.is_empty(), "cli/cli always has open pull requests");
        assert!(f.budget.is_some(), "rateLimit is still in the schema");
        // Removal is decided by this commit id; an empty one would quietly
        // mean no worktree is ever removable.
        assert!(
            !f.merged.is_empty() && f.merged.iter().all(|m| m.head_oid.len() >= 40),
            "headRefOid on merged pull requests"
        );
        // Posting a review draft is decided by this one.
        assert!(
            f.prs.iter().all(|p| p.head_oid.len() >= 40),
            "headRefOid on open pull requests"
        );
        assert!(
            f.prs
                .iter()
                .all(|p| p.number > 0 && !p.url.is_empty() && !p.head_ref.is_empty()),
            "every PR parsed its core fields"
        );
        assert!(
            f.prs.iter().any(|p| !p.checks.is_empty()),
            "check runs still come through the rollup"
        );
    }
}

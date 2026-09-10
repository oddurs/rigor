//! GitHub access, entirely through the `gh` CLI so we inherit its auth.
//!
//! One GraphQL query per refresh pulls every open PR with its author, reviewers,
//! assignees and full check-run rollup — far cheaper than a REST call per PR.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::HashMap;
use std::process::Command;

use crate::model::{Check, CheckState, Mergeable, MergedPr, Pr, ReviewDecision};
use crate::util::parse_iso8601;

const QUERY: &str = r#"
query($owner:String!, $name:String!, $cursor:String) {
  viewer { login }
  repository(owner:$owner, name:$name) {
    defaultBranchRef { name }
    mergedPullRequests: pullRequests(states:MERGED, first:50, orderBy:{field:UPDATED_AT, direction:DESC}) {
      nodes { number title url headRefName mergedAt }
    }
    pullRequests(states:OPEN, first:50, orderBy:{field:UPDATED_AT, direction:DESC}, after:$cursor) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number title url isDraft updatedAt
        author { login }
        headRefName baseRefName
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
"#;

pub struct Fetched {
    pub viewer: String,
    pub default_branch: String,
    pub prs: Vec<Pr>,
    /// Recently merged, used only to mark worktrees whose branch has landed.
    pub merged: Vec<MergedPr>,
}

/// Fetch every open PR (up to `max`), paginating the GraphQL connection.
pub fn fetch_prs(owner: &str, name: &str, max: usize) -> Result<Fetched> {
    let mut prs = Vec::new();
    let mut cursor: Option<String> = None;
    let mut viewer = String::new();
    let mut default_branch = String::new();
    let mut merged: Vec<MergedPr> = Vec::new();

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

        let out = cmd
            .output()
            .context("running `gh api graphql` (is gh installed?)")?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            bail!("gh api graphql failed: {}", err.trim());
        }

        let v: Value = serde_json::from_slice(&out.stdout).context("parsing gh JSON response")?;
        if let Some(errs) = v.get("errors").and_then(|e| e.as_array()) {
            let msg: Vec<_> = errs
                .iter()
                .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                .collect();
            bail!("GitHub returned errors: {}", msg.join("; "));
        }

        let data = v.get("data").context("response had no `data`")?;
        // The merged list rides along on the same query. Pagination past the
        // first page re-fetches it, which only happens on repos with more than
        // 50 open PRs; reading it once keeps that harmless.
        if viewer.is_empty() {
            viewer = str_at(data, &["viewer", "login"]).unwrap_or_default();
            default_branch = str_at(data, &["repository", "defaultBranchRef", "name"])
                .unwrap_or_else(|| "main".into());
            for n in data
                .pointer("/repository/mergedPullRequests/nodes")
                .and_then(|n| n.as_array())
                .into_iter()
                .flatten()
            {
                merged.push(MergedPr {
                    number: n.get("number").and_then(|x| x.as_u64()).unwrap_or(0),
                    title: string(n, "title"),
                    url: string(n, "url"),
                    head_ref: string(n, "headRefName"),
                    merged_at: opt_ts(n, "mergedAt").unwrap_or(0),
                });
            }
        }

        let conn = data
            .pointer("/repository/pullRequests")
            .context("repository not found or not accessible")?;
        for node in conn
            .pointer("/nodes")
            .and_then(|n| n.as_array())
            .into_iter()
            .flatten()
        {
            prs.push(parse_pr(node));
        }

        let has_next = conn
            .pointer("/pageInfo/hasNextPage")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        if !has_next || prs.len() >= max {
            break;
        }
        cursor = conn
            .pointer("/pageInfo/endCursor")
            .and_then(|c| c.as_str())
            .map(String::from);
        if cursor.is_none() {
            break;
        }
    }

    prs.truncate(max);
    Ok(Fetched {
        viewer,
        default_branch,
        prs,
        merged,
    })
}

fn parse_pr(n: &Value) -> Pr {
    let checks = parse_checks(n);
    let rollup = n
        .pointer("/commits/nodes/0/commit/statusCheckRollup/state")
        .and_then(|s| s.as_str())
        .map(CheckState::from_status)
        .unwrap_or(CheckState::None);

    Pr {
        number: n.get("number").and_then(|x| x.as_u64()).unwrap_or(0),
        title: string(n, "title"),
        url: string(n, "url"),
        is_draft: n.get("isDraft").and_then(|x| x.as_bool()).unwrap_or(false),
        updated_at: opt_ts(n, "updatedAt").unwrap_or(0),
        author: str_at(n, &["author", "login"]).unwrap_or_else(|| "ghost".into()),
        head_ref: string(n, "headRefName"),
        base_ref: string(n, "baseRefName"),
        additions: num(n, "additions"),
        deletions: num(n, "deletions"),
        changed_files: num(n, "changedFiles"),
        mergeable: n
            .get("mergeable")
            .and_then(|x| x.as_str())
            .map(Mergeable::parse)
            .unwrap_or(Mergeable::Unknown),
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
        comments: n
            .pointer("/comments/totalCount")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as u32,
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
                    .map(CheckState::from_status)
                    .unwrap_or(CheckState::None),
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
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0) as u32
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

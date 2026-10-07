//! Review drafts: files some other tool leaves beside a pull request, which
//! rigor lists and — on a confirmed keypress — posts as a review.
//!
//! rigor does not write drafts and does not know who does. The file is the
//! whole contract: `<drafts dir>/<owner>/<repo>/<number>.json`. The only fields
//! rigor ever changes are `status` and `posted_at`.
//!
//! Posting is the one thing rigor sends anywhere, so it is decided here, in
//! one place, against what GitHub says at that moment: see `deliver`.

use serde::{Deserialize, Deserializer, Serialize};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::util::parse_iso8601;

/// The one file format this rigor reads.
const VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Approve,
    Comment,
}

impl Verdict {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Comment => "comment",
        }
    }

    /// What GitHub's review API calls it.
    const fn event(self) -> &'static str {
        match self {
            Self::Approve => "APPROVE",
            Self::Comment => "COMMENT",
        }
    }

    /// The state GitHub gives a review of this kind once it is submitted.
    const fn submitted_as(self) -> &'static str {
        match self {
            Self::Approve => "APPROVED",
            Self::Comment => "COMMENTED",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Drafted,
    Posted,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Comment {
    pub path: String,
    pub line: u64,
    pub side: Side,
    pub body: String,
}

/// The file, field for field and in the contract's order, so that writing one
/// back changes only what was meant to change. An unknown key is an error, as
/// it is in a config file: a draft rigor does not fully understand is not one
/// it should offer to post.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub version: u64,
    pub repo: String,
    pub number: u64,
    pub head_sha: String,
    pub verdict: Verdict,
    pub body: String,
    pub comments: Vec<Comment>,
    /// For the reader of the draft. Never posted.
    pub note: String,
    /// Why this pull request was chosen. Never posted.
    pub picked: String,
    pub created_at: String,
    pub status: Status,
    #[serde(deserialize_with = "present")]
    pub posted_at: Option<String>,
}

/// serde reads a missing `Option` as `None`. The contract says the key is
/// always there, `null` until posted, so its absence is a malformed file.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::deserialize(d)
}

impl Draft {
    /// What posting would send, in a few characters: `approve`, or
    /// `comment +2` when two inline comments ride along.
    pub fn summary(&self) -> String {
        match self.comments.len() {
            0 => self.verdict.label().to_string(),
            n => format!("{} +{n}", self.verdict.label()),
        }
    }

    /// The same, spelled out for the confirmation prompt.
    pub fn describe(&self) -> String {
        match self.comments.len() {
            0 => self.verdict.label().to_string(),
            1 => format!("{} with 1 inline comment", self.verdict.label()),
            n => format!("{} with {n} inline comments", self.verdict.label()),
        }
    }

    /// Everything the JSON shape alone cannot say. `slug` is the repository
    /// rigor is looking at and `number` the pull request the file is named
    /// for; a draft that disagrees with either would be posted somewhere its
    /// author did not mean.
    fn validate(&self, slug: &str, number: u64) -> Result<(), String> {
        if !self.repo.eq_ignore_ascii_case(slug) {
            return Err(format!("written for {}, not {slug}", self.repo));
        }
        if self.number != number {
            return Err(format!(
                "says #{}, but the file is named for #{number}",
                self.number
            ));
        }
        if self.head_sha.len() != 40 || !self.head_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("head_sha is not a 40-character commit id".into());
        }
        if parse_iso8601(&self.created_at).is_none() {
            return Err("created_at is not an RFC 3339 timestamp".into());
        }
        if let Some(at) = &self.posted_at
            && parse_iso8601(at).is_none()
        {
            return Err("posted_at is not an RFC 3339 timestamp".into());
        }
        // GitHub rejects a comment review with nothing in it; an approval
        // needs no words.
        if self.verdict == Verdict::Comment
            && self.body.trim().is_empty()
            && self.comments.is_empty()
        {
            return Err("a comment review needs a body or an inline comment".into());
        }
        Ok(())
    }
}

/// Read one draft's text. The error is the reason shown beside an invalid
/// draft, so it is written for the person looking at the list.
pub fn parse(text: &str, slug: &str, number: u64) -> Result<Draft, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    // Checked before the shape: a later version may have other fields, and
    // "unknown field" would be the wrong thing to say about it.
    match value.get("version") {
        Some(v) if v.as_u64() == Some(VERSION) => {}
        Some(v) => return Err(format!("version {v} is not one this rigor reads")),
        None => return Err("no `version` field".into()),
    }
    let draft: Draft = serde_json::from_value(value).map_err(|e| e.to_string())?;
    draft.validate(slug, number)?;
    Ok(draft)
}

/// Whether a draft may be posted, and if not, why not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The file could not be understood. Never postable.
    Invalid,
    Posted,
    Skipped,
    /// The pull request has moved on since the draft was written.
    Stale,
    /// The pull request's head is not known, so the draft cannot be checked
    /// against it.
    Unverified,
    /// Written against the pull request's current head, and not yet decided.
    Ready,
}

/// `head` is the pull request's current head commit when the last fetch
/// included it. Unknown is never taken to match: a review is only offered for
/// the exact commit it was written against.
pub fn state(draft: &Result<Draft, String>, head: Option<&str>) -> State {
    let Ok(d) = draft else {
        return State::Invalid;
    };
    match (d.status, head.filter(|h| !h.is_empty())) {
        (Status::Posted, _) => State::Posted,
        (Status::Skipped, _) => State::Skipped,
        (Status::Drafted, None) => State::Unverified,
        (Status::Drafted, Some(h)) if h.eq_ignore_ascii_case(&d.head_sha) => State::Ready,
        (Status::Drafted, Some(_)) => State::Stale,
    }
}

#[derive(Serialize)]
struct Request<'a> {
    commit_id: &'a str,
    event: &'static str,
    body: &'a str,
    comments: &'a [Comment],
}

/// The body of the one request rigor ever sends. Built from a type with no
/// place for `note` or `picked`, so neither can reach GitHub.
pub fn payload(d: &Draft) -> serde_json::Result<String> {
    serde_json::to_string(&Request {
        commit_id: &d.head_sha,
        event: d.verdict.event(),
        body: &d.body,
        comments: &d.comments,
    })
}

/// A pull request as GitHub describes it right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live {
    pub open: bool,
    pub merged: bool,
    pub head: String,
}

/// A review already on a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub author: String,
    /// `APPROVED`, `COMMENTED`, `CHANGES_REQUESTED`, `DISMISSED` or `PENDING`.
    pub state: String,
    pub commit: String,
    pub body: String,
    /// `None` while the review is still pending.
    pub submitted_at: Option<String>,
}

/// What posting needs from GitHub, for one repository. `main` wires in `gh`;
/// a test hands over a recorder, so nothing built in a test can reach GitHub.
pub trait Remote: Send + Sync {
    fn pull(&self, number: u64) -> Result<Live, String>;
    /// Every review on the pull request, all pages.
    fn reviews(&self, number: u64) -> Result<Vec<Review>, String>;
    fn post(&self, number: u64, body: &str) -> Result<(), String>;
}

/// Why nothing was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The pull request was closed without being merged.
    Closed,
    /// The pull request is at this commit now, not the draft's.
    Moved(String),
    /// The viewer has reviewed the pull request since the draft was written,
    /// and not with this review.
    ReviewedSince,
    /// Who is asking is not known, so their reviews cannot be told from
    /// anyone else's.
    NoViewer,
    /// GitHub could not be asked.
    Unchecked(String),
    /// GitHub was asked to take the review and did not.
    Rejected(String),
}

/// How an attempt to post ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Sent,
    /// The same review is already on the pull request, submitted at this
    /// time. Nothing was sent.
    AlreadySent(String),
    Refused(Refusal),
}

/// A pull request that is gone, or no longer at the draft's commit, cannot
/// take the draft. A merged one still at that commit can.
fn check_pull(d: &Draft, live: &Live) -> Result<(), Refusal> {
    if !live.open && !live.merged {
        return Err(Refusal::Closed);
    }
    if !live.head.eq_ignore_ascii_case(&d.head_sha) {
        return Err(Refusal::Moved(live.head.clone()));
    }
    Ok(())
}

/// The viewer's own reviews since the draft was written decide whether it is
/// still theirs to send. The same review — event, commit and body — means it
/// already went out: a post that timed out, or was cut off by quitting, still
/// landed. Any other review means they have since said something else, and
/// the draft is not sent on top of it.
fn check_reviews(d: &Draft, viewer: &str, reviews: &[Review]) -> Option<Outcome> {
    // Checked when the draft was read. Were it somehow not a time, every
    // review counts as since: the doubt must not end in a second post.
    let written = parse_iso8601(&d.created_at).unwrap_or(i64::MIN);
    let mut since = reviews.iter().filter(|r| {
        r.author.eq_ignore_ascii_case(viewer)
            && r.submitted_at
                .as_deref()
                .and_then(parse_iso8601)
                .is_some_and(|at| at >= written)
    });
    let same = |r: &&Review| {
        r.state == d.verdict.submitted_as()
            && r.commit.eq_ignore_ascii_case(&d.head_sha)
            && r.body.trim() == d.body.trim()
    };
    if let Some(sent) = since.clone().find(same) {
        return Some(Outcome::AlreadySent(
            sent.submitted_at.clone().unwrap_or_default(),
        ));
    }
    since
        .next()
        .map(|_| Outcome::Refused(Refusal::ReviewedSince))
}

/// Post `d` to pull request `number`, if GitHub's own account of the pull
/// request, read here and now, still allows it. Two reads, then at most one
/// write; a read that fails sends nothing.
///
/// This is what a confirming keypress runs. What the list showed when the
/// prompt went up is as old as the last refresh; this is as old as the time
/// between these calls.
pub fn deliver(remote: &dyn Remote, viewer: &str, number: u64, d: &Draft) -> Outcome {
    if viewer.is_empty() {
        return Outcome::Refused(Refusal::NoViewer);
    }
    let live = match remote.pull(number) {
        Ok(live) => live,
        Err(e) => return Outcome::Refused(Refusal::Unchecked(e)),
    };
    if let Err(why) = check_pull(d, &live) {
        return Outcome::Refused(why);
    }
    let reviews = match remote.reviews(number) {
        Ok(reviews) => reviews,
        Err(e) => return Outcome::Refused(Refusal::Unchecked(e)),
    };
    if let Some(settled) = check_reviews(d, viewer, &reviews) {
        return settled;
    }
    let sent = payload(d)
        .map_err(|e| format!("could not build the review: {e}"))
        .and_then(|body| remote.post(number, &body));
    match sent {
        Ok(()) => Outcome::Sent,
        Err(e) => Outcome::Refused(Refusal::Rejected(e)),
    }
}

/// `$RIGOR_DRAFTS_DIR`, else `$XDG_DATA_HOME/rigor/drafts`, else
/// `~/.local/share/rigor/drafts`. A variable set to nothing counts as unset.
pub fn dir(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let set = |key: &str| env(key).filter(|v| !v.is_empty()).map(PathBuf::from);
    set("RIGOR_DRAFTS_DIR")
        .or_else(|| set("XDG_DATA_HOME").map(|p| p.join("rigor").join("drafts")))
        .or_else(|| {
            set("HOME").map(|p| p.join(".local").join("share").join("rigor").join("drafts"))
        })
}

/// One file in a repository's drafts directory, understood or not.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    /// The pull request the file is named for. `None` when the name is not a
    /// number, which makes the draft invalid but still worth showing.
    pub number: Option<u64>,
    /// The file as read, so a pending post can tell if it changed underneath.
    pub raw: String,
    pub draft: Result<Draft, String>,
}

impl Entry {
    /// `#101`, or the file's own name when it is not named for a pull request.
    pub fn label(&self) -> String {
        self.number.map_or_else(
            || {
                self.path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            },
            |n| format!("#{n}"),
        )
    }
}

/// Far more than any review needs. A file past this is not read.
const MAX_BYTES: u64 = 256 * 1024;

/// A draft's text, if the path is an ordinary file of ordinary size. Judged
/// before it is opened: a link is not followed to wherever it points, and a
/// pipe or a device left under a draft's name would never finish being read.
fn read_text(path: &Path) -> Result<String, String> {
    let unreadable = |e: std::io::Error| format!("could not be read: {e}");
    let meta = std::fs::symlink_metadata(path).map_err(unreadable)?;
    if meta.file_type().is_symlink() {
        return Err("a symbolic link, not a regular file".into());
    }
    if !meta.is_file() {
        return Err("not a regular file".into());
    }
    // Capped as it is read, not by the size it reported a moment ago.
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_BYTES + 1).read_to_string(&mut text))
        .map_err(unreadable)?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_BYTES {
        return Err(format!("larger than {} KB", MAX_BYTES / 1024));
    }
    Ok(text)
}

fn read(path: PathBuf, slug: &str) -> Entry {
    let number = path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.parse::<u64>().ok());
    let (raw, draft) = match (read_text(&path), number) {
        (Ok(raw), Some(n)) => {
            let draft = parse(&raw, slug, n);
            (raw, draft)
        }
        (Ok(raw), None) => (
            raw,
            Err("the file is not named for a pull request number".into()),
        ),
        (Err(why), _) => (String::new(), Err(why)),
    };
    Entry {
        path,
        number,
        raw,
        draft,
    }
}

/// Every draft for `owner/name` under `dir`, ordered by pull request. No
/// directory means no drafts; a directory that cannot be read is an error.
pub fn load(dir: &Path, owner: &str, name: &str) -> std::io::Result<Vec<Entry>> {
    let files = match std::fs::read_dir(dir.join(owner).join(name)) {
        Ok(files) => files,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let slug = format!("{owner}/{name}");
    let mut entries = Vec::new();
    for file in files {
        let path = file?.path();
        // Only `*.json`: a status rewrite in progress sits beside its file
        // under another extension.
        if path.extension().is_some_and(|e| e == "json") {
            entries.push(read(path, &slug));
        }
    }
    entries.sort_by(|a, b| a.number.cmp(&b.number).then_with(|| a.path.cmp(&b.path)));
    Ok(entries)
}

/// Set a draft's `status` and `posted_at`, leaving every other field as it is
/// on disk right now — read fresh, so an edit made since the list was loaded
/// survives. Written beside the file and renamed over it, so nothing reading
/// the directory ever sees half a draft.
pub fn set_status(
    path: &Path,
    slug: &str,
    status: Status,
    posted_at: Option<String>,
) -> Result<(), String> {
    let mut draft = read(path.to_path_buf(), slug).draft?;
    draft.status = status;
    draft.posted_at = posted_at;
    let mut text = serde_json::to_string_pretty(&draft).map_err(|e| e.to_string())?;
    text.push('\n');

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", path.display())
        })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use serde_json::{Value, json};

    pub const HEAD: &str = "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b";

    /// A complete, valid draft for `acme/widget#101`, as its writer leaves it.
    pub fn sample() -> Value {
        json!({
            "version": 1,
            "repo": "acme/widget",
            "number": 101,
            "head_sha": HEAD,
            "verdict": "approve",
            "body": "Reads well.",
            "comments": [
                { "path": "src/x.rs", "line": 84, "side": "RIGHT", "body": "Worth a test." }
            ],
            "note": "PRIVATE-NOTE",
            "picked": "PRIVATE-PICKED",
            "created_at": "2027-01-15T07:00:00Z",
            "status": "drafted",
            "posted_at": null
        })
    }

    fn with(edit: impl FnOnce(&mut Value)) -> String {
        let mut v = sample();
        edit(&mut v);
        v.to_string()
    }

    fn without(key: &str) -> String {
        with(|v| {
            v.as_object_mut().unwrap().remove(key);
        })
    }

    #[test]
    fn a_draft_is_read_field_for_field() {
        let d = parse(&sample().to_string(), "acme/widget", 101).unwrap();
        assert_eq!((d.number, d.head_sha.as_str()), (101, HEAD));
        assert_eq!((d.verdict, d.status), (Verdict::Approve, Status::Drafted));
        assert_eq!(d.comments.len(), 1);
        assert_eq!(
            (d.comments[0].path.as_str(), d.comments[0].line),
            ("src/x.rs", 84)
        );
        assert_eq!(d.comments[0].side, Side::Right);
        assert_eq!(d.posted_at, None);
        assert_eq!(d.summary(), "approve +1");
        assert_eq!(d.describe(), "approve with 1 inline comment");
    }

    /// One draft's text and what reading it must say. `reason: None` is a
    /// draft that must be accepted.
    struct Case {
        name: &'static str,
        text: String,
        reason: Option<&'static str>,
    }

    fn check(cases: impl IntoIterator<Item = Case>) {
        for case in cases {
            let got = parse(&case.text, "acme/widget", 101);
            match (case.reason, got) {
                (None, Ok(_)) => {}
                (None, Err(e)) => panic!("{}: rejected: {e}", case.name),
                (Some(want), Err(e)) => {
                    assert!(e.contains(want), "{}: {e:?} lacks {want:?}", case.name);
                }
                (Some(want), Ok(_)) => panic!("{}: accepted, wanted {want:?}", case.name),
            }
        }
    }

    #[test]
    fn a_valid_draft_may_be_sparse() {
        check([
            Case {
                name: "the sample itself",
                text: sample().to_string(),
                reason: None,
            },
            Case {
                name: "an approval needs no words",
                text: with(|v| {
                    v["body"] = json!("");
                    v["comments"] = json!([]);
                }),
                reason: None,
            },
            Case {
                name: "a comment review may be inline comments alone",
                text: with(|v| {
                    v["verdict"] = json!("comment");
                    v["body"] = json!("");
                }),
                reason: None,
            },
            Case {
                name: "the repository's case does not matter",
                text: with(|v| v["repo"] = json!("Acme/Widget")),
                reason: None,
            },
            Case {
                name: "a created_at with an offset is a time like any other",
                text: with(|v| v["created_at"] = json!("2027-01-15T09:00:00.25+02:00")),
                reason: None,
            },
        ]);
    }

    /// Anything rigor does not fully understand is invalid, with a reason a
    /// person can act on. Never a best-effort read: an unknown key is as much
    /// an error here as it is in a config file.
    #[test]
    fn a_draft_of_the_wrong_shape_is_invalid() {
        check([
            Case {
                name: "malformed JSON",
                text: "{ \"version\": 1,".into(),
                reason: Some("not valid JSON"),
            },
            Case {
                name: "not an object",
                text: "[]".into(),
                reason: Some("no `version` field"),
            },
            Case {
                name: "an unknown version",
                text: with(|v| v["version"] = json!(2)),
                reason: Some("version 2 is not one this rigor reads"),
            },
            Case {
                name: "a version that is not a number",
                text: with(|v| v["version"] = json!("1")),
                reason: Some("is not one this rigor reads"),
            },
            Case {
                name: "no version",
                text: without("version"),
                reason: Some("no `version` field"),
            },
            Case {
                name: "a missing field",
                text: without("head_sha"),
                reason: Some("missing field `head_sha`"),
            },
            Case {
                name: "a missing private field is still a missing field",
                text: without("note"),
                reason: Some("missing field `note`"),
            },
            Case {
                name: "posted_at must be present, even as null",
                text: without("posted_at"),
                reason: Some("missing field `posted_at`"),
            },
            Case {
                name: "an unknown field",
                text: with(|v| v["priority"] = json!("high")),
                reason: Some("unknown field `priority`"),
            },
            Case {
                name: "an unknown field in a comment",
                text: with(|v| v["comments"][0]["start_line"] = json!(80)),
                reason: Some("unknown field `start_line`"),
            },
            Case {
                name: "an unknown verdict",
                text: with(|v| v["verdict"] = json!("request_changes")),
                reason: Some("unknown variant `request_changes`"),
            },
            Case {
                name: "an unknown status",
                text: with(|v| v["status"] = json!("queued")),
                reason: Some("unknown variant `queued`"),
            },
            Case {
                name: "an unknown side",
                text: with(|v| v["comments"][0]["side"] = json!("right")),
                reason: Some("unknown variant `right`"),
            },
        ]);
    }

    /// Well-formed, but not something to post: aimed at the wrong place, or
    /// sure to be rejected.
    #[test]
    fn a_draft_that_says_the_wrong_thing_is_invalid() {
        check([
            Case {
                name: "a short commit id",
                text: with(|v| v["head_sha"] = json!("1a2b3c4")),
                reason: Some("head_sha is not a 40-character commit id"),
            },
            Case {
                name: "a commit id that is not hex",
                text: with(|v| v["head_sha"] = json!("z".repeat(40))),
                reason: Some("head_sha is not a 40-character commit id"),
            },
            Case {
                name: "another repository's draft",
                text: with(|v| v["repo"] = json!("acme/gadget")),
                reason: Some("written for acme/gadget, not acme/widget"),
            },
            Case {
                name: "a number that disagrees with the file name",
                text: with(|v| v["number"] = json!(102)),
                reason: Some("says #102, but the file is named for #101"),
            },
            Case {
                name: "a created_at that is not a timestamp",
                text: with(|v| v["created_at"] = json!("yesterday")),
                reason: Some("created_at is not an RFC 3339 timestamp"),
            },
            Case {
                name: "a created_at with no zone could be any time",
                text: with(|v| v["created_at"] = json!("2027-01-15T07:00:00")),
                reason: Some("created_at is not an RFC 3339 timestamp"),
            },
            Case {
                name: "a created_at with something after the zone",
                text: with(|v| v["created_at"] = json!("2027-01-15T07:00:00Z+02:00")),
                reason: Some("created_at is not an RFC 3339 timestamp"),
            },
            Case {
                name: "a created_at on a day that does not exist",
                text: with(|v| v["created_at"] = json!("2027-02-30T07:00:00Z")),
                reason: Some("created_at is not an RFC 3339 timestamp"),
            },
            Case {
                name: "a posted_at that is not a timestamp",
                text: with(|v| v["posted_at"] = json!("soon")),
                reason: Some("posted_at is not an RFC 3339 timestamp"),
            },
            Case {
                name: "a comment review with nothing in it",
                text: with(|v| {
                    v["verdict"] = json!("comment");
                    v["body"] = json!("  \n");
                    v["comments"] = json!([]);
                }),
                reason: Some("a comment review needs a body or an inline comment"),
            },
        ]);
    }

    /// The decision posting hangs on: only a draft still awaiting a decision,
    /// written against the commit the pull request is at right now.
    #[test]
    fn only_a_draft_for_the_current_head_is_ready() {
        struct Case {
            name: &'static str,
            status: &'static str,
            head: Option<&'static str>,
            want: State,
        }
        const MOVED: &str = "9f8e7d6c5b4a39281706f5e4d3c2b1a098765432";
        let cases = [
            Case {
                name: "the head matches",
                status: "drafted",
                head: Some(HEAD),
                want: State::Ready,
            },
            Case {
                name: "the pull request moved on",
                status: "drafted",
                head: Some(MOVED),
                want: State::Stale,
            },
            Case {
                name: "the pull request was not fetched",
                status: "drafted",
                head: None,
                want: State::Unverified,
            },
            Case {
                name: "the fetch carried no head commit",
                status: "drafted",
                head: Some(""),
                want: State::Unverified,
            },
            Case {
                name: "posted stays posted whatever the head",
                status: "posted",
                head: Some(MOVED),
                want: State::Posted,
            },
            Case {
                name: "skipped stays skipped when the head matches",
                status: "skipped",
                head: Some(HEAD),
                want: State::Skipped,
            },
        ];
        for case in cases {
            let text = with(|v| v["status"] = json!(case.status));
            let draft = parse(&text, "acme/widget", 101);
            assert_eq!(state(&draft, case.head), case.want, "{}", case.name);
        }
        assert_eq!(
            state(&Err("not valid JSON".into()), Some(HEAD)),
            State::Invalid,
            "an invalid draft is never ready, whatever the head"
        );
    }

    /// The exact bytes sent. `note` and `picked` are for the reader of the
    /// draft and must never leave the machine.
    #[test]
    fn the_payload_is_the_review_and_nothing_else() {
        let d = parse(&sample().to_string(), "acme/widget", 101).unwrap();
        let sent = payload(&d).unwrap();
        assert_eq!(
            sent,
            format!(
                r#"{{"commit_id":"{HEAD}","event":"APPROVE","body":"Reads well.","comments":[{{"path":"src/x.rs","line":84,"side":"RIGHT","body":"Worth a test."}}]}}"#
            )
        );
        assert!(!sent.contains("PRIVATE"), "{sent}");

        let text = with(|v| {
            v["verdict"] = json!("comment");
            v["comments"] = json!([]);
        });
        let sent = payload(&parse(&text, "acme/widget", 101).unwrap()).unwrap();
        assert_eq!(
            sent,
            format!(
                r#"{{"commit_id":"{HEAD}","event":"COMMENT","body":"Reads well.","comments":[]}}"#
            )
        );
    }

    /// GitHub as one case of the table describes it. Records the calls made,
    /// in order, so a case can say not only what was decided but how much was
    /// asked before deciding.
    struct Github {
        pull: Result<Live, String>,
        reviews: Result<Vec<Review>, String>,
        post: Result<(), String>,
        calls: std::sync::Mutex<Vec<&'static str>>,
    }

    impl Remote for Github {
        fn pull(&self, _: u64) -> Result<Live, String> {
            self.calls.lock().unwrap().push("pull");
            self.pull.clone()
        }
        fn reviews(&self, _: u64) -> Result<Vec<Review>, String> {
            self.calls.lock().unwrap().push("reviews");
            self.reviews.clone()
        }
        fn post(&self, _: u64, _: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push("post");
            self.post.clone()
        }
    }

    const MOVED: &str = "9f8e7d6c5b4a39281706f5e4d3c2b1a098765432";
    /// The sample draft was written at 09:00.
    const BEFORE: &str = "2027-01-15T08:59:59Z";
    const AT: &str = "2027-01-15T09:00:00Z";
    const AFTER: &str = "2027-01-15T09:30:00Z";

    fn open_at(head: &str) -> Live {
        Live {
            open: true,
            merged: false,
            head: head.into(),
        }
    }

    /// A review that is exactly what the sample draft would send, by `author`.
    fn review(author: &str, at: &str) -> Review {
        Review {
            author: author.into(),
            state: "APPROVED".into(),
            commit: HEAD.into(),
            body: "Reads well.".into(),
            submitted_at: Some(at.into()),
        }
    }

    /// One situation on GitHub at the moment of posting, and what must come
    /// of it. Anything a case leaves out is the ordinary one: an open pull
    /// request at the draft's commit, no reviews, a post that is accepted.
    struct Moment {
        name: &'static str,
        viewer: &'static str,
        pull: Result<Live, String>,
        reviews: Result<Vec<Review>, String>,
        post: Result<(), String>,
        want: Outcome,
        calls: &'static [&'static str],
    }

    impl Default for Moment {
        fn default() -> Self {
            Self {
                name: "",
                viewer: "octocat",
                pull: Ok(open_at(HEAD)),
                reviews: Ok(Vec::new()),
                post: Ok(()),
                want: Outcome::Sent,
                calls: &["pull", "reviews", "post"],
            }
        }
    }

    fn live_through(moments: impl IntoIterator<Item = Moment>) {
        let draft = parse(&sample().to_string(), "acme/widget", 101).unwrap();
        for m in moments {
            let github = Github {
                pull: m.pull,
                reviews: m.reviews,
                post: m.post,
                calls: std::sync::Mutex::default(),
            };
            let got = deliver(&github, m.viewer, 101, &draft);
            assert_eq!(got, m.want, "{}", m.name);
            assert_eq!(*github.calls.lock().unwrap(), m.calls, "{}", m.name);
        }
    }

    /// Where the pull request is right now decides first, before anyone's
    /// reviews are even read.
    #[test]
    fn posting_is_decided_by_where_the_pull_request_is_now() {
        live_through([
            Moment {
                name: "open, at the draft's commit",
                ..Moment::default()
            },
            Moment {
                name: "merged, still at the draft's commit",
                pull: Ok(Live {
                    open: false,
                    merged: true,
                    head: HEAD.into(),
                }),
                ..Moment::default()
            },
            Moment {
                name: "the same commit, spelled in capitals",
                pull: Ok(open_at(&HEAD.to_uppercase())),
                ..Moment::default()
            },
            Moment {
                name: "closed without merging",
                pull: Ok(Live {
                    open: false,
                    merged: false,
                    head: HEAD.into(),
                }),
                want: Outcome::Refused(Refusal::Closed),
                calls: &["pull"],
                ..Moment::default()
            },
            Moment {
                name: "moved on",
                pull: Ok(open_at(MOVED)),
                want: Outcome::Refused(Refusal::Moved(MOVED.into())),
                calls: &["pull"],
                ..Moment::default()
            },
            Moment {
                name: "merged, but at another commit",
                pull: Ok(Live {
                    open: false,
                    merged: true,
                    head: MOVED.into(),
                }),
                want: Outcome::Refused(Refusal::Moved(MOVED.into())),
                calls: &["pull"],
                ..Moment::default()
            },
            Moment {
                name: "closed and moved: closed is the reason",
                pull: Ok(Live {
                    open: false,
                    merged: false,
                    head: MOVED.into(),
                }),
                want: Outcome::Refused(Refusal::Closed),
                calls: &["pull"],
                ..Moment::default()
            },
            Moment {
                name: "the pull request could not be read",
                pull: Err("timed out after 45s".into()),
                want: Outcome::Refused(Refusal::Unchecked("timed out after 45s".into())),
                calls: &["pull"],
                ..Moment::default()
            },
            Moment {
                name: "the reviews could not be read",
                reviews: Err("HTTP 502".into()),
                want: Outcome::Refused(Refusal::Unchecked("HTTP 502".into())),
                calls: &["pull", "reviews"],
                ..Moment::default()
            },
            Moment {
                name: "nobody knows who is asking",
                viewer: "",
                want: Outcome::Refused(Refusal::NoViewer),
                calls: &[],
                ..Moment::default()
            },
            Moment {
                name: "GitHub declines the review",
                post: Err("Unprocessable Entity (HTTP 422)".into()),
                want: Outcome::Refused(Refusal::Rejected("Unprocessable Entity (HTTP 422)".into())),
                ..Moment::default()
            },
        ]);
    }

    /// The viewer's own reviews since the draft was written: the same one
    /// means it was already sent, any other means it must not be.
    #[test]
    fn the_viewers_own_reviews_since_the_draft_decide_it() {
        let already = |at: &str| Outcome::AlreadySent(at.into());
        let reviewed = Outcome::Refused(Refusal::ReviewedSince);
        let read_only: &[&str] = &["pull", "reviews"];
        live_through([
            Moment {
                name: "the same review, after the draft",
                reviews: Ok(vec![review("octocat", AFTER)]),
                want: already(AFTER),
                calls: read_only,
                ..Moment::default()
            },
            Moment {
                name: "the same review, in the very second the draft was written",
                reviews: Ok(vec![review("octocat", AT)]),
                want: already(AT),
                calls: read_only,
                ..Moment::default()
            },
            Moment {
                name: "the login in another case, the body with stray whitespace",
                reviews: Ok(vec![Review {
                    body: "Reads well.\n".into(),
                    ..review("Octocat", AFTER)
                }]),
                want: already(AFTER),
                calls: read_only,
                ..Moment::default()
            },
            Moment {
                name: "other words",
                reviews: Ok(vec![Review {
                    body: "Not yet.".into(),
                    ..review("octocat", AFTER)
                }]),
                want: reviewed.clone(),
                calls: read_only,
                ..Moment::default()
            },
            Moment {
                name: "another verdict",
                reviews: Ok(vec![Review {
                    state: "COMMENTED".into(),
                    ..review("octocat", AFTER)
                }]),
                want: reviewed.clone(),
                calls: read_only,
                ..Moment::default()
            },
            Moment {
                name: "an approval since dismissed",
                reviews: Ok(vec![Review {
                    state: "DISMISSED".into(),
                    ..review("octocat", AFTER)
                }]),
                want: reviewed.clone(),
                calls: read_only,
                ..Moment::default()
            },
            Moment {
                name: "another commit",
                reviews: Ok(vec![Review {
                    commit: MOVED.into(),
                    ..review("octocat", AFTER)
                }]),
                want: reviewed,
                calls: read_only,
                ..Moment::default()
            },
            Moment {
                name: "the same review among others",
                reviews: Ok(vec![
                    Review {
                        body: "Not yet.".into(),
                        ..review("octocat", AFTER)
                    },
                    review("hubot", AFTER),
                    review("octocat", AFTER),
                ]),
                want: already(AFTER),
                calls: read_only,
                ..Moment::default()
            },
        ]);
    }

    /// Reviews that say nothing about this draft: it is sent as if they were
    /// not there.
    #[test]
    fn reviews_that_are_older_pending_or_someone_elses_do_not_count() {
        live_through([
            Moment {
                name: "the same review from before the draft is an older one",
                reviews: Ok(vec![review("octocat", BEFORE)]),
                ..Moment::default()
            },
            Moment {
                name: "someone else's identical review is not the viewer's",
                reviews: Ok(vec![review("hubot", AFTER)]),
                ..Moment::default()
            },
            Moment {
                name: "a pending review has not been submitted",
                reviews: Ok(vec![Review {
                    state: "PENDING".into(),
                    submitted_at: None,
                    ..review("octocat", AFTER)
                }]),
                ..Moment::default()
            },
        ]);
    }

    #[test]
    fn the_drafts_directory_comes_from_the_environment() {
        struct Case {
            name: &'static str,
            env: &'static [(&'static str, &'static str)],
            want: Option<&'static str>,
        }
        let cases = [
            Case {
                name: "RIGOR_DRAFTS_DIR wins",
                env: &[
                    ("RIGOR_DRAFTS_DIR", "/srv/drafts"),
                    ("XDG_DATA_HOME", "/data"),
                    ("HOME", "/home/dev"),
                ],
                want: Some("/srv/drafts"),
            },
            Case {
                name: "then XDG_DATA_HOME",
                env: &[("XDG_DATA_HOME", "/data"), ("HOME", "/home/dev")],
                want: Some("/data/rigor/drafts"),
            },
            Case {
                name: "then the home directory",
                env: &[("HOME", "/home/dev")],
                want: Some("/home/dev/.local/share/rigor/drafts"),
            },
            Case {
                name: "a variable set to nothing is unset",
                env: &[
                    ("RIGOR_DRAFTS_DIR", ""),
                    ("XDG_DATA_HOME", ""),
                    ("HOME", "/home/dev"),
                ],
                want: Some("/home/dev/.local/share/rigor/drafts"),
            },
            Case {
                name: "nothing to go on",
                env: &[],
                want: None,
            },
        ];
        for case in cases {
            let got = dir(|key| {
                case.env
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| OsString::from(v))
            });
            assert_eq!(got, case.want.map(PathBuf::from), "{}", case.name);
        }
    }

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let repo = dir.join("acme").join("widget");
        std::fs::create_dir_all(&repo).unwrap();
        let path = repo.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    /// Only an ordinary file of ordinary size is ever opened. Anything else
    /// under a draft's name is listed as invalid, with the reason, unread.
    #[cfg(unix)]
    #[test]
    fn only_a_regular_file_of_sane_size_is_read() {
        let d = tempfile::tempdir().unwrap();
        let real = write(d.path(), "101.json", &sample().to_string());
        let repo = real.parent().unwrap().to_path_buf();

        // A link to a perfectly good draft is still not the draft.
        std::os::unix::fs::symlink(&real, repo.join("102.json")).unwrap();
        std::fs::create_dir(repo.join("103.json")).unwrap();
        // A pipe nobody writes to: opening it for reading would never return.
        let fifo = std::ffi::CString::new(repo.join("104.json").to_str().unwrap()).unwrap();
        // SAFETY: mkfifo(2) reads a NUL-terminated path, which `fifo` is for
        // as long as this call, and touches no other memory.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let padded = with(|v| {
            v["number"] = json!(105);
            v["body"] = json!("x".repeat(300 * 1024));
        });
        write(d.path(), "105.json", &padded);

        let got = load(d.path(), "acme", "widget").unwrap();
        let why = |n: u64| {
            let entry = got.iter().find(|e| e.number == Some(n)).unwrap();
            entry.draft.clone().err()
        };
        assert_eq!(why(101), None);
        assert_eq!(why(102).unwrap(), "a symbolic link, not a regular file");
        assert_eq!(why(103).unwrap(), "not a regular file");
        assert_eq!(why(104).unwrap(), "not a regular file");
        assert_eq!(why(105).unwrap(), "larger than 256 KB");
    }

    #[test]
    fn loading_lists_every_json_file_and_only_those() {
        let d = tempfile::tempdir().unwrap();
        assert!(
            load(d.path(), "acme", "widget").unwrap().is_empty(),
            "no directory is no drafts, not an error"
        );

        write(d.path(), "101.json", &sample().to_string());
        write(d.path(), "7.json", "{");
        write(d.path(), "notes.json", &sample().to_string());
        write(d.path(), ".101.json.4242.tmp", "{");
        write(d.path(), "README.md", "not a draft");
        std::fs::create_dir_all(d.path().join("acme").join("gadget")).unwrap();
        std::fs::write(d.path().join("acme/gadget/5.json"), "{").unwrap();

        let got = load(d.path(), "acme", "widget").unwrap();
        let labels: Vec<String> = got.iter().map(Entry::label).collect();
        assert_eq!(labels, ["notes.json", "#7", "#101"]);
        assert!(
            got[0]
                .draft
                .as_ref()
                .unwrap_err()
                .contains("not named for a pull request number")
        );
        assert!(got[1].draft.is_err(), "malformed, but still listed");
        assert!(got[2].draft.is_ok());
        assert_eq!(got[2].raw, sample().to_string());
    }

    /// The only write rigor makes to a draft. Everything its author wrote
    /// comes back unchanged, including text JSON has to escape.
    #[test]
    fn a_status_rewrite_changes_only_the_status() {
        let d = tempfile::tempdir().unwrap();
        let text = with(|v| {
            v["body"] = json!("Line one.\n\n\"Quoted\" — and a tab:\there. ✓");
            v["note"] = json!("C:\\path and a \u{1f642}");
        });
        let path = write(d.path(), "101.json", &text);
        let before = parse(&text, "acme/widget", 101).unwrap();
        // A second name for the file as it is now: what a reader that already
        // had it open goes on seeing.
        let held = d.path().join("held-open");
        std::fs::hard_link(&path, &held).unwrap();

        set_status(
            &path,
            "acme/widget",
            Status::Posted,
            Some("2027-01-15T10:00:00Z".into()),
        )
        .unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let after = parse(&written, "acme/widget", 101).unwrap();
        assert_eq!(after.status, Status::Posted);
        assert_eq!(after.posted_at.as_deref(), Some("2027-01-15T10:00:00Z"));
        assert_eq!(
            Draft {
                status: before.status,
                posted_at: before.posted_at.clone(),
                ..after
            },
            before,
            "a field other than status and posted_at changed"
        );
        assert!(written.ends_with("}\n"), "a complete file: {written:?}");
        // Replaced by a rename, not written over in place: the old file is
        // whole for anyone still reading it, and the new one appears at once.
        assert_eq!(
            std::fs::read_to_string(&held).unwrap(),
            text,
            "the draft was rewritten in place"
        );

        let left: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|f| f.unwrap().file_name())
            .collect();
        assert_eq!(left, ["101.json"], "the temporary file was left behind");
    }

    /// A draft rigor cannot read is one it must not rewrite: there is no way
    /// to keep the other fields of a file it did not understand.
    #[test]
    fn an_invalid_draft_is_never_rewritten() {
        let d = tempfile::tempdir().unwrap();
        let text = with(|v| v["version"] = json!(2));
        let path = write(d.path(), "101.json", &text);
        let err = set_status(&path, "acme/widget", Status::Skipped, None).unwrap_err();
        assert!(err.contains("version 2"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
}

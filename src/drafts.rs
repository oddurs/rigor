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
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
    pub id: u64,
    pub author: String,
    /// `APPROVED`, `COMMENTED`, `CHANGES_REQUESTED`, `DISMISSED` or `PENDING`.
    pub state: String,
    pub commit: String,
    pub body: String,
    /// `None` while the review is still pending.
    pub submitted_at: Option<String>,
}

/// How a write that did not succeed ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostFailure {
    /// GitHub answered, and the answer was no. Nothing was posted.
    Declined(String),
    /// No answer came back: a timeout, a dropped connection, a gateway error.
    /// The review may or may not be on the pull request.
    NoAnswer(String),
}

/// What posting needs from GitHub, for one repository. `main` wires in `gh`;
/// a test hands over a recorder, so nothing built in a test can reach GitHub.
pub trait Remote: Send + Sync {
    fn pull(&self, number: u64) -> Result<Live, String>;
    /// Every review on the pull request, all pages.
    fn reviews(&self, number: u64) -> Result<Vec<Review>, String>;
    /// The inline comments of one review, all pages.
    fn review_comments(&self, number: u64, review: u64) -> Result<Vec<Comment>, String>;
    fn post(&self, number: u64, body: &str) -> Result<(), PostFailure>;
    /// How long any one of these calls may take.
    fn timeout(&self) -> Duration;
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
    /// The viewer has a review started and not submitted. GitHub allows one.
    Pending,
    /// Who is asking is not known, so their reviews cannot be told from
    /// anyone else's.
    NoViewer,
    /// GitHub could not be asked.
    Unchecked(String),
    /// GitHub was asked to take the review and said no.
    Rejected(String),
    /// Another rigor is posting this same draft right now.
    Busy,
}

/// How an attempt to post ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Sent,
    /// The same review is already on the pull request, submitted at this
    /// time. Nothing was sent.
    AlreadySent(String),
    Refused(Refusal),
    /// The review was sent and no answer came back. Whether it is on the
    /// pull request is not known; asking again will find out before sending.
    Unknown(String),
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

/// Inline comments as a thing to compare: every one, in no particular order,
/// with the whitespace GitHub may have trimmed from the ends left out.
fn comment_set(comments: &[Comment]) -> Vec<(&str, u64, Side, &str)> {
    let mut set: Vec<_> = comments
        .iter()
        .map(|c| (c.path.as_str(), c.line, c.side, c.body.trim()))
        .collect();
    set.sort_unstable();
    set
}

/// The viewer's own reviews decide whether the draft is still theirs to send.
///
/// The same review — verdict, commit, body and every inline comment — means
/// it already went out, whenever that was: a post that timed out, or was cut
/// off by quitting, still landed. The verdict, commit and body alone are not
/// enough to say so: a reply typed in the browser is a comment review with no
/// body, just like a draft that is all inline comments. So a review that
/// looks the same has its comments read and compared before it is believed.
///
/// Failing that, a review of theirs since the draft was written means they
/// have said something else, and one still pending means GitHub would refuse
/// another. Either way the draft is not sent on top of it.
fn check_reviews(
    remote: &dyn Remote,
    number: u64,
    d: &Draft,
    viewer: &str,
    reviews: &[Review],
) -> Result<Option<Outcome>, String> {
    let mine: Vec<&Review> = reviews
        .iter()
        .filter(|r| r.author.eq_ignore_ascii_case(viewer))
        .collect();
    let looks_the_same = |r: &&&Review| {
        r.state == d.verdict.submitted_as()
            && r.commit.eq_ignore_ascii_case(&d.head_sha)
            && r.body.trim() == d.body.trim()
    };
    for candidate in mine.iter().filter(looks_the_same) {
        let theirs = remote.review_comments(number, candidate.id)?;
        if comment_set(&theirs) == comment_set(&d.comments) {
            return Ok(Some(Outcome::AlreadySent(
                candidate.submitted_at.clone().unwrap_or_default(),
            )));
        }
    }

    // Checked when the draft was read. Were it somehow not a time, every
    // review counts as since: the doubt must not end in a second post.
    let written = parse_iso8601(&d.created_at).unwrap_or(i64::MIN);
    let since = mine.iter().any(|r| {
        r.submitted_at
            .as_deref()
            .and_then(parse_iso8601)
            .is_some_and(|at| at >= written)
    });
    if since {
        return Ok(Some(Outcome::Refused(Refusal::ReviewedSince)));
    }
    if mine.iter().any(|r| r.state == "PENDING") {
        return Ok(Some(Outcome::Refused(Refusal::Pending)));
    }
    Ok(None)
}

/// Post `d` to pull request `number`, if GitHub's own account of the pull
/// request, read here and now, still allows it. Reads, then at most one
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
    let settled = remote
        .reviews(number)
        .and_then(|reviews| check_reviews(remote, number, d, viewer, &reviews));
    match settled {
        Ok(Some(outcome)) => return outcome,
        Ok(None) => {}
        Err(e) => return Outcome::Refused(Refusal::Unchecked(e)),
    }
    let body = match payload(d) {
        Ok(body) => body,
        Err(e) => {
            return Outcome::Refused(Refusal::Rejected(format!(
                "could not build the review: {e}"
            )));
        }
    };
    match remote.post(number, &body) {
        Ok(()) => Outcome::Sent,
        Err(PostFailure::Declined(e)) => Outcome::Refused(Refusal::Rejected(e)),
        Err(PostFailure::NoAnswer(e)) => Outcome::Unknown(e),
    }
}

/// How much longer than one call a lock may sit before it is taken for one
/// its owner left behind.
const LOCK_MARGIN: Duration = Duration::from_secs(15);

/// A lock file beside a draft, held for as long as one rigor is reading and
/// posting for it, so that two windows on the same repository cannot both
/// send it. Created with `O_EXCL`, so only one of them can; removed when the
/// attempt is over.
struct Lock {
    path: PathBuf,
}

impl Lock {
    fn beside(draft: &Path) -> PathBuf {
        let name = draft
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        // Not `*.json`, so the loader never takes it for a draft.
        draft.with_file_name(format!(".{name}.lock"))
    }

    /// `None` when another rigor holds it. A lock not touched for longer
    /// than a call can last, plus a margin, was left by one that died, and
    /// is replaced.
    fn take(draft: &Path, call: Duration) -> std::io::Result<Option<Self>> {
        let path = Self::beside(draft);
        for _ in 0..2 {
            let made = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path);
            match made {
                Ok(mut file) => {
                    let _ = writeln!(file, "{}", std::process::id());
                    return Ok(Some(Self { path }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let age = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|at| at.elapsed().ok());
                    // No age to read means it was just removed, or its clock
                    // is ahead of ours: either way, try once more.
                    if age.is_some_and(|age| age <= call + LOCK_MARGIN) {
                        return Ok(None);
                    }
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    /// Say the owner is still alive. Called before every call to GitHub, so
    /// the lock's age is the age of the call in flight, not of the attempt.
    fn touch(&self) {
        if let Ok(file) = std::fs::File::options().write(true).open(&self.path) {
            let _ = file.set_modified(std::time::SystemTime::now());
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A remote that keeps a lock fresh for as long as it is being used.
struct Held<'a> {
    remote: &'a dyn Remote,
    lock: &'a Lock,
}

impl Remote for Held<'_> {
    fn pull(&self, number: u64) -> Result<Live, String> {
        self.lock.touch();
        self.remote.pull(number)
    }
    fn reviews(&self, number: u64) -> Result<Vec<Review>, String> {
        self.lock.touch();
        self.remote.reviews(number)
    }
    fn review_comments(&self, number: u64, review: u64) -> Result<Vec<Comment>, String> {
        self.lock.touch();
        self.remote.review_comments(number, review)
    }
    fn post(&self, number: u64, body: &str) -> Result<(), PostFailure> {
        self.lock.touch();
        self.remote.post(number, body)
    }
    fn timeout(&self) -> Duration {
        self.remote.timeout()
    }
}

/// `deliver`, with the draft at `path` locked against any other rigor for
/// the whole of it: the reads and the write are one attempt, and two
/// attempts at once could each find nothing posted and both post.
pub fn deliver_exclusively(
    remote: &dyn Remote,
    viewer: &str,
    number: u64,
    path: &Path,
    d: &Draft,
) -> Outcome {
    match Lock::take(path, remote.timeout()) {
        Ok(Some(lock)) => deliver(
            &Held {
                remote,
                lock: &lock,
            },
            viewer,
            number,
            d,
        ),
        Ok(None) => Outcome::Refused(Refusal::Busy),
        Err(e) => Outcome::Refused(Refusal::Unchecked(format!("could not lock the draft: {e}"))),
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

/// Replace the file at `path` with `draft`. Written beside the file, flushed
/// to disk, and renamed over it, so nothing reading the directory ever sees
/// half a draft and a crash leaves either the old file or the new one. The
/// new file keeps the old one's permissions.
fn replace(path: &Path, draft: &Draft) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(draft).map_err(|e| e.to_string())?;
    text.push('\n');

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        if let Ok(meta) = std::fs::metadata(path) {
            file.set_permissions(meta.permissions())?;
        }
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    written.map_err(|e: std::io::Error| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// Set a draft's `status` and `posted_at`, leaving every other field as it is
/// on disk right now — read fresh, so an edit made since the list was loaded
/// survives. A file rigor cannot read as a draft, a symbolic link among them,
/// is never rewritten.
pub fn set_status(
    path: &Path,
    slug: &str,
    status: Status,
    posted_at: Option<String>,
) -> Result<(), String> {
    let mut draft = read(path.to_path_buf(), slug).draft?;
    draft.status = status;
    draft.posted_at = posted_at;
    replace(path, &draft)
}

/// Record that `sent` is on GitHub, posted at `at`. `raw` is the file as it
/// was when `sent` was read from it.
///
/// If the file still says that, only its status changes. If it says anything
/// else by now — edited, regenerated, gone — then what it says is not what
/// was posted, and marking *that* posted would be a false record: the draft
/// that was sent is written back instead. Returns whether it had to be.
pub fn record_posted(path: &Path, raw: &str, sent: &Draft, at: String) -> Result<bool, String> {
    // A link is not followed to be overwritten, whatever it points at.
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(format!("{}: a symbolic link", path.display()));
    }
    let changed = read_text(path).ok().as_deref() != Some(raw);
    let posted = Draft {
        status: Status::Posted,
        posted_at: Some(at),
        ..sent.clone()
    };
    replace(path, &posted).map(|()| changed)
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
        comments: Result<Vec<Comment>, String>,
        post: Result<(), PostFailure>,
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
        fn review_comments(&self, _: u64, _: u64) -> Result<Vec<Comment>, String> {
            self.calls.lock().unwrap().push("comments");
            self.comments.clone()
        }
        fn post(&self, _: u64, _: &str) -> Result<(), PostFailure> {
            self.calls.lock().unwrap().push("post");
            self.post.clone()
        }
        fn timeout(&self) -> Duration {
            Duration::from_secs(45)
        }
    }

    const MOVED: &str = "9f8e7d6c5b4a39281706f5e4d3c2b1a098765432";
    /// The sample draft was written at 07:00. The frozen clock of the screen
    /// tests is 08:00, which is when the app would have posted it.
    const BEFORE: &str = "2027-01-15T06:59:59Z";
    const AT: &str = "2027-01-15T07:00:00Z";
    const AFTER: &str = "2027-01-15T08:00:00Z";

    fn open_at(head: &str) -> Live {
        Live {
            open: true,
            merged: false,
            head: head.into(),
        }
    }

    /// A review that reads exactly as the sample draft would, by `author`.
    fn review(author: &str, at: &str) -> Review {
        Review {
            id: 7,
            author: author.into(),
            state: "APPROVED".into(),
            commit: HEAD.into(),
            body: "Reads well.".into(),
            submitted_at: Some(at.into()),
        }
    }

    /// The sample draft's one inline comment, as GitHub would list it.
    fn its_comment() -> Comment {
        Comment {
            path: "src/x.rs".into(),
            line: 84,
            side: Side::Right,
            body: "Worth a test.".into(),
        }
    }

    /// One situation on GitHub at the moment of posting, and what must come
    /// of it. Anything a case leaves out is the ordinary one: an open pull
    /// request at the draft's commit, no reviews, a post that is accepted —
    /// and, should a review's comments be asked for, the draft's own.
    struct Moment {
        name: &'static str,
        viewer: &'static str,
        pull: Result<Live, String>,
        reviews: Result<Vec<Review>, String>,
        comments: Result<Vec<Comment>, String>,
        post: Result<(), PostFailure>,
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
                comments: Ok(vec![its_comment()]),
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
                comments: m.comments,
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
        ]);
    }

    /// A write that GitHub answers with a no is a no. One that gets no
    /// answer at all is not: the review may be there.
    #[test]
    fn only_an_answer_from_github_says_the_review_was_not_posted() {
        live_through([
            Moment {
                name: "GitHub declines the review",
                post: Err(PostFailure::Declined(
                    "Unprocessable Entity (HTTP 422)".into(),
                )),
                want: Outcome::Refused(Refusal::Rejected("Unprocessable Entity (HTTP 422)".into())),
                ..Moment::default()
            },
            Moment {
                name: "the write times out",
                post: Err(PostFailure::NoAnswer("timed out after 45s".into())),
                want: Outcome::Unknown("timed out after 45s".into()),
                ..Moment::default()
            },
        ]);
    }

    /// The very same review is on the pull request already: it was sent,
    /// whenever that was, and is not sent again.
    #[test]
    fn the_same_review_already_there_was_already_sent() {
        let already = |at: &str| Outcome::AlreadySent(at.into());
        let read_only: &[&str] = &["pull", "reviews", "comments"];
        live_through([
            Moment {
                name: "submitted when the app would have posted it",
                reviews: Ok(vec![review("octocat", AFTER)]),
                want: already(AFTER),
                calls: read_only,
                ..Moment::default()
            },
            Moment {
                name: "in the very second the draft was written",
                reviews: Ok(vec![review("octocat", AT)]),
                want: already(AT),
                calls: read_only,
                ..Moment::default()
            },
            Moment {
                name: "before the draft says it was written: a clock is wrong, not the review",
                reviews: Ok(vec![review("octocat", BEFORE)]),
                want: already(BEFORE),
                calls: read_only,
                ..Moment::default()
            },
            Moment {
                name: "the login in another case, the words with stray whitespace",
                reviews: Ok(vec![Review {
                    body: "Reads well.\n".into(),
                    ..review("Octocat", AFTER)
                }]),
                comments: Ok(vec![Comment {
                    body: " Worth a test.\n".into(),
                    ..its_comment()
                }]),
                want: already(AFTER),
                calls: read_only,
                ..Moment::default()
            },
            Moment {
                name: "among other reviews, and with one of the viewer's still pending",
                reviews: Ok(vec![
                    Review {
                        body: "Not yet.".into(),
                        ..review("octocat", AFTER)
                    },
                    review("hubot", AFTER),
                    Review {
                        state: "PENDING".into(),
                        submitted_at: None,
                        ..review("octocat", AFTER)
                    },
                    review("octocat", AFTER),
                ]),
                want: already(AFTER),
                calls: read_only,
                ..Moment::default()
            },
        ]);
    }

    /// The verdict, the commit and the words can all agree and the review
    /// still be another one: only its inline comments say. A review that
    /// merely looks the same is not taken for the draft having been sent.
    #[test]
    fn a_review_that_only_looks_the_same_is_not_the_same() {
        let reviewed = Outcome::Refused(Refusal::ReviewedSince);
        let compared: &[&str] = &["pull", "reviews", "comments"];
        let but = |edit: fn(&mut Comment)| {
            let mut c = its_comment();
            edit(&mut c);
            Ok(vec![c])
        };
        live_through([
            Moment {
                name: "no inline comments where the draft has one",
                reviews: Ok(vec![review("octocat", AFTER)]),
                comments: Ok(Vec::new()),
                want: reviewed.clone(),
                calls: compared,
                ..Moment::default()
            },
            Moment {
                name: "a comment with other words",
                reviews: Ok(vec![review("octocat", AFTER)]),
                comments: but(|c| c.body = "Worth two tests.".into()),
                want: reviewed.clone(),
                calls: compared,
                ..Moment::default()
            },
            Moment {
                name: "a comment on another line",
                reviews: Ok(vec![review("octocat", AFTER)]),
                comments: but(|c| c.line = 85),
                want: reviewed.clone(),
                calls: compared,
                ..Moment::default()
            },
            Moment {
                name: "a comment on the other side",
                reviews: Ok(vec![review("octocat", AFTER)]),
                comments: but(|c| c.side = Side::Left),
                want: reviewed.clone(),
                calls: compared,
                ..Moment::default()
            },
            Moment {
                name: "a comment on another file",
                reviews: Ok(vec![review("octocat", AFTER)]),
                comments: but(|c| c.path = "src/y.rs".into()),
                want: reviewed.clone(),
                calls: compared,
                ..Moment::default()
            },
            Moment {
                name: "one comment more than the draft has",
                reviews: Ok(vec![review("octocat", AFTER)]),
                comments: Ok(vec![its_comment(), its_comment()]),
                want: reviewed,
                calls: compared,
                ..Moment::default()
            },
            Moment {
                name: "from before the draft, and not the same: an older review",
                reviews: Ok(vec![review("octocat", BEFORE)]),
                comments: Ok(Vec::new()),
                calls: &["pull", "reviews", "comments", "post"],
                ..Moment::default()
            },
            Moment {
                name: "its comments could not be read",
                reviews: Ok(vec![review("octocat", AFTER)]),
                comments: Err("HTTP 502".into()),
                want: Outcome::Refused(Refusal::Unchecked("HTTP 502".into())),
                calls: compared,
                ..Moment::default()
            },
        ]);
    }

    /// The viewer's other reviews: one since the draft was written means
    /// they have said something else; one still pending means GitHub would
    /// refuse another. Neither is posted on top of.
    #[test]
    fn the_viewers_other_reviews_block_the_draft() {
        let reviewed = Outcome::Refused(Refusal::ReviewedSince);
        let read_only: &[&str] = &["pull", "reviews"];
        let pending = |author: &str| Review {
            state: "PENDING".into(),
            submitted_at: None,
            body: String::new(),
            ..review(author, AFTER)
        };
        live_through([
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
                want: reviewed.clone(),
                calls: read_only,
                ..Moment::default()
            },
            Moment {
                name: "a review started in the browser and not submitted",
                reviews: Ok(vec![pending("octocat")]),
                want: Outcome::Refused(Refusal::Pending),
                calls: read_only,
                ..Moment::default()
            },
            Moment {
                name: "one pending, and another since: the one since is the reason",
                reviews: Ok(vec![
                    pending("octocat"),
                    Review {
                        body: "Not yet.".into(),
                        ..review("octocat", AFTER)
                    },
                ]),
                want: reviewed,
                calls: read_only,
                ..Moment::default()
            },
        ]);
    }

    /// Reviews that say nothing about this draft: it is sent as if they were
    /// not there.
    #[test]
    fn reviews_that_are_older_or_someone_elses_do_not_count() {
        live_through([
            Moment {
                name: "other words from before the draft",
                reviews: Ok(vec![Review {
                    body: "Not yet.".into(),
                    ..review("octocat", BEFORE)
                }]),
                ..Moment::default()
            },
            Moment {
                name: "someone else's identical review is not the viewer's",
                reviews: Ok(vec![review("hubot", AFTER)]),
                ..Moment::default()
            },
            Moment {
                name: "someone else's pending review is theirs to finish",
                reviews: Ok(vec![Review {
                    state: "PENDING".into(),
                    submitted_at: None,
                    ..review("hubot", AFTER)
                }]),
                ..Moment::default()
            },
        ]);
    }

    /// The case that showed the gap: a draft that is all inline comments, and
    /// a reply typed in the browser. Both are a comment review with no body
    /// on the same commit. Only one of them is the draft.
    #[test]
    fn a_reply_in_the_browser_is_not_a_comment_only_draft_already_sent() {
        let text = with(|v| {
            v["verdict"] = json!("comment");
            v["body"] = json!("");
        });
        let draft = parse(&text, "acme/widget", 101).unwrap();
        let reply = Review {
            state: "COMMENTED".into(),
            body: String::new(),
            ..review("octocat", AFTER)
        };
        let github = |comments: Vec<Comment>| Github {
            pull: Ok(open_at(HEAD)),
            reviews: Ok(vec![reply.clone()]),
            comments: Ok(comments),
            post: Ok(()),
            calls: std::sync::Mutex::default(),
        };

        let replied = github(vec![Comment {
            path: "src/y.rs".into(),
            line: 3,
            side: Side::Right,
            body: "Good point, done.".into(),
        }]);
        assert_eq!(
            deliver(&replied, "octocat", 101, &draft),
            Outcome::Refused(Refusal::ReviewedSince),
            "the draft's comments were never sent, and it must not be marked posted"
        );

        // The draft itself, posted earlier, is found by its comments —
        // in whatever order GitHub lists them.
        let two = with(|v| {
            v["verdict"] = json!("comment");
            v["body"] = json!("");
            v["comments"] = json!([
                { "path": "src/x.rs", "line": 84, "side": "RIGHT", "body": "Worth a test." },
                { "path": "src/a.rs", "line": 2, "side": "LEFT", "body": "Why was this dropped?" }
            ]);
        });
        let draft = parse(&two, "acme/widget", 101).unwrap();
        let mut listed = draft.comments.clone();
        listed.reverse();
        assert_eq!(
            deliver(&github(listed), "octocat", 101, &draft),
            Outcome::AlreadySent(AFTER.into())
        );
    }

    /// Two windows on one repository must not both post a draft: each could
    /// read GitHub, find nothing there yet, and send. The lock beside the
    /// draft lets one of them through.
    #[test]
    fn a_draft_being_posted_elsewhere_is_left_to_it() {
        let d = tempfile::tempdir().unwrap();
        let path = write(d.path(), "101.json", &sample().to_string());
        let draft = parse(&sample().to_string(), "acme/widget", 101).unwrap();
        let lock = Lock::beside(&path);
        let github = || Github {
            pull: Ok(open_at(HEAD)),
            reviews: Ok(Vec::new()),
            comments: Ok(Vec::new()),
            post: Ok(()),
            calls: std::sync::Mutex::default(),
        };

        // Another rigor is in the middle of it.
        std::fs::write(&lock, "4242\n").unwrap();
        let gh = github();
        assert_eq!(
            deliver_exclusively(&gh, "octocat", 101, &path, &draft),
            Outcome::Refused(Refusal::Busy)
        );
        assert!(
            gh.calls.lock().unwrap().is_empty(),
            "GitHub was asked anyway"
        );
        assert!(lock.exists(), "another window's lock was removed");

        // That one died without letting go: longer ago than any call lasts.
        let long_ago = std::time::SystemTime::now() - (gh.timeout() + LOCK_MARGIN * 2);
        std::fs::File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        let gh = github();
        assert_eq!(
            deliver_exclusively(&gh, "octocat", 101, &path, &draft),
            Outcome::Sent
        );
        assert!(!lock.exists(), "the lock outlived the attempt");
        assert!(
            load(d.path(), "acme", "widget").unwrap().len() == 1,
            "the lock was never taken for a draft"
        );
    }

    /// The lock is held for the whole attempt and kept fresh through it, so a
    /// second window arriving at any point finds it.
    #[test]
    fn the_lock_is_held_through_every_call_and_let_go_after() {
        struct Watching {
            lock: PathBuf,
            seen: std::sync::Mutex<Vec<bool>>,
        }
        impl Watching {
            fn look(&self) {
                // Aged before each call returns: only a touch at the start
                // of the next call can make it fresh again.
                let fresh = std::fs::metadata(&self.lock)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|at| at.elapsed().ok())
                    .is_some_and(|age| age < Duration::from_secs(30));
                self.seen.lock().unwrap().push(fresh);
                let old = std::time::SystemTime::now() - Duration::from_secs(3600);
                let file = std::fs::File::options()
                    .write(true)
                    .open(&self.lock)
                    .unwrap();
                file.set_modified(old).unwrap();
            }
        }
        impl Remote for Watching {
            fn pull(&self, _: u64) -> Result<Live, String> {
                self.look();
                Ok(open_at(HEAD))
            }
            fn reviews(&self, _: u64) -> Result<Vec<Review>, String> {
                self.look();
                Ok(vec![review("octocat", BEFORE)])
            }
            fn review_comments(&self, _: u64, _: u64) -> Result<Vec<Comment>, String> {
                self.look();
                Ok(Vec::new())
            }
            fn post(&self, _: u64, _: &str) -> Result<(), PostFailure> {
                self.look();
                Ok(())
            }
            fn timeout(&self) -> Duration {
                Duration::from_secs(45)
            }
        }
        let d = tempfile::tempdir().unwrap();
        let path = write(d.path(), "101.json", &sample().to_string());
        let draft = parse(&sample().to_string(), "acme/widget", 101).unwrap();
        let gh = Watching {
            lock: Lock::beside(&path),
            seen: std::sync::Mutex::default(),
        };
        assert_eq!(
            deliver_exclusively(&gh, "octocat", 101, &path, &draft),
            Outcome::Sent
        );
        assert_eq!(
            *gh.seen.lock().unwrap(),
            [true, true, true, true],
            "held and fresh at the pull, the reviews, the comments and the post"
        );
        assert!(!gh.lock.exists());
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

    /// The rewritten file is the same file to everything else: it keeps the
    /// permissions its author gave it, rather than whatever a new file gets.
    #[cfg(unix)]
    #[test]
    fn a_status_rewrite_keeps_the_files_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let path = write(d.path(), "101.json", &sample().to_string());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        set_status(&path, "acme/widget", Status::Skipped, None).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
    }

    /// A draft that is a link is never written through, by either of the
    /// writes rigor makes: whatever it points at is not rigor's to change.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_draft_is_never_written_through() {
        let d = tempfile::tempdir().unwrap();
        let text = sample().to_string();
        let target = d.path().join("elsewhere.json");
        std::fs::write(&target, &text).unwrap();
        let link = write(d.path(), "placeholder", "").with_file_name("101.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let draft = parse(&text, "acme/widget", 101).unwrap();

        let err = set_status(&link, "acme/widget", Status::Skipped, None).unwrap_err();
        assert!(err.contains("symbolic link"), "{err}");
        let err = record_posted(&link, &text, &draft, AFTER.into()).unwrap_err();
        assert!(err.contains("symbolic link"), "{err}");

        assert_eq!(std::fs::read_to_string(&target).unwrap(), text);
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    /// What is marked posted is what was posted. A file that still says what
    /// was sent only changes status; one that has come to say anything else
    /// is put back to what was sent, and the caller is told.
    #[test]
    fn what_is_recorded_as_posted_is_what_was_sent() {
        let d = tempfile::tempdir().unwrap();
        let text = sample().to_string();
        let sent = parse(&text, "acme/widget", 101).unwrap();
        let posted = Draft {
            status: Status::Posted,
            posted_at: Some(AFTER.into()),
            ..sent.clone()
        };
        let on_disk = |path: &Path| {
            parse(&std::fs::read_to_string(path).unwrap(), "acme/widget", 101).unwrap()
        };

        let path = write(d.path(), "101.json", &text);
        assert_eq!(record_posted(&path, &text, &sent, AFTER.into()), Ok(false));
        assert_eq!(on_disk(&path), posted);

        // Edited while the review was on its way: another verdict, other
        // words. None of that went out.
        let edited = with(|v| {
            v["verdict"] = json!("comment");
            v["body"] = json!("Something else entirely.");
            v["comments"] = json!([]);
        });
        std::fs::write(&path, &edited).unwrap();
        assert_eq!(record_posted(&path, &text, &sent, AFTER.into()), Ok(true));
        assert_eq!(on_disk(&path), posted, "the edit was marked as posted");

        // Gone altogether: the record of what was sent is still written.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(record_posted(&path, &text, &sent, AFTER.into()), Ok(true));
        assert_eq!(on_disk(&path), posted);
    }
}

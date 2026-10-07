//! Review drafts: files some other tool leaves beside a pull request, which
//! rigor lists.
//!
//! rigor does not write drafts and does not know who does. The file is the
//! whole contract: `<drafts dir>/<owner>/<repo>/<number>.json`.

use serde::{Deserialize, Deserializer};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::util::parse_iso8601;

/// The one file format this rigor reads.
const VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Drafted,
    Posted,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Comment {
    pub path: String,
    pub line: u64,
    pub side: Side,
    pub body: String,
}

/// The file, field for field. An unknown key is an error, as it is in a
/// config file: a draft rigor does not fully understand is not one it should
/// vouch for.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
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
    let draft = match (read_text(&path), number) {
        (Ok(text), Some(n)) => parse(&text, slug, n),
        (Ok(_), None) => Err("the file is not named for a pull request number".into()),
        (Err(why), _) => Err(why),
    };
    Entry {
        path,
        number,
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
    }
}

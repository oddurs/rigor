//! The detail pane: everything about the selected PR, including every check run
//! with its own state, duration and clickable link.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Desk, DraftPr, WtState};
use crate::drafts::{Draft, Entry, State, Status, Verdict};
use crate::model::{CheckState, Mergeable, Pr, Worktree};
use crate::theme::Theme;
use crate::util::{
    cell, cols, dur_short, now_secs, pad, parse_iso8601, rel_time, right, truncate, wrap,
};

pub fn draw(f: &mut Frame<'_>, area: Rect, app: &mut App) {
    if area.width < 4 || area.height < 2 {
        return;
    }
    let t = app.theme;
    let now = now_secs();

    // (line index -> check index), resolved to screen rows once we know the scroll.
    let mut check_lines: Vec<(usize, usize)> = Vec::new();
    let mut lines: Vec<Line<'_>> = Vec::new();

    match (
        app.selected_draft(),
        app.selected_pr(),
        app.selected_worktree(),
    ) {
        (Some(entry), _, _) => draft_detail(
            &mut lines,
            entry,
            (app.draft_state(entry), app.draft_head(entry)),
            app.draft_pr(entry),
            now,
            &t,
            area.width as usize,
        ),
        (None, Some(pr), wt) => pr_detail(
            &mut lines,
            &mut check_lines,
            pr,
            wt,
            now,
            &t,
            area.width as usize,
        ),
        (None, None, Some(wt)) => wt_detail(
            &mut lines,
            &app.desk(wt),
            app.settings.worktree_status,
            app.home.as_deref(),
            now,
            &t,
        ),
        // The list pane already explains an empty view; saying it twice is noise.
        (None, None, None) => {}
    }

    let max_scroll = cols(lines.len().saturating_sub(usize::from(area.height)));
    app.detail_scroll = app.detail_scroll.min(max_scroll);
    let scroll = app.detail_scroll;

    for (li, ci) in check_lines {
        if li >= scroll as usize {
            let y = area.y + cols(li - usize::from(scroll));
            if y < area.bottom() {
                app.hits.checks.push((y, ci));
            }
        }
    }

    // A draft is read here before it is decided, so one that runs past the
    // pane says so rather than just stopping.
    let more = app.selected_draft().is_some() && scroll < max_scroll;

    let lines: Vec<Line<'_>> = lines
        .into_iter()
        .map(|l| super::clip(l, area.width as usize))
        .collect();
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), area);

    if more {
        let text = " ↓ more below · J ";
        let w = cols(crate::util::width(text)).min(area.width);
        let corner = Rect::new(area.right() - w, area.bottom() - 1, w, 1);
        f.render_widget(
            Paragraph::new(Span::styled(text, Style::new().fg(t.pending))),
            corner,
        );
    }
}

/// Where values begin: two columns of margin plus a ten-column label.
const VALUE_COL: usize = 12;

fn field<'a>(k: &'a str, t: &Theme) -> Span<'a> {
    Span::styled(format!("  {}", pad(k, 10)), Style::new().fg(t.muted))
}

fn pr_detail(
    lines: &mut Vec<Line<'static>>,
    check_lines: &mut Vec<(usize, usize)>,
    pr: &Pr,
    wt: Option<&Worktree>,
    now: i64,
    t: &Theme,
    width: usize,
) {
    pr_header(lines, pr, now, t, width);
    pr_fields(lines, pr, t);
    worktree_line(lines, wt, now, t);
    checks(lines, check_lines, pr, now, t, width);
}

/// Number, title and draft marker; then author, age, diff size and comments.
fn pr_header(lines: &mut Vec<Line<'static>>, pr: &Pr, now: i64, t: &Theme, width: usize) {
    let mut head = vec![
        Span::styled(
            format!(" #{} ", pr.number),
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            truncate(
                &pr.title,
                width.saturating_sub(if pr.is_draft { 18 } else { 10 }),
            ),
            Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
        ),
    ];
    if pr.is_draft {
        head.push(Span::styled("  draft", Style::new().fg(t.warn)));
    }
    lines.push(Line::from(head));

    let muted = Style::new().fg(t.muted);
    let plural = |n: u32, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} · {} ago · ", pr.author, rel_time(pr.updated_at, now)),
            muted,
        ),
        Span::styled(format!("+{}", pr.additions), Style::new().fg(t.success)),
        Span::styled(" ", muted),
        Span::styled(format!("−{}", pr.deletions), Style::new().fg(t.failure)),
        Span::styled(
            format!(
                " · {} · {}",
                plural(pr.changed_files, "file"),
                plural(pr.comments, "comment")
            ),
            muted,
        ),
    ]));
}

/// Review state, mergeability (and the base it merges into), labels, branch.
fn pr_fields(lines: &mut Vec<Line<'static>>, pr: &Pr, t: &Theme) {
    let (rv_text, rv_color) = match pr.review_decision {
        Some(d) => (
            d.label().to_string(),
            match d {
                crate::model::ReviewDecision::Approved => t.success,
                crate::model::ReviewDecision::ChangesRequested => t.failure,
                crate::model::ReviewDecision::ReviewRequired => t.pending,
            },
        ),
        None => ("no review required".to_string(), t.muted),
    };
    let mut rv = vec![
        field("review", t),
        Span::styled(rv_text, Style::new().fg(rv_color)),
    ];
    if !pr.review_requests.is_empty() {
        rv.push(Span::styled(
            format!("  ← {}", pr.review_requests.join(", ")),
            Style::new().fg(t.muted),
        ));
    }
    if !pr.assignees.is_empty() {
        rv.push(Span::styled(
            format!("  ⊙ {}", pr.assignees.join(", ")),
            Style::new().fg(t.muted),
        ));
    }
    lines.push(Line::from(rv));

    let (m_text, m_color) = match pr.mergeable {
        Mergeable::Clean => ("no conflicts".to_string(), t.success),
        Mergeable::Conflicting => ("conflicts with base".to_string(), t.failure),
        Mergeable::Unknown => ("checking…".to_string(), t.muted),
    };
    lines.push(Line::from(vec![
        field("merge", t),
        Span::styled(m_text, Style::new().fg(m_color)),
        Span::styled(format!("  → {}", pr.base_ref), Style::new().fg(t.muted)),
    ]));

    if !pr.labels.is_empty() {
        lines.push(Line::from(vec![
            field("labels", t),
            Span::styled(pr.labels.join(", "), Style::new().fg(t.muted)),
        ]));
    }

    lines.push(Line::from(vec![
        field("branch", t),
        Span::styled(pr.head_ref.clone(), Style::new().fg(t.fg)),
    ]));
}

fn worktree_line(lines: &mut Vec<Line<'static>>, wt: Option<&Worktree>, now: i64, t: &Theme) {
    // The worktree line answers "which agent is holding this branch, and is its
    // desk clean?" — so it carries the local state and the tip commit age.
    match wt {
        Some(w) => {
            let st = w.status.clone().unwrap_or_default();
            let mut spans = vec![
                field("worktree", t),
                Span::styled(format!("{}  ", w.label()), Style::new().fg(t.fg)),
            ];
            // "clean" means nothing outstanding at all — saying it next to a
            // count of unpushed commits reads as a contradiction.
            if st.dirty > 0 {
                spans.push(Span::styled(
                    format!("● {} uncommitted  ", st.dirty),
                    Style::new().fg(t.warn),
                ));
            }
            if st.unpushed > 0 {
                spans.push(Span::styled(
                    format!("⇡{} unpushed  ", st.unpushed),
                    Style::new().fg(t.warn),
                ));
            }
            if st.dirty == 0 && st.unpushed == 0 {
                spans.push(Span::styled("clean  ", Style::new().fg(t.success)));
            }
            if let Some(ts) = st.last_commit_at {
                spans.push(Span::styled(
                    format!("commit {} ago  ", rel_time(ts, now)),
                    Style::new().fg(t.muted),
                ));
            }
            lines.push(Line::from(spans));
        }
        None => lines.push(Line::from(vec![
            field("worktree", t),
            Span::styled("none checked out locally", Style::new().fg(t.muted)),
        ])),
    }
}

fn checks(
    lines: &mut Vec<Line<'static>>,
    check_lines: &mut Vec<(usize, usize)>,
    pr: &Pr,
    now: i64,
    t: &Theme,
    width: usize,
) {
    let tally = pr.tally();
    if tally.total == 0 {
        lines.push(Line::from(vec![
            field("checks", t),
            Span::styled("none reported", Style::new().fg(t.muted)),
        ]));
        return;
    }

    // `checks` is a label like the others, and its tally is the value.
    let mut head = vec![
        field("checks", t),
        Span::styled(
            format!("{} passed", tally.passed),
            Style::new().fg(t.success),
        ),
    ];
    if tally.failed > 0 {
        head.push(Span::styled(
            format!(" · {} failing", tally.failed),
            Style::new().fg(t.failure),
        ));
    }
    if tally.running > 0 {
        head.push(Span::styled(
            format!(" · {} running", tally.running),
            Style::new().fg(t.pending),
        ));
    }
    lines.push(Line::from(head));

    // Check rows start in the value column, so the list reads as belonging to
    // its label. Skipped jobs are in the tally below as one line rather than
    // one row each: a path-filtered repo skips most of its matrix, and listing
    // every skip buries the checks that ran.
    let indent = " ".repeat(VALUE_COL);
    let name_w = width.saturating_sub(VALUE_COL + 2 + 8).max(10);
    for (i, c) in pr.checks.iter().enumerate() {
        if matches!(c.state, CheckState::Skipped | CheckState::Neutral) {
            continue;
        }
        let color = match c.state {
            CheckState::Success => t.success,
            CheckState::Failure => t.failure,
            CheckState::Pending => t.pending,
            CheckState::Cancelled => t.warn,
            _ => t.muted,
        };
        let dur = c
            .elapsed(now)
            .filter(|s| *s > 0)
            .map(dur_short)
            .unwrap_or_default();
        check_lines.push((lines.len(), i));
        lines.push(Line::from(vec![
            Span::raw(indent.clone()),
            Span::styled(format!("{} ", c.state.glyph()), Style::new().fg(color)),
            Span::styled(
                cell(&c.name, name_w),
                Style::new().fg(if c.state.is_bad() { t.fg } else { t.muted }),
            ),
            // Durations right-aligned, so they read down as a column of numbers.
            Span::styled(right(&dur, 7), Style::new().fg(t.muted)),
        ]));
    }
    if tally.skipped > 0 {
        lines.push(Line::from(vec![
            Span::raw(indent),
            Span::styled(
                format!("– {} skipped", tally.skipped),
                Style::new().fg(t.muted),
            ),
        ]));
    }
}

/// `scanning` is whether worktree status is on at all, which decides how an
/// unknown status is explained.
fn wt_detail(
    lines: &mut Vec<Line<'static>>,
    desk: &Desk<'_>,
    scanning: bool,
    home: Option<&str>,
    now: i64,
    t: &Theme,
) {
    let w = desk.wt;
    lines.push(Line::from(Span::styled(
        format!(
            " {}{}",
            w.name(),
            if w.is_main { "  (main worktree)" } else { "" }
        ),
        Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        format!(" {}", w.short_path(home)),
        Style::new().fg(t.muted),
    )));
    lines.push(Line::from(""));

    let branch = match (&w.branch, w.detached) {
        (Some(b), _) => b.clone(),
        (None, true) => format!("detached at {}", super::list::short_sha(&w.head)),
        _ => "unknown".into(),
    };
    lines.push(Line::from(vec![
        field("branch", t),
        Span::styled(branch, Style::new().fg(t.fg)),
    ]));

    let Some(st) = desk.status() else {
        let why = if scanning {
            "not scanned yet"
        } else {
            "not checked: worktree_status is off"
        };
        lines.push(Line::from(vec![
            field("state", t),
            Span::styled(why, Style::new().fg(t.muted)),
        ]));
        merged_pr(lines, desk, now, t);
        return;
    };
    let mut state = vec![field("state", t)];
    if st.dirty > 0 {
        state.push(Span::styled(
            format!("● {} uncommitted", st.dirty),
            Style::new().fg(t.warn),
        ));
    }
    if st.unpushed > 0 {
        state.push(Span::styled(
            format!(
                "{}⇡{} unpushed",
                if st.dirty > 0 { "  " } else { "" },
                st.unpushed
            ),
            Style::new().fg(t.warn),
        ));
    }
    if st.dirty == 0 && st.unpushed == 0 {
        state.push(Span::styled("clean", Style::new().fg(t.success)));
    }
    if !st.published && !w.detached {
        state.push(Span::styled("  · not on origin", Style::new().fg(t.muted)));
    }
    lines.push(Line::from(state));

    if let Some(ts) = st.last_commit_at {
        lines.push(Line::from(vec![
            field("last", t),
            Span::styled(
                format!("{} ago by {}", rel_time(ts, now), st.last_author),
                Style::new().fg(t.muted),
            ),
        ]));
        lines.push(Line::from(vec![
            field("", t),
            Span::styled(truncate(&st.last_subject, 70), Style::new().fg(t.fg)),
        ]));
    }

    merged_pr(lines, desk, now, t);
    cleanup(lines, desk, home, t);
}

/// The PR line: the merged PR this branch landed as, if any.
fn merged_pr(lines: &mut Vec<Line<'static>>, desk: &Desk<'_>, now: i64, t: &Theme) {
    match desk.merged {
        Some(m) => {
            lines.push(Line::from(vec![
                field("pr", t),
                Span::styled(
                    format!("#{} merged {} ago", m.number, rel_time(m.merged_at, now)),
                    Style::new().fg(t.success),
                ),
            ]));
            lines.push(Line::from(vec![
                field("", t),
                Span::styled(truncate(&m.title, 70), Style::new().fg(t.muted)),
            ]));
        }
        None => lines.push(Line::from(vec![
            field("pr", t),
            Span::styled("no open PR for this branch", Style::new().fg(t.muted)),
        ])),
    }
}

/// The one line of advice the worktree view exists for: whether this desk can
/// be collected, and the command to do it.
fn cleanup(lines: &mut Vec<Line<'static>>, desk: &Desk<'_>, home: Option<&str>, t: &Theme) {
    let (w, merged) = (desk.wt, desk.merged);
    // Only ever say a desk is collectable when nothing local would be lost.
    match desk.state {
        WtState::Removable => {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                field("cleanup", t),
                Span::styled(
                    "branch landed, nothing local at risk",
                    Style::new().fg(t.success),
                ),
            ]));
            lines.push(Line::from(vec![
                field("", t),
                Span::styled(
                    format!("git worktree remove {}", shell_path(w, home)),
                    Style::new().fg(t.fg),
                ),
            ]));
        }
        WtState::Working if merged.is_some() => {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                field("cleanup", t),
                Span::styled(
                    "branch landed, but this desk still holds local work",
                    Style::new().fg(t.warn),
                ),
            ]));
        }
        // Pushed and clean, but not at the commit that merged: the branch has
        // moved on, or someone added to the PR from elsewhere. Say why this
        // is not offered for removal rather than leave it looking forgotten.
        WtState::Idle if merged.is_some() && !desk.landed && w.status.is_some() && !w.is_main => {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                field("cleanup", t),
                Span::styled(
                    "a PR from this branch merged, but not at this commit",
                    Style::new().fg(t.muted),
                ),
            ]));
        }
        _ => {}
    }
}

/// A review draft: the pull request it is for, then everything the draft
/// says, in the order a reader decides in — why this one, the verdict, the
/// words — and last whether it still matches the commit it was written for.
fn draft_detail(
    lines: &mut Vec<Line<'static>>,
    entry: &Entry,
    (state, head): (State, Option<&str>),
    pr: DraftPr<'_>,
    now: i64,
    t: &Theme,
    width: usize,
) {
    let muted = Style::new().fg(t.muted);
    match pr {
        DraftPr::Open(p) => pr_header(lines, p, now, t, width),
        DraftPr::Merged(m) => {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" #{} ", m.number),
                    Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate(&m.title, width.saturating_sub(10)),
                    Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(Span::styled(
                format!(" merged {} ago", rel_time(m.merged_at, now)),
                muted,
            )));
        }
        DraftPr::Unknown => {
            lines.push(Line::from(Span::styled(
                format!(" {}", entry.label()),
                Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                " not among the fetched pull requests",
                muted,
            )));
        }
    }

    let text_w = width.saturating_sub(VALUE_COL + 1).max(10);
    let d = match &entry.draft {
        Ok(d) => d,
        Err(why) => {
            prose(lines, "invalid", why, Style::new().fg(t.failure), t, text_w);
            prose(
                lines,
                "file",
                &entry.path.to_string_lossy(),
                muted,
                t,
                text_w,
            );
            return;
        }
    };

    prose(lines, "picked", &d.picked, Style::new().fg(t.fg), t, text_w);
    lines.push(verdict_line(d, now, t));
    prose(lines, "body", &d.body, Style::new().fg(t.fg), t, text_w);
    for c in &d.comments {
        lines.push(Line::from(vec![
            field("comment", t),
            Span::styled(
                truncate(&format!("{}:{}", c.path, c.line), text_w),
                Style::new().fg(t.pending),
            ),
        ]));
        for l in wrap(&c.body, text_w) {
            lines.push(Line::from(vec![
                field("", t),
                Span::styled(l, Style::new().fg(t.fg)),
            ]));
        }
    }
    // Labeled on a line of its own, so the note can never be read as part
    // of what goes out.
    let note = wrap(&d.note, text_w);
    lines.push(Line::from(vec![
        field("note", t),
        Span::styled(
            if note.is_empty() {
                "none"
            } else {
                "private · never posted"
            },
            muted,
        ),
    ]));
    for l in note {
        lines.push(Line::from(vec![field("", t), Span::styled(l, muted)]));
    }
    lines.push(head_line(d, state, head, t));
}

/// A labeled block of prose, wrapped to the pane. An empty one says `none`
/// rather than leaving the label hanging.
fn prose(
    lines: &mut Vec<Line<'static>>,
    label: &'static str,
    text: &str,
    style: Style,
    t: &Theme,
    width: usize,
) {
    let wrapped = wrap(text, width);
    if wrapped.is_empty() {
        lines.push(Line::from(vec![
            field(label, t),
            Span::styled("none", Style::new().fg(t.muted)),
        ]));
        return;
    }
    for (i, l) in wrapped.into_iter().enumerate() {
        lines.push(Line::from(vec![
            field(if i == 0 { label } else { "" }, t),
            Span::styled(l, style),
        ]));
    }
}

/// The verdict, and what became of the draft once it was decided.
fn verdict_line(d: &Draft, now: i64, t: &Theme) -> Line<'static> {
    let color = match d.verdict {
        Verdict::Approve => t.success,
        Verdict::Comment => t.fg,
    };
    let mut spans = vec![
        field("verdict", t),
        Span::styled(d.verdict.label(), Style::new().fg(color)),
    ];
    let outcome = match d.status {
        Status::Drafted => None,
        Status::Skipped => Some("skipped".to_string()),
        Status::Posted => Some(d.posted_at.as_deref().and_then(parse_iso8601).map_or_else(
            || "posted".to_string(),
            |at| format!("posted {} ago", rel_time(at, now)),
        )),
    };
    if let Some(outcome) = outcome {
        spans.push(Span::styled(
            format!("  · {outcome}"),
            Style::new().fg(t.muted),
        ));
    }
    Line::from(spans)
}

/// The commit the draft was written against, and whether the pull request is
/// still there. This is the row that explains `stale` and `unverified`.
fn head_line(d: &Draft, state: State, head: Option<&str>, t: &Theme) -> Line<'static> {
    let sha = super::list::short_sha(&d.head_sha).to_string();
    let current = head.filter(|h| !h.is_empty());
    let (text, color) = match current {
        Some(h) if h.eq_ignore_ascii_case(&d.head_sha) => {
            ("matches the pull request".to_string(), t.success)
        }
        Some(h) => (
            format!("the pull request is now at {}", super::list::short_sha(h)),
            if state == State::Stale {
                t.warn
            } else {
                t.muted
            },
        ),
        None => ("the pull request's head is not known".to_string(), t.muted),
    };
    Line::from(vec![
        field("head", t),
        Span::styled(format!("{sha}  "), Style::new().fg(t.fg)),
        Span::styled(text, Style::new().fg(color)),
    ])
}

/// The worktree's path as one shell word. `~`-shortened when it needs no
/// quoting, since the shell expands `~` only outside quotes; otherwise the
/// full path, single-quoted.
pub fn shell_path(w: &Worktree, home: Option<&str>) -> String {
    let short = w.short_path(home);
    if short
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-~+@%:,=".contains(c))
    {
        return short;
    }
    format!("'{}'", w.path.to_string_lossy().replace('\'', r"'\''"))
}

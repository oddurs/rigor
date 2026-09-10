//! The detail pane: everything about the selected PR, including every check run
//! with its own state, duration and clickable link.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, WtState};
use crate::model::{CheckState, Mergeable, MergedPr, Pr, Worktree};
use crate::theme::Theme;
use crate::util::{dur_short, now_secs, pad, rel_time, truncate};

pub fn draw(f: &mut Frame, area: Rect, app: &mut App) {
    if area.width < 4 || area.height < 2 {
        return;
    }
    let t = app.theme;
    let now = now_secs();

    // (line index -> check index), resolved to screen rows once we know the scroll.
    let mut check_lines: Vec<(usize, usize)> = Vec::new();
    let mut lines: Vec<Line> = Vec::new();

    match (app.selected_pr(), app.selected_worktree()) {
        (Some(pr), wt) => pr_detail(
            &mut lines,
            &mut check_lines,
            pr,
            wt,
            now,
            &t,
            area.width as usize,
        ),
        (None, Some(wt)) => {
            let merged = wt.branch.as_deref().and_then(|b| app.merged_for_branch(b));
            let state = app.wt_state(wt);
            wt_detail(&mut lines, wt, merged, state, app.home.as_deref(), now, &t)
        }
        (None, None) => {
            lines.push(Line::from(Span::styled(
                "  Nothing selected.",
                Style::new().fg(t.muted),
            )));
        }
    }

    let max_scroll = lines.len().saturating_sub(area.height as usize) as u16;
    app.detail_scroll = app.detail_scroll.min(max_scroll);
    let scroll = app.detail_scroll;

    for (li, ci) in check_lines {
        if li >= scroll as usize {
            let y = area.y + (li - scroll as usize) as u16;
            if y < area.bottom() {
                app.hits.checks.push((y, ci));
            }
        }
    }

    let lines: Vec<Line> = lines
        .into_iter()
        .map(|l| super::clip(l, area.width as usize))
        .collect();
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), area);
}

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

    let meta = format!(
        " {} · {} ago · +{} −{} · {} file{} · {} comment{} · → {}",
        pr.author,
        rel_time(pr.updated_at, now),
        pr.additions,
        pr.deletions,
        pr.changed_files,
        if pr.changed_files == 1 { "" } else { "s" },
        pr.comments,
        if pr.comments == 1 { "" } else { "s" },
        pr.base_ref,
    );
    lines.push(Line::from(Span::styled(meta, Style::new().fg(t.muted))));

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

    let tally = pr.tally();
    if tally.total == 0 {
        lines.push(Line::from(Span::styled(
            "  CHECKS   none reported",
            Style::new().fg(t.muted),
        )));
        return;
    }

    let mut head = vec![Span::styled(
        "  CHECKS   ",
        Style::new().fg(t.muted).add_modifier(Modifier::BOLD),
    )];
    head.push(Span::styled(
        format!("{} passed", tally.passed),
        Style::new().fg(t.success),
    ));
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
    if tally.skipped > 0 {
        head.push(Span::styled(
            format!(" · {} skipped", tally.skipped),
            Style::new().fg(t.muted),
        ));
    }
    lines.push(Line::from(head));

    for (i, c) in pr.checks.iter().enumerate() {
        let color = match c.state {
            CheckState::Success => t.success,
            CheckState::Failure => t.failure,
            CheckState::Pending => t.pending,
            CheckState::Cancelled => t.warn,
            _ => t.muted,
        };
        // A skipped job reports a zero-length run; printing "0s" is just noise.
        let dur = c
            .elapsed(now)
            .filter(|s| *s > 0)
            .map(dur_short)
            .unwrap_or_default();
        let name_w = width.saturating_sub(16).max(10);
        check_lines.push((lines.len(), i));
        lines.push(Line::from(vec![
            Span::styled(format!("  {} ", c.state.glyph()), Style::new().fg(color)),
            Span::styled(
                pad(&c.name, name_w),
                Style::new().fg(if c.state.is_bad() { t.fg } else { t.muted }),
            ),
            Span::styled(pad(&dur, 8), Style::new().fg(t.muted)),
        ]));
    }
}

fn wt_detail(
    lines: &mut Vec<Line<'static>>,
    w: &Worktree,
    merged: Option<&MergedPr>,
    disposition: WtState,
    home: Option<&str>,
    now: i64,
    t: &Theme,
) {
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
        (None, true) => format!("detached at {}", truncate(&w.head, 10)),
        _ => "unknown".into(),
    };
    lines.push(Line::from(vec![
        field("branch", t),
        Span::styled(branch, Style::new().fg(t.fg)),
    ]));

    let st = w.status.clone().unwrap_or_default();
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
    if !st.published {
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

    match merged {
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

    // Only ever say a desk is collectable when nothing local would be lost.
    match disposition {
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
                    format!("git worktree remove {}", w.short_path(home)),
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
        _ => {}
    }
}

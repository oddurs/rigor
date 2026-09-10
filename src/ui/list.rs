//! The scrolling list: one line per PR (or per worktree in the worktree view).
//! Columns drop out as the terminal narrows rather than wrapping.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Row, WtState};
use crate::model::{CheckState, Mergeable, MergedPr, Pr, Worktree};
use crate::theme::Theme;
use crate::util::{now_secs, pad, rel_time, truncate};

pub fn draw(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    let height = area.height as usize;
    app.clamp_scroll(height);

    if app.rows.is_empty() {
        let msg = if app.loading_prs && app.prs.is_empty() {
            "loading…".to_string()
        } else if !app.filter.is_empty() {
            format!("Nothing matches “{}”.", app.filter)
        } else {
            app.view.empty_hint().to_string()
        };
        let p = Paragraph::new(Line::from(Span::styled(
            format!("  {msg}"),
            Style::new().fg(t.muted),
        )));
        f.render_widget(p, area);
        return;
    }

    let now = now_secs();
    let width = area.width as usize;
    let mut lines = Vec::with_capacity(height);

    let end = (app.offset + height).min(app.rows.len());
    for i in app.offset..end {
        let selected = i == app.selected;
        let y = area.y + (i - app.offset) as u16;
        app.hits.rows.push((y, i));

        let line = match app.rows[i] {
            Row::Pr(pi) => {
                let pr = &app.prs[pi];
                pr_line(pr, app.worktree_for(pr), selected, width, now, &t)
            }
            Row::Wt(wi) => {
                let w = &app.worktrees[wi];
                let pr = w.branch.as_deref().and_then(|b| app.pr_for_branch(b));
                let merged = w.branch.as_deref().and_then(|b| app.merged_for_branch(b));
                let state = app.wt_state(w);
                wt_line(
                    w,
                    pr,
                    merged,
                    state,
                    selected,
                    width,
                    now,
                    &t,
                    app.home.as_deref(),
                )
            }
        };
        lines.push(line);
    }

    f.render_widget(Paragraph::new(lines), area);
}

/// The selected row is marked without painting an absolute background: the
/// accent bar, a bold title, and the dim columns lifted to full foreground.
/// That reads on any terminal theme. Setting `theme.sel_bg` adds a band on top.
fn row_style(selected: bool, t: &Theme) -> Style {
    if selected {
        Style::new().bg(t.sel_bg).fg(t.sel_fg)
    } else {
        Style::new()
    }
}

/// Columns that are dim on an unselected row come up to foreground on the
/// selected one, so the whole row brightens as you move through the list.
fn dim(selected: bool, t: &Theme) -> Color {
    if selected { t.fg } else { t.muted }
}

/// `▌ #4846  ✗ 2/14  ✔ ⌂  Give the read path a retry budget   octocat   3m`
fn pr_line(
    pr: &Pr,
    wt: Option<&Worktree>,
    selected: bool,
    width: usize,
    now: i64,
    t: &Theme,
) -> Line<'static> {
    let base = row_style(selected, t);
    let title_color = if pr.is_draft { dim(selected, t) } else { t.fg };

    let mut spans = vec![
        Span::styled(if selected { "▌" } else { " " }, base.fg(t.accent)),
        Span::styled(
            pad(&format!("#{}", pr.number), 7),
            base.fg(if pr.is_draft { t.muted } else { t.accent }),
        ),
    ];

    let (ci_text, ci_color) = ci_cell(pr, t);
    spans.push(Span::styled(pad(&ci_text, 8), base.fg(ci_color)));

    let (rv_glyph, rv_color) = review_cell(pr, t);
    spans.push(Span::styled(format!("{rv_glyph} "), base.fg(rv_color)));

    let (wt_glyph, wt_color) = match wt {
        Some(w) if w.status.as_ref().is_some_and(|s| s.dirty > 0) => ("⌂", t.warn),
        Some(_) => ("⌂", t.muted),
        None => (" ", t.muted),
    };
    spans.push(Span::styled(format!("{wt_glyph} "), base.fg(wt_color)));

    // Fixed columns consumed so far: 1 + 7 + 8 + 2 + 2.
    let left = 20;
    let show_author = width >= 92;
    let show_age = width >= 62;
    let right = (if show_author { 14 } else { 0 }) + (if show_age { 5 } else { 0 });
    let title_w = width.saturating_sub(left + right).max(8);

    let title = if pr.is_draft {
        format!("draft · {}", pr.title)
    } else {
        pr.title.clone()
    };
    // Truncate one column short of the field so the title never abuts the author.
    let title_style = if selected {
        base.fg(title_color).add_modifier(Modifier::BOLD)
    } else {
        base.fg(title_color)
    };
    spans.push(Span::styled(
        pad(&truncate(&title, title_w.saturating_sub(1)), title_w),
        title_style,
    ));

    if show_author {
        spans.push(Span::styled(pad(&pr.author, 14), base.fg(dim(selected, t))));
    }
    if show_age {
        spans.push(Span::styled(
            pad(&rel_time(pr.updated_at, now), 5),
            base.fg(dim(selected, t)),
        ));
    }

    Line::from(spans).style(base)
}

/// The CI cell carries counts, not just a verdict — `✗ 2/14` says how much is broken.
fn ci_cell(pr: &Pr, t: &Theme) -> (String, Color) {
    let tally = pr.tally();
    let settled = tally.total - tally.running;
    match pr.rollup {
        CheckState::Success => (format!("✓ {}", tally.total), t.success),
        CheckState::Failure => (format!("✗ {}/{}", tally.failed, tally.total), t.failure),
        CheckState::Pending => (format!("◐ {}/{}", settled, tally.total), t.pending),
        _ => ("·".into(), t.muted),
    }
}

fn review_cell(pr: &Pr, t: &Theme) -> (&'static str, Color) {
    if pr.mergeable == Mergeable::Conflicting {
        return ("⚠", t.warn);
    }
    match pr.review_decision {
        Some(d) => (
            d.glyph(),
            match d {
                crate::model::ReviewDecision::Approved => t.success,
                crate::model::ReviewDecision::ChangesRequested => t.failure,
                crate::model::ReviewDecision::ReviewRequired => t.muted,
            },
        ),
        None => (" ", t.muted),
    }
}

/// Worktree view: the workspace, what it is on, and whether it has a PR yet.
#[allow(clippy::too_many_arguments)]
fn wt_line(
    w: &Worktree,
    pr: Option<&Pr>,
    merged: Option<&MergedPr>,
    state: WtState,
    selected: bool,
    width: usize,
    now: i64,
    t: &Theme,
    home: Option<&str>,
) -> Line<'static> {
    let base = row_style(selected, t);
    let mut spans = vec![Span::styled(
        if selected { "▌" } else { " " },
        base.fg(t.accent),
    )];

    let st = w.status.clone().unwrap_or_default();
    let (glyph, glyph_color) = match state {
        WtState::Working => ("●", t.warn),
        WtState::Removable => ("⌫", t.success),
        WtState::Idle => (" ", t.muted),
    };
    spans.push(Span::styled(format!("{glyph} "), base.fg(glyph_color)));

    let name = if w.is_main {
        format!("{} (main)", w.name())
    } else {
        w.name()
    };
    spans.push(Span::styled(
        pad(&name, 24),
        base.fg(t.fg).add_modifier(Modifier::BOLD),
    ));

    let (pr_text, pr_color) = match (pr, merged) {
        (Some(p), _) => (
            format!("#{} {}", p.number, ci_cell(p, t).0),
            match p.rollup {
                CheckState::Failure => t.failure,
                CheckState::Pending => t.pending,
                CheckState::Success => t.success,
                _ => t.muted,
            },
        ),
        (None, Some(m)) => (format!("#{} merged", m.number), t.muted),
        (None, None) => ("—".into(), t.muted),
    };
    spans.push(Span::styled(pad(&pr_text, 14), base.fg(pr_color)));

    let (drift, drift_color) = match state {
        WtState::Removable => ("removable".to_string(), t.success),
        _ if st.unpushed > 0 => (st.drift(), t.warn),
        _ => (st.drift(), t.muted),
    };
    spans.push(Span::styled(
        pad(&truncate(&drift, 9), 10),
        base.fg(drift_color),
    ));

    let left = 1 + 2 + 24 + 14 + 10;
    let show_age = width >= 70;
    let right = if show_age { 5 } else { 0 };
    let rest_w = width.saturating_sub(left + right).max(8);

    let rest = match (&w.branch, w.detached) {
        (Some(b), _) => b.clone(),
        (None, true) => format!("detached at {}", truncate(&w.head, 8)),
        _ => w.short_path(home),
    };
    spans.push(Span::styled(
        pad(&truncate(&rest, rest_w.saturating_sub(1)), rest_w),
        base.fg(dim(selected, t)),
    ));

    if show_age {
        let age = st
            .last_commit_at
            .map(|ts| rel_time(ts, now))
            .unwrap_or_default();
        spans.push(Span::styled(pad(&age, 5), base.fg(dim(selected, t))));
    }

    Line::from(spans).style(base)
}

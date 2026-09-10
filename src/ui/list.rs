//! The scrolling list: one line per PR (or per worktree in the worktree view).
//! Columns drop out as the terminal narrows rather than wrapping.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Row, WtState};
use crate::config::View;
use crate::model::{CheckState, Mergeable, MergedPr, Pr, Worktree, WorktreeStatus};
use crate::theme::Theme;
use crate::util::{cell, cols, now_secs, pad, rel_time, right, truncate};

pub fn draw(f: &mut Frame<'_>, area: Rect, app: &mut App) {
    let t = app.theme;
    let height = area.height as usize;
    app.clamp_scroll(height);

    if app.rows.is_empty() {
        let msg = if app.in_flight.prs && app.prs.is_empty() {
            "loading…".to_string()
        } else if !app.filter.is_empty() {
            format!("Nothing matches “{}”.", app.filter)
        } else {
            app.view.empty_hint().to_string()
        };
        let mut lines = vec![Line::from(Span::styled(
            format!("  {msg}"),
            Style::new().fg(t.muted),
        ))];
        // Under a filter the way out is to clear it; pointing at another view
        // would only lead to the same filter there.
        if !app.filter.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::new()),
                Span::styled("esc", Style::new().fg(t.fg)),
                Span::styled(" clears the filter", Style::new().fg(t.muted)),
            ]));
        } else if let Some((view, count, key)) = next_view(app) {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {count} in {} — press ", view.title()),
                    Style::new().fg(t.muted),
                ),
                Span::styled(key, Style::new().fg(t.fg)),
            ]));
        }
        f.render_widget(Paragraph::new(lines), area);
        return;
    }

    let home = app.home.clone();
    let ctx = RowCtx {
        width: usize::from(area.width),
        now: now_secs(),
        theme: &t,
        home: home.as_deref(),
        // In Mine every author is you; give the title the room instead.
        authors: app.view != View::Mine,
    };
    let mut lines = Vec::with_capacity(height);

    let end = (app.offset + height).min(app.rows.len());
    for i in app.offset..end {
        let selected = i == app.selected;
        let y = area.y + cols(i - app.offset);
        app.hits.rows.push((y, i));

        let line = match app.rows[i] {
            Row::Pr(pi) => {
                let pr = &app.prs[pi];
                pr_line(pr, app.worktree_for(pr), selected, &ctx)
            }
            Row::Wt(wi) => {
                let w = &app.worktrees[wi];
                let pr = w.branch.as_deref().and_then(|b| app.pr_for_branch(b));
                let merged = w.branch.as_deref().and_then(|b| app.merged_for_branch(b));
                let state = app.wt_state(w);
                wt_line(w, pr, merged, state, selected, &ctx)
            }
        };
        lines.push(line);
    }

    f.render_widget(Paragraph::new(lines), area);
}

/// The selected row: a full-width band mixed from the terminal's own
/// background (see `probe`), a slim accent bar at the edge, a bold title, and
/// the dim columns lifted to full foreground. Where the terminal does not
/// report its colors there is no band, and the bar and weight carry it alone.
const fn row_style(selected: bool, t: &Theme) -> Style {
    if selected {
        Style::new().bg(t.sel_bg).fg(t.sel_fg)
    } else {
        Style::new()
    }
}

/// Columns that are dim on an unselected row come up to foreground on the
/// selected one, so the whole row brightens as you move through the list.
const fn dim(selected: bool, t: &Theme) -> Color {
    if selected { t.fg } else { t.muted }
}

/// An empty view is a dead end unless it points somewhere. Suggest the first
/// view worth visiting — Ready, then Mine, then All — that has something in it.
fn next_view(app: &App) -> Option<(View, usize, String)> {
    [View::Ready, View::Mine, View::All]
        .into_iter()
        .filter(|v| *v != app.view)
        .find_map(|v| {
            let count = app.count_for(v);
            let pos = app.settings.views.iter().position(|x| *x == v)?;
            (count > 0 && pos < 9).then(|| (v, count, (pos + 1).to_string()))
        })
}

/// What every row of the list shares.
struct RowCtx<'a> {
    width: usize,
    /// The instant the frame is drawn at, for ages.
    now: i64,
    theme: &'a Theme,
    /// `$HOME`, to shorten paths to `~`.
    home: Option<&'a str>,
    /// Whether the author column earns its width.
    authors: bool,
}

/// `▎#4846  ✗ 1/3   ✔ ⌂ Give the read path a retry budget      octocat     3m`
fn pr_line(pr: &Pr, wt: Option<&Worktree>, selected: bool, ctx: &RowCtx<'_>) -> Line<'static> {
    let (t, width, now, authors) = (ctx.theme, ctx.width, ctx.now, ctx.authors);
    let base = row_style(selected, t);
    let title_color = if pr.is_draft { dim(selected, t) } else { t.fg };

    let mut spans = vec![
        Span::styled(if selected { "▎" } else { " " }, base.fg(t.accent)),
        Span::styled(
            pad(&format!("#{}", pr.number), 7),
            base.fg(if pr.is_draft { t.muted } else { t.accent }),
        ),
    ];

    // The glyph carries the verdict. A passing count is kept — `✓ 2` on a draft
    // says the full suite never ran — but muted, so a failure stands out.
    let (ci_text, ci_color) = ci_cell(pr, t);
    let (glyph, count) = ci_text.split_once(' ').unwrap_or((ci_text.as_str(), ""));
    let count_color = if pr.rollup == CheckState::Success {
        dim(selected, t)
    } else {
        ci_color
    };
    spans.push(Span::styled(format!("{glyph} "), base.fg(ci_color)));
    spans.push(Span::styled(pad(count, 6), base.fg(count_color)));

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
    let show_author = authors && width >= 92;
    let show_age = width >= 62;
    let right_w = (if show_author { 14 } else { 0 }) + (if show_age { 5 } else { 0 });
    let title_w = width.saturating_sub(left + right_w).max(8);

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
        spans.push(Span::styled(
            cell(&pr.author, 14),
            base.fg(dim(selected, t)),
        ));
    }
    if show_age {
        // Right-aligned: ages read down a column as numbers.
        spans.push(Span::styled(
            format!("{} ", right(&rel_time(pr.updated_at, now), 4)),
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
    // "Review required" is the resting state of nearly every open PR, so a
    // glyph for it marks every row and tells you nothing. Only the states that
    // change what you do next get one; the detail pane still spells it out.
    match pr.review_decision {
        Some(crate::model::ReviewDecision::Approved) => ("✔", t.success),
        Some(crate::model::ReviewDecision::ChangesRequested) => ("✘", t.failure),
        _ => (" ", t.muted),
    }
}

/// Worktree view: the workspace, what it is on, and whether it has a PR yet.
fn wt_line(
    w: &Worktree,
    pr: Option<&Pr>,
    merged: Option<&MergedPr>,
    state: WtState,
    selected: bool,
    ctx: &RowCtx<'_>,
) -> Line<'static> {
    let (t, width, now, home) = (ctx.theme, ctx.width, ctx.now, ctx.home);
    let base = row_style(selected, t);
    let mut spans = vec![Span::styled(
        if selected { "▎" } else { " " },
        base.fg(t.accent),
    )];

    let unknown = WorktreeStatus::default();
    let st = w.status.as_ref().unwrap_or(&unknown);
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
    // Bold only on the selected row, as in the PR list: a whole column of bold
    // names has no hierarchy left in it.
    let name_style = if selected {
        base.fg(t.fg).add_modifier(Modifier::BOLD)
    } else {
        base.fg(t.fg)
    };
    spans.push(Span::styled(cell(&name, 24), name_style));

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
        // A detached checkout has no branch to be local to or unpushed from.
        _ if w.detached => (String::new(), t.muted),
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
    let right_w = if show_age { 5 } else { 0 };
    let rest_w = width.saturating_sub(left + right_w).max(8);

    let rest = match (&w.branch, w.detached) {
        (Some(b), _) => b.clone(),
        (None, true) => format!("detached at {}", short_sha(&w.head)),
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
        spans.push(Span::styled(
            format!("{} ", right(&age, 4)),
            base.fg(dim(selected, t)),
        ));
    }

    Line::from(spans).style(base)
}

/// Seven characters, git's own short form — complete, so no ellipsis.
pub fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

//! The question asked before a review is posted, overlaid on the dashboard:
//! exactly what would be sent, and nothing that would not be.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::detail::field;
use crate::app::{App, DraftPr};
use crate::drafts::{Draft, Side, Verdict};
use crate::theme::Theme;
use crate::util::{cols, truncate, wrap};

/// Where values begin inside the overlay: the detail pane's own columns.
const VALUE_COL: usize = 12;

/// Everything the review would put on the pull request, in the order GitHub
/// will show it: which pull request, the verdict, the words, then each
/// inline comment where it lands. Built from the very draft the request is
/// built from. The draft's private `note` and `picked` are not sent, so they
/// are not here.
pub(super) fn lines(title: &str, d: &Draft, t: &Theme, width: usize) -> Vec<Line<'static>> {
    let text_w = width.saturating_sub(VALUE_COL + 1).max(10);
    let words = Style::new().fg(t.fg);
    let mut out = vec![
        Line::from(Span::styled(
            format!(" {}", truncate(title, width.saturating_sub(2))),
            Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            field("verdict", t),
            Span::styled(
                d.verdict.label(),
                Style::new().fg(match d.verdict {
                    Verdict::Approve => t.success,
                    Verdict::Comment => t.fg,
                }),
            ),
        ]),
    ];

    let body = wrap(&d.body, text_w);
    if body.is_empty() {
        out.push(Line::from(vec![
            field("body", t),
            Span::styled("no text", Style::new().fg(t.muted)),
        ]));
    }
    for (i, l) in body.into_iter().enumerate() {
        out.push(Line::from(vec![
            field(if i == 0 { "body" } else { "" }, t),
            Span::styled(l, words),
        ]));
    }

    for c in &d.comments {
        // The right side is where nearly every comment goes. The left is the
        // old version of the file, and worth saying.
        let place = match c.side {
            Side::Right => format!("{}:{}", c.path, c.line),
            Side::Left => format!("{}:{} (LEFT)", c.path, c.line),
        };
        out.push(Line::from(vec![
            field("comment", t),
            Span::styled(truncate(&place, text_w), Style::new().fg(t.pending)),
        ]));
        for l in wrap(&c.body, text_w) {
            out.push(Line::from(vec![field("", t), Span::styled(l, words)]));
        }
    }
    out
}

/// Where the overlay goes and what is in it, for a frame of this size.
struct Plan {
    rect: Rect,
    content: Vec<Line<'static>>,
    /// Lines of content that fit inside the border.
    rows: usize,
    max_scroll: u16,
}

fn plan(area: Rect, app: &App) -> Option<Plan> {
    let armed = app.armed.as_ref()?;
    // The pull request's number comes first, always; its title when the
    // fetch knows it.
    let pr = app
        .drafts
        .iter()
        .find(|e| e.path == armed.path)
        .map_or(DraftPr::Unknown, |e| app.draft_pr(e));
    let title = match pr {
        DraftPr::Open(p) => format!("#{} {}", armed.number, p.title),
        DraftPr::Merged(m) => format!("#{} {} (merged)", armed.number, m.title),
        DraftPr::Unknown => format!("#{} — not among the fetched pull requests", armed.number),
    };
    let w = 84u16.min(area.width.saturating_sub(4));
    let content = lines(
        &title,
        &armed.draft,
        &app.theme,
        usize::from(w.saturating_sub(2)),
    );
    let h = cols(content.len() + 2).min(area.height.saturating_sub(2));
    let rows = usize::from(h.saturating_sub(2));
    Some(Plan {
        rect: Rect::new(
            area.x + (area.width.saturating_sub(w)) / 2,
            area.y + (area.height.saturating_sub(h)) / 2,
            w,
            h,
        ),
        max_scroll: cols(content.len().saturating_sub(rows)),
        content,
        rows,
    })
}

/// Bring the question's own state up to date with the frame about to be
/// drawn: the scroll kept inside what there is to scroll, and the end
/// counted as shown once its last line is on screen. Done before anything is
/// drawn, so the footer and the overlay of one frame agree.
pub fn settle(area: Rect, app: &mut App) {
    let Some(plan) = plan(area, app) else {
        return;
    };
    if let Some(armed) = &mut app.armed {
        armed.scroll = armed.scroll.min(plan.max_scroll);
        if plan.rows > 0 && armed.scroll == plan.max_scroll {
            armed.seen_end = true;
        }
    }
}

pub fn draw(f: &mut Frame<'_>, area: Rect, app: &App) {
    let t = app.theme;
    let (Some(plan), Some(armed)) = (plan(area, app), &app.armed) else {
        return;
    };

    // Recede everything behind it, as the help overlay does.
    let buf = f.buffer_mut();
    for pos in area.positions() {
        if let Some(c) = buf.cell_mut(pos) {
            c.modifier.insert(Modifier::DIM);
        }
    }

    // The bottom edge says where the reader is, and what `a` would do.
    let scroll = armed.scroll.min(plan.max_scroll);
    let below = plan
        .content
        .len()
        .saturating_sub(usize::from(scroll) + plan.rows);
    let (edge, edge_style) = if below > 0 {
        let more = Style::new().fg(t.pending);
        (
            format!(" ↓ {below} more below · j "),
            if armed.nudged {
                more.add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                more
            },
        )
    } else {
        (
            " this is all of it · a posts · esc cancels ".to_string(),
            Style::new().fg(t.muted),
        )
    };

    f.render_widget(Clear, plan.rect);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(t.border))
        .title(Span::styled(
            " this is what will be posted ",
            Style::new().fg(t.pending).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(edge, edge_style))
        .style(Style::new().bg(t.bg));
    let inner = block.inner(plan.rect);
    f.render_widget(block, plan.rect);
    let content: Vec<Line<'_>> = plan
        .content
        .into_iter()
        .map(|l| super::clip(l, usize::from(inner.width)))
        .collect();
    f.render_widget(Paragraph::new(content).scroll((scroll, 0)), inner);
}

//! The question asked before a review is posted, overlaid on the dashboard:
//! exactly what would be sent, and nothing that would not be.
//!
//! Nothing here is cut short. A path too long for the line wraps; so does
//! everything else. What cannot be shown whole is not offered for posting.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::detail::field;
use crate::app::{App, DraftPr};
use crate::drafts::{Draft, Side, Verdict};
use crate::theme::Theme;
use crate::util::{cols, now_millis, width, wrap};

/// Where values begin inside the overlay: the detail pane's own columns.
const VALUE_COL: usize = 12;

/// The narrowest a value may be laid out. Below this the overlay would be a
/// column of fragments, and the window is called too small instead.
const MIN_TEXT: usize = 24;

/// The fewest rows of content worth showing at a time.
const MIN_ROWS: usize = 4;

/// Where a comment lands, wrapped rather than cut: a long path keeps its
/// file name and its line, which are the parts that say where. Breaks after
/// a `/` where it can.
pub(super) fn wrap_place(place: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for piece in place.split_inclusive('/') {
        if !line.is_empty() && width(&line) + width(piece) > max {
            out.push(std::mem::take(&mut line));
        }
        // A single directory or file name longer than the line is cut
        // wherever the line ends, and carried on.
        for ch in piece.chars() {
            if !line.is_empty() && width(&line) + width(ch.encode_utf8(&mut [0; 4])) > max {
                out.push(std::mem::take(&mut line));
            }
            line.push(ch);
        }
    }
    out.push(line);
    out
}

/// Everything the review would put on the pull request, in the order GitHub
/// will show it: which pull request, the verdict, the words, then each
/// inline comment where it lands. Built from the very draft the request is
/// built from. The draft's private `note` and `picked` are not sent, so they
/// are not here.
///
/// `width` is the room there is; every row fits in it without being cut.
pub(super) fn lines(title: &str, d: &Draft, t: &Theme, width: usize) -> Vec<Line<'static>> {
    let text_w = width.saturating_sub(VALUE_COL + 1).max(2);
    let words = Style::new().fg(t.fg);
    let mut out: Vec<Line<'static>> = wrap(title, width.saturating_sub(2).max(2))
        .into_iter()
        .map(|l| {
            Line::from(Span::styled(
                format!(" {l}"),
                Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
            ))
        })
        .collect();
    out.push(Line::from(""));
    out.push(Line::from(vec![
        field("verdict", t),
        Span::styled(
            d.verdict.label(),
            Style::new().fg(match d.verdict {
                Verdict::Approve => t.success,
                Verdict::Comment => t.fg,
            }),
        ),
    ]));

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
        for (i, l) in wrap_place(&place, text_w).into_iter().enumerate() {
            out.push(Line::from(vec![
                field(if i == 0 { "comment" } else { "" }, t),
                Span::styled(l, Style::new().fg(t.pending)),
            ]));
        }
        for l in wrap(&c.body, text_w) {
            out.push(Line::from(vec![field("", t), Span::styled(l, words)]));
        }
    }
    out
}

/// Where the overlay goes and what is in it, for a frame of this size.
struct Plan {
    rect: Rect,
    /// `None` when the window is too small to show the content whole.
    content: Option<Vec<Line<'static>>>,
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
    let inner_w = usize::from(w.saturating_sub(2));
    let room = usize::from(area.height.saturating_sub(4));
    if inner_w < VALUE_COL + 1 + MIN_TEXT || room < MIN_ROWS {
        // Too small to show it without cutting something. A box as large as
        // the window allows, saying so.
        let w = area.width.min(48);
        let h = area.height.min(3);
        return Some(Plan {
            rect: Rect::new(
                area.x + (area.width - w) / 2,
                area.y + (area.height - h) / 2,
                w,
                h,
            ),
            content: None,
            rows: 0,
            max_scroll: 0,
        });
    }
    let content = lines(&title, &armed.draft, &app.theme, inner_w);
    let rows = content.len().min(room);
    let h = cols(rows + 2);
    Some(Plan {
        rect: Rect::new(
            area.x + (area.width.saturating_sub(w)) / 2,
            area.y + (area.height.saturating_sub(h)) / 2,
            w,
            h,
        ),
        max_scroll: cols(content.len().saturating_sub(rows)),
        content: Some(content),
        rows,
    })
}

/// Bring the question's own state up to date with the frame about to be
/// drawn: the scroll kept inside what there is to scroll, the rows this
/// frame shows counted as shown, and the moment the overlay first appeared
/// noted. Done before anything is drawn, so the footer and the overlay of
/// one frame agree.
pub fn settle(area: Rect, app: &mut App) {
    let Some(plan) = plan(area, app) else {
        return;
    };
    let Some(armed) = &mut app.armed else {
        return;
    };
    armed.drawn_at.get_or_insert_with(now_millis);
    let Some(content) = &plan.content else {
        armed.cramped = true;
        return;
    };
    armed.cramped = false;
    // Laid out afresh — the window was resized, so the rows are other rows —
    // and what was read of the old layout says nothing about this one.
    if armed.seen_width != plan.rect.width || armed.seen.len() != content.len() {
        armed.seen = vec![false; content.len()];
        armed.seen_width = plan.rect.width;
    }
    armed.scroll = armed.scroll.min(plan.max_scroll);
    let top = usize::from(armed.scroll);
    for seen in armed.seen.iter_mut().skip(top).take(plan.rows) {
        *seen = true;
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

    let asking = Style::new().fg(t.pending).add_modifier(Modifier::BOLD);
    let louder = |style: Style| {
        if armed.nudged {
            style.add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            style
        }
    };
    f.render_widget(Clear, plan.rect);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(t.border))
        .style(Style::new().bg(t.bg));

    let Some(content) = plan.content else {
        let inner = block.inner(plan.rect);
        f.render_widget(block, plan.rect);
        f.render_widget(
            Paragraph::new(Span::styled(
                " enlarge the window to post",
                louder(Style::new().fg(t.pending)),
            )),
            inner,
        );
        return;
    };

    // The bottom edge says how much is left to read, and what `y` would do
    // once there is none.
    let unread = armed.unread();
    let (edge, edge_style) = if unread > 0 {
        let rows = if unread == 1 { "row" } else { "rows" };
        (
            format!(" {unread} {rows} still to read · j / k "),
            louder(Style::new().fg(t.pending)),
        )
    } else {
        (
            " this is all of it · y posts · esc cancels ".to_string(),
            Style::new().fg(t.muted),
        )
    };
    let block = block
        .title(Span::styled(" this is what will be posted ", asking))
        .title_bottom(Span::styled(edge, edge_style));
    let inner = block.inner(plan.rect);
    f.render_widget(block, plan.rect);
    let scroll = armed.scroll.min(plan.max_scroll);
    f.render_widget(Paragraph::new(content).scroll((scroll, 0)), inner);
}

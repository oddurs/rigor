//! Key reference, overlaid on the dashboard.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::app::App;

const KEYS: &[(&str, &str)] = &[
    ("j / k, ↓ / ↑", "move selection"),
    ("g / G, home / end", "first / last"),
    ("PgUp / PgDn", "page"),
    ("1–9, tab / ⇧tab", "switch view"),
    ("enter, o", "open PR in browser"),
    ("c", "open the PR's checks page"),
    ("y", "copy PR URL to clipboard"),
    ("s", "toggle sort: recent ⇄ attention"),
    ("d", "show / hide drafts"),
    ("/", "filter: title, branch, author, label"),
    ("esc", "clear filter"),
    ("r", "refresh now"),
    ("J / K", "scroll the detail pane"),
    ("?", "close this help"),
    ("q", "quit"),
];

const MOUSE: &[(&str, &str)] = &[
    ("click a tab", "switch view"),
    ("click a row", "select it"),
    ("double-click a row", "open it in the browser"),
    ("click a check", "open that check run"),
    ("scroll wheel", "scroll the list or detail pane"),
];

pub fn draw(f: &mut Frame<'_>, area: Rect, app: &App) {
    let t = app.theme;
    // Recede everything behind the modal so it reads as a layer on top, not
    // as more text on the same plane.
    let buf = f.buffer_mut();
    for pos in area.positions() {
        if let Some(c) = buf.cell_mut(pos) {
            c.modifier.insert(Modifier::DIM);
        }
    }

    let w = 64u16.min(area.width.saturating_sub(4));
    // Rows: every key and mouse binding, a blank line, the "mouse" heading,
    // and the two borders.
    let h = crate::util::cols(KEYS.len() + MOUSE.len() + 4);
    let h = h.min(area.height.saturating_sub(2));
    let rect = Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    );

    f.render_widget(Clear, rect);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(t.border))
        .title(Span::styled(
            " keys ",
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ))
        .style(Style::new().bg(t.bg));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let mut lines: Vec<Line<'_>> = Vec::new();
    for (k, d) in KEYS {
        lines.push(row(k, d, app));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  mouse",
        Style::new().fg(t.muted).add_modifier(Modifier::BOLD),
    )));
    for (k, d) in MOUSE {
        lines.push(row(k, d, app));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn row<'a>(k: &'a str, d: &'a str, app: &App) -> Line<'a> {
    let t = app.theme;
    Line::from(vec![
        Span::styled(format!("  {k:<20}"), Style::new().fg(t.accent)),
        Span::styled(d, Style::new().fg(t.fg)),
    ])
}

//! Rendering. Every frame also records the screen regions it drew into
//! (`app.hits`) so the mouse handler can turn a click back into a row.

mod detail;
mod help;
mod list;
#[cfg(test)]
mod tests;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, Hits};
use crate::config::LayoutMode;
use crate::util::{now_secs, rel_time};

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// Cut a composed line to `width` columns. The detail pane must not wrap: a
/// wrapped line would shift every row below it and put the recorded check
/// hitboxes out of step with what was drawn.
pub fn clip(line: Line<'static>, width: usize) -> Line<'static> {
    let style = line.style;
    let mut used = 0usize;
    let mut out = Vec::new();
    for span in line.spans {
        if used >= width {
            break;
        }
        let w = crate::util::width(&span.content);
        if used + w <= width {
            used += w;
            out.push(span);
        } else {
            let cut = crate::util::truncate(&span.content, width - used);
            out.push(Span::styled(cut, span.style));
            break;
        }
    }
    Line::from(out).style(style)
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.hits = Hits::default();

    let t = app.theme;
    f.render_widget(Block::new().style(Style::new().bg(t.bg).fg(t.fg)), area);

    let [header, tabs, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_header(f, header, app);
    draw_tabs(f, tabs, app);
    draw_body(f, body, app);
    draw_footer(f, footer, app);

    if app.show_help {
        help::draw(f, area, app);
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let t = app.theme;
    let mut left = vec![
        Span::styled(
            "rigor",
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::new()),
        Span::styled(
            app.repo.slug(),
            Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(b) = &app.repo.current_branch {
        left.push(Span::styled(format!("  ⎇ {b}"), Style::new().fg(t.muted)));
    }
    if !app.viewer.is_empty() {
        left.push(Span::styled(
            format!("  @{}", app.viewer),
            Style::new().fg(t.muted),
        ));
    }

    let right = if app.loading_prs || app.loading_wts {
        format!("{} loading ", SPINNER[app.spinner % SPINNER.len()])
    } else if app.last_refresh > 0 {
        format!("⟳ {} ago ", rel_time(app.last_refresh, now_secs()))
    } else {
        String::new()
    };

    let used: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let gap = (area.width as usize).saturating_sub(used + right.chars().count());
    left.push(Span::raw(" ".repeat(gap)));
    left.push(Span::styled(right, Style::new().fg(t.muted)));

    f.render_widget(Paragraph::new(Line::from(left)), area);
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    let mut spans = Vec::new();
    let mut x = area.x;

    for (i, v) in app.settings.views.clone().iter().enumerate() {
        let count = app.count_for(*v);
        let active = *v == app.view;
        let base = if active {
            Style::new().bg(t.sel_bg)
        } else {
            Style::new()
        };
        let key = format!(" {} ", i + 1);
        let title = v.title().to_string();
        let num = format!(" {count} ");
        let w = (key.chars().count() + title.chars().count() + num.chars().count()) as u16;

        spans.push(Span::styled(key, base.fg(t.muted)));
        spans.push(Span::styled(
            title,
            if active {
                base.fg(t.accent).add_modifier(Modifier::BOLD)
            } else {
                base.fg(t.fg)
            },
        ));
        spans.push(Span::styled(
            num,
            base.fg(if active { t.accent } else { t.muted }),
        ));
        if x < area.right() {
            app.hits
                .tabs
                .push((Rect::new(x, area.y, w.min(area.right() - x), 1), *v));
        }
        x = x.saturating_add(w);
    }

    let mut right = format!("sort {} ", app.sort.label());
    // The one number worth carrying on the chrome: desks that can be collected.
    let removable = app.removable_count();
    if removable > 0 {
        right = format!("{removable} removable   {right}");
    }
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let gap = (area.width as usize).saturating_sub(used + right.chars().count());
    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(Span::styled(right, Style::new().fg(t.muted)));

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_body(f: &mut Frame, area: Rect, app: &mut App) {
    let t = app.theme;
    let side_by_side = match app.settings.layout {
        LayoutMode::Split => true,
        LayoutMode::Stack => false,
        LayoutMode::Auto => area.width >= 140,
    };

    let (list_area, detail_outer) = if side_by_side {
        let [l, d] =
            Layout::horizontal([Constraint::Min(48), Constraint::Percentage(42)]).areas(area);
        (l, d)
    } else {
        let [l, d] = Layout::vertical([Constraint::Min(4), Constraint::Percentage(48)]).areas(area);
        (l, d)
    };

    let block = Block::new()
        .borders(if side_by_side {
            Borders::LEFT
        } else {
            Borders::TOP
        })
        .border_style(Style::new().fg(t.border));
    let detail_area = block.inner(detail_outer);
    f.render_widget(block, detail_outer);

    app.hits.list = list_area;
    app.hits.detail = detail_area;

    list::draw(f, list_area, app);
    detail::draw(f, detail_area, app);
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let t = app.theme;

    if app.filter_mode {
        let line = Line::from(vec![
            Span::styled(" filter ", Style::new().fg(t.sel_fg).bg(t.accent)),
            Span::styled(format!(" {}", app.filter), Style::new().fg(t.fg)),
            Span::styled("▏", Style::new().fg(t.accent)),
            Span::styled("   enter accept · esc clear", Style::new().fg(t.muted)),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }

    if let Some(e) = &app.error {
        let line = Line::from(vec![
            Span::styled(" error ", Style::new().fg(t.sel_fg).bg(t.failure)),
            Span::styled(format!(" {e}"), Style::new().fg(t.failure)),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }

    if let Some((msg, at)) = &app.notice
        && now_secs() - at < 4
    {
        let line = Line::from(Span::styled(format!(" {msg}"), Style::new().fg(t.accent)));
        f.render_widget(Paragraph::new(line), area);
        return;
    }

    let mut spans = vec![Span::raw(" ")];
    for (k, d) in [
        ("o", "open"),
        ("c", "checks"),
        ("y", "copy"),
        ("/", "filter"),
        ("s", "sort"),
        ("r", "refresh"),
        ("?", "help"),
        ("q", "quit"),
    ] {
        spans.push(Span::styled(k, Style::new().fg(t.accent)));
        spans.push(Span::styled(format!(" {d}   "), Style::new().fg(t.muted)));
    }
    if !app.filter.is_empty() {
        spans.push(Span::styled(
            format!("filter: {}", app.filter),
            Style::new().fg(t.warn),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

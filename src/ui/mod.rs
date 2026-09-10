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
use crate::config::{LayoutMode, View};
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

    // Three rows of chrome: a top nav (who and where), a subnav (the views),
    // and a rail that underlines the active view and separates chrome from
    // content.
    let [nav, subnav, rail, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(area);

    // Laid out before the chrome is drawn so the rail knows where the detail
    // pane's divider will meet it.
    let split = body_layout(app, body);

    draw_nav(f, nav, app);
    draw_subnav(f, subnav, rail, app, split.divider_x);
    draw_body(f, split, app);
    draw_footer(f, footer, app);

    if app.show_help {
        help::draw(f, area, app);
    }
}

fn width_of(spans: &[Span]) -> usize {
    spans.iter().map(|s| crate::util::width(&s.content)).sum()
}

/// ` rigor │ acme/widget  ⎇ main                                @octocat   ● 12s ago`
///
/// No filled badge: hierarchy comes from value and weight. The wordmark takes
/// the accent, a hairline sets it apart, and the repository is the one bold
/// thing on the line. When the line is too narrow, context goes in order of
/// how little it helps — the branch, then the user, then the status.
fn draw_nav(f: &mut Frame, area: Rect, app: &App) {
    let t = app.theme;
    let muted = Style::new().fg(t.muted);

    let head = [
        Span::raw(" "),
        Span::styled(
            "rigor",
            Style::new().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::new().fg(t.border)),
        Span::styled(
            app.repo.slug(),
            Style::new().fg(t.fg).add_modifier(Modifier::BOLD),
        ),
    ];
    let branch = app
        .repo
        .current_branch
        .as_ref()
        .map(|b| [Span::raw("  "), Span::styled(format!("⎇ {b}"), muted)]);
    let status = sync_status(app);
    let viewer = (!app.viewer.is_empty()).then(|| Span::styled(format!("@{}", app.viewer), muted));

    let assemble = |sb: bool, sv: bool, ss: bool| -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let mut left: Vec<Span> = head.to_vec();
        if sb && let Some(b) = &branch {
            left.extend(b.iter().cloned());
        }
        let mut right = Vec::new();
        if sv && let Some(v) = &viewer {
            right.push(v.clone());
        }
        if ss && !status.is_empty() {
            if !right.is_empty() {
                right.push(Span::raw("   "));
            }
            right.extend(status.iter().cloned());
        }
        if !right.is_empty() {
            right.push(Span::raw(" "));
        }
        (left, right)
    };

    let width = area.width as usize;
    let mut show = [true, true, true];
    let (mut left, mut right) = assemble(true, true, true);
    for step in 0..3 {
        if width_of(&left) + width_of(&right) < width {
            break;
        }
        show[step] = false;
        (left, right) = assemble(show[0], show[1], show[2]);
    }

    let gap = width.saturating_sub(width_of(&left) + width_of(&right));
    left.push(Span::raw(" ".repeat(gap)));
    left.extend(right);
    f.render_widget(Paragraph::new(Line::from(left)), area);
}

/// A coloured dot that answers "can I trust what I'm looking at?": green when
/// fresh, amber once the data is older than two refresh intervals (ten minutes
/// when auto-refresh is off), red when the last sync failed. A spinner while a
/// sync is in flight.
fn sync_status(app: &App) -> Vec<Span<'static>> {
    let t = app.theme;
    let muted = Style::new().fg(t.muted);
    if app.loading_prs || app.loading_wts {
        return vec![
            Span::styled(
                SPINNER[app.spinner % SPINNER.len()],
                Style::new().fg(t.accent),
            ),
            Span::styled(" syncing", muted),
        ];
    }
    if app.error.is_some() {
        return vec![
            Span::styled("●", Style::new().fg(t.failure)),
            Span::styled(" sync failed", muted),
        ];
    }
    if app.last_refresh == 0 {
        return Vec::new();
    }
    let now = now_secs();
    let age = now - app.last_refresh;
    let every = app.settings.refresh_secs as i64;
    let stale = if every > 0 {
        age > every * 2
    } else {
        age > 600
    };
    vec![
        Span::styled(
            "●",
            Style::new().fg(if stale { t.pending } else { t.success }),
        ),
        Span::styled(format!(" {} ago", rel_time(app.last_refresh, now)), muted),
    ]
}

/// ` Ready 2   Mine 14   Review 0   Blocked 9   All 33   Worktrees 49        ↕ recent `
/// `─━━━━━━━──────────────────────────────────────┬────────────────────────────────────`
///
/// Tabs carry a title and a count and nothing else. Key numbers used to sit in
/// front of each title, which put two bare digits side by side (`Ready 2  2
/// Mine`); the number keys are positional and listed in the footer instead.
fn draw_subnav(f: &mut Frame, area: Rect, rail: Rect, app: &mut App, divider_x: Option<u16>) {
    let t = app.theme;
    let mut spans: Vec<Span> = Vec::new();
    let mut x = area.x;
    let mut underline: Option<(u16, u16)> = None;

    for v in app.settings.views.clone() {
        let count = app.count_for(v);
        let active = v == app.view;
        let base = Style::new();
        let title_style = if active {
            base.fg(t.accent).add_modifier(Modifier::BOLD)
        } else {
            base.fg(t.fg)
        };
        // Two counts carry meaning at a glance: something is ready to merge,
        // or something is blocked. The rest are just sizes.
        let count_color = match v {
            View::Ready if count > 0 => t.success,
            View::Blocked if count > 0 => t.failure,
            _ if active => t.accent,
            _ => t.muted,
        };

        let title = v.title();
        let num = count.to_string();
        let w = (title.chars().count() + num.chars().count() + 3) as u16;

        spans.push(Span::styled(" ", base));
        spans.push(Span::styled(title, title_style));
        spans.push(Span::styled(" ", base));
        spans.push(Span::styled(num, base.fg(count_color)));
        spans.push(Span::styled(" ", base));
        spans.push(Span::raw(" "));

        if x < area.right() {
            let visible = w.min(area.right() - x);
            // The hitbox covers the tab and its stretch of rail, so the target
            // is two rows tall.
            app.hits.tabs.push((Rect::new(x, area.y, visible, 2), v));
            // The underline hugs the label, not the padding around it.
            if active {
                let label = w.saturating_sub(2);
                underline = Some((x + 1, label.min(visible.saturating_sub(1))));
            }
        }
        x = x.saturating_add(w + 1);
    }

    let mut right: Vec<Span> = Vec::new();
    if !app.filter.is_empty() && !app.filter_mode {
        right.push(Span::styled(
            format!("/{}   ", app.filter),
            Style::new().fg(t.accent),
        ));
    }
    let removable = app.removable_count();
    if removable > 0 {
        right.push(Span::styled("⌫ ", Style::new().fg(t.success)));
        right.push(Span::styled(
            format!("{removable} removable   "),
            Style::new().fg(t.muted),
        ));
    }
    right.push(Span::styled("↕ ", Style::new().fg(t.border)));
    right.push(Span::styled(
        format!("{} ", app.sort.label()),
        Style::new().fg(t.muted),
    ));

    let used = width_of(&spans);
    let width = area.width as usize;
    if used + width_of(&right) < width {
        spans.push(Span::raw(" ".repeat(width - used - width_of(&right))));
        spans.extend(right);
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);

    f.render_widget(
        Paragraph::new(rail_line(rail, underline, divider_x, &t)),
        rail,
    );
}

/// The rule under the tabs: light everywhere, heavy and accented under the
/// active tab, and a `┬` where the detail pane's divider comes down to meet it.
fn rail_line(
    rail: Rect,
    underline: Option<(u16, u16)>,
    divider_x: Option<u16>,
    t: &crate::theme::Theme,
) -> Line<'static> {
    let light = Style::new().fg(t.border);
    let heavy = Style::new().fg(t.accent);

    let mut cells: Vec<(char, Style)> = (rail.x..rail.right()).map(|_| ('─', light)).collect();
    if let Some(dx) = divider_x
        && dx >= rail.x
        && dx < rail.right()
    {
        cells[(dx - rail.x) as usize] = ('┬', light);
    }
    if let Some((ux, uw)) = underline {
        for cx in ux..(ux + uw).min(rail.right()) {
            cells[(cx - rail.x) as usize] = ('━', heavy);
        }
    }

    // Merge runs of one style into a single span.
    let mut spans: Vec<Span> = Vec::new();
    let mut run = String::new();
    let mut run_style = light;
    for (ch, st) in cells {
        if st != run_style && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), run_style));
        }
        run_style = st;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, run_style));
    }
    Line::from(spans)
}

struct BodySplit {
    list: Rect,
    detail_outer: Rect,
    side_by_side: bool,
    /// Column of the vertical divider when list and detail sit side by side.
    divider_x: Option<u16>,
}

fn body_layout(app: &App, area: Rect) -> BodySplit {
    let side_by_side = match app.settings.layout {
        LayoutMode::Split => true,
        LayoutMode::Stack => false,
        LayoutMode::Auto => area.width >= 140,
    };
    let (list, detail_outer) = if side_by_side {
        let [l, d] =
            Layout::horizontal([Constraint::Min(48), Constraint::Percentage(42)]).areas(area);
        (l, d)
    } else {
        let [l, d] = Layout::vertical([Constraint::Min(4), Constraint::Percentage(48)]).areas(area);
        (l, d)
    };
    BodySplit {
        list,
        detail_outer,
        side_by_side,
        divider_x: side_by_side.then_some(detail_outer.x),
    }
}

fn draw_body(f: &mut Frame, split: BodySplit, app: &mut App) {
    let t = app.theme;
    let block = Block::new()
        .borders(if split.side_by_side {
            Borders::LEFT
        } else {
            Borders::TOP
        })
        .border_style(Style::new().fg(t.border));
    let detail_area = block.inner(split.detail_outer);
    f.render_widget(block, split.detail_outer);

    app.hits.list = split.list;
    app.hits.detail = detail_area;

    list::draw(f, split.list, app);
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

    let views = app.settings.views.len();
    let view_keys = if views > 1 {
        format!("1–{}", views.min(9))
    } else {
        "1".into()
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, d) in [
        (view_keys.as_str(), "view"),
        ("o", "open"),
        ("c", "checks"),
        ("y", "copy"),
        ("/", "filter"),
        ("s", "sort"),
        ("r", "refresh"),
        ("?", "help"),
        ("q", "quit"),
    ] {
        spans.push(Span::styled(k.to_string(), Style::new().fg(t.accent)));
        spans.push(Span::styled(format!(" {d}  "), Style::new().fg(t.muted)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

//! Input handling: keys and mouse, resolved against the regions the last frame
//! recorded in `app.hits`.

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::time::{Duration, Instant};

use crate::app::{App, Mode, Sort};

/// Two clicks on the same row inside this window count as a double-click,
/// which crossterm does not report on its own.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Handle one key. What a key means depends on the mode: while typing a filter
/// letters are text, and while help is open everything else is swallowed.
pub fn key(app: &mut App, k: KeyEvent) {
    if k.kind == KeyEventKind::Release {
        return;
    }
    match app.mode {
        Mode::Filter => filter_key(app, k),
        Mode::Help => help_key(app, k.code),
        Mode::Browse => browse_key(app, k),
    }
}

fn filter_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Esc => {
            app.filter.clear();
            app.mode = Mode::Browse;
            app.rebuild();
        }
        KeyCode::Enter => app.mode = Mode::Browse,
        KeyCode::Backspace => {
            app.filter.pop();
            app.rebuild();
        }
        // Only plain typing is text: a Ctrl or Alt chord reports its letter
        // too, and Ctrl-A must not type an `a` into the filter.
        KeyCode::Char(c)
            if !k
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            app.filter.push(c);
            app.rebuild();
        }
        _ => {}
    }
}

/// `q` closes help rather than quitting, so a reflexive `q` never loses the
/// session; only a second one quits.
const fn help_key(app: &mut App, code: KeyCode) {
    if matches!(code, KeyCode::Char('?' | 'q') | KeyCode::Esc) {
        app.mode = Mode::Browse;
    }
}

fn browse_key(app: &mut App, k: KeyEvent) {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Char('c') if ctrl => app.quit = true,
        KeyCode::Char('q') => app.quit = true,
        KeyCode::Char('?') => app.mode = Mode::Help,
        KeyCode::Esc => {
            if !app.filter.is_empty() {
                app.filter.clear();
                app.rebuild();
            }
        }

        KeyCode::Char('j') | KeyCode::Down => app.move_sel(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_sel(-1),
        KeyCode::Char('g') | KeyCode::Home => app.select(0),
        KeyCode::Char('G') | KeyCode::End => {
            let last = app.rows.len().saturating_sub(1);
            app.select(last);
        }
        KeyCode::PageDown => app.move_sel(10),
        KeyCode::PageUp => app.move_sel(-10),
        KeyCode::Char('J') => app.detail_scroll = app.detail_scroll.saturating_add(1),
        KeyCode::Char('K') => app.detail_scroll = app.detail_scroll.saturating_sub(1),

        KeyCode::Tab => app.cycle_view(1),
        KeyCode::BackTab => app.cycle_view(-1),
        KeyCode::Char(c @ '1'..='9') => {
            // Positional: the digit indexes the configured tab bar.
            let position = c
                .to_digit(10)
                .and_then(|d| usize::try_from(d).ok())
                .map_or(0, |d| d - 1);
            if let Some(v) = app.view_at(position) {
                app.set_view(v);
            }
        }

        KeyCode::Enter | KeyCode::Char('o') => app.open_selected(),
        KeyCode::Char('c') => app.open_checks(),
        KeyCode::Char('y') => app.copy_url(),
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Char('/') => {
            app.mode = Mode::Filter;
            app.filter.clear();
            app.rebuild();
        }
        KeyCode::Char('s') => {
            app.sort = match app.sort {
                Sort::Recent => Sort::Attention,
                Sort::Attention => Sort::Recent,
            };
            app.rebuild();
        }
        KeyCode::Char('d') => {
            app.settings.show_drafts = !app.settings.show_drafts;
            let state = if app.settings.show_drafts {
                "shown"
            } else {
                "hidden"
            };
            app.note(format!("drafts {state}"));
            app.rebuild();
        }
        _ => {}
    }
}

/// Mouse state: the last click, to recognize a double-click.
#[derive(Default)]
pub struct Input {
    last_click: Option<(u16, Instant)>,
}

impl Input {
    pub fn mouse(&mut self, app: &mut App, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        // Help is modal, as it is for keys: a click closes it, and nothing
        // reaches the dashboard underneath.
        if app.mode == Mode::Help {
            if m.kind == MouseEventKind::Down(MouseButton::Left) {
                app.mode = Mode::Browse;
            }
            return;
        }
        match m.kind {
            MouseEventKind::ScrollDown => {
                if in_rect(app.hits.detail, x, y) {
                    app.detail_scroll = app.detail_scroll.saturating_add(2);
                } else {
                    app.move_sel(2);
                }
            }
            MouseEventKind::ScrollUp => {
                if in_rect(app.hits.detail, x, y) {
                    app.detail_scroll = app.detail_scroll.saturating_sub(2);
                } else {
                    app.move_sel(-2);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, v)) = app
                    .hits
                    .tabs
                    .iter()
                    .find(|(r, _)| in_rect(*r, x, y))
                    .map(|(r, v)| (*r, *v))
                {
                    app.set_view(v);
                    return;
                }

                if let Some(idx) = app
                    .hits
                    .checks
                    .iter()
                    .find(|(cy, _)| *cy == y)
                    .map(|(_, i)| *i)
                    && in_rect(app.hits.detail, x, y)
                {
                    app.open_check(idx);
                    return;
                }

                // Rows are recorded by screen line, so the column has to be
                // checked too: side by side, the detail pane shares those lines.
                if let Some(idx) = app
                    .hits
                    .rows
                    .iter()
                    .find(|(ry, _)| *ry == y)
                    .map(|(_, i)| *i)
                    && in_rect(app.hits.list, x, y)
                {
                    let now = Instant::now();
                    let double = self
                        .last_click
                        .is_some_and(|(ly, lt)| ly == y && now.duration_since(lt) < DOUBLE_CLICK);
                    app.select(idx);
                    self.last_click = Some((y, now));
                    // `select` already reset the detail scroll. A double-click
                    // on a worktree with no PR opens nothing, which
                    // `open_selected` reports for us.
                    if double {
                        app.open_selected();
                    }
                }
            }
            _ => {}
        }
    }
}

const fn in_rect(r: ratatui::layout::Rect, x: u16, y: u16) -> bool {
    r.width > 0 && r.height > 0 && x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
}

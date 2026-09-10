//! Input handling: keys and mouse, resolved against the regions the last frame
//! recorded in `app.hits`.

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::time::{Duration, Instant};

use crate::app::{App, Row, Sort};

/// Two clicks on the same row inside this window count as a double-click,
/// which crossterm does not report on its own.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

#[derive(Default)]
pub struct Input {
    last_click: Option<(u16, Instant)>,
}

impl Input {
    pub fn key(&mut self, app: &mut App, k: KeyEvent) {
        if k.kind == KeyEventKind::Release {
            return;
        }

        if app.filter_mode {
            match k.code {
                KeyCode::Esc => {
                    app.filter.clear();
                    app.filter_mode = false;
                    app.rebuild();
                }
                KeyCode::Enter => app.filter_mode = false,
                KeyCode::Backspace => {
                    app.filter.pop();
                    app.rebuild();
                }
                KeyCode::Char(c) => {
                    app.filter.push(c);
                    app.rebuild();
                }
                _ => {}
            }
            return;
        }

        if app.show_help
            && !matches!(
                k.code,
                KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q')
            )
        {
            return;
        }

        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('c') if ctrl => app.quit = true,
            KeyCode::Char('q') => {
                if app.show_help {
                    app.show_help = false;
                } else {
                    app.quit = true;
                }
            }
            KeyCode::Char('?') => app.show_help = !app.show_help,
            KeyCode::Esc => {
                if app.show_help {
                    app.show_help = false;
                } else if !app.filter.is_empty() {
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
                let i = c.to_digit(10).unwrap_or(1).saturating_sub(1) as usize;
                if let Some(v) = app.view_at(i) {
                    app.set_view(v);
                }
            }

            KeyCode::Enter | KeyCode::Char('o') => app.open_selected(),
            KeyCode::Char('c') => app.open_checks(),
            KeyCode::Char('y') => app.copy_url(),
            KeyCode::Char('r') => app.refresh(),
            KeyCode::Char('/') => {
                app.filter_mode = true;
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

    pub fn mouse(&mut self, app: &mut App, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
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
                if app.show_help {
                    app.show_help = false;
                    return;
                }

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

                if let Some(idx) = app
                    .hits
                    .rows
                    .iter()
                    .find(|(ry, _)| *ry == y)
                    .map(|(_, i)| *i)
                {
                    let now = Instant::now();
                    let double = self
                        .last_click
                        .is_some_and(|(ly, lt)| ly == y && now.duration_since(lt) < DOUBLE_CLICK);
                    app.select(idx);
                    self.last_click = Some((y, now));
                    if double {
                        // A double-click on a worktree with no PR opens nothing,
                        // which `open_selected` reports for us.
                        app.open_selected();
                    } else if matches!(app.rows.get(idx), Some(Row::Wt(_)) | Some(Row::Pr(_))) {
                        app.detail_scroll = 0;
                    }
                }
            }
            _ => {}
        }
    }
}

fn in_rect(r: ratatui::layout::Rect, x: u16, y: u16) -> bool {
    r.width > 0 && r.height > 0 && x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
}

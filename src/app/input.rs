//! Terminal event dispatch, including counted keys and viewport-relative mouse input.
//! Mouse rows include the top status bar; subtract it exactly once before
//! handing coordinates to PDF geometry. IPC actions use the same App methods.

use super::{geometry::take_count, App};
use crate::{resolution::CellPixels, terminal};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};

/// Preserve event-loop control without coupling input code to worker channels.
pub(super) enum InputOutcome {
    Handled,
    Skip,
    Quit,
}

impl App {
    /// Dispatch one terminal event and report its control-flow effect to the main loop.
    pub(super) fn handle_input(&mut self, event: Event) -> InputOutcome {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press && self.help_open => {
                match key.code {
                    KeyCode::Char('?') | KeyCode::Esc => self.close_help(),
                    KeyCode::Char('q') => return InputOutcome::Quit,
                    KeyCode::Down | KeyCode::Char('j') => self.scroll_help(1),
                    KeyCode::Up | KeyCode::Char('k') => self.scroll_help(-1),
                    KeyCode::PageDown => self.scroll_help(10),
                    KeyCode::PageUp => self.scroll_help(-10),
                    KeyCode::Home | KeyCode::Char('g') => self.set_help_offset(0),
                    KeyCode::End | KeyCode::Char('G') => self.set_help_offset(usize::MAX),
                    _ => {}
                }
            }
            Event::Key(key) if key.kind == KeyEventKind::Press && self.command_mode => {
                self.command_key(key.code);
            }
            Event::Key(key)
                if key.kind == KeyEventKind::Press
                    && key.code == KeyCode::Char('0')
                    && self.page_input.is_empty() =>
            {
                if self.count > 0 {
                    self.reset_zoom();
                }
            }
            Event::Key(key)
                if key.kind == KeyEventKind::Press
                    && matches!(key.code, KeyCode::Char(c) if c.is_ascii_digit()) =>
            {
                if self.page_input.len() < 8 {
                    if let KeyCode::Char(c) = key.code {
                        self.page_input.push(c);
                    }
                }
            }
            Event::Key(key)
                if key.kind == KeyEventKind::Press && key.code == KeyCode::Backspace =>
            {
                self.page_input.pop();
            }
            Event::Key(key) if key.kind == KeyEventKind::Press && self.count == 0 => {
                let step = take_count(&mut self.page_input) as i32;
                match key.code {
                    KeyCode::Char('q') => return InputOutcome::Quit,
                    KeyCode::Char('?') => self.open_help(),
                    KeyCode::Char('w') => self.move_cursor(0, -step),
                    KeyCode::Char('s') => self.move_cursor(0, step),
                    KeyCode::Char('a') => self.move_cursor(-step, 0),
                    KeyCode::Char('d') => self.move_cursor(step, 0),
                    KeyCode::Char('c') => self.center_cursor(),
                    _ => {}
                }
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let has_count = !self.page_input.is_empty();
                let count = if key.code == KeyCode::Enter {
                    1
                } else {
                    take_count(&mut self.page_input)
                };
                let step = count as i32;
                match key.code {
                    KeyCode::Char('q') => return InputOutcome::Quit,
                    KeyCode::Char('?') => self.open_help(),
                    KeyCode::Esc => {
                        self.page_input.clear();
                        self.clear_highlights();
                    }
                    KeyCode::Enter if !self.page_input.is_empty() => {
                        if let Ok(page) = self.page_input.parse() {
                            self.go(page);
                        }
                        self.page_input.clear();
                    }
                    KeyCode::Char('t') => self.toggle_view(),
                    KeyCode::Char('[') => self.switch_relative(-(count as isize)),
                    KeyCode::Char(']') => self.switch_relative(count as isize),
                    KeyCode::Char(':') => {
                        self.command_mode = true;
                        self.command_input.clear();
                        self.page_input.clear();
                    }
                    KeyCode::Char('=') | KeyCode::Char('+') => {
                        self.change_zoom(25 * count.min(10) as i16)
                    }
                    KeyCode::Char('-') => self.change_zoom(-25 * count.min(10) as i16),
                    KeyCode::Char('H') => self.pan(-4 * step, 0),
                    KeyCode::Char('L') => self.pan(4 * step, 0),
                    KeyCode::Char('J') if !self.scroll_mode => self.pan(0, 4 * step),
                    KeyCode::Char('K') if !self.scroll_mode => self.pan(0, -4 * step),
                    KeyCode::Down | KeyCode::Char('j') if self.scroll_mode => {
                        self.scroll_by(3 * step)
                    }
                    KeyCode::Up | KeyCode::Char('k') if self.scroll_mode => {
                        self.scroll_by(-3 * step)
                    }
                    KeyCode::PageDown if self.scroll_mode => {
                        self.scroll_by((self.view_height() as i32 - 1).saturating_mul(step))
                    }
                    KeyCode::PageUp if self.scroll_mode => {
                        self.scroll_by((1 - self.view_height() as i32).saturating_mul(step))
                    }
                    KeyCode::Right
                    | KeyCode::PageDown
                    | KeyCode::Char('j')
                    | KeyCode::Char('l') => {
                        self.go(self.page.saturating_add(count).min(self.count))
                    }
                    KeyCode::Left | KeyCode::PageUp | KeyCode::Char('k') | KeyCode::Char('h') => {
                        self.go(self.page.saturating_sub(count).max(1))
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        self.go(if has_count { count } else { 1 })
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        self.go(if has_count { count } else { self.count })
                    }
                    KeyCode::Char('r') => self.reload(),
                    KeyCode::Char('v') => self.start_selection(),
                    KeyCode::Char('y') => self.copy_selection(),
                    KeyCode::Char('I') => self.inverse(true),
                    KeyCode::Char('i') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        self.inverse(true)
                    }
                    KeyCode::Char('i') | KeyCode::Enter => self.inverse(false),
                    KeyCode::Char('w') => self.move_cursor(0, -step),
                    KeyCode::Char('s') => self.move_cursor(0, step),
                    KeyCode::Char('a') => self.move_cursor(-step, 0),
                    KeyCode::Char('d') => self.move_cursor(step, 0),
                    KeyCode::Char('c') => self.center_cursor(),
                    _ => {}
                }
            }
            Event::Mouse(mouse)
                if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Right)) =>
            {
                if self.in_document_row(mouse.row) {
                    self.cursor = (
                        mouse.column.min(self.view_width().saturating_sub(1)),
                        mouse.row - 1,
                    );
                    self.set_scroll_cursor_page();
                    self.inverse(false);
                }
            }
            Event::Resize(w, h) => {
                let cell_pixels = CellPixels::current();
                if self.dimensions == (w, h) && self.cell_pixels == cell_pixels {
                    return InputOutcome::Skip;
                }
                self.dimensions = (w, h);
                self.cell_pixels = cell_pixels;
                self.cursor.0 = self.cursor.0.min(self.view_width().saturating_sub(1));
                self.cursor.1 = self.cursor.1.min(self.view_height().saturating_sub(1));
                self.cache.clear();
                self.prefetch_failed.clear();
                self.displayed = None;
                self.base_image = None;
                self.selection = None;
                self.selection_image = None;
                self.marker_image = None;
                self.scroll_surface_initialized = false;
                self.scroll_generation += 1;
                self.single_surface_initialized = false;
                terminal::clear();
                terminal::marker(self.cursor.0, self.cursor.1 + 1);
                if self.count > 0 {
                    self.request();
                }
                if self.help_open {
                    self.draw_help();
                }
            }
            Event::Mouse(mouse) if self.help_open => match mouse.kind {
                MouseEventKind::ScrollDown => self.scroll_help(3),
                MouseEventKind::ScrollUp => self.scroll_help(-3),
                _ => {}
            },
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollDown if mouse.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.pan_wheel(3)
                }
                MouseEventKind::ScrollUp if mouse.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.pan_wheel(-3)
                }
                MouseEventKind::ScrollDown if self.scroll_mode => self.scroll_by(3),
                MouseEventKind::ScrollUp if self.scroll_mode => self.scroll_by(-3),
                MouseEventKind::ScrollDown if self.zoom > 100 => self.pan_wheel_vertical(3),
                MouseEventKind::ScrollUp if self.zoom > 100 => self.pan_wheel_vertical(-3),
                MouseEventKind::ScrollLeft => self.pan_wheel(-3),
                MouseEventKind::ScrollRight => self.pan_wheel(3),
                _ => {}
            },
            _ => {}
        }
        InputOutcome::Handled
    }
}

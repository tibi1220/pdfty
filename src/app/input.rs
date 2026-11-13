//! Keyboard and mouse handling.

use super::{App, Panel, ZOOM_RANGE};
use crate::{layout::Mode, terminal};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

/// Rows scrolled per j/k or wheel notch.
const LINE_STEP: i64 = 3;
const PAN_STEP: i64 = 4;

impl App {
    pub(super) fn input(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key),
            Event::Mouse(mouse) => self.mouse(mouse),
            Event::Resize(w, h) => self.resize(w, h),
            _ => {}
        }
    }

    fn resize(&mut self, w: u16, h: u16) {
        self.term = (w, h);
        self.cell = terminal::cell_pixels(self.gfx.tty());
        let (vw, vh) = self.view_size();
        self.cursor = (self.cursor.0.min(vw - 1), self.cursor.1.min(vh - 1));
        self.screen.clear();
        self.keep_view_in_bounds();
        self.dirty = true;
    }

    fn take_count(&mut self) -> Option<u32> {
        let count = self.count.parse::<u32>().ok().filter(|&c| c > 0);
        self.count.clear();
        count
    }

    fn key(&mut self, key: KeyEvent) {
        self.dirty = true;
        if let Panel::Help(offset) | Panel::Log(offset) = self.panel {
            let log = matches!(self.panel, Panel::Log(_));
            let offset = match key.code {
                KeyCode::Esc => return self.panel = Panel::None,
                KeyCode::Char('?') if !log => return self.panel = Panel::None,
                KeyCode::Char('M') if log => return self.panel = Panel::None,
                KeyCode::Char('q') if log => return self.panel = Panel::None,
                KeyCode::Char('q') => return self.quit = true,
                KeyCode::Down | KeyCode::Char('j') => offset + 1,
                KeyCode::Up | KeyCode::Char('k') => offset.saturating_sub(1),
                KeyCode::PageDown | KeyCode::Char(' ') => offset + 10,
                KeyCode::PageUp => offset.saturating_sub(10),
                KeyCode::Char('g') | KeyCode::Home => 0,
                KeyCode::Char('G') | KeyCode::End => usize::MAX,
                _ => offset,
            };
            self.set_panel_offset(offset);
            return;
        }
        if let Some(prompt) = &mut self.prompt {
            match key.code {
                KeyCode::Esc => self.prompt = None,
                KeyCode::Backspace => {
                    if prompt.pop().is_none() {
                        self.prompt = None;
                    }
                }
                KeyCode::Char(c) if c.is_ascii_digit() && prompt.len() < 8 => prompt.push(c),
                KeyCode::Enter => {
                    let input = self.prompt.take().unwrap_or_default();
                    match input.parse::<usize>() {
                        Ok(page) if page >= 1 && page <= self.page_count() => self.goto_page(page - 1),
                        _ => self.note(format!("page must be between 1 and {}", self.page_count()), true),
                    }
                }
                _ => {}
            }
            return;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char(c) if c.is_ascii_digit() && !ctrl && !(c == '0' && self.count.is_empty()) => {
                if self.count.len() < 8 {
                    self.count.push(c);
                }
                return;
            }
            KeyCode::Backspace => {
                self.count.pop();
                return;
            }
            _ => {}
        }
        let explicit = self.take_count();
        let n = explicit.unwrap_or(1) as i64;
        let (_, vh) = self.view_size();
        let has_doc = self.doc().is_some();
        let continuous = self.mode == Mode::Continuous;
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('?') => self.panel = Panel::Help(0),
            KeyCode::Char('M') => self.panel = Panel::Log(0),
            KeyCode::Char('D') => {
                self.debug_lines = !self.debug_lines;
                let (w, h) = self.term;
                self.resize(w, h);
            }
            KeyCode::Esc => self.clear_highlights(),
            KeyCode::Char(':') => self.prompt = Some(String::new()),
            KeyCode::Char('w') => self.move_cursor(0, -n),
            KeyCode::Char('s') => self.move_cursor(0, n),
            KeyCode::Char('a') => self.move_cursor(-n, 0),
            KeyCode::Char('d') if !ctrl => self.move_cursor(n, 0),
            KeyCode::Char('c') => self.center_cursor(),
            _ if !has_doc => {}
            KeyCode::Enter if explicit.is_some() => self.goto_page(n as usize - 1),
            KeyCode::Char('d') => self.scroll_lines(n * (vh as i64 / 2)),
            KeyCode::Char('u') if ctrl => self.scroll_lines(-n * (vh as i64 / 2)),
            KeyCode::Char('f') if ctrl => self.screen_down(n),
            KeyCode::Char('b') if ctrl => self.screen_down(-n),
            KeyCode::Char('e') if ctrl => self.scroll_lines(n),
            KeyCode::Char('y') if ctrl => self.scroll_lines(-n),
            KeyCode::Char('j') | KeyCode::Down if continuous => self.scroll_lines(LINE_STEP * n),
            KeyCode::Char('k') | KeyCode::Up if continuous => self.scroll_lines(-LINE_STEP * n),
            KeyCode::Char('j') | KeyCode::Down => self.page_relative(n),
            KeyCode::Char('k') | KeyCode::Up => self.page_relative(-n),
            KeyCode::Char(' ') if key.modifiers.contains(KeyModifiers::SHIFT) => self.screen_down(-n),
            KeyCode::Char(' ') | KeyCode::PageDown => self.screen_down(n),
            KeyCode::PageUp => self.screen_down(-n),
            KeyCode::Char('l') | KeyCode::Right => self.page_relative(n),
            KeyCode::Char('h') | KeyCode::Left => self.page_relative(-n),
            KeyCode::Char('g') | KeyCode::Home => self.goto_page(explicit.map_or(0, |p| p as usize - 1)),
            KeyCode::Char('G') | KeyCode::End => {
                self.goto_page(explicit.map_or(usize::MAX, |p| p as usize - 1))
            }
            KeyCode::Char('H') => {
                self.scroll(-PAN_STEP * n, 0);
            }
            KeyCode::Char('L') => {
                self.scroll(PAN_STEP * n, 0);
            }
            KeyCode::Char('J') => {
                self.scroll(0, PAN_STEP * n);
            }
            KeyCode::Char('K') => {
                self.scroll(0, -PAN_STEP * n);
            }
            KeyCode::Char('=') | KeyCode::Char('+') => self.zoom_step(n),
            KeyCode::Char('-') => self.zoom_step(-n),
            KeyCode::Char('0') => self.reset_zoom(),
            KeyCode::Char('t') => {
                self.set_mode(if continuous { Mode::Single } else { Mode::Continuous })
            }
            KeyCode::Char('[') => self.switch_relative(-n as isize),
            KeyCode::Char(']') => self.switch_relative(n as isize),
            KeyCode::Char('r') => self.reload_current(),
            KeyCode::Char('v') => self.start_selection(),
            KeyCode::Char('y') => self.copy_selection(),
            KeyCode::Char('I') => self.inverse(true),
            KeyCode::Char('i') | KeyCode::Enter => self.inverse(key.modifiers.contains(KeyModifiers::SHIFT)),
            _ => {}
        }
    }

    fn set_panel_offset(&mut self, offset: usize) {
        let offset = offset.min(self.panel_max_offset());
        self.panel = match self.panel {
            Panel::Help(_) => Panel::Help(offset),
            Panel::Log(_) => Panel::Log(offset),
            Panel::None => Panel::None,
        };
        self.dirty = true;
    }

    fn screen_down(&mut self, screens: i64) {
        if self.mode == Mode::Single {
            // Within a tall page scroll it; otherwise turn the page.
            let (_, vh) = self.view_size();
            let before = self.doc().map(|d| d.view);
            self.scroll(0, screens * (vh as i64 - 2).max(1));
            if self.doc().map(|d| d.view) == before {
                self.page_relative(screens.signum());
            }
        } else {
            let (_, vh) = self.view_size();
            self.scroll_lines(screens * (vh as i64 - 2).max(1));
        }
    }

    fn zoom_step(&mut self, steps: i64) {
        let Some(zoom) = self.doc().map(|d| d.zoom as i64) else { return };
        let next = (zoom + 25 * steps.clamp(-20, 20)).clamp(ZOOM_RANGE.0 as i64, ZOOM_RANGE.1 as i64);
        self.set_zoom(next as u32);
    }

    fn mouse(&mut self, mouse: MouseEvent) {
        let (vw, vh) = self.view_size();
        // Row 0 is the status line; page rows start at 1.
        let in_view = mouse.row >= 1 && mouse.row - 1 < vh && mouse.column < vw;
        let cell = (mouse.column, mouse.row.saturating_sub(1));
        let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = mouse.modifiers.contains(KeyModifiers::CONTROL);
        match self.panel {
            Panel::Help(offset) | Panel::Log(offset) => {
                match mouse.kind {
                    MouseEventKind::ScrollDown => self.set_panel_offset(offset + 3),
                    MouseEventKind::ScrollUp => self.set_panel_offset(offset.saturating_sub(3)),
                    _ => {}
                }
                return;
            }
            Panel::None => {}
        }
        match mouse.kind {
            MouseEventKind::ScrollDown if shift => {
                self.scroll(2 * LINE_STEP, 0);
            }
            MouseEventKind::ScrollUp if shift => {
                self.scroll(-2 * LINE_STEP, 0);
            }
            MouseEventKind::ScrollRight => {
                self.scroll(2 * LINE_STEP, 0);
            }
            MouseEventKind::ScrollLeft => {
                self.scroll(-2 * LINE_STEP, 0);
            }
            MouseEventKind::ScrollDown => self.scroll_lines(LINE_STEP),
            MouseEventKind::ScrollUp => self.scroll_lines(-LINE_STEP),
            MouseEventKind::Down(MouseButton::Left) if in_view && ctrl => {
                self.set_cursor(cell);
                self.inverse(true);
            }
            MouseEventKind::Down(MouseButton::Left) if in_view => {
                self.selection = None;
                self.set_cursor(cell);
                self.drag_from = Some(cell);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(from) = self.drag_from {
                    if self.selection.is_none() && from != cell {
                        self.set_cursor(from);
                        self.start_selection();
                    }
                    self.set_cursor((cell.0.min(vw - 1), cell.1.min(vh - 1)));
                }
            }
            MouseEventKind::Up(MouseButton::Left) => self.drag_from = None,
            MouseEventKind::Down(MouseButton::Right) if in_view => {
                self.set_cursor(cell);
                self.inverse(false);
            }
            _ => {}
        }
    }
}

//! Text overlays and the page-number prompt.
//! Help suspends image placements until it closes, so Kitty cannot erase the
//! terminal text while a background composition finishes.

use super::{geometry::parse_page_command, App};
use crate::terminal;
use crossterm::event::KeyCode;

impl App {
    /// Suspend frame updates and paint the terminal help overlay.
    pub(super) fn open_help(&mut self) {
        self.help_open = true;
        self.help_offset = 0;
        self.scroll_generation += 1;
        terminal::clear();
        self.draw_help();
    }

    /// Restore the PDF frame after the text overlay is dismissed.
    pub(super) fn close_help(&mut self) {
        self.help_open = false;
        terminal::clear();
        self.scroll_surface_initialized = false;
        self.single_surface_initialized = false;
        self.redraw();
    }

    /// Paint the current slice of help without changing image state.
    pub(super) fn draw_help(&self) {
        terminal::help(HELP_LINES, self.help_offset, self.dimensions);
    }

    /// Clamp help scrolling to the number of lines that fit in the terminal.
    pub(super) fn set_help_offset(&mut self, offset: usize) {
        let visible = terminal::help_visible_lines(self.dimensions);
        self.help_offset = offset.min(HELP_LINES.len().saturating_sub(visible));
        self.draw_help();
    }

    /// Apply a signed line delta to the help overlay.
    pub(super) fn scroll_help(&mut self, delta: i32) {
        let next = (self.help_offset as i64 + delta as i64).max(0) as usize;
        self.set_help_offset(next);
    }

    /// Edit or submit the absolute-page prompt, independent of movement counts.
    pub(super) fn command_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.command_mode = false;
                self.command_input.clear();
            }
            KeyCode::Backspace => {
                self.command_input.pop();
            }
            KeyCode::Enter => {
                self.command_mode = false;
                let input = std::mem::take(&mut self.command_input);
                if !input.is_empty() {
                    match parse_page_command(&input, self.count) {
                        Ok(page) => self.go(page),
                        Err(e) => self.status = e,
                    }
                }
            }
            KeyCode::Char(c) if c.is_ascii_digit() && self.command_input.len() < 8 => {
                self.command_input.push(c);
            }
            _ => {}
        }
    }
}

// Keep the help text next to its input/scroll behavior so shortcuts stay in sync.
const HELP_LINES: &[&str] = &[
    "NAVIGATION",
    "h / Left              Previous page (both views)",
    "l / Right             Next page (both views)",
    "k / j                 Previous / next page (single view)",
    "j / k, Down / Up      Scroll 3 rows (continuous view)",
    "Page Down / Page Up  Next / previous page (single view)",
    "Page Down / Page Up  Scroll one viewport (continuous)",
    "g / Home, G / End     First / last page",
    "Digits then Enter    Jump to a page",
    "Count before a key    Repeat movement: 10w, 3j, 2=",
    "Backspace / Esc       Edit / cancel a count",
    "10g / 10G             Jump to page 10",
    ": number Enter        Jump to a page; Backspace edits, Esc cancels",
    "",
    "VIEW",
    "t                     Toggle single-page / continuous view",
    "[ / ]                 Previous / next PDF",
    "= / +, -              Zoom in / out",
    "0                     Reset zoom to 100%",
    "w / a / s / d         Move cursor up / left / down / right",
    "                      Pan / scroll automatically at viewport edges",
    "c                     Center the document point under cursor",
    "H / L                 Pan left / right",
    "J / K                 Pan down / up (single-page view)",
    "r                     Reload PDF",
    "",
    "TEXT AND SOURCE",
    "v                     Start selection at the visible cursor",
    "w / a / s / d         Extend selection up / left / down / right",
    "y                     Copy selected text",
    "Esc                   Clear all highlights and pending count",
    "i / Enter             Inverse search (requires --synctex)",
    "Shift + I             Inverse search and focus Neovim",
    "",
    "MOUSE",
    "Right click           Open source at click (inverse search)",
    "Wheel                 Scroll pages; pan when zoomed in single view",
    "Shift + wheel         Pan horizontally",
    "Horizontal wheel      Pan horizontally",
    "",
    "?                     Open / close this help",
    "q                     Quit viewer",
];

//! Status/footer painting and rolling frame-rate accounting.
//! These text rows update less often than PDF images to avoid flooding tmux.

use super::{App, FrameRate};
use crate::terminal;
use std::time::{Duration, Instant};

impl App {
    /// Repaint status/call rows at the slower cadence selected by the event loop.
    pub(super) fn draw_status(&mut self) {
        let top = if self.count == 0 {
            format!(
                "No PDF | Waiting for :PdftyAdd in Neovim | Status: {}",
                self.status
            )
        } else {
            format!(
                "{} ({}/{}) | Page {}/{} | Zoom {}% | Prepared {}/{} | {} FPS | Status: {}",
                self.pdf.file_name().unwrap_or_default().to_string_lossy(),
                self.documents
                    .iter()
                    .position(|pdf| pdf == &self.pdf)
                    .unwrap_or(0)
                    + 1,
                self.documents.len(),
                self.page,
                self.count,
                self.zoom,
                self.cache.len(),
                self.count,
                self.frame_rate.current(),
                self.status
            )
        };
        terminal::status_at(&top, self.dimensions.0, 0);
        if self.dimensions.1 >= 6 {
            for index in 0..3 {
                let line = self
                    .recent_calls
                    .get(index)
                    .map(|call| format!("Call {}: {call}", index + 1))
                    .unwrap_or_else(|| format!("Call {}: —", index + 1));
                terminal::call_at(
                    &line,
                    self.dimensions.0,
                    self.dimensions.1 - 4 + index as u16,
                );
            }
        }
        if self.command_mode {
            terminal::status(
                &format!(":{}", self.command_input),
                self.dimensions.0,
                self.dimensions.1,
            );
        } else {
            let view = if self.scroll_mode {
                "continuous"
            } else {
                "single"
            };
            let nvim = if self.nvim.is_some() {
                "connected"
            } else {
                "disconnected"
            };
            let socket = self
                .socket_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy())
                .unwrap_or_else(|| "none".into());
            let bottom = format!(
                "View: {view} | Neovim: {nvim} | Socket: {socket} | {}  [? help]",
                self.page_input
            );
            terminal::status(&bottom, self.dimensions.0, self.dimensions.1);
        }
    }
}

impl FrameRate {
    pub(super) fn record(&mut self) {
        self.recent.push_back(Instant::now());
        self.trim();
    }

    pub(super) fn current(&mut self) -> usize {
        self.trim();
        self.recent.len()
    }

    pub(super) fn trim(&mut self) {
        let now = Instant::now();
        while self
            .recent
            .front()
            .is_some_and(|time| now.duration_since(*time) >= Duration::from_secs(1))
        {
            self.recent.pop_front();
        }
    }
}

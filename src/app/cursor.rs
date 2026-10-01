//! Cursor movement and document-preserving centering.
//! Cursor coordinates are viewport cells; pan/scroll offsets locate those cells
//! in the PDF. Moving both together preserves the point used for selection.

use super::{
    geometry::{center_cursor_axis, move_cursor_axis, pan_bounds, scroll_bounds},
    App,
};
use crate::{image, terminal};

impl App {
    /// Capture the cursor state included in the next raster frame.
    pub(super) fn image_cursor(&self) -> image::Cursor {
        image::Cursor {
            cell: self.cursor,
            selecting: self.selection.is_some(),
        }
    }

    /// Center the current document point without changing selection endpoints.
    pub(super) fn center_cursor(&mut self) {
        let old = self.cursor;
        if self.count == 0 {
            self.cursor = (self.view_width() / 2, self.view_height() / 2);
            terminal::erase_marker(old.0, old.1 + 1);
            terminal::marker(self.cursor.0, self.cursor.1 + 1);
            return;
        }
        // Translate the viewport and cursor together. Their sum is the same
        // document position, including near edges where blank margins appear.
        (self.cursor.0, self.pan_x) = center_cursor_axis(old.0, self.pan_x, self.view_width());
        if self.scroll_mode {
            (self.cursor.1, self.scroll_offset) =
                center_cursor_axis(old.1, self.scroll_offset, self.view_height());
            self.scroll_generation += 1;
            self.prefetch_next();
        } else {
            (self.cursor.1, self.pan_y) = center_cursor_axis(old.1, self.pan_y, self.view_height());
        }
        // The document point has not moved, so selection endpoints stay intact.
        self.redraw();
    }

    /// Move within the viewport, then pan/scroll when a viewport edge is reached.
    pub(super) fn move_cursor(&mut self, dx: i32, mut dy: i32) {
        let old = (self.cursor, self.pan_x, self.pan_y, self.scroll_offset);
        let pane = (self.view_width(), self.view_height());
        let (width, height) = if self.scroll_mode {
            (self.scroll_page_width(self.page), self.total_rows())
        } else {
            self.single_page_size(self.page_size_cached(self.page))
        };
        // Selections remain on their anchor page. Normal cursor movement can
        // cross pages in continuous view and updates the text beneath it.
        if self.scroll_mode && self.selection.is_some() {
            let row = self.scroll_offset + self.cursor.1 as i64;
            let start = self.page_start(self.page);
            let end = start + self.page_rows(self.page) - 1;
            let next = (row + dy as i64).clamp(start as i64, end as i64);
            dy = (next - row) as i32;
        }
        (self.cursor.0, self.pan_x) = move_cursor_axis(
            self.cursor.0,
            self.pan_x,
            pane.0,
            pan_bounds(width, pane.0, self.pan_x),
            dx,
        );
        let vertical = if self.scroll_mode {
            self.scroll_offset
        } else {
            self.pan_y
        };
        let (row, offset) = move_cursor_axis(
            self.cursor.1,
            vertical,
            pane.1,
            if self.scroll_mode {
                scroll_bounds(height, pane.1, vertical)
            } else {
                pan_bounds(height, pane.1, vertical)
            },
            dy,
        );
        self.cursor.1 = row;
        if self.scroll_mode {
            self.scroll_offset = offset;
        } else {
            self.pan_y = offset;
        }
        if old == (self.cursor, self.pan_x, self.pan_y, self.scroll_offset) {
            return;
        }
        if self.count == 0 {
            terminal::erase_marker(old.0 .0, old.0 .1 + 1);
            terminal::marker(self.cursor.0, self.cursor.1 + 1);
            return;
        }
        if self.selection.is_none() {
            self.set_scroll_cursor_page();
        }
        if self.scroll_mode {
            self.scroll_generation += 1;
            self.prefetch_next();
        }
        if !self.extend_selection() {
            self.redraw();
        }
    }
}

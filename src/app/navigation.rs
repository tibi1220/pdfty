//! Viewport changes: zoom, pan, scroll, and search-target reveal.
//! Methods invalidate only the renders/compositions affected by a change.
//! PDF-point conversion stays centralized in geometry.rs.

use super::{
    geometry::{pan_bounds, reveal_axis, scroll_bounds},
    App,
};
use crate::synctex;

impl App {
    /// Invalidate size-dependent pixels/text overlays and request the new scale.
    pub(super) fn change_zoom(&mut self, delta: i16) {
        let next = (self.zoom as i16 + delta).clamp(50, 300) as u16;
        if next == self.zoom {
            return;
        }
        self.zoom = next;
        self.scroll_generation += 1;
        self.pan_x = 0;
        self.pan_y = 0;
        self.selection = None;
        self.selection_image = None;
        self.marker_image = None;
        self.single_surface_initialized = false;
        self.cache.clear();
        self.prefetch_failed.clear();
        self.base_image = None;
        self.displayed = None;
        if self.scroll_mode {
            self.scroll_offset = self.centered_scroll_offset(self.page);
        }
        if let Some(marker) = self.marker.take() {
            self.reveal_marker(&marker);
            self.marker = Some(marker);
        }
        self.request();
    }
    /// Restore the normal zoom and pan origin.
    pub(super) fn reset_zoom(&mut self) {
        let delta = 100_i16 - self.zoom as i16;
        if delta == 0 {
            self.pan_x = 0;
            self.pan_y = 0;
            self.redraw();
        } else {
            self.change_zoom(delta);
        }
    }
    /// Apply keyboard pan and immediately paint its viewport.
    pub(super) fn pan(&mut self, dx: i32, dy: i32) {
        if self.move_pan(dx, dy) {
            self.redraw();
        }
    }
    /// Accumulate horizontal wheel movement for the event loop to coalesce.
    pub(super) fn pan_wheel(&mut self, dx: i32) {
        if self.move_pan(dx, 0) {
            self.scroll_dirty = true;
        }
    }
    /// Accumulate vertical wheel pan for the event loop to coalesce.
    pub(super) fn pan_wheel_vertical(&mut self, dy: i32) {
        if self.move_pan(0, dy) {
            self.scroll_dirty = true;
        }
    }
    /// Clamp signed pan while preserving margins introduced by centering.
    pub(super) fn move_pan(&mut self, dx: i32, dy: i32) -> bool {
        // Allow margins introduced by centering, while keeping the document
        // within reach of the viewport.
        let (width, height) = if self.scroll_mode {
            (self.scroll_page_width(self.page), self.page_rows(self.page))
        } else {
            self.single_page_size(self.page_size_cached(self.page))
        };
        let (min_x, max_x) = pan_bounds(width, self.view_width(), self.pan_x);
        let (min_y, max_y) = pan_bounds(height, self.view_height(), self.pan_y);
        let old = (self.pan_x, self.pan_y);
        self.pan_x = (self.pan_x + dx as i64).clamp(min_x, max_x);
        self.pan_y = (self.pan_y + dy as i64).clamp(min_y, max_y);
        if self.scroll_mode && old != (self.pan_x, self.pan_y) {
            self.scroll_generation += 1;
        }
        old != (self.pan_x, self.pan_y)
    }
    /// Bring a SyncTeX target into view and place the cursor on that target.
    pub(super) fn reveal_marker(&mut self, position: &synctex::Position) {
        // A forward-search target may lie outside the viewport after zoom.
        // Center only axes where the target is currently hidden.
        let size = self.page_size_cached(position.page);
        let width = if self.scroll_mode {
            self.scroll_page_width(position.page)
        } else {
            self.single_page_size(size).0
        };
        let target_x = position.x / size.0 * width as f64;
        self.pan_x = reveal_axis(self.pan_x, target_x, width, self.view_width() as u32);
        if self.scroll_mode {
            let target_row = self.page_start(position.page) as f64
                + position.y / size.1 * self.page_rows(position.page) as f64;
            self.scroll_offset = reveal_axis(
                self.scroll_offset,
                target_row,
                self.total_rows(),
                self.view_height() as u32,
            );
        } else {
            let height = self.single_page_size(size).1;
            let target_y = position.y / size.1 * height as f64;
            self.pan_y = reveal_axis(self.pan_y, target_y, height, self.view_height() as u32);
        }
        let (x, y) = if self.scroll_mode {
            (
                self.page_x(width) as f64 + target_x,
                self.page_start(position.page) as f64
                    + position.y / size.1 * self.page_rows(position.page) as f64
                    - self.scroll_offset as f64,
            )
        } else {
            let (left, top, width, height) = self.single_rect(size);
            (
                left as f64 + position.x / size.0 * width as f64,
                top as f64 + position.y / size.1 * height as f64,
            )
        };
        self.cursor = (
            x.floor()
                .clamp(0.0, self.view_width().saturating_sub(1) as f64) as u16,
            y.floor()
                .clamp(0.0, self.view_height().saturating_sub(1) as f64) as u16,
        );
    }
    /// Load text/cache metadata for the page underneath the cursor.
    pub(super) fn set_scroll_cursor_page(&mut self) {
        if !self.scroll_mode {
            return;
        }
        let virtual_row = self.scroll_offset + self.cursor.1 as i64;
        for page in 1..=self.count {
            let start = self.page_start(page);
            if virtual_row < (start + self.page_rows(page) + 1) as i64 {
                self.page = page;
                self.lines = self.text_cache.get(&page).cloned().unwrap_or_default();
                self.base_image = self.cache.get(&self.key_for(page));
                self.displayed = Some(self.page_size_cached(page));
                return;
            }
        }
    }
    /// Switch view modes while retaining the search target.
    pub(super) fn toggle_view(&mut self) {
        self.scroll_mode = !self.scroll_mode;
        self.selection = None;
        self.selection_image = None;
        if self.scroll_mode {
            self.scroll_surface_initialized = false;
            self.scroll_offset = self.centered_scroll_offset(self.page);
            if let Some(marker) = self.marker.take() {
                self.reveal_marker(&marker);
                self.marker = Some(marker);
            }
            self.draw_scroll();
        } else {
            self.single_surface_initialized = false;
            if let Some(marker) = self.marker.take() {
                self.reveal_marker(&marker);
                self.marker = Some(marker);
            }
            self.request();
        }
    }
    /// Scroll virtual document rows and schedule newly visible pages.
    pub(super) fn scroll_by(&mut self, delta: i32) {
        let (min, max) = scroll_bounds(self.total_rows(), self.view_height(), self.scroll_offset);
        self.scroll_offset = (self.scroll_offset + delta as i64).clamp(min, max);
        self.scroll_generation += 1;
        self.selection = None;
        self.selection_image = None;
        for page in 1..=self.count {
            if self.scroll_offset < (self.page_start(page) + self.page_rows(page) + 1) as i64 {
                self.page = page;
                self.lines = self.text_cache.get(&page).cloned().unwrap_or_default();
                self.base_image = self.cache.get(&self.key_for(page));
                self.displayed = Some(self.page_size_cached(page));
                break;
            }
        }
        if !self.cache.contains(&self.key()) {
            self.request();
        }
        self.prefetch_next();
        self.scroll_dirty = true;
    }
}

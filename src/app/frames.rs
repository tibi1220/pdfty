//! Compose and place visible PDF frames.
//! Fresh image IDs and synchronized placement/deletion keep tmux focus redraws
//! from exposing blank or stale images. Status bars are outside this viewport.

use super::App;
use crate::{image, terminal};
use std::{fs, time::Instant};

impl App {
    /// Compose available visible pages immediately and prefetch missing neighbors.
    pub(super) fn draw_scroll(&mut self) {
        if self.help_open {
            return;
        }
        let mut pages = Vec::new();
        let viewport_end = self.scroll_offset + self.view_height() as i64;
        for page in 1..=self.count {
            let start = self.page_start(page);
            let rows = self.page_rows(page);
            if start as i64 >= viewport_end || (start + rows) as i64 <= self.scroll_offset {
                continue;
            }
            let Some(base) = self.cache.get(&self.key_for(page)) else {
                continue;
            };
            let source = (page == self.page)
                .then_some(())
                .and(self.selection_image.as_ref())
                .or_else(|| {
                    self.marker
                        .as_ref()
                        .filter(|marker| marker.page == page)
                        .and(self.marker_image.as_ref())
                })
                .unwrap_or(&base);
            let page_width = self.scroll_page_width(page);
            let rect = (
                self.page_x(page_width),
                start as i32 - self.scroll_offset as i32,
                page_width,
                rows,
            );
            pages.push((source.clone(), rect));
        }
        // Display available pages immediately; waiting for every visible page
        // delays the first frame when an adjacent page is still rendering.
        if pages.is_empty() {
            self.prefetch_next();
            return;
        }
        self.scroll_serial += 1;
        let surface = self.dir.join(format!("scroll-{}.png", self.scroll_serial));
        let pane = (self.view_width(), self.view_height());
        // A later wheel event can overtake this request; the generation lets
        // receive_scroll_frame discard the obsolete result.
        self.scroll_generation += 1;
        let generation = self.scroll_generation;
        if self
            .compose_requests
            .send(image::ComposeRequest {
                pages,
                pane,
                cell_pixels: self.cell_pixels,
                output: surface,
                generation,
                cursor: self.image_cursor(),
            })
            .is_err()
        {
            self.status = "scroll worker stopped".into();
        }
        self.scroll_dirty = false;
        self.scroll_last_draw = Instant::now();
    }
    /// Reject overtaken compositions before placing their image in the terminal.
    pub(super) fn receive_scroll_frame(&mut self, frame: image::ComposeResult) {
        // Keep the current image until a complete new surface and its markers
        // exist. Each frame needs a new ID so tmux redraws its placeholder cells.
        if self.help_open || !self.scroll_mode || frame.generation != self.scroll_generation {
            let _ = fs::remove_file(frame.output);
            return;
        }
        if let Err(e) = frame.result {
            self.status = format!("scroll render: {e}");
            let _ = fs::remove_file(frame.output);
            return;
        }
        let pane = (self.view_width(), self.view_height());
        let _frame = terminal::Frame::begin();
        if !self.scroll_surface_initialized {
            terminal::clear();
        }
        let image_id = image::next_image_id();
        if let Err(e) = image::place_at_id(
            &frame.output,
            &image::Placement {
                viewport: pane,
                origin: (0, 1), // Leave terminal row zero for the summary bar.
                image_id: Some(image_id),
                terminal: self.dimensions,
                cell_pixels: self.cell_pixels,
            },
        ) {
            self.status = format!("scroll display: {e}");
            let _ = fs::remove_file(frame.output);
            return;
        }
        self.frame_rate.record();
        if let Some(old) = self.displayed_image_id.replace(image_id) {
            image::delete_image(old);
        }
        self.scroll_surface_initialized = true;
        for old in self.scroll_files.drain(..) {
            let _ = fs::remove_file(old);
        }
        self.scroll_files.push(frame.output);
    }
    /// Choose selection, outline, or clean pixels for the active view.
    pub(super) fn redraw(&mut self) {
        if self.count == 0 {
            terminal::marker(self.cursor.0, self.cursor.1 + 1);
            return;
        }
        if self.scroll_mode {
            self.draw_scroll();
            return;
        }
        if let (Some(path), Some(size)) = (
            self.selection_image
                .clone()
                .or_else(|| self.current_marker_image().cloned())
                .or_else(|| self.base_image.clone()),
            self.displayed,
        ) {
            if let Err(e) = self.draw_single_source(&path, size) {
                self.status = e;
            }
        }
    }
    /// Place one cropped raster frame and delete its predecessor atomically.
    pub(super) fn draw_single_source(
        &mut self,
        path: &std::path::Path,
        size: (f64, f64),
    ) -> Result<(), String> {
        // Crop after fitting/zooming, then place the visible portion one row
        // below the top bar. Switch to a new image before deleting the old one.
        if self.help_open {
            return Ok(());
        }
        self.scroll_serial += 1;
        let cropped = self.dir.join(format!("single-{}.png", self.scroll_serial));
        let rect = self.single_rect(size);
        let pane = (self.view_width(), self.view_height());
        image::single_frame(
            path,
            &cropped,
            rect,
            pane,
            self.cell_pixels,
            self.image_cursor(),
        )?;
        let _frame = terminal::Frame::begin();
        if !self.single_surface_initialized {
            terminal::clear();
        }
        let image_id = image::next_image_id();
        image::place_at_id(
            &cropped,
            &image::Placement {
                viewport: pane,
                origin: (0, 1), // Leave terminal row zero for the summary bar.
                image_id: Some(image_id),
                terminal: self.dimensions,
                cell_pixels: self.cell_pixels,
            },
        )?;
        self.frame_rate.record();
        if let Some(old) = self.displayed_image_id.replace(image_id) {
            image::delete_image(old);
        }
        self.single_surface_initialized = true;
        for old in self.scroll_files.drain(..) {
            let _ = fs::remove_file(old);
        }
        self.scroll_files.push(cropped);
        Ok(())
    }
}

//! Render scheduling, prefetch, and result validation.
//! Workers may finish after navigation. Cache keys validate PDF version/size,
//! while generations reject obsolete visible frames without wasting valid prefetches.

use super::App;
use crate::{
    cache::Key,
    render::{self, Request},
};
use std::{fs, path::PathBuf};

impl App {
    /// Identify the exact PDF version, page, viewport size, and zoom being rendered.
    pub(super) fn key(&self) -> Key {
        Key {
            version: self.version,
            page: self.page,
            width: self.dimensions.0,
            height: self.view_height(),
            zoom: self.zoom,
        }
    }
    /// Build an equivalent render key for a neighboring page.
    pub(super) fn key_for(&self, page: u32) -> Key {
        Key { page, ..self.key() }
    }
    /// Prefer visible neighbors and keep only one prefetch request in flight.
    pub(super) fn prefetch_next(&mut self) {
        // Prepare visible neighbors first so scrolling does not expose blank
        // pages while earlier pages elsewhere in the document are rendered.
        if self.prefetch_inflight.is_some() {
            return;
        }
        let viewport_end = self.scroll_offset + self.view_height() as i64;
        let mut order: Vec<u32> = (1..=self.count).collect();
        if self.scroll_mode {
            order.sort_by_key(|&page| {
                let start = self.page_start(page) as i64;
                let end = start + self.page_rows(page) as i64;
                let distance = if end < self.scroll_offset {
                    self.scroll_offset - end
                } else {
                    (start - viewport_end).max(0)
                };
                (distance, page.abs_diff(self.page))
            });
        } else {
            order.sort_by_key(|&page| page.abs_diff(self.page));
        }
        for page in order {
            if page == self.page || self.prefetch_failed.contains(&page) {
                continue;
            }
            let key = self.key_for(page);
            if self.cache.contains(&key) {
                continue;
            }
            if self
                .background
                .send(Request {
                    key: key.clone(),
                    pdf: self.pdf.clone(),
                    dir: self.dir.clone(),
                    generation: self.generation,
                })
                .is_ok()
            {
                self.prefetch_inflight = Some(key);
            }
            break;
        }
    }
    /// Reuse cached pixels or enqueue the active page for the rendering worker.
    pub(super) fn request(&mut self) {
        // A cache hit still updates the display immediately; the render worker
        // is used only when this exact PDF version, pane, and zoom is missing.
        self.generation += 1;
        self.status = "rendering".into();
        self.lines = self.text_cache.get(&self.page).cloned().unwrap_or_default();
        if let Some(path) = self.cache.get(&self.key()) {
            if let Ok(size) = render::page_size(&self.pdf, self.page) {
                self.display(&path, size);
                self.prefetch_next();
                return;
            }
        }
        let _ = self.requests.send(Request {
            key: self.key(),
            pdf: self.pdf.clone(),
            dir: self.dir.clone(),
            generation: self.generation,
        });
    }
    /// Adopt a valid render and derive the current overlays before painting.
    pub(super) fn display(&mut self, path: &std::path::Path, size: (f64, f64)) {
        self.base_image = Some(path.to_path_buf());
        self.displayed = Some(size);
        let outline_error = self.prepare_marker_image(path, size).err();
        if self.scroll_mode {
            self.draw_scroll();
            self.status = outline_error.unwrap_or_else(|| "ready".into());
            return;
        }
        let source = self
            .current_marker_image()
            .map_or(path, PathBuf::as_path)
            .to_path_buf();
        self.status = match self.draw_single_source(&source, size) {
            Ok(()) => "ready".into(),
            Err(e) => e,
        };
        if let Some(error) = outline_error {
            self.status = error;
        }
        if self.selection.is_some() {
            self.draw_selection();
        }
    }
    /// Drain worker results, discarding outdated PDF versions and viewport generations.
    pub(super) fn receive_work(&mut self) {
        // Render results may arrive after navigation, resize, zoom, or reload.
        // Keep only results for the current key/generation, but allow valid
        // prefetches into the cache even when they are not the visible page.
        while let Ok(result) = self.results.try_recv() {
            if result.pdf != self.pdf {
                if let Ok((path, _, _, _)) = result.result {
                    let _ = fs::remove_file(path);
                }
                continue;
            }
            if result.prefetch {
                if self.prefetch_inflight.as_ref() == Some(&result.key) {
                    self.prefetch_inflight = None;
                }
                if result.key.version == self.version
                    && result.key.zoom == self.zoom
                    && result.key.width == self.dimensions.0
                    && result.key.height == self.view_height()
                {
                    match result.result {
                        Ok((path, width, height, _)) => {
                            self.page_sizes.insert(result.key.page, (width, height));
                            let path = if let Some(cached) = self.cache.get(&result.key) {
                                let _ = fs::remove_file(path);
                                cached
                            } else {
                                self.cache.put(result.key.clone(), path.clone());
                                path
                            };
                            if result.key == self.key() && self.displayed.is_none() {
                                self.lines =
                                    self.text_cache.get(&self.page).cloned().unwrap_or_default();
                                self.display(&path, (width, height));
                            } else if self.scroll_mode {
                                self.scroll_dirty = true;
                            }
                        }
                        Err(error) => {
                            self.prefetch_failed.insert(result.key.page);
                            self.status = format!("page {}: {error}", result.key.page);
                        }
                    }
                } else if let Ok((path, _, _, _)) = result.result {
                    let _ = fs::remove_file(path);
                }
                self.prefetch_next();
                continue;
            }
            if result.generation != self.generation || result.key != self.key() {
                if let Ok((path, _, _, _)) = result.result {
                    let _ = fs::remove_file(path);
                }
                continue;
            }
            match result.result {
                Ok((path, width, height, _)) => {
                    self.page_sizes.insert(result.key.page, (width, height));
                    self.lines = self.text_cache.get(&self.page).cloned().unwrap_or_default();
                    let path = if let Some(cached) = self.cache.get(&result.key) {
                        let _ = fs::remove_file(path);
                        cached
                    } else {
                        self.cache.put(result.key, path.clone());
                        path
                    };
                    self.display(&path, (width, height));
                    self.prefetch_next();
                }
                Err(e) => {
                    self.status = e;
                    self.prefetch_next();
                }
            }
        }
        while let Ok(result) = self.text_results.try_recv() {
            if result.pdf == self.pdf && result.key.version == self.version {
                if result.key.page == self.page {
                    self.lines = result.lines.clone();
                    if let (Some(path), Some(size)) = (self.base_image.clone(), self.displayed) {
                        if self
                            .marker
                            .as_ref()
                            .is_some_and(|marker| marker.page == self.page)
                        {
                            if let Err(error) = self.prepare_marker_image(&path, size) {
                                self.status = error;
                            } else {
                                self.redraw();
                            }
                        }
                    }
                }
                self.text_cache.insert(result.key.page, result.lines);
            }
        }
        while let Ok(result) = self.compose_results.try_recv() {
            self.receive_scroll_frame(result);
        }
    }
}

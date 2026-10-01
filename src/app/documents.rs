//! Document lifecycle and per-document state.
//! Each open PDF owns its cache/version/watch/socket; switching transfers that
//! state instead of mixing render results from different documents.

use super::{App, SavedDocument};
use crate::cache::Cache;
use crate::{
    ipc::{self, Socket},
    reload::Watch,
    render, terminal,
};
use std::{env, fs, path::PathBuf};

impl App {
    /// Create IPC ownership for a new PDF before showing it.
    pub(super) fn add_document(&mut self, pdf: PathBuf) -> Result<(), String> {
        if !self.documents.contains(&pdf) {
            render::page_count(&pdf)?;
            if self.synctex_enabled {
                let path = ipc::socket_path(&pdf);
                let socket = Socket::start(path, Some(pdf.clone()), self.ipc_tx.clone())?;
                self.sockets.insert(pdf.clone(), socket);
            }
            self.documents.push(pdf.clone());
        }
        self.switch_to(pdf)
    }

    /// Cycle documents with wraparound while retaining their individual caches.
    pub(super) fn switch_relative(&mut self, delta: isize) {
        if self.documents.len() < 2 {
            return;
        }
        let current = self
            .documents
            .iter()
            .position(|pdf| pdf == &self.pdf)
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(self.documents.len() as isize) as usize;
        if let Err(error) = self.switch_to(self.documents[next].clone()) {
            self.status = error;
        }
    }

    /// Save the outgoing PDF state and restore or initialize the requested PDF.
    pub(super) fn switch_to(&mut self, pdf: PathBuf) -> Result<(), String> {
        if self.pdf == pdf && self.count > 0 {
            return Ok(());
        }
        let saved = self.saved_documents.remove(&pdf);
        let count = match &saved {
            Some(state) => state.count,
            None => render::page_count(&pdf)?,
        };
        if self.count > 0 {
            self.saved_documents.insert(
                self.pdf.clone(),
                SavedDocument {
                    count: self.count,
                    page: self.page,
                    version: self.version,
                    zoom: self.zoom,
                    cache: std::mem::replace(&mut self.cache, Cache::new()),
                    text_cache: std::mem::take(&mut self.text_cache),
                    page_sizes: std::mem::take(&mut self.page_sizes),
                    prefetch_failed: std::mem::take(&mut self.prefetch_failed),
                    watch: std::mem::replace(&mut self.watch, Watch::new(&pdf)),
                },
            );
        }
        self.pdf = pdf;
        self.count = count;
        if let Some(state) = saved {
            self.page = state.page.min(count).max(1);
            self.version = state.version;
            self.zoom = state.zoom;
            self.cache = state.cache;
            self.text_cache = state.text_cache;
            self.page_sizes = state.page_sizes;
            self.prefetch_failed = state.prefetch_failed;
            self.watch = state.watch;
        } else {
            self.page = 1;
            self.version = 1;
            self.zoom = 100;
            self.cache = Cache::new();
            self.text_cache.clear();
            self.page_sizes.clear();
            self.prefetch_failed.clear();
            self.watch = Watch::new(&self.pdf);
        }
        self.socket_path = if self.synctex_enabled {
            Some(ipc::socket_path(&self.pdf))
        } else {
            self.control_socket_path.clone()
        };
        self.generation += 1;
        self.scroll_generation += 1;
        self.prefetch_inflight = None;
        self.lines.clear();
        self.base_image = None;
        self.displayed = None;
        self.marker = None;
        self.marker_image = None;
        self.selection = None;
        self.selection_image = None;
        self.pan_x = 0;
        self.pan_y = 0;
        self.scroll_offset = self.centered_scroll_offset(self.page);
        self.single_surface_initialized = false;
        self.scroll_surface_initialized = false;
        terminal::clear();
        terminal::marker(self.cursor.0, self.cursor.1 + 1);
        self.request();
        Ok(())
    }

    /// Change the active page and invalidate overlays tied to the previous page.
    pub(super) fn go(&mut self, page: u32) {
        if page > 0 && page <= self.count && page != self.page {
            self.page = page;
            self.scroll_generation += 1;
            self.marker = None;
            self.marker_image = None;
            self.displayed = None;
            self.base_image = None;
            self.selection = None;
            self.selection_image = None;
            self.single_surface_initialized = false;
            if self.scroll_mode {
                self.scroll_offset = self.centered_scroll_offset(page);
            }
            self.request();
        }
    }
    /// Bump the PDF version so cached pixels and extracted text cannot outlive the file.
    pub(super) fn reload(&mut self) {
        // A new PDF version invalidates pixel and text caches together so
        // selection boxes cannot refer to the previous compilation.
        match render::page_count(&self.pdf) {
            Ok(count) => {
                self.count = count;
                self.page = self.page.min(count).max(1);
                self.version += 1;
                self.scroll_generation += 1;
                self.cache.clear();
                self.prefetch_failed.clear();
                self.text_cache.clear();
                self.page_sizes.clear();
                self.lines.clear();
                self.selection = None;
                self.selection_image = None;
                self.marker_image = None;
                self.single_surface_initialized = false;
                self.base_image = None;
                self.displayed = None;
                self.request();
            }
            Err(e) => self.status = format!("reload: {e}"),
        }
    }
}

/// Remove only abandoned process render directories; live viewers retain their pixels.
pub(super) fn cleanup_old() {
    let Ok(entries) = fs::read_dir(env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(pid) = name
            .strip_prefix("pdfty-")
            .and_then(|x| x.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != std::process::id() && !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            // macOS has no /proc; `kill -0` is the process liveness check.
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !alive {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

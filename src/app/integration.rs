//! Neovim IPC and SyncTeX actions.
//! Commands resolve the paired PDF before changing its view. Inverse search
//! opens the paired editor first and changes terminal focus only when requested.

use super::{geometry::parse_page_command, App};
use crate::{
    ipc::{Control, Message, ViewMode},
    render, synctex,
};
use std::path::PathBuf;

impl App {
    /// Keep a short newest-first history of external commands for the footer.
    pub(super) fn record_call(&mut self, call: String) {
        self.recent_calls.push_front(call);
        self.recent_calls.truncate(3);
    }
    /// Attach the result to the most recently recorded external command.
    pub(super) fn finish_call(&mut self, result: Result<(), &str>) {
        if let Some(call) = self.recent_calls.front_mut() {
            let outcome = match result {
                Ok(()) => "OK".to_string(),
                Err(error) => format!("ERROR: {error}"),
            };
            *call = format!("{outcome} | {call}");
        }
    }
    /// Route a validated IPC message without bypassing document ownership.
    pub(super) fn message(&mut self, message: Message) {
        match message {
            Message::Control { pdf, nvim, command } => {
                self.nvim = Some(nvim);
                self.record_call(format!("Neovim {command:?}"));
                let result = self.control(pdf, command);
                self.finish_call(result.as_ref().map(|_| ()).map_err(String::as_str));
                if let Err(error) = result {
                    self.status = error;
                }
            }
            Message::Add { pdf, nvim } => {
                self.nvim = Some(nvim);
                self.record_call(format!("add PDF {}", pdf.display()));
                match self.add_document(pdf) {
                    Ok(()) => self.finish_call(Ok(())),
                    Err(error) => {
                        self.finish_call(Err(&error));
                        self.status = format!("add PDF: {error}");
                    }
                }
            }
            Message::Register { pdf, nvim } if self.documents.contains(&pdf) => {
                self.nvim = Some(nvim);
            }
            Message::Forward {
                pdf,
                source,
                line,
                column,
                nvim,
            } => {
                self.nvim = Some(nvim);
                if let Err(error) = self.add_document(pdf) {
                    self.status = format!("forward PDF: {error}");
                    return;
                }
                self.record_call(format!(
                    "synctex view -i {line}:{column}:{} -o {}",
                    source.display(),
                    self.pdf.display()
                ));
                match synctex::forward(&self.pdf, &source, line, column) {
                    Ok(position) => {
                        self.finish_call(Ok(()));
                        self.page = position.page.min(self.count).max(1);
                        self.scroll_generation += 1;
                        self.marker = Some(position);
                        self.marker_image = None;
                        self.selection = None;
                        self.selection_image = None;
                        self.single_surface_initialized = false;
                        self.displayed = None;
                        if self.scroll_mode {
                            self.scroll_offset = self.centered_scroll_offset(self.page);
                        }
                        if let Ok(size) = render::page_size(&self.pdf, self.page) {
                            self.page_sizes.insert(self.page, size);
                        }
                        let marker = self.marker.take().unwrap();
                        self.reveal_marker(&marker);
                        self.marker = Some(marker);
                        self.request();
                    }
                    Err(e) => {
                        self.finish_call(Err(&e));
                        self.status = e;
                    }
                }
            }
            _ => self.status = "request for another PDF".into(),
        }
    }
    /// Apply Neovim navigation using the same methods as viewer keys.
    pub(super) fn control(&mut self, pdf: PathBuf, command: Control) -> Result<(), String> {
        if !self.documents.contains(&pdf) {
            return Err("request for another PDF".into());
        }
        // Cycling acts on the displayed document. Other commands first show
        // the PDF paired with the calling TeX buffer.
        if !matches!(&command, Control::NextPdf | Control::PrevPdf) {
            self.switch_to(pdf)?;
        }
        if self.help_open {
            self.close_help();
        }
        match command {
            Control::Move { page } => {
                let page = parse_page_command(&page.to_string(), self.count)?;
                self.go(page);
                // Also center a page that is already current in scroll view.
                if self.scroll_mode {
                    self.scroll_offset = self.centered_scroll_offset(page);
                    self.scroll_generation += 1;
                    self.scroll_dirty = true;
                }
            }
            Control::Next { count } => {
                if count == 0 {
                    return Err("page count must be positive".into());
                }
                self.go(self.page.saturating_add(count).min(self.count));
            }
            Control::Prev { count } => {
                if count == 0 {
                    return Err("page count must be positive".into());
                }
                self.go(self.page.saturating_sub(count).max(1));
            }
            Control::First => self.go(1),
            Control::Last => self.go(self.count),
            Control::Zoom { percent } => {
                if !(50..=300).contains(&percent) {
                    return Err("zoom must be between 50 and 300 percent".into());
                }
                self.change_zoom(percent as i16 - self.zoom as i16);
            }
            Control::ZoomIn => self.change_zoom(25),
            Control::ZoomOut => self.change_zoom(-25),
            Control::ZoomReset => self.reset_zoom(),
            Control::View { mode } => {
                let scroll = match mode {
                    ViewMode::Single => false,
                    ViewMode::Scroll => true,
                    ViewMode::Toggle => !self.scroll_mode,
                };
                if scroll != self.scroll_mode {
                    self.toggle_view();
                }
            }
            Control::Reload => self.reload(),
            Control::Center => self.center_cursor(),
            Control::Clear => self.clear_highlights(),
            Control::Switch => {}
            Control::NextPdf => self.switch_relative(1),
            Control::PrevPdf => self.switch_relative(-1),
        }
        Ok(())
    }
    /// Resolve the cursor point to source, then optionally focus its Neovim terminal.
    pub(super) fn inverse(&mut self, refocus: bool) {
        if !self.synctex_enabled {
            self.status = "SyncTeX disabled; restart with --synctex".into();
            return;
        }
        let Some((x, y)) = self.pdf_point() else {
            self.status = "page is not ready".into();
            return;
        };
        self.record_call(format!(
            "synctex edit -o {}:{x:.2}:{y:.2}:{}",
            self.page,
            self.pdf.display()
        ));
        match synctex::inverse(&self.pdf, self.page, x, y) {
            Ok(source) => {
                self.finish_call(Ok(()));
                self.marker = Some(synctex::Position {
                    page: self.page,
                    x,
                    y,
                });
                if let (Some(base), Some(size)) = (self.base_image.clone(), self.displayed) {
                    if let Err(error) = self.prepare_marker_image(&base, size) {
                        self.status = error;
                    }
                    self.redraw();
                }
                let nvim = self.nvim.clone();
                let result = synctex::open_nvim(&source, nvim.as_deref(), refocus, |call| {
                    self.record_call(call)
                });
                match result {
                    Ok(()) => {
                        self.finish_call(Ok(()));
                        self.status = if refocus {
                            "opened source and focused Neovim"
                        } else {
                            "opened source"
                        }
                        .into();
                    }
                    Err(e) => {
                        self.finish_call(Err(&e));
                        self.status = e;
                    }
                }
            }
            Err(e) => {
                self.finish_call(Err(&e));
                self.status = e;
            }
        }
    }
}

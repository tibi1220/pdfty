//! Neovim IPC, SyncTeX forward/inverse search, text selection and copying.

use super::{App, Event, Marker, Selection, ZOOM_RANGE};
use crate::{
    ipc::{Control, Message, ViewMode},
    layout::Mode,
    nvim,
    synctex::Synctex,
    text,
};
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
    thread,
};

impl App {
    pub(super) fn message(&mut self, message: Message) {
        self.dirty = true;
        match message {
            Message::Add { pdf, nvim } => {
                self.nvim = Some(nvim);
                match self.add_document(pdf.clone()) {
                    Ok(()) => {
                        self.switch_to(&pdf);
                        self.log(format!("added {}", pdf.display()));
                    }
                    Err(error) => self.note(format!("add PDF: {error}"), true),
                }
            }
            Message::Register { pdf, nvim } => {
                if self.docs.iter().any(|d| *d.path == pdf) {
                    self.nvim = Some(nvim);
                }
            }
            Message::Forward { pdf, source, line, column: _, nvim } => {
                self.nvim = Some(nvim);
                if let Err(error) = self.add_document(pdf.clone()) {
                    return self.note(format!("forward search: {error}"), true);
                }
                self.switch_to(&pdf);
                if let Err(error) = self.forward(&source, line) {
                    self.note(format!("forward search: {error}"), true);
                }
            }
            Message::Control { pdf, nvim, command } => {
                self.nvim = Some(nvim);
                if let Err(error) = self.control(pdf, command) {
                    self.note(error, true);
                }
            }
        }
    }

    fn control(&mut self, pdf: PathBuf, command: Control) -> Result<(), String> {
        if !self.docs.iter().any(|d| *d.path == pdf) {
            return Err("request for a PDF that is not open".into());
        }
        // Cycling acts on the displayed PDF; everything else shows the PDF
        // paired with the calling TeX buffer first.
        if !matches!(command, Control::NextPdf | Control::PrevPdf) {
            self.switch_to(&pdf);
        }
        self.panel = super::Panel::None;
        let count = self.page_count();
        match command {
            Control::Move { page } => {
                if page == 0 || page as usize > count {
                    return Err(format!("page must be between 1 and {count}"));
                }
                self.goto_page(page as usize - 1);
            }
            Control::Next { count: n } | Control::Prev { count: n } if n == 0 => {
                return Err("page count must be positive".into())
            }
            Control::Next { count: n } => self.page_relative(n as i64),
            Control::Prev { count: n } => self.page_relative(-(n as i64)),
            Control::First => self.goto_page(0),
            Control::Last => self.goto_page(usize::MAX),
            Control::Zoom { percent } => {
                if !(ZOOM_RANGE.0..=ZOOM_RANGE.1).contains(&(percent as u32)) {
                    return Err(format!("zoom must be between {} and {} percent", ZOOM_RANGE.0, ZOOM_RANGE.1));
                }
                self.set_zoom(percent as u32);
            }
            Control::ZoomIn => self.set_zoom(self.doc().map_or(100, |d| d.zoom) + 25),
            Control::ZoomOut => self.set_zoom(self.doc().map_or(100, |d| d.zoom).saturating_sub(25)),
            Control::ZoomReset => self.reset_zoom(),
            Control::View { mode } => {
                let mode = match mode {
                    ViewMode::Single => Mode::Single,
                    ViewMode::Scroll => Mode::Continuous,
                    ViewMode::Toggle if self.mode == Mode::Single => Mode::Continuous,
                    ViewMode::Toggle => Mode::Single,
                };
                self.set_mode(mode);
            }
            Control::Reload => self.reload_current(),
            Control::Center => self.center_cursor(),
            Control::Clear => self.clear_highlights(),
            Control::Switch => {}
            Control::NextPdf => self.switch_relative(1),
            Control::PrevPdf => self.switch_relative(-1),
        }
        Ok(())
    }

    fn synctex(&mut self) -> Result<Arc<Synctex>, String> {
        if !self.synctex_enabled {
            return Err("SyncTeX is disabled; start pdfty with --synctex".into());
        }
        let doc = self.doc_mut().ok_or("no PDF open")?;
        if let Some(synctex) = &doc.synctex {
            return Ok(Arc::clone(synctex));
        }
        let synctex = Arc::new(Synctex::load(&doc.path)?);
        doc.synctex = Some(Arc::clone(&synctex));
        Ok(synctex)
    }

    fn forward(&mut self, source: &std::path::Path, line: u32) -> Result<(), String> {
        let synctex = self.synctex()?;
        let position = synctex.forward(source, line)?;
        let doc = self.doc().ok_or("no PDF open")?;
        let page = (position.page as usize).clamp(1, doc.sizes.len()) - 1;
        let size = doc.sizes[page];
        let (x, y) = (position.x + size.x0, position.y + size.y0);
        self.marker = Some(Marker { doc: doc.id, page, x, y });
        self.log(format!("forward {}:{line} → page {}", source.display(), page + 1));
        self.selection = None;
        self.reveal(page, x, y);
        Ok(())
    }

    /// Open the source under the cursor in Neovim; optionally focus it.
    pub(super) fn inverse(&mut self, focus: bool) {
        let Some((page, x, y)) = self.cursor_point() else {
            return self.note("no PDF open".into(), true);
        };
        let synctex = match self.synctex() {
            Ok(synctex) => synctex,
            Err(error) => return self.note(error, true),
        };
        let doc = self.doc().unwrap();
        let size = doc.sizes[page];
        let source = match synctex.inverse(page as u32 + 1, x - size.x0, y - size.y0) {
            Ok(source) => source,
            Err(error) => return self.note(format!("inverse search: {error}"), true),
        };
        self.marker = Some(Marker { doc: doc.id, page, x, y });
        let server = match nvim::resolve_server(self.nvim.as_deref()) {
            Ok(server) => server,
            Err(error) => return self.note(error, true),
        };
        let label = format!(
            "{}:{}",
            source.file.file_name().unwrap_or_default().to_string_lossy(),
            source.line
        );
        self.log(format!("inverse page {} ({x:.1}, {y:.1}) → {}:{}", page + 1, source.file.display(), source.line));
        let tx = self.tx.clone();
        // RPC can block while Neovim is busy; never stall the viewer on it.
        thread::spawn(move || {
            let mut steps = Vec::new();
            let result = nvim::open_source(&server, &source.file, source.line, focus).and_then(|handled| {
                if focus && !handled {
                    nvim::focus(&server, &mut |s| steps.push(s)).map_err(|e| format!("opened; focus failed: {e}"))
                } else {
                    Ok(())
                }
            });
            for step in steps {
                let _ = tx.send(Event::Note { text: step, error: false });
            }
            let note = match result {
                Ok(()) => Event::Note { text: format!("opened {label}"), error: false },
                Err(error) => Event::Note { text: format!("Neovim: {error}"), error: true },
            };
            let _ = tx.send(note);
        });
    }

    pub(super) fn clear_highlights(&mut self) {
        self.selection = None;
        self.marker = None;
        self.drag_from = None;
        self.message = (String::new(), false);
        self.dirty = true;
    }

    pub(super) fn start_selection(&mut self) {
        let Some((page, x, y)) = self.cursor_point() else { return };
        let Some(doc) = self.doc().map(|d| d.id) else { return };
        self.selection = Some(Selection { doc, page, anchor: (x, y), head: (x, y) });
        self.message = ("selecting: move to extend, y copies, Esc cancels".into(), false);
        self.dirty = true;
    }

    pub(super) fn update_selection_head(&mut self) {
        let Some(selection) = self.selection else { return };
        if let Some(point) = self.cursor_point_on(selection.page) {
            self.selection = Some(Selection { head: point, ..selection });
        }
    }

    pub(super) fn copy_selection(&mut self) {
        let Some(selection) = self.selection else {
            return self.note("nothing selected; press v or drag with the mouse".into(), true);
        };
        let Some(lines) = self.doc().and_then(|d| d.text.get(&selection.page)).cloned() else {
            return self.note("page text is still loading".into(), true);
        };
        let (Some(a), Some(b)) = (
            text::position(&lines, selection.anchor.0, selection.anchor.1),
            text::position(&lines, selection.head.0, selection.head.1),
        ) else {
            return self.note("no text on this page".into(), true);
        };
        let value = text::selection_text(&lines, a, b);
        match copy_to_clipboard(&value, &self.gfx) {
            Ok(()) => self.note(format!("copied {} characters", value.chars().count()), false),
            Err(error) => self.note(format!("copy: {error}"), true),
        }
    }
}

fn copy_to_clipboard(value: &str, gfx: &crate::kitty::Graphics) -> Result<(), String> {
    for (program, args) in [("pbcopy", &[][..]), ("wl-copy", &[][..]), ("xclip", &["-selection", "clipboard"][..])] {
        let Ok(mut child) = Command::new(program).args(args).stdin(Stdio::piped()).spawn() else {
            continue;
        };
        let written = child.stdin.take().map(|mut input| input.write_all(value.as_bytes()));
        return match (written, child.wait()) {
            (Some(Ok(())), Ok(status)) if status.success() => Ok(()),
            _ => Err(format!("{program} failed")),
        };
    }
    // OSC 52: let the terminal set the clipboard.
    let encoded = String::from_utf8(crate::kitty::base64(value.as_bytes())).unwrap_or_default();
    let (mut batch, mut out) = (Vec::new(), Vec::new());
    gfx.emit(&mut batch, format!("\x1b]52;c;{encoded}\x07").as_bytes());
    gfx.deliver(&mut batch, &mut out);
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&out).and_then(|_| stdout.flush()).map_err(|e| e.to_string())
}

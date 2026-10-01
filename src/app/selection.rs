//! Text selection and paragraph outlines over immutable PDF renders.
//! Selection/outline PNGs are derived overlays; the clean base image survives
//! so clearing highlights can redraw without rendering the PDF again.

use super::App;
use crate::{image, text};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

impl App {
    /// Clear both overlay sources and invalidate queued highlighted frames.
    pub(super) fn clear_highlights(&mut self) {
        self.selection = None;
        self.selection_image = None;
        self.marker = None;
        self.marker_image = None;
        // Discard any queued frame containing the highlights being removed.
        self.scroll_generation += 1;
        self.status = "highlights cleared".into();
        self.redraw();
    }

    /// Only expose an outline belonging to the currently active page.
    pub(super) fn current_marker_image(&self) -> Option<&PathBuf> {
        self.marker
            .as_ref()
            .filter(|marker| marker.page == self.page)
            .and(self.marker_image.as_ref())
    }
    /// Anchor selection on the extracted text nearest the visible cursor.
    pub(super) fn start_selection(&mut self) {
        self.set_scroll_cursor_page();
        let Some((x, y)) = self.pdf_point() else {
            self.status = "page is not ready".into();
            return;
        };
        if let Some(index) = text::nearest(&self.lines, x, y) {
            self.selection = Some((index, index));
            self.draw_selection();
        } else {
            self.status = "no selectable text on this page".into();
        }
    }
    /// Update the endpoint only when cursor movement reaches another text line.
    pub(super) fn extend_selection(&mut self) -> bool {
        let Some((anchor, _)) = self.selection else {
            return false;
        };
        let Some((x, y)) = self.pdf_point() else {
            return false;
        };
        if let Some(index) = text::nearest(&self.lines, x, y) {
            let changed = self.selection != Some((anchor, index));
            if changed {
                self.selection = Some((anchor, index));
            }
            if changed {
                self.draw_selection();
                return true;
            }
        }
        false
    }
    /// Tint selected lines on a derived PNG without modifying cached pixels.
    pub(super) fn draw_selection(&mut self) {
        // Render highlight into a separate PNG so clearing a selection can
        // redisplay the untouched cached page without rerendering the PDF.
        let (Some((a, b)), Some(base), Some(size)) =
            (self.selection, &self.base_image, self.displayed)
        else {
            return;
        };
        let start = a.min(b);
        let end = a.max(b);
        let rects: Vec<_> = self.lines[start..=end.min(self.lines.len().saturating_sub(1))]
            .iter()
            .map(|line| line.bbox.clone())
            .collect();
        self.selection_serial += 1;
        let path = self
            .dir
            .join(format!("selection-{}.png", self.selection_serial));
        let highlight_source = self.current_marker_image().unwrap_or(base);
        let result = image::highlight(highlight_source, &path, size, &rects).and_then(|_| {
            self.selection_image = Some(path.clone());
            if self.scroll_mode {
                self.draw_scroll();
                Ok(())
            } else {
                self.draw_single_source(&path, size)
            }
        });
        match result {
            Ok(()) => self.status = format!("{} lines selected; y copies", end - start + 1),
            Err(e) => self.status = format!("highlight: {e}"),
        }
        if self.selection_serial > 2 {
            let _ = fs::remove_file(
                self.dir
                    .join(format!("selection-{}.png", self.selection_serial - 2)),
            );
        }
    }
    /// Send selected whole lines to the system clipboard.
    pub(super) fn copy_selection(&mut self) {
        let Some((a, b)) = self.selection else {
            self.status = "select text first".into();
            return;
        };
        let value = text::selected(&self.lines, a, b);
        match Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
            Ok(mut child) => {
                let written = child
                    .stdin
                    .take()
                    .map(|mut input| input.write_all(value.as_bytes()));
                match (written, child.wait()) {
                    (Some(Ok(())), Ok(status)) if status.success() => {
                        self.status = "copied text".into()
                    }
                    _ => self.status = "could not copy text".into(),
                }
            }
            Err(e) => self.status = format!("pbcopy: {e}"),
        }
    }
    /// Build a paragraph outline from clean page pixels and extracted text.
    pub(super) fn prepare_marker_image(
        &mut self,
        base: &std::path::Path,
        size: (f64, f64),
    ) -> Result<(), String> {
        let Some(marker) = self.marker.as_ref() else {
            self.marker_image = None;
            return Ok(());
        };
        if marker.page != self.page {
            return Ok(());
        }
        self.marker_image = None;
        let Some(rect) = text::paragraph_at(&self.lines, marker.x, marker.y) else {
            return Ok(());
        };
        self.marker_serial += 1;
        let path = self
            .dir
            .join(format!("paragraph-{}.png", self.marker_serial));
        image::outline_paragraph(base, &path, size, &rect)
            .map_err(|error| format!("paragraph outline: {error}"))?;
        self.marker_image = Some(path);
        Ok(())
    }
}

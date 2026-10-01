//! Conversions between PDF points, document rows, and viewport cells.
//! Terminal row zero is a status bar; all geometry here uses the PDF viewport.
//! Signed offsets allow blank margins when centering a point near a page edge.

use super::App;
use crate::resolution::CellPixels;

impl App {
    /// Reserve one terminal column to prevent Kitty placeholders from wrapping.
    pub(super) fn view_width(&self) -> u16 {
        // Leave Kitty one column of slack to avoid terminal edge wrapping.
        self.dimensions.0.saturating_sub(1).max(1)
    }
    /// Exclude the summary and four footer rows from PDF geometry.
    pub(super) fn view_height(&self) -> u16 {
        // Row zero is the summary; the final four rows contain three recent
        // calls and the debug/command bar.
        self.dimensions.1.saturating_sub(5).max(1)
    }
    /// Reject mouse clicks on status rows before converting them to PDF points.
    pub(super) fn in_document_row(&self, row: u16) -> bool {
        row > 0 && row < self.dimensions.1.saturating_sub(4)
    }
    /// Size continuous-view pages to the viewport width and zoom.
    pub(super) fn scroll_page_width(&self, _page: u32) -> u32 {
        ((self.view_width() as f64 * self.zoom as f64 / 100.0).round() as u32).max(1)
    }
    /// Fit physical PDF dimensions using the measured terminal cell aspect.
    pub(super) fn single_page_size(&self, size: (f64, f64)) -> (u32, u32) {
        let (_, _, w, h) = fit(
            size,
            (self.view_width(), self.view_height()),
            self.cell_pixels,
        );
        let factor = self.zoom as f64 / 100.0;
        (
            (w * factor).round().max(1.0) as u32,
            (h * factor).round().max(1.0) as u32,
        )
    }
    /// Project horizontal pan into a signed page origin in viewport cells.
    pub(super) fn page_x(&self, width: u32) -> i32 {
        ((self.view_width() as u32).saturating_sub(width) as i64 / 2 - self.pan_x) as i32
    }
    /// Return the zoomed page rectangle, including any centering margins.
    pub(super) fn single_rect(&self, size: (f64, f64)) -> (i32, i32, u32, u32) {
        let (width, height) = self.single_page_size(size);
        let x = self.page_x(width);
        let y = ((self.view_height() as u32).saturating_sub(height) as i64 / 2 - self.pan_y) as i32;
        (x, y, width, height)
    }

    /// Use known page dimensions while the requested page is still rendering.
    pub(super) fn page_size_cached(&self, page: u32) -> (f64, f64) {
        self.page_sizes
            .get(&page)
            .copied()
            .or_else(|| self.page_sizes.values().next().copied())
            .unwrap_or((612.0, 792.0))
    }
    /// Convert a page aspect ratio to document rows without cutting off its last row.
    pub(super) fn page_rows(&self, page: u32) -> u32 {
        // Convert the PDF's physical aspect into terminal cells using the
        // measured cell aspect. Round up to avoid losing the final row.
        let (w, h) = self.page_size_cached(page);
        ((self.scroll_page_width(page) as f64 * h / w * self.cell_pixels.aspect()).ceil() as u32)
            .max(1)
    }
    /// Locate a page in the continuous document, including inter-page gaps.
    pub(super) fn page_start(&self, page: u32) -> u32 {
        // Continuous view is a virtual row sequence with one blank row after
        // every page. Pages outside the viewport need no PNG composition.
        let first_page_padding = self
            .view_height()
            .saturating_sub(self.page_rows(1).min(u16::MAX as u32) as u16)
            / 2;
        first_page_padding as u32 + (1..page).map(|p| self.page_rows(p) + 1).sum::<u32>()
    }
    /// Include leading/trailing padding in the continuous document extent.
    pub(super) fn total_rows(&self) -> u32 {
        let last_page_padding =
            (self.view_height() as u32).saturating_sub(self.page_rows(self.count)) / 2;
        self.page_start(self.count + 1) + last_page_padding
    }
    /// Choose the usual starting viewport when navigating to a page.
    pub(super) fn centered_scroll_offset(&self, page: u32) -> i64 {
        let spare = (self.view_height() as u32).saturating_sub(self.page_rows(page));
        self.page_start(page)
            .saturating_sub(spare / 2)
            .min(self.total_rows().saturating_sub(self.view_height() as u32)) as i64
    }
    /// Invert placement at the cursor cell center for selection and SyncTeX.
    pub(super) fn pdf_point(&self) -> Option<(f64, f64)> {
        // Invert the page placement to map a cursor cell into PDF points for
        // text selection and inverse SyncTeX. The two view modes have
        // different page origins but return the same PDF coordinate system.
        let (width, height) = self.displayed?;
        if self.scroll_mode {
            let virtual_row = self.scroll_offset as f64 + self.cursor.1 as f64 + 0.5;
            let y = ((virtual_row - self.page_start(self.page) as f64)
                / self.page_rows(self.page) as f64
                * height)
                .clamp(0.0, height);
            let page_width = self.scroll_page_width(self.page);
            let x = ((self.cursor.0.min(self.view_width().saturating_sub(1)) as f64 + 0.5
                - self.page_x(page_width) as f64)
                / page_width as f64
                * width)
                .clamp(0.0, width);
            return Some((x, y));
        }
        let (left, top, fitted_w, fitted_h) = self.single_rect((width, height));
        Some((
            ((self.cursor.0 as f64 + 0.5 - left as f64) / fitted_w as f64 * width)
                .clamp(0.0, width),
            ((self.cursor.1 as f64 + 0.5 - top as f64) / fitted_h as f64 * height)
                .clamp(0.0, height),
        ))
    }
}

// Pure axis helpers keep movement/count/centering invariants independently testable.
/// Consume a typed prefix once; subsequent commands return to a count of one.
pub(super) fn take_count(input: &mut String) -> u32 {
    let count = input.parse::<u32>().unwrap_or(1).max(1);
    input.clear();
    count
}

/// Translate viewport offset and screen cursor together, preserving their document sum.
pub(super) fn center_cursor_axis(cursor: u16, offset: i64, viewport: u16) -> (u16, i64) {
    let center = viewport / 2;
    (center, offset + cursor as i64 - center as i64)
}

/// Permit page-edge centering margins without snapping an existing offset back into range.
pub(super) fn pan_bounds(extent: u32, viewport: u16, current: i64) -> (i64, i64) {
    let padding = (viewport as u32).saturating_sub(extent) as i64 / 2;
    let center = viewport as i64 / 2;
    (
        (padding - center).min(current),
        (padding + extent.saturating_sub(1) as i64 - center).max(current),
    )
}

/// Allow the first and last document rows to reach the viewport center.
pub(super) fn scroll_bounds(extent: u32, viewport: u16, current: i64) -> (i64, i64) {
    let center = viewport as i64 / 2;
    (
        (-center).min(current),
        (extent.saturating_sub(1) as i64 - center).max(current),
    )
}

/// Keep the cursor inside the viewport and transfer excess movement into the offset.
pub(super) fn move_cursor_axis(
    cursor: u16,
    offset: i64,
    viewport: u16,
    bounds: (i64, i64),
    delta: i32,
) -> (u16, i64) {
    let target = cursor as i64 + delta as i64;
    let cell = target.clamp(0, viewport.saturating_sub(1) as i64);
    let offset = (offset + target - cell).clamp(bounds.0, bounds.1);
    (cell as u16, offset)
}

/// Validate absolute pages separately from saturating relative page movement.
pub(super) fn parse_page_command(input: &str, page_count: u32) -> Result<u32, String> {
    let page: u32 = input.parse().map_err(|_| "invalid page number")?;
    if page == 0 || page > page_count {
        Err(format!("page must be between 1 and {page_count}"))
    } else {
        Ok(page)
    }
}

/// Keep a visible search target stable; pan only when it lies outside the viewport.
pub(super) fn reveal_axis(offset: i64, target: f64, extent: u32, viewport: u32) -> i64 {
    let max_offset = extent.saturating_sub(viewport);
    if max_offset == 0 {
        return 0;
    }
    let target = target.clamp(0.0, extent.saturating_sub(1) as f64);
    let offset = offset.min(max_offset as i64);
    if target >= offset as f64 + 1.0 && target < (offset + viewport as i64).saturating_sub(1) as f64
    {
        offset
    } else {
        ((target - viewport as f64 / 2.0).round() as i64).clamp(0, max_offset as i64)
    }
}

/// Fit PDF points into terminal cells using physical cell aspect instead of assuming square cells.
pub(super) fn fit(page: (f64, f64), pane: (u16, u16), cell: CellPixels) -> (f64, f64, f64, f64) {
    // Both PDF dimensions and cell dimensions are physical pixel ratios, so
    // use the measured terminal cell shape to preserve the page's aspect.
    let available_w = pane.0.max(1) as f64;
    // Keep a row of space above and below a fitted page, so the default
    // single-page view looks centered inside the two status bars.
    let available_h = pane.1.saturating_sub(2).max(1) as f64;
    let scale =
        (available_w * cell.width as f64 / page.0).min(available_h * cell.height as f64 / page.1);
    let width = page.0 * scale / cell.width as f64;
    let height = page.1 * scale / cell.height as f64;
    (
        (available_w - width) / 2.0,
        (pane.1.max(1) as f64 - height) / 2.0,
        width,
        height,
    )
}

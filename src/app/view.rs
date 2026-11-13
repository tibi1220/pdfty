//! Viewport movement: scrolling, paging, zoom, cursor, and tile addressing.

use super::{App, Doc, ZOOM_RANGE};
use crate::{
    layout::{clamp_soft, move_axis, Mode, Placed},
    render::{Overlay, TileBase, TileKey, TILE_COLS, TILE_ROWS},
    text,
};
use std::hash::{Hash, Hasher};

/// Tiles of a placed page intersecting the canvas rectangle [x0,x1)×[y0,y1).
pub(super) fn tiles_in(placed: &Placed, x0: i64, y0: i64, x1: i64, y1: i64) -> Vec<(u32, u32)> {
    let grid = placed.grid;
    let (left, top) = (x0 - placed.x, y0 - placed.y);
    let (right, bottom) = (x1 - placed.x, y1 - placed.y);
    if right <= 0 || bottom <= 0 || left >= grid.cols as i64 || top >= grid.rows as i64 {
        return Vec::new();
    }
    let tx0 = left.max(0) as u32 / TILE_COLS;
    let ty0 = top.max(0) as u32 / TILE_ROWS;
    let tx1 = (right.min(grid.cols as i64) as u32 - 1) / TILE_COLS;
    let ty1 = (bottom.min(grid.rows as i64) as u32 - 1) / TILE_ROWS;
    (ty0..=ty1).flat_map(|ty| (tx0..=tx1).map(move |tx| (tx, ty))).collect()
}

impl App {
    pub(super) fn tile_key(&self, doc: &Doc, placed: &Placed, tx: u32, ty: u32) -> TileKey {
        let overlays = self.tile_overlays(doc, placed, tx, ty);
        let overlay = if overlays.is_empty() {
            0
        } else {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for o in &overlays {
                let (kind, r) = match o {
                    Overlay::Highlight(r) => (1u8, r),
                    Overlay::Outline(r) => (2u8, r),
                };
                (kind, r.x0.to_bits(), r.y0.to_bits(), r.x1.to_bits(), r.y1.to_bits()).hash(&mut hasher);
            }
            hasher.finish() | 1
        };
        TileKey {
            base: TileBase { doc: doc.id, page: placed.page as u32, grid: placed.grid, tx, ty },
            version: doc.version,
            overlay,
        }
    }

    /// Overlays touching one tile, so a selection only re-renders the tiles it covers.
    pub(super) fn tile_overlays(&self, doc: &Doc, placed: &Placed, tx: u32, ty: u32) -> Vec<Overlay> {
        let overlays = self.page_overlays(doc, placed.page);
        if overlays.is_empty() {
            return overlays;
        }
        let rect = placed.grid.tile_rect(&doc.sizes[placed.page], tx, ty);
        overlays.into_iter().filter(|o| o.bounds().intersects(&rect)).collect()
    }

    fn page_overlays(&self, doc: &Doc, page: usize) -> Vec<Overlay> {
        let mut overlays = Vec::new();
        let Some(lines) = doc.text.get(&page) else { return overlays };
        if let Some(sel) = self.selection.filter(|s| s.doc == doc.id && s.page == page) {
            if let (Some(a), Some(b)) = (
                text::position(lines, sel.anchor.0, sel.anchor.1),
                text::position(lines, sel.head.0, sel.head.1),
            ) {
                overlays.extend(text::selection_rects(lines, a, b).into_iter().map(Overlay::Highlight));
            }
        }
        if let Some(marker) = self.marker.filter(|m| m.doc == doc.id && m.page == page) {
            if let Some(rect) = text::paragraph_at(lines, marker.x, marker.y) {
                overlays.push(Overlay::Outline(rect));
            }
        }
        overlays
    }

    /// Canvas position of the cursor cell's center.
    fn cursor_canvas(&self) -> Option<(f64, f64)> {
        let doc = self.doc()?;
        Some((
            doc.view.0 as f64 + self.cursor.0 as f64 + 0.5,
            doc.view.1 as f64 + self.cursor.1 as f64 + 0.5,
        ))
    }

    /// Page and page-space point under the cursor.
    pub(super) fn cursor_point(&self) -> Option<(usize, f64, f64)> {
        let doc = self.doc()?;
        let (cx, cy) = self.cursor_canvas()?;
        let layout = self.layout();
        let placed = layout.page_at(cx.floor() as i64, cy.floor() as i64)?;
        let (x, y) = placed.to_point(&doc.sizes[placed.page], cx, cy);
        Some((placed.page, x, y))
    }

    /// The cursor projected onto a specific page (clamped to its edges).
    pub(super) fn cursor_point_on(&self, page: usize) -> Option<(f64, f64)> {
        let doc = self.doc()?;
        let (cx, cy) = self.cursor_canvas()?;
        let layout = self.layout();
        let placed = layout.placed(page)?;
        Some(placed.to_point(&doc.sizes[page], cx, cy))
    }

    /// The page the user is looking at: under the cursor, else mid-screen.
    pub(super) fn current_page(&self) -> usize {
        let Some(doc) = self.doc() else { return 0 };
        if self.mode == Mode::Single {
            return doc.page;
        }
        let layout = self.layout();
        let (cx, cy) = self.cursor_canvas().unwrap_or_default();
        if let Some(p) = layout.pages.iter().find(|p| p.contains(cx as i64, cy as i64)) {
            return p.page;
        }
        let mid = doc.view.1 + self.view_size().1 as i64 / 2;
        layout.page_at(doc.view.0, mid).map_or(0, |p| p.page)
    }

    pub(super) fn page_count(&self) -> usize {
        self.doc().map_or(0, |d| d.sizes.len())
    }

    fn set_view(&mut self, view: (i64, i64)) {
        if let Some(doc) = self.doc_mut() {
            if doc.view != view {
                doc.view = view;
                self.dirty = true;
            }
        }
    }

    pub(super) fn keep_view_in_bounds(&mut self) {
        let Some(view) = self.doc().map(|d| d.view) else { return };
        let (bx, by) = self.layout().bounds(self.view_size());
        self.set_view((view.0.clamp(bx.0, bx.1), view.1.clamp(by.0, by.1)));
    }

    /// Scroll by rows/columns within the document. Returns whether it moved.
    pub(super) fn scroll(&mut self, dx: i64, dy: i64) -> bool {
        let Some(view) = self.doc().map(|d| d.view) else { return false };
        let (bx, by) = self.layout().bounds(self.view_size());
        let next = (clamp_soft(view.0 + dx, bx, view.0), clamp_soft(view.1 + dy, by, view.1));
        self.set_view(next);
        next != view
    }

    /// Wheel/line scrolling. In single view, scrolling past an edge turns the page.
    pub(super) fn scroll_lines(&mut self, dy: i64) {
        if self.scroll(0, dy) || self.mode == Mode::Continuous || dy == 0 {
            return;
        }
        let page = self.current_page();
        if dy > 0 && page + 1 < self.page_count() {
            self.goto_page(page + 1);
        } else if dy < 0 && page > 0 {
            self.goto_page(page - 1);
            let bottom = self.layout().bounds(self.view_size()).1 .1;
            let x = self.doc().map_or(0, |d| d.view.0);
            self.set_view((x, bottom));
        }
    }

    /// Show a page from its top (centered when it fits).
    pub(super) fn goto_page(&mut self, page: usize) {
        let count = self.page_count();
        if count == 0 {
            return;
        }
        let page = page.min(count - 1);
        if self.mode == Mode::Single {
            if let Some(doc) = self.doc_mut() {
                doc.page = page;
            }
        }
        let layout = self.layout();
        let (_, vh) = self.view_size();
        let Some(top) = layout.page_top(page, vh) else { return };
        let (bx, by) = layout.bounds(self.view_size());
        let x = self.doc().map_or(0, |d| d.view.0).clamp(bx.0, bx.1);
        self.set_view((x, top.clamp(by.0, by.1)));
        self.dirty = true;
    }

    pub(super) fn page_relative(&mut self, delta: i64) {
        let target = (self.current_page() as i64 + delta).clamp(0, self.page_count().saturating_sub(1) as i64);
        self.goto_page(target as usize);
    }

    pub(super) fn set_zoom(&mut self, zoom: u32) {
        let zoom = zoom.clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);
        let Some(old) = self.doc().map(|d| d.zoom) else { return };
        if zoom == old {
            return;
        }
        // Keep the document point under the cursor fixed on screen.
        let anchor = self.cursor_point();
        self.doc_mut().unwrap().zoom = zoom;
        let layout = self.layout();
        let (bx, by) = layout.bounds(self.view_size());
        let Some((page, x, y)) = anchor else { return self.keep_view_in_bounds() };
        let doc = self.doc().unwrap();
        let Some(placed) = layout.placed(page) else { return self.keep_view_in_bounds() };
        let (cx, cy) = placed.to_cell(&doc.sizes[page], x, y);
        let view = (
            (cx - self.cursor.0 as f64).floor() as i64,
            (cy - self.cursor.1 as f64).floor() as i64,
        );
        let view = (view.0.clamp(bx.0, bx.1), view.1.clamp(by.0, by.1));
        self.set_view(view);
        let (vw, vh) = self.view_size();
        self.cursor = (
            (cx.floor() as i64 - view.0).clamp(0, vw as i64 - 1) as u16,
            (cy.floor() as i64 - view.1).clamp(0, vh as i64 - 1) as u16,
        );
        self.dirty = true;
    }

    pub(super) fn reset_zoom(&mut self) {
        if self.doc().is_some_and(|d| d.zoom != 100) {
            self.set_zoom(100);
        }
        if let Some(y) = self.doc().map(|d| d.view.1) {
            let (bx, _) = self.layout().bounds(self.view_size());
            self.set_view((0.clamp(bx.0, bx.1), y));
        }
    }

    pub(super) fn set_mode(&mut self, mode: Mode) {
        if mode == self.mode {
            return;
        }
        let page = self.current_page();
        self.mode = mode;
        if mode == Mode::Single {
            if let Some(doc) = self.doc_mut() {
                doc.page = page;
            }
        }
        self.goto_page(page);
        self.dirty = true;
    }

    pub(super) fn move_cursor(&mut self, dx: i64, dy: i64) {
        let (vw, vh) = self.view_size();
        let Some(view) = self.doc().map(|d| d.view) else {
            self.cursor = (
                (self.cursor.0 as i64 + dx).clamp(0, vw as i64 - 1) as u16,
                (self.cursor.1 as i64 + dy).clamp(0, vh as i64 - 1) as u16,
            );
            self.dirty = true;
            return;
        };
        let (bx, by) = self.layout().bounds((vw, vh));
        let (cx, x) = move_axis(self.cursor.0, view.0, vw, bx, dx);
        let (cy, y) = move_axis(self.cursor.1, view.1, vh, by, dy);
        self.cursor = (cx, cy);
        self.set_view((x, y));
        self.update_selection_head();
        self.dirty = true;
    }

    pub(super) fn set_cursor(&mut self, cell: (u16, u16)) {
        let (vw, vh) = self.view_size();
        self.cursor = (cell.0.min(vw - 1), cell.1.min(vh - 1));
        self.update_selection_head();
        self.dirty = true;
    }

    /// Scroll so the cursor's document point sits at the viewport center.
    pub(super) fn center_cursor(&mut self) {
        let (vw, vh) = self.view_size();
        let center = (vw / 2, vh / 2);
        if let Some(view) = self.doc().map(|d| d.view) {
            self.set_view((
                view.0 + self.cursor.0 as i64 - center.0 as i64,
                view.1 + self.cursor.1 as i64 - center.1 as i64,
            ));
        }
        self.cursor = center;
        self.dirty = true;
    }

    /// Bring a page-space point into view and put the cursor on it.
    pub(super) fn reveal(&mut self, page: usize, x: f64, y: f64) {
        if self.mode == Mode::Single && self.doc().is_some_and(|d| d.page != page) {
            self.goto_page(page);
        }
        let layout = self.layout();
        let Some(doc) = self.doc() else { return };
        let Some(placed) = layout.placed(page) else { return };
        let (cx, cy) = placed.to_cell(&doc.sizes[page], x, y);
        let (cx, cy) = (cx.floor() as i64, cy.floor() as i64);
        let (vw, vh) = self.view_size();
        let (vw, vh) = (vw as i64, vh as i64);
        let (mut vx, mut vy) = doc.view;
        let (bx, by) = layout.bounds(self.view_size());
        if cx < vx || cx >= vx + vw {
            vx = (cx - vw / 2).clamp(bx.0, bx.1);
        }
        if cy < vy + 1 || cy >= vy + vh - 1 {
            vy = (cy - vh / 3).clamp(by.0, by.1);
        }
        self.set_view((vx, vy));
        self.cursor = ((cx - vx).clamp(0, vw - 1) as u16, (cy - vy).clamp(0, vh - 1) as u16);
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Grid;

    #[test]
    fn tiles_in_covers_only_intersecting_tiles() {
        let placed = Placed { page: 0, x: 10, y: 100, grid: Grid::new(200, 60, 10, 20, 1.0) };
        assert!(tiles_in(&placed, 0, 0, 80, 100).is_empty());
        assert_eq!(tiles_in(&placed, 0, 100, 80, 101), vec![(0, 0)]);
        let all = tiles_in(&placed, 0, 0, 1000, 1000);
        assert_eq!(all.len(), 2 * 3);
        assert_eq!(tiles_in(&placed, 150, 130, 151, 131), vec![(1, 1)]);
    }
}

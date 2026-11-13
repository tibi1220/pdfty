//! Page placement on a virtual canvas measured in terminal cells.
//!
//! Continuous view stacks every page; single view holds one page. The
//! viewport is a window onto the canvas at (view_x, view_y). Every page is a
//! whole number of cells, so placeholder text maps onto pixels exactly.

use crate::render::{Grid, PageSize};

/// Blank rows between pages in continuous view.
pub const PAGE_GAP: i64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Continuous,
    Single,
}

#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub mode: Mode,
    /// Percent of the fitted size: fit-width in continuous, fit-page in single view.
    pub zoom: u32,
    /// Viewport size in cells.
    pub view: (u16, u16),
    /// Cell size in pixels.
    pub cell: (u32, u32),
    /// Page shown in single view (0-based).
    pub page: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    pub page: usize,
    pub x: i64,
    pub y: i64,
    pub grid: Grid,
}

impl Placed {
    pub fn contains(&self, cx: i64, cy: i64) -> bool {
        cx >= self.x && cy >= self.y && cx < self.x + self.grid.cols as i64 && cy < self.y + self.grid.rows as i64
    }
    /// Canvas cell → page-space point (cell centers map to fractional cells).
    pub fn to_point(self, size: &PageSize, cx: f64, cy: f64) -> (f64, f64) {
        let s = self.grid.scale();
        let x = (cx - self.x as f64) * self.grid.cw as f64 / s + size.x0;
        let y = (cy - self.y as f64) * self.grid.ch as f64 / s + size.y0;
        (x.clamp(size.x0, size.x0 + size.w), y.clamp(size.y0, size.y0 + size.h))
    }
    /// Page-space point → fractional canvas cell.
    pub fn to_cell(self, size: &PageSize, x: f64, y: f64) -> (f64, f64) {
        let s = self.grid.scale();
        (
            self.x as f64 + (x - size.x0) * s / self.grid.cw as f64,
            self.y as f64 + (y - size.y0) * s / self.grid.ch as f64,
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub pages: Vec<Placed>,
    pub width: i64,
    pub height: i64,
}

fn grid(size: &PageSize, scale: f64, cell: (u32, u32)) -> Grid {
    let scale = scale.max(0.01) as f32;
    // Tolerate float noise so a fitted page is not one cell too large.
    let cells = |extent: f64, cell: u32| ((extent * scale as f64) / cell as f64 - 1e-3).ceil().max(1.0) as u32;
    let (cols, rows) = (cells(size.w, cell.0), cells(size.h, cell.1));
    Grid::new(cols, rows, cell.0, cell.1, scale)
}

pub fn compute(sizes: &[PageSize], p: &Params) -> Layout {
    let (vw, vh) = (p.view.0.max(1) as f64, p.view.1.max(1) as f64);
    let (cw, ch) = (p.cell.0.max(1) as f64, p.cell.1.max(1) as f64);
    let zoom = p.zoom.max(1) as f64 / 100.0;
    if sizes.is_empty() {
        return Layout::default();
    }
    match p.mode {
        Mode::Continuous => {
            // One scale for all pages, so mixed page sizes keep their proportions.
            let widest = sizes.iter().map(|s| s.w).fold(1.0, f64::max);
            let scale = vw * cw / widest * zoom;
            let grids: Vec<Grid> = sizes.iter().map(|s| grid(s, scale, p.cell)).collect();
            let width = grids.iter().map(|g| g.cols as i64).max().unwrap_or(0).max(p.view.0 as i64);
            let mut y = 0;
            let pages = grids
                .into_iter()
                .enumerate()
                .map(|(page, grid)| {
                    let placed = Placed { page, x: (width - grid.cols as i64) / 2, y, grid };
                    y += grid.rows as i64 + PAGE_GAP;
                    placed
                })
                .collect();
            Layout { pages, width, height: (y - PAGE_GAP).max(0) }
        }
        Mode::Single => {
            let page = p.page.min(sizes.len() - 1);
            let size = &sizes[page];
            let scale = (vw * cw / size.w).min(vh * ch / size.h) * zoom;
            let grid = grid(size, scale, p.cell);
            let width = (grid.cols as i64).max(p.view.0 as i64);
            let height = (grid.rows as i64).max(p.view.1 as i64);
            let placed = Placed {
                page,
                x: (width - grid.cols as i64) / 2,
                y: (height - grid.rows as i64) / 2,
                grid,
            };
            Layout { pages: vec![placed], width, height }
        }
    }
}

impl Layout {
    pub fn placed(&self, page: usize) -> Option<&Placed> {
        self.pages.iter().find(|p| p.page == page)
    }

    /// The page containing a canvas cell, else the vertically nearest page.
    pub fn page_at(&self, cx: i64, cy: i64) -> Option<&Placed> {
        self.pages.iter().find(|p| p.contains(cx, cy)).or_else(|| {
            self.pages.iter().min_by_key(|p| {
                let bottom = p.y + p.grid.rows as i64 - 1;
                if cy < p.y {
                    p.y - cy
                } else {
                    (cy - bottom).max(0)
                }
            })
        })
    }

    /// Natural scroll range: the canvas edges meet the viewport edges.
    pub fn bounds(&self, view: (u16, u16)) -> ((i64, i64), (i64, i64)) {
        (
            (0, (self.width - view.0 as i64).max(0)),
            (0, (self.height - view.1 as i64).max(0)),
        )
    }

    /// Top offset that shows `page` from its top, or centered if it fits.
    pub fn page_top(&self, page: usize, view_rows: u16) -> Option<i64> {
        let placed = self.placed(page)?;
        let spare = view_rows as i64 - placed.grid.rows as i64;
        Some(if spare > 0 { placed.y - spare / 2 } else { placed.y })
    }
}

/// Clamp to `bounds`, but never pull an offset that is already outside the
/// range (after centering near an edge) further than where it is.
pub fn clamp_soft(value: i64, bounds: (i64, i64), current: i64) -> i64 {
    value.clamp(bounds.0.min(current), bounds.1.max(current))
}

/// Move a cursor inside a viewport; movement past an edge scrolls instead.
pub fn move_axis(cursor: u16, offset: i64, viewport: u16, bounds: (i64, i64), delta: i64) -> (u16, i64) {
    let target = cursor as i64 + delta;
    let cell = target.clamp(0, viewport.saturating_sub(1) as i64);
    let offset = clamp_soft(offset + target - cell, bounds, offset);
    (cell as u16, offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A4: PageSize = PageSize { x0: 0.0, y0: 0.0, w: 595.0, h: 842.0 };

    fn params(mode: Mode, zoom: u32) -> Params {
        Params { mode, zoom, view: (100, 40), cell: (10, 20), page: 1 }
    }

    #[test]
    fn continuous_fits_width_and_stacks_pages() {
        let layout = compute(&[A4, A4, A4], &params(Mode::Continuous, 100));
        assert_eq!(layout.pages[0].grid.cols, 100);
        let rows = layout.pages[0].grid.rows as i64;
        assert_eq!(rows, (842.0 * 1000.0 / 595.0 / 20.0f64).ceil() as i64);
        assert_eq!(layout.pages[1].y, rows + PAGE_GAP);
        assert_eq!(layout.height, 3 * rows + 2 * PAGE_GAP);
        let zoomed = compute(&[A4], &params(Mode::Continuous, 50));
        assert_eq!(zoomed.pages[0].x, 25);
    }

    #[test]
    fn single_fits_page_and_centers() {
        let layout = compute(&[A4, A4], &params(Mode::Single, 100));
        assert_eq!(layout.pages.len(), 1);
        let p = layout.pages[0];
        assert_eq!(p.page, 1);
        assert!(p.grid.rows <= 40 && p.grid.cols < 100);
        assert_eq!(p.x, (100 - p.grid.cols as i64) / 2);
    }

    #[test]
    fn point_and_cell_conversions_invert() {
        let layout = compute(&[A4, A4], &params(Mode::Continuous, 150));
        let p = layout.pages[1];
        let (cx, cy) = p.to_cell(&A4, 100.0, 400.0);
        let (x, y) = p.to_point(&A4, cx, cy);
        assert!((x - 100.0).abs() < 1e-3 && (y - 400.0).abs() < 1e-3);
        assert_eq!(layout.page_at(cx as i64, cy as i64).unwrap().page, 1);
    }

    #[test]
    fn cursor_moves_before_scrolling_and_stops_at_edges() {
        assert_eq!(move_axis(5, 0, 10, (0, 20), 1), (6, 0));
        assert_eq!(move_axis(9, 0, 10, (0, 20), 3), (9, 3));
        assert_eq!(move_axis(0, 5, 10, (0, 20), -1), (0, 4));
        assert_eq!(move_axis(0, 0, 10, (0, 20), -1), (0, 0));
        assert_eq!(move_axis(9, 20, 10, (0, 20), 1), (9, 20));
        // An offset already outside the range does not jump back.
        assert_eq!(clamp_soft(-3, (0, 10), -5), -3);
        assert_eq!(clamp_soft(-8, (0, 10), -5), -5);
    }
}

//! Image API shared by the viewer coordinator and its domain modules.
//! Composition/overlays manipulate pixels; transport owns Kitty escape codes.

mod compose;
mod overlays;
#[cfg(test)]
mod tests;
mod transport;

pub use compose::{composition_worker, single_frame};
pub use overlays::{highlight, outline_paragraph};
pub use transport::{delete_image, next_image_id, place_at_id, Placement};

use crate::resolution::CellPixels;
use std::path::PathBuf;

/// Page rectangle in viewport cells: signed x/y origin, positive width/height.
pub type CellRect = (i32, i32, u32, u32);
/// A rendered page file plus its cell rectangle in the composed viewport.
pub type PagePlacement = (PathBuf, CellRect);

/// Visible pages and their rectangles in document cells, relative to the
/// image viewport (not the full terminal, which includes status bars).
pub struct ComposeRequest {
    pub pages: Vec<PagePlacement>,
    pub pane: (u16, u16),
    pub cell_pixels: CellPixels,
    pub output: PathBuf,
    pub generation: u64,
    pub cursor: Cursor,
}

/// A viewport cursor drawn last into the raster; it remains visible without terminal focus.
#[derive(Clone, Copy)]
pub struct Cursor {
    pub cell: (u16, u16),
    pub selecting: bool,
}

pub struct ComposeResult {
    pub output: PathBuf,
    pub generation: u64,
    pub result: Result<(), String>,
}

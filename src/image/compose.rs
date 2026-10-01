//! Raster composition and decoded-page reuse for both view modes.
//! Rectangles use viewport cells, then convert once to pixels. Cursor pixels
//! are drawn last; caching excludes them so movement cannot leave trails.

use super::{CellRect, ComposeRequest, ComposeResult, Cursor, PagePlacement};
use crate::resolution::CellPixels;
use ::image::{
    codecs::png::{CompressionType, FilterType, PngEncoder},
    imageops, ImageEncoder, Rgba, RgbaImage,
};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex, OnceLock,
    },
    thread,
};

/// Coalesce queued viewport requests while keeping PNG work off the input thread.
pub fn composition_worker() -> (Sender<ComposeRequest>, Receiver<ComposeResult>) {
    // Composition can be slower than wheel events. Skip queued intermediate
    // views and produce the most recent requested frame.
    let (requests, incoming) = mpsc::channel::<ComposeRequest>();
    let (outgoing, results) = mpsc::channel();
    thread::spawn(move || {
        let mut composer = Composer::default();
        while let Ok(mut request) = incoming.recv() {
            while let Ok(next) = incoming.try_recv() {
                request = next;
            }
            let result = composer.compose(
                &request.pages,
                request.pane,
                request.cell_pixels,
                &request.output,
                Rgba([255, 255, 255, 255]),
                Some(request.cursor),
            );
            if outgoing
                .send(ComposeResult {
                    output: request.output,
                    generation: request.generation,
                    result,
                })
                .is_err()
            {
                break;
            }
        }
    });
    (requests, results)
}

struct CachedPage {
    path: PathBuf,
    size: (u32, u32),
    pixels: Arc<RgbaImage>,
}

#[derive(Default)]
pub(super) struct Composer {
    pages: VecDeque<CachedPage>,
}

impl Composer {
    fn prepared(&mut self, path: &Path, size: (u32, u32)) -> Result<Arc<RgbaImage>, String> {
        // Cache a few decoded and resized pages. Reusing these pixels avoids
        // PNG decode and resize on every scroll frame.
        if let Some(index) = self
            .pages
            .iter()
            .position(|page| page.path == path && page.size == size)
        {
            let page = self.pages.remove(index).unwrap();
            let pixels = Arc::clone(&page.pixels);
            self.pages.push_front(page);
            return Ok(pixels);
        }
        let source = ::image::open(path).map_err(|e| e.to_string())?.to_rgba8();
        let pixels = Arc::new(if source.dimensions() == size {
            source
        } else {
            imageops::resize(&source, size.0, size.1, imageops::FilterType::Triangle)
        });
        let bytes = pixels.as_raw().len();
        if bytes <= 128 * 1024 * 1024 {
            // Bound both entry count and total memory; very large pages bypass
            // the cache rather than evicting everything repeatedly.
            while self.pages.len() >= 3
                || self
                    .pages
                    .iter()
                    .map(|p| p.pixels.as_raw().len())
                    .sum::<usize>()
                    + bytes
                    > 192 * 1024 * 1024
            {
                self.pages.pop_back();
            }
            self.pages.push_front(CachedPage {
                path: path.to_owned(),
                size,
                pixels: Arc::clone(&pixels),
            });
        }
        Ok(pixels)
    }

    pub(super) fn compose(
        &mut self,
        pages: &[PagePlacement],
        pane: (u16, u16),
        cell_pixels: CellPixels,
        output: &Path,
        background: Rgba<u8>,
        cursor: Option<Cursor>,
    ) -> Result<(), String> {
        // One opaque surface holds every visible page and gap. Updating one
        // Kitty image ID avoids flashing when pages enter or leave the pane.
        let mut canvas = RgbaImage::from_pixel(
            pane.0.max(1) as u32 * cell_pixels.width,
            pane.1.max(1) as u32 * cell_pixels.height,
            background,
        );
        for (path, (x, y, width, height)) in pages {
            if *width == 0 || *height == 0 {
                continue;
            }
            let left = (*x).max(0);
            let top = (*y).max(0);
            let right = (*x + *width as i32).min(pane.0 as i32);
            let bottom = (*y + *height as i32).min(pane.1 as i32);
            if right <= left || bottom <= top {
                continue;
            }
            let bitmap = self.prepared(
                path,
                (*width * cell_pixels.width, *height * cell_pixels.height),
            )?;
            // All rectangle edges are cell aligned. Copy RGBA rows directly so
            // adjacent pages cannot acquire seams from repeated interpolation.
            let row_bytes = (right - left) as usize * cell_pixels.width as usize * 4;
            let source_x = (left - *x) as usize * cell_pixels.width as usize * 4;
            let source_y = (top - *y) as usize * cell_pixels.height as usize;
            let target_x = left as usize * cell_pixels.width as usize * 4;
            let target_y = top as usize * cell_pixels.height as usize;
            let source_stride = bitmap.width() as usize * 4;
            let target_stride = canvas.width() as usize * 4;
            for row in 0..(bottom - top) as usize * cell_pixels.height as usize {
                let source_start = (source_y + row) * source_stride + source_x;
                let target_start = (target_y + row) * target_stride + target_x;
                canvas.as_mut()[target_start..target_start + row_bytes]
                    .copy_from_slice(&bitmap.as_raw()[source_start..source_start + row_bytes]);
            }
        }
        if let Some(cursor) = cursor {
            draw_cursor(&mut canvas, cursor, cell_pixels);
        }
        let file = std::fs::File::create(output).map_err(|e| e.to_string())?;
        PngEncoder::new_with_quality(file, CompressionType::Fast, FilterType::Adaptive)
            .write_image(
                canvas.as_raw(),
                canvas.width(),
                canvas.height(),
                ::image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| e.to_string())
    }
}

/// Cache the resized page while the cursor moves. Only the small overlay and
/// visible crop change; PDF rendering and page resizing are reused.
/// Reuse decoded page pixels while producing a fresh cursor-bearing single-page frame.
pub fn single_frame(
    source: &Path,
    output: &Path,
    page: CellRect,
    pane: (u16, u16),
    cell_pixels: CellPixels,
    cursor: Cursor,
) -> Result<(), String> {
    static COMPOSER: OnceLock<Mutex<Composer>> = OnceLock::new();
    COMPOSER
        .get_or_init(|| Mutex::new(Composer::default()))
        .lock()
        .map_err(|e| e.to_string())?
        .compose(
            &[(source.to_owned(), page)],
            pane,
            cell_pixels,
            output,
            Rgba([0, 0, 0, 0]),
            Some(cursor),
        )
}

fn draw_cursor(canvas: &mut RgbaImage, cursor: Cursor, cell: CellPixels) {
    let center_x = cursor.cell.0 as i32 * cell.width as i32 + cell.width as i32 / 2;
    let center_y = cursor.cell.1 as i32 * cell.height as i32 + cell.height as i32 / 2;
    let arm_x = (cell.width as i32 / 2 - 2).max(2);
    let arm_y = (cell.height as i32 / 3).max(3);
    let color = if cursor.selecting {
        Rgba([20, 110, 240, 255])
    } else {
        Rgba([225, 45, 45, 255])
    };
    // A black terminal-cell block matches the original cursor and remains
    // visible in image frames even while another pane has focus.
    let left = cursor.cell.0 as u32 * cell.width;
    let top = cursor.cell.1 as u32 * cell.height;
    for y in top..(top + cell.height).min(canvas.height()) {
        for x in left..(left + cell.width).min(canvas.width()) {
            let dx = x as i32 - center_x;
            let dy = y as i32 - center_y;
            let plus = (dx == 0 && dy.abs() <= arm_y) || (dy == 0 && dx.abs() <= arm_x);
            canvas.put_pixel(x, y, if plus { color } else { Rgba([0, 0, 0, 255]) });
        }
    }
}

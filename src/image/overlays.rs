//! Derived text/paragraph overlays in PDF point coordinates.
//! Always start from clean page pixels so highlight clearing is reversible.

use crate::text::Rect;
use ::image::Rgba;
use std::path::Path;

pub fn highlight(
    base: &Path,
    output: &Path,
    page: (f64, f64),
    rects: &[Rect],
) -> Result<(), String> {
    // Text boxes are PDF points; project them onto the rendered PNG before
    // tinting pixels. The source PNG is left intact for clearing selection.
    let mut bitmap = ::image::open(base).map_err(|e| e.to_string())?.to_rgba8();
    let width = bitmap.width();
    let height = bitmap.height();
    for rect in rects {
        let x0 = ((rect.x / page.0) * width as f64).floor().max(0.0) as u32;
        let y0 = ((rect.y / page.1) * height as f64).floor().max(0.0) as u32;
        let x1 = (((rect.x + rect.w) / page.0) * width as f64)
            .ceil()
            .min(width as f64) as u32;
        let y1 = (((rect.y + rect.h) / page.1) * height as f64)
            .ceil()
            .min(height as f64) as u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let pixel = bitmap.get_pixel_mut(x, y);
                for channel in 0..3 {
                    pixel.0[channel] =
                        ((pixel.0[channel] as u16 * 2 + [255, 210, 40][channel]) / 3) as u8;
                }
            }
        }
    }
    bitmap.save(output).map_err(|e| e.to_string())
}

/// Draw a visible outline around a MuPDF text block without covering its text.
/// The block rectangle and page size are in PDF points; only this overlay PNG
/// changes, leaving the cached render available for later navigation.
pub fn outline_paragraph(
    base: &Path,
    output: &Path,
    page: (f64, f64),
    rect: &Rect,
) -> Result<(), String> {
    let mut bitmap = ::image::open(base).map_err(|e| e.to_string())?.to_rgba8();
    let (width, height) = bitmap.dimensions();
    if page.0 <= 0.0 || page.1 <= 0.0 || width == 0 || height == 0 {
        return Err("invalid page dimensions for paragraph outline".into());
    }
    let x0 = (((rect.x - 3.0) / page.0) * width as f64)
        .floor()
        .clamp(0.0, (width - 1) as f64) as u32;
    let y0 = (((rect.y - 3.0) / page.1) * height as f64)
        .floor()
        .clamp(0.0, (height - 1) as f64) as u32;
    let x1 = (((rect.x + rect.w + 3.0) / page.0) * width as f64)
        .ceil()
        .clamp((x0 + 1) as f64, width as f64) as u32;
    let y1 = (((rect.y + rect.h + 3.0) / page.1) * height as f64)
        .ceil()
        .clamp((y0 + 1) as f64, height as f64) as u32;
    let stroke = ((width as f64 / page.0) * 1.5).round().clamp(2.0, 8.0) as u32;
    let color = Rgba([225, 45, 45, 255]);
    for y in y0..y1 {
        for x in x0..x1 {
            if x - x0 < stroke || x1 - 1 - x < stroke || y - y0 < stroke || y1 - 1 - y < stroke {
                bitmap.put_pixel(x, y, color);
            }
        }
    }
    bitmap.save(output).map_err(|e| e.to_string())
}

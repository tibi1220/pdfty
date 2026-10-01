//! Exercise raster coordinates, overlay reversibility, and terminal-free icat output.

use super::compose::Composer;
use super::transport::placement_command;
use super::{composition_worker, outline_paragraph, single_frame, ComposeRequest, Placement};
use crate::resolution::CellPixels;
use crate::text::Rect;

#[test]
fn cursor_is_visible_in_page_margins_and_does_not_change_cached_page() {
    let dir = std::env::temp_dir();
    let id = std::process::id();
    let base = dir.join(format!("pdfty-cursor-{id}-base.png"));
    let output = dir.join(format!("pdfty-cursor-{id}-frame.png"));
    ::image::RgbaImage::from_pixel(12, 48, ::image::Rgba([255, 255, 255, 255]))
        .save(&base)
        .unwrap();
    let cell = CellPixels::default();
    for (position, selecting, color) in [
        ((0, 0), false, [225, 45, 45, 255]),
        ((3, 1), false, [225, 45, 45, 255]),
        ((3, 1), true, [20, 110, 240, 255]),
    ] {
        super::single_frame(
            &base,
            &output,
            (1, 0, 1, 2),
            (4, 2),
            cell,
            super::Cursor {
                cell: position,
                selecting,
            },
        )
        .unwrap();
        let frame = ::image::open(&output).unwrap().to_rgba8();
        assert_eq!(frame.dimensions(), (48, 48));
        assert_eq!(
            frame
                .get_pixel(position.0 as u32 * 12 + 6, position.1 as u32 * 24 + 12)
                .0,
            color
        );
        assert_eq!(
            frame
                .get_pixel(position.0 as u32 * 12, position.1 as u32 * 24)
                .0,
            [0, 0, 0, 255],
            "cursor must have a black block background"
        );
        assert_eq!(frame.get_pixel(18, 12).0, [255, 255, 255, 255]);
        if position != (0, 0) {
            assert_eq!(frame.get_pixel(6, 12).0, [0, 0, 0, 0]);
        }
    }
    assert!(::image::open(&base)
        .unwrap()
        .to_rgba8()
        .pixels()
        .all(|pixel| pixel.0 == [255, 255, 255, 255]));
    let _ = std::fs::remove_file(base);
    let _ = std::fs::remove_file(output);
}

#[test]
fn icat_generates_image_output_without_terminal_access() {
    let path = std::env::temp_dir().join(format!("pdfty-icat-{}.png", std::process::id()));
    ::image::RgbaImage::from_pixel(12, 24, ::image::Rgba([0, 0, 0, 255]))
        .save(&path)
        .unwrap();
    for tmux in [false, true] {
        if tmux && std::env::var_os("TMUX").is_none() {
            continue;
        }
        let output = placement_command(
            &path,
            &Placement {
                viewport: (1, 1),
                origin: (0, 1),
                image_id: Some(42),
                terminal: (80, 24),
                cell_pixels: CellPixels::default(),
            },
            tmux,
        )
        .output()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.windows(3).any(|bytes| bytes == b"\x1b_G"));
        if tmux {
            assert!(output.stdout.windows(7).any(|bytes| bytes == b"\x1bPtmux;"));
            assert!(String::from_utf8_lossy(&output.stdout).contains('\u{10eeee}'));
        }
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn paragraph_outline_surrounds_text_without_covering_it() {
    let dir = std::env::temp_dir();
    let id = std::process::id();
    let base = dir.join(format!("pdfty-outline-{id}-base.png"));
    let output = dir.join(format!("pdfty-outline-{id}-result.png"));
    ::image::RgbaImage::from_pixel(100, 100, ::image::Rgba([255, 255, 255, 255]))
        .save(&base)
        .unwrap();
    outline_paragraph(
        &base,
        &output,
        (100.0, 100.0),
        &Rect {
            x: 20.0,
            y: 20.0,
            w: 40.0,
            h: 30.0,
        },
    )
    .unwrap();
    let bitmap = ::image::open(&output).unwrap().to_rgba8();
    assert_eq!(bitmap.get_pixel(17, 35).0, [225, 45, 45, 255]);
    assert_eq!(bitmap.get_pixel(40, 35).0, [255, 255, 255, 255]);
    let _ = std::fs::remove_file(base);
    let _ = std::fs::remove_file(output);
}
#[test]
fn crop_maps_visible_rows_to_pixels() {
    let dir = std::env::temp_dir();
    let id = std::process::id();
    let base = dir.join(format!("pdfty-crop-{id}-base.png"));
    let clipped = dir.join(format!("pdfty-crop-{id}-clipped.png"));
    let bitmap = ::image::RgbaImage::from_pixel(20, 100, ::image::Rgba([255, 255, 255, 255]));
    bitmap.save(&base).unwrap();
    single_frame(
        &base,
        &clipped,
        (0, -20, 20, 100),
        (20, 30),
        CellPixels::default(),
        super::Cursor {
            cell: (0, 0),
            selecting: false,
        },
    )
    .unwrap();
    let result = ::image::open(&clipped).unwrap();
    assert_eq!((result.width(), result.height()), (240, 720));
    let _ = std::fs::remove_file(base);
    let _ = std::fs::remove_file(clipped);
}
#[test]
fn crop_handles_zoomed_page_in_both_axes() {
    let dir = std::env::temp_dir();
    let id = std::process::id();
    let base = dir.join(format!("pdfty-zoom-{id}-base.png"));
    let clipped = dir.join(format!("pdfty-zoom-{id}-clipped.png"));
    ::image::RgbaImage::from_pixel(100, 100, ::image::Rgba([255, 255, 255, 255]))
        .save(&base)
        .unwrap();
    single_frame(
        &base,
        &clipped,
        (-20, -20, 100, 100),
        (30, 30),
        CellPixels {
            width: 3,
            height: 7,
        },
        super::Cursor {
            cell: (0, 0),
            selecting: false,
        },
    )
    .unwrap();
    let cropped = ::image::open(&clipped).unwrap();
    assert_eq!((cropped.width(), cropped.height()), (90, 210));
    let _ = std::fs::remove_file(base);
    let _ = std::fs::remove_file(clipped);
}
#[test]
fn compose_keeps_previous_page_and_gap_in_one_surface() {
    let dir = std::env::temp_dir();
    let id = std::process::id();
    let first = dir.join(format!("pdfty-compose-{id}-first.png"));
    let second = dir.join(format!("pdfty-compose-{id}-second.png"));
    let output = dir.join(format!("pdfty-compose-{id}-out.png"));
    ::image::RgbaImage::from_pixel(24, 48, ::image::Rgba([255, 0, 0, 255]))
        .save(&first)
        .unwrap();
    ::image::RgbaImage::from_pixel(24, 48, ::image::Rgba([0, 0, 255, 255]))
        .save(&second)
        .unwrap();
    Composer::default()
        .compose(
            &[
                (first.clone(), (0, 0, 1, 1)),
                (second.clone(), (0, 2, 1, 1)),
            ],
            (2, 3),
            CellPixels {
                width: 36,
                height: 84,
            },
            &output,
            ::image::Rgba([255, 255, 255, 255]),
            None,
        )
        .unwrap();
    let bitmap = ::image::open(&output).unwrap().to_rgba8();
    assert_eq!((bitmap.width(), bitmap.height()), (72, 252));
    assert_eq!(bitmap.get_pixel(0, 0).0, [255, 0, 0, 255]);
    assert_eq!(bitmap.get_pixel(0, 84).0, [255, 255, 255, 255]);
    assert_eq!(bitmap.get_pixel(0, 168).0, [0, 0, 255, 255]);
    for path in [first, second, output] {
        let _ = std::fs::remove_file(path);
    }
}
#[test]
fn composition_worker_finishes_latest_view() {
    let dir = std::env::temp_dir();
    let id = std::process::id();
    let (requests, results) = composition_worker();
    for generation in 1..=3 {
        requests
            .send(ComposeRequest {
                pages: Vec::new(),
                pane: (2, 2),
                cell_pixels: CellPixels::default(),
                output: dir.join(format!("pdfty-frame-{id}-{generation}.png")),
                generation,
                cursor: super::Cursor {
                    cell: (0, 0),
                    selecting: false,
                },
            })
            .unwrap();
    }
    loop {
        let result = results
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(result.result.is_ok());
        let generation = result.generation;
        let _ = std::fs::remove_file(result.output);
        if generation == 3 {
            break;
        }
    }
}

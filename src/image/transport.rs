//! Kitty image transport, with pane-safe tmux passthrough.
//! icat generates escape codes without querying the terminal. Callers place a
//! new image and delete its predecessor in one synchronized output frame.

use crate::resolution::CellPixels;
use std::{
    io::{self, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU32, Ordering},
        OnceLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

/// All placement coordinates are terminal cells, including status-bar rows.
/// Keeping size/origin together prevents swapping positional arguments at call sites.
#[derive(Clone, Copy)]
pub struct Placement {
    pub viewport: (u16, u16),
    pub origin: (u16, u16),
    pub image_id: Option<u32>,
    pub terminal: (u16, u16),
    pub cell_pixels: CellPixels,
}

/// Emit an image placement using the exact terminal dimensions supplied by the viewer.
pub fn place_at_id(path: &Path, placement: &Placement) -> Result<(), String> {
    let mut command = placement_command(path, placement, std::env::var_os("TMUX").is_some());
    // icat is only an escape-code generator here. It must not query/read the
    // viewer's TTY or change its modes while crossterm owns terminal input.
    let output = command.output().map_err(|e| e.to_string())?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(format!(
            "kitten icat: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(&output.stdout)
        .and_then(|_| stdout.flush())
        .map_err(|e| e.to_string())
}

pub fn next_image_id() -> u32 {
    // Keep IDs in the foreground color's 24 bits. This avoids relying on the
    // third combining character to identify the image during tmux redraws.
    static NEXT: OnceLock<AtomicU32> = OnceLock::new();
    NEXT.get_or_init(|| {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            ^ std::process::id().rotate_left(16);
        AtomicU32::new((seed & 0x00ff_ffff).max(1))
    })
    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
        Some(((id + 1) & 0x00ff_ffff).max(1))
    })
    .unwrap()
}

pub fn delete_image(image_id: u32) {
    // Delete only the viewer's previous image, after the new placeholders have
    // been written. Reusing an ID during upload deletes its visible placements.
    let graphics = format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\");
    let mut stdout = io::stdout().lock();
    if std::env::var_os("TMUX").is_some() {
        let _ = write!(
            stdout,
            "\x1bPtmux;{}\x1b\\",
            graphics.replace('\x1b', "\x1b\x1b")
        );
    } else {
        let _ = stdout.write_all(graphics.as_bytes());
    }
    let _ = stdout.flush();
}

/// Construct icat's explicit geometry without terminal queries or input-mode changes.
pub(super) fn placement_command(path: &Path, placement: &Placement, tmux: bool) -> Command {
    let Placement {
        viewport: (cols, rows),
        origin: (x, y),
        image_id,
        terminal: window,
        cell_pixels,
    } = *placement;
    let place = format!("{cols}x{rows}@{x}x{y}");
    let mut command = Command::new("kitten");
    let transfer_mode = if tmux {
        "--transfer-mode=stream"
    } else {
        "--transfer-mode=file"
    };
    command.args([
        "icat",
        transfer_mode,
        if tmux {
            "--passthrough=tmux"
        } else {
            "--passthrough=none"
        },
        "--stdin=no",
        "--engine=builtin",
        &format!(
            "--use-window-size={},{},{},{}",
            window.0,
            window.1,
            window.0 as u32 * cell_pixels.width,
            window.1 as u32 * cell_pixels.height
        ),
        &format!("--place={place}"),
        "--scale-up=yes",
    ]);
    if let Some(id) = image_id {
        command.arg(format!("--image-id={id}"));
    }
    command.arg(path).stdin(Stdio::null());
    command
}

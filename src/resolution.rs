/// Target bitmap pixels per terminal cell. Both rendering and composition use
/// these values so page boundaries remain aligned after zoom and scrolling.
pub const CELL_WIDTH: u32 = 12;
pub const CELL_HEIGHT: u32 = 24;

use std::process::Command;

/// High-resolution output pixels per terminal cell. The width stays at the
/// requested 12-pixel target; height follows the terminal's real cell aspect
/// ratio so Kitty does not letterbox the PNG inside a `--place` rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellPixels {
    pub width: u32,
    pub height: u32,
}

impl Default for CellPixels {
    fn default() -> Self {
        Self {
            width: CELL_WIDTH,
            height: CELL_HEIGHT,
        }
    }
}

impl CellPixels {
    fn from_cell_dimensions(cell_width: f64, cell_height: f64) -> Self {
        let ratio = cell_height / cell_width;
        if !(0.8..=4.0).contains(&ratio) {
            return Self::default();
        }
        Self {
            width: CELL_WIDTH,
            height: (CELL_WIDTH as f64 * ratio).round().max(1.0) as u32,
        }
    }

    pub fn from_window(window: crossterm::terminal::WindowSize) -> Self {
        if window.columns == 0 || window.rows == 0 || window.width == 0 || window.height == 0 {
            return Self::default();
        }
        let cell_width = window.width as f64 / window.columns as f64;
        let cell_height = window.height as f64 / window.rows as f64;
        Self::from_cell_dimensions(cell_width, cell_height)
    }

    pub fn current() -> Self {
        if std::env::var_os("TMUX").is_some() {
            if let Ok(output) = Command::new("tmux")
                .args([
                    "display-message",
                    "-p",
                    "#{client_cell_width} #{client_cell_height}",
                ])
                .output()
            {
                if output.status.success() {
                    let text = String::from_utf8_lossy(&output.stdout);
                    let mut dimensions = text
                        .split_whitespace()
                        .filter_map(|value| value.parse::<f64>().ok());
                    if let (Some(width), Some(height)) = (dimensions.next(), dimensions.next()) {
                        if width > 0.0 && height > 0.0 {
                            return Self::from_cell_dimensions(width, height);
                        }
                    }
                }
            }
        }
        crossterm::terminal::window_size()
            .map(Self::from_window)
            .unwrap_or_default()
    }

    pub fn aspect(self) -> f64 {
        self.width as f64 / self.height as f64
    }
}

#[cfg(test)]
mod tests {
    use super::CellPixels;
    use crossterm::terminal::WindowSize;

    #[test]
    fn output_pixels_follow_terminal_cell_aspect() {
        let cells = CellPixels::from_window(WindowSize {
            columns: 100,
            rows: 40,
            width: 900,
            height: 840,
        });
        assert_eq!((cells.width, cells.height), (12, 28));
    }
}

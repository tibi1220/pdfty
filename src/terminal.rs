//! Terminal mode setup/restore, size and cell-pixel queries.

use std::{
    env,
    io::{self, Write},
    process::Command,
};

const ENTER: &str = concat!(
    "\x1b[?1049h", // alternate screen
    "\x1b[?25l",   // hide cursor
    "\x1b[?7l",    // no autowrap: writing the last column never scrolls
    "\x1b[?1000h\x1b[?1002h\x1b[?1006h", // clicks, drags and wheel (SGR)
    "\x1b[2J"
);
const LEAVE: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?7h\x1b[?25h\x1b[0m\x1b[?1049l";

/// Restores the terminal (and the tmux passthrough option) on drop.
pub struct Guard {
    _passthrough: Option<TmuxPassthrough>,
}

impl Guard {
    pub fn enter() -> Result<Self, String> {
        let passthrough = TmuxPassthrough::enter()?;
        crossterm::terminal::enable_raw_mode().map_err(|e| e.to_string())?;
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            previous(info);
        }));
        let mut out = io::stdout().lock();
        out.write_all(ENTER.as_bytes()).and_then(|_| out.flush()).map_err(|e| e.to_string())?;
        Ok(Self { _passthrough: passthrough })
    }
}

fn restore() {
    let mut out = io::stdout().lock();
    let _ = out.write_all(LEAVE.as_bytes());
    let _ = out.flush();
    let _ = crossterm::terminal::disable_raw_mode();
}

impl Drop for Guard {
    fn drop(&mut self) {
        restore();
    }
}

pub fn in_tmux() -> bool {
    env::var_os("TMUX").is_some()
}

pub fn size() -> (u16, u16) {
    crossterm::terminal::size().unwrap_or((80, 24))
}

/// The outer terminal when running inside a Neovim terminal (see kitty::Graphics).
pub fn outer_tty() -> Option<std::fs::File> {
    let path = env::var_os("PDFTY_TTY")?;
    std::fs::OpenOptions::new().write(true).open(path).ok()
}

/// Window size of a terminal device: (columns, rows, width px, height px).
fn window_size(tty: &std::fs::File) -> Option<(u32, u32, u32, u32)> {
    use std::os::fd::AsRawFd;
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(tty.as_raw_fd(), libc::TIOCGWINSZ, &mut size) } == 0;
    ok.then_some((size.ws_col as u32, size.ws_row as u32, size.ws_xpixel as u32, size.ws_ypixel as u32))
}

/// Cell size in device pixels. Pages are rendered at exactly this density so
/// Kitty never rescales them. Inside Neovim, the pty Neovim gives us reports no
/// pixel size, so the outer terminal is asked instead.
pub fn cell_pixels(outer: Option<&std::fs::File>) -> (u32, u32) {
    if in_tmux() {
        if let Ok(output) = Command::new("tmux")
            .args(["display-message", "-p", "#{client_cell_width} #{client_cell_height}"])
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut values = text.split_whitespace().filter_map(|v| v.parse::<u32>().ok());
            if let (Some(w), Some(h)) = (values.next(), values.next()) {
                if w > 0 && h > 0 {
                    return (w, h);
                }
            }
        }
    }
    if let Some((cols, rows, w, h)) = outer.and_then(window_size) {
        if cols > 0 && rows > 0 && w >= cols && h >= rows {
            return (w / cols, h / rows);
        }
    }
    if let Ok(window) = crossterm::terminal::window_size() {
        if window.columns > 0 && window.rows > 0 && window.width > 0 && window.height > 0 {
            let w = window.width as u32 / window.columns as u32;
            let h = window.height as u32 / window.rows as u32;
            if w > 0 && h > 0 {
                return (w, h);
            }
        }
    }
    (10, 20)
}

/// tmux drops passthrough sequences while redrawing unless the pane allows
/// `all`. Override only this pane and restore the previous value on exit.
struct TmuxPassthrough {
    pane: String,
    original: String,
}

impl TmuxPassthrough {
    fn enter() -> Result<Option<Self>, String> {
        if !in_tmux() {
            return Ok(None);
        }
        let pane = env::var("TMUX_PANE").map_err(|e| format!("TMUX_PANE: {e}"))?;
        let tmux = |args: &[&str]| -> Result<String, String> {
            let output = Command::new("tmux").args(args).output().map_err(|e| e.to_string())?;
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
            } else {
                Err(format!("tmux passthrough: {}", String::from_utf8_lossy(&output.stderr).trim()))
            }
        };
        let original = tmux(&["show-options", "-p", "-v", "-t", &pane, "allow-passthrough"])?;
        tmux(&["set-option", "-p", "-t", &pane, "allow-passthrough", "all"])?;
        Ok(Some(Self { pane, original }))
    }
}

impl Drop for TmuxPassthrough {
    fn drop(&mut self) {
        let mut command = Command::new("tmux");
        command.args(["set-option", "-p", "-t", &self.pane]);
        if self.original.is_empty() {
            command.args(["-u", "allow-passthrough"]);
        } else {
            command.args(["allow-passthrough", &self.original]);
        }
        let _ = command.output();
    }
}

use crossterm::{
    cursor, event, execute, queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use std::{
    env,
    io::{self, Write},
    process::Command,
    time::Duration,
};

/// Restores the terminal even when the app returns early with an error.
pub struct Guard {
    _passthrough: Option<TmuxPassthrough>,
}
impl Guard {
    pub fn enter() -> Result<Self, String> {
        let passthrough = TmuxPassthrough::enter()?;
        terminal::enable_raw_mode().map_err(|e| e.to_string())?;
        if let Err(e) = execute!(
            io::stdout(),
            terminal::EnterAlternateScreen,
            cursor::Hide,
            // Report clicks and wheel events, without mouse motion or drags.
            Print("\x1b[?1002l\x1b[?1003l\x1b[?1000h\x1b[?1006h"),
            event::DisableFocusChange
        ) {
            let _ = terminal::disable_raw_mode();
            return Err(e.to_string());
        }
        Ok(Self {
            _passthrough: passthrough,
        })
    }
}

/// `on` drops passthrough commands during tmux redraws. `all` also delivers
/// uploads/deletions while a pane is being redrawn or synchronized. Override
/// only this pane, and restore its original setting when the viewer exits.
struct TmuxPassthrough {
    pane: String,
    original: String,
}

impl TmuxPassthrough {
    fn enter() -> Result<Option<Self>, String> {
        if env::var_os("TMUX").is_none() {
            return Ok(None);
        }
        let pane = env::var("TMUX_PANE").map_err(|e| e.to_string())?;
        let output = Command::new("tmux")
            .args(["show-options", "-p", "-v", "-t", &pane, "allow-passthrough"])
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "tmux passthrough: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let original = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let output = Command::new("tmux")
            .args(["set-option", "-p", "-t", &pane, "allow-passthrough", "all"])
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "tmux passthrough: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
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

/// Keep image placeholders, the search marker and image deletion in one frame.
/// tmux builds the complete grid before it redraws the pane, instead of sending
/// incomplete combining characters to Kitty during incremental updates.
pub struct Frame;

impl Frame {
    pub fn begin() -> Self {
        let _ = execute!(io::stdout(), terminal::BeginSynchronizedUpdate);
        Self
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), terminal::EndSynchronizedUpdate);
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            event::DisableMouseCapture,
            event::DisableFocusChange,
            terminal::LeaveAlternateScreen,
            cursor::Show
        );
        let _ = terminal::disable_raw_mode();
    }
}
pub fn size() -> (u16, u16) {
    terminal::size().unwrap_or((80, 24))
}
pub fn input() -> Option<event::Event> {
    if event::poll(Duration::from_millis(35)).ok()? {
        event::read().ok()
    } else {
        None
    }
}
pub fn status(line: &str, width: u16, height: u16) {
    status_at(line, width, height.saturating_sub(1));
}
pub fn status_at(line: &str, width: u16, row: u16) {
    let mut out = io::stdout();
    // Avoid writing the last column: some terminals wrap there and scroll the
    // entire screen when this is the bottom status row.
    let truncated: String = line
        .chars()
        .take(width.saturating_sub(1) as usize)
        .collect();
    let _ = execute!(
        out,
        cursor::MoveTo(0, row),
        SetBackgroundColor(Color::DarkGrey),
        SetForegroundColor(Color::White),
        terminal::Clear(ClearType::CurrentLine),
        Print(truncated),
        ResetColor
    );
    let _ = out.flush();
}
pub fn call_at(line: &str, width: u16, row: u16) {
    let limit = width.saturating_sub(1) as usize;
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= limit {
        status_at(line, width, row);
        return;
    }
    if limit <= 3 {
        status_at(line, width, row);
        return;
    }
    // Keep the result/command prefix and the final -c line number visible.
    let head = (limit - 3) / 2;
    let tail = limit - 3 - head;
    let clipped = format!(
        "{}...{}",
        chars[..head].iter().collect::<String>(),
        chars[chars.len() - tail..].iter().collect::<String>()
    );
    status_at(&clipped, width, row);
}
pub fn clear() {
    let _ = execute!(io::stdout(), terminal::Clear(ClearType::All));
}
pub fn marker(x: u16, y: u16) {
    let mut out = io::stdout();
    let _ = execute!(out, cursor::MoveTo(x, y));
    let _ = write!(out, "\x1b[1;31;40m+\x1b[0m");
    let _ = out.flush();
}

pub fn erase_marker(x: u16, y: u16) {
    let _ = execute!(io::stdout(), cursor::MoveTo(x, y), Print(" "));
}

pub fn help_visible_lines((_, height): (u16, u16)) -> usize {
    height.saturating_sub(6) as usize
}

pub fn help(lines: &[&str], offset: usize, (width, height): (u16, u16)) {
    // The help overlay is text drawn over the image; callers suspend image
    // updates until it closes so Kitty does not erase the popup.
    if width < 20 || height < 7 {
        status("Help: enlarge the terminal; ? or Esc closes", width, height);
        return;
    }
    let visible = help_visible_lines((width, height)).min(lines.len());
    let box_width = width.saturating_sub(4).min(76);
    let inner = box_width.saturating_sub(4) as usize;
    let box_height = visible as u16 + 4;
    let left = (width - box_width) / 2;
    let top = (height - box_height) / 2;
    let mut out = io::stdout().lock();
    let border = format!("+{}+", "-".repeat(box_width.saturating_sub(2) as usize));
    let _ = queue!(
        out,
        SetBackgroundColor(Color::DarkGrey),
        SetForegroundColor(Color::White)
    );
    let _ = queue!(out, cursor::MoveTo(left, top), Print(&border));
    let heading = help_row("pdfty keys", inner);
    let _ = queue!(
        out,
        cursor::MoveTo(left, top + 1),
        SetForegroundColor(Color::Yellow),
        Print(heading),
        SetForegroundColor(Color::White)
    );
    for row in 0..visible {
        let line = lines.get(offset + row).copied().unwrap_or("");
        let padded = help_row(line, inner);
        let _ = queue!(
            out,
            cursor::MoveTo(left, top + 2 + row as u16),
            Print(padded)
        );
    }
    let footer = help_row("Up/Down scroll   ?/Esc close   q quit", inner);
    let _ = queue!(
        out,
        cursor::MoveTo(left, top + 2 + visible as u16),
        SetForegroundColor(Color::Yellow),
        Print(footer),
        SetForegroundColor(Color::White)
    );
    let _ = queue!(
        out,
        cursor::MoveTo(left, top + 3 + visible as u16),
        Print(border),
        ResetColor
    );
    let _ = out.flush();
}

fn help_row(line: &str, width: usize) -> String {
    let clipped: String = line.chars().take(width).collect();
    format!("| {clipped:width$} |")
}

//! Frame composition. The screen is a grid of cells: image placeholders,
//! blank paper for tiles still rendering, and text (cursor, panels). Rows
//! are diffed against the previous frame; pure vertical scrolls are sent as
//! a terminal scroll so only the newly exposed rows are written.

use super::{view::tiles_in, App, Panel};
use crate::{kitty, layout::Mode};
use std::{
    collections::HashSet,
    fmt::Write as _,
    io::Write,
    time::{Duration, Instant},
};

type Rgb = (u8, u8, u8);

#[derive(Clone, Copy, PartialEq, Eq)]
struct Style {
    fg: Rgb,
    bg: Option<Rgb>,
    bold: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cell {
    Blank,
    Paper,
    Image { id: u32, row: u16, col: u16 },
    Text { ch: char, style: Style },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pen {
    Blank,
    Paper,
    Image(u32),
    Text(Style),
}

// Nightfox palette (as in Kitty's nightfox.conf).
const BG0: Rgb = (0x13, 0x1a, 0x24);
const FG: Rgb = (0xcd, 0xce, 0xcf);
const COMMENT: Rgb = (0x73, 0x80, 0x91);
const BLUE: Rgb = (0x71, 0x9c, 0xd6);
const RED: Rgb = (0xc9, 0x4f, 0x6d);
const GREEN: Rgb = (0x81, 0xb2, 0x9a);
const YELLOW: Rgb = (0xdb, 0xc0, 0x74);

const fn on_bg0(fg: Rgb, bold: bool) -> Style {
    Style { fg, bg: Some(BG0), bold }
}
const PANEL: Style = on_bg0(FG, false);
const PANEL_TITLE: Style = on_bg0(YELLOW, true);
const STATUS: Style = on_bg0(FG, false);
const STATUS_NAME: Style = on_bg0(BLUE, true);
const STATUS_DIM: Style = on_bg0(COMMENT, false);
const STATUS_BUSY: Style = on_bg0(YELLOW, false);
const STATUS_READY: Style = on_bg0(GREEN, false);
const STATUS_ERROR: Style = on_bg0(RED, true);
const STATUS_COUNT: Style = on_bg0(YELLOW, true);
const FOOTER: Style = on_bg0(COMMENT, false);
const DIM: Style = Style { fg: COMMENT, bg: None, bold: false };
const BRIGHT: Style = Style { fg: FG, bg: None, bold: true };

fn sgr(out: &mut String, style: Style) {
    let (r, g, b) = style.fg;
    let _ = write!(out, "\x1b[0;38;2;{r};{g};{b}");
    if let Some((r, g, b)) = style.bg {
        let _ = write!(out, ";48;2;{r};{g};{b}");
    }
    out.push_str(if style.bold { ";1m" } else { "m" });
}

fn serialize(cells: &[Cell]) -> String {
    let mut out = String::with_capacity(cells.len() * 9);
    let mut pen = None;
    for cell in cells {
        match *cell {
            Cell::Blank => {
                if pen != Some(Pen::Blank) {
                    out.push_str("\x1b[0m");
                    pen = Some(Pen::Blank);
                }
                out.push(' ');
            }
            Cell::Paper => {
                if pen != Some(Pen::Paper) {
                    out.push_str("\x1b[0;48;2;255;255;255m");
                    pen = Some(Pen::Paper);
                }
                out.push(' ');
            }
            Cell::Image { id, row, col } => {
                if pen != Some(Pen::Image(id)) {
                    kitty::image_color(&mut out, id);
                    pen = Some(Pen::Image(id));
                }
                kitty::placeholder(&mut out, row, col);
            }
            Cell::Text { ch, style } => {
                if pen != Some(Pen::Text(style)) {
                    sgr(&mut out, style);
                    pen = Some(Pen::Text(style));
                }
                out.push(ch);
            }
        }
    }
    out.push_str("\x1b[0m");
    out
}

struct Canvas {
    cells: Vec<Cell>,
    width: usize,
    height: usize,
}

impl Canvas {
    fn put(&mut self, x: usize, y: usize, cell: Cell) {
        if x < self.width && y < self.height {
            self.cells[y * self.width + x] = cell;
        }
    }
    fn text(&mut self, x: usize, y: usize, text: &str, style: Style) {
        for (i, ch) in text.chars().enumerate() {
            let ch = if ch.is_control() { ' ' } else { ch };
            self.put(x + i, y, Cell::Text { ch, style });
        }
    }
    fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, style: Style) {
        for row in y..y + h {
            for col in x..x + w {
                self.put(col, row, Cell::Text { ch: ' ', style });
            }
        }
    }
}

impl App {
    pub(super) fn draw(&mut self) {
        self.dirty = false;
        self.frame += 1;
        let (vw, vh) = self.view_size();
        let (width, height) = (vw as usize, vh as usize);
        let mut canvas = Canvas { cells: vec![Cell::Blank; width * height], width, height };
        let mut used = Vec::new();
        let mut visible = HashSet::new();

        if let Some(doc) = self.doc() {
            let layout = self.layout();
            let (vx, vy) = doc.view;
            let (x1, y1) = (vx + vw as i64, vy + vh as i64);
            for placed in &layout.pages {
                for (tx, ty) in tiles_in(placed, vx, vy, x1, y1) {
                    let key = self.tile_key(doc, placed, tx, ty);
                    // Prefer the exact tile; otherwise keep showing the most
                    // recent image of this tile (older overlay or PDF version)
                    // until the replacement arrives, so nothing flashes.
                    let found = if self.tiles.contains_key(&key) {
                        Some(key)
                    } else {
                        self.latest.get(&key.base).copied().filter(|k| self.tiles.contains_key(k))
                    };
                    let id = found.map(|k| {
                        used.push(k);
                        self.tiles[&k].id
                    });
                    visible.extend(id);
                    let (c0, r0, cols, rows) = placed.grid.tile_cells(tx, ty);
                    let (ox, oy) = (placed.x + c0 as i64, placed.y + r0 as i64);
                    for y in oy.max(vy)..(oy + rows as i64).min(y1) {
                        for x in ox.max(vx)..(ox + cols as i64).min(x1) {
                            let cell = match id {
                                Some(id) => Cell::Image { id, row: (y - oy) as u16, col: (x - ox) as u16 },
                                None => Cell::Paper,
                            };
                            canvas.put((x - vx) as usize, (y - vy) as usize, cell);
                        }
                    }
                }
            }
        } else {
            self.draw_welcome(&mut canvas);
        }
        for key in used {
            if let Some(tile) = self.tiles.get_mut(&key) {
                tile.used = self.frame;
            }
        }

        let selecting = self.selection.is_some();
        let cursor_style = on_bg0(if selecting { BLUE } else { RED }, true);
        canvas.put(self.cursor.0 as usize, self.cursor.1 as usize, Cell::Text { ch: '+', style: cursor_style });
        match self.panel {
            Panel::Help(offset) | Panel::Log(offset) => {
                let (title, lines) = self.panel_content();
                self.draw_panel(&mut canvas, title, &lines, offset);
            }
            Panel::None => {}
        }

        let rows: Vec<String> = canvas.cells.chunks(width).map(serialize).collect();
        let mut out = Vec::with_capacity(64 * 1024);
        out.extend_from_slice(b"\x1b[?2026h");
        self.gfx.deliver(&mut self.pending, &mut out);
        if self.screen.len() != height {
            out.extend_from_slice(b"\x1b[0m\x1b[2J");
            self.screen = vec![None; height];
            self.status_line = None;
            self.footer = None;
        }
        // Row 1 is the status line; the page area starts at row 2.
        let mut changed = self.scroll_screen(&rows, &mut out);
        for (index, row) in rows.into_iter().enumerate() {
            if self.screen[index].as_deref() != Some(row.as_str()) {
                let _ = write!(out, "\x1b[{};1H", index + 2);
                out.extend_from_slice(row.as_bytes());
                self.screen[index] = Some(row);
                changed = true;
            }
        }
        if changed {
            let now = Instant::now();
            self.frames.push_back(now);
            while self.frames.front().is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(1)) {
                self.frames.pop_front();
            }
        }
        let status = self.status_text();
        if self.status_line.as_deref() != Some(status.as_str()) {
            let mut line = String::new();
            for (text, style) in self.status_segments() {
                sgr(&mut line, style);
                line.push_str(&text);
            }
            line.push_str("\x1b[0m");
            out.extend_from_slice(b"\x1b[1;1H");
            out.extend_from_slice(line.as_bytes());
            self.status_line = Some(status);
        }
        self.draw_footer(&mut out, height);
        self.evict(&visible);
        self.gfx.deliver(&mut self.pending, &mut out);
        out.extend_from_slice(b"\x1b[?2026l");
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(&out);
        let _ = stdout.flush();
        self.last_view = self.doc().map(|d| (self.view_signature(), d.view.0, d.view.1));
    }

    /// Everything other than the vertical offset that affects row contents.
    fn view_signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        if let Some(doc) = self.doc() {
            (doc.id, doc.version, doc.zoom, doc.page, doc.view.0).hash(&mut hasher);
        }
        (self.mode == Mode::Single, self.term, self.cell).hash(&mut hasher);
        hasher.finish()
    }

    /// If the view only moved vertically, scroll the terminal region instead
    /// of rewriting rows that merely shifted.
    /// Returns whether it scrolled.
    fn scroll_screen(&mut self, rows: &[String], out: &mut Vec<u8>) -> bool {
        let Some((signature, _, last_y)) = self.last_view else { return false };
        let Some(y) = self.doc().map(|d| d.view.1) else { return false };
        let height = rows.len() as i64;
        let dy = y - last_y;
        if dy == 0 || dy.abs() >= height || signature != self.view_signature() {
            return false;
        }
        let shifted = |i: i64| -> Option<&String> { self.screen.get((i + dy) as usize).and_then(|r| r.as_ref()) };
        let reused = (0..height).filter(|&i| shifted(i).is_some_and(|r| *r == rows[i as usize])).count();
        if reused * 2 < rows.len() {
            return false;
        }
        let n = dy.unsigned_abs() as usize;
        let _ = write!(out, "\x1b[0m\x1b[2;{}r", height + 1);
        if dy > 0 {
            let _ = write!(out, "\x1b[{n}S");
            self.screen.drain(..n);
            self.screen.extend(std::iter::repeat_n(None, n));
        } else {
            let _ = write!(out, "\x1b[{n}T");
            self.screen.truncate(rows.len() - n);
            self.screen.splice(0..0, std::iter::repeat_n(None, n));
        }
        out.extend_from_slice(b"\x1b[r");
        true
    }

    /// The three newest log lines under the page, newest first.
    fn draw_footer(&mut self, out: &mut Vec<u8>, height: usize) {
        let rows = self.debug_rows() as usize;
        if rows == 0 {
            return;
        }
        let width = self.term.0 as usize;
        let lines: Vec<String> = (0..rows)
            .map(|i| {
                let text = self.log.get(i).map_or("—", String::as_str);
                let mut line: String = format!(" {text}").chars().take(width).collect();
                let len = line.chars().count();
                line.push_str(&" ".repeat(width - len));
                line
            })
            .collect();
        let joined = lines.join("\n");
        if self.footer.as_deref() == Some(joined.as_str()) {
            return;
        }
        for (i, line) in lines.iter().enumerate() {
            let mut row = String::new();
            sgr(&mut row, FOOTER);
            row.push_str(line);
            row.push_str("\x1b[0m");
            let _ = write!(out, "\x1b[{};1H", height + 2 + i);
            out.extend_from_slice(row.as_bytes());
        }
        self.footer = Some(joined);
    }

    /// Page-area frames written during the last second.
    fn fps(&self) -> usize {
        let now = Instant::now();
        self.frames.iter().filter(|t| now.duration_since(**t) < Duration::from_secs(1)).count()
    }

    /// The status line as styled segments, exactly as wide as the terminal.
    fn status_segments(&self) -> Vec<(String, Style)> {
        let sep = || (" │ ".to_string(), STATUS_DIM);
        let mut left: Vec<(String, Style)> = vec![(" ".into(), STATUS)];
        match (&self.prompt, self.doc()) {
            (Some(prompt), _) => {
                left.push((format!(":{prompt}"), STATUS_COUNT));
                left.push(sep());
            }
            (None, Some(doc)) => {
                let name = doc.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                left.push((name, STATUS_NAME));
                if self.docs.len() > 1 {
                    left.push((format!(" ({}/{})", self.current.unwrap_or(0) + 1, self.docs.len()), STATUS_DIM));
                }
                let mode = if self.mode == Mode::Single { "single" } else { "scroll" };
                let (ready, wanted) = self.progress;
                let progress = if ready < wanted || self.pool.is_busy() {
                    (format!("rendering {ready}/{wanted}"), STATUS_BUSY)
                } else {
                    (format!("ready {ready}/{wanted}"), STATUS_READY)
                };
                for field in [
                    (format!("{}/{}", self.current_page() + 1, doc.sizes.len()), STATUS),
                    (format!("{}%", doc.zoom), STATUS),
                    (mode.to_string(), STATUS),
                    (format!("{} fps", self.fps()), STATUS),
                    progress,
                ] {
                    left.push(sep());
                    left.push(field);
                }
                left.push(sep());
            }
            (None, None) => {
                left.push(("pdfty".into(), STATUS_NAME));
                left.push(sep());
            }
        }
        let mut right: Vec<(String, Style)> = Vec::new();
        if !self.count.is_empty() {
            right.push((format!("{}  ", self.count), STATUS_COUNT));
        }
        right.push(("nvim ".into(), STATUS));
        right.push(if self.nvim.is_some() { ("●".into(), STATUS_READY) } else { ("○".into(), STATUS_DIM) });
        right.push(("  ? help ".into(), STATUS_DIM));

        let width = self.term.0 as usize;
        let len = |segments: &[(String, Style)]| segments.iter().map(|(t, _)| t.chars().count()).sum::<usize>();
        let (left_len, right_len) = (len(&left), len(&right));
        let room = width.saturating_sub(left_len + right_len + 1);
        let message: String = self.message.0.chars().take(room).collect();
        let pad = width.saturating_sub(left_len + message.chars().count() + right_len);
        let mut segments = left;
        segments.push((message, if self.message.1 { STATUS_ERROR } else { STATUS }));
        segments.push((" ".repeat(pad), STATUS));
        segments.extend(right);
        // Never wider than the terminal.
        let mut remaining = width;
        segments
            .into_iter()
            .map(|(text, style)| {
                let text: String = text.chars().take(remaining).collect();
                remaining -= text.chars().count();
                (text, style)
            })
            .filter(|(text, _)| !text.is_empty())
            .collect()
    }

    /// The status line as plain text (to detect changes).
    pub(super) fn status_text(&self) -> String {
        self.status_segments().into_iter().map(|(text, _)| text).collect()
    }

    fn draw_welcome(&self, canvas: &mut Canvas) {
        let mut lines = vec![
            ("pdfty".to_string(), BRIGHT),
            (String::new(), DIM),
            ("No PDF open.".to_string(), DIM),
            ("Run :PdftyAdd in Neovim, or start pdfty with a PDF path.".to_string(), DIM),
        ];
        if let Some(path) = &self.control_path {
            lines.push((String::new(), DIM));
            lines.push((format!("control socket: {}", path.display()), DIM));
        }
        let top = canvas.height.saturating_sub(lines.len()) / 2;
        for (i, (text, style)) in lines.iter().enumerate() {
            let x = canvas.width.saturating_sub(text.chars().count()) / 2;
            canvas.text(x, top + i, text, *style);
        }
    }

    /// Panel width: help reads best narrow; the log uses the whole width so
    /// long messages (paths, commands) need fewer wrapped lines.
    fn panel_width(&self, canvas_width: usize) -> usize {
        let full = canvas_width.saturating_sub(4);
        match self.panel {
            Panel::Log(_) => full,
            _ => full.min(80),
        }
    }

    /// Title and lines of the open panel, log entries wrapped to its width.
    pub(super) fn panel_content(&self) -> (&'static str, Vec<String>) {
        match self.panel {
            Panel::Log(_) => {
                let width = self.panel_width(self.view_size().0 as usize).saturating_sub(4);
                let lines = self
                    .log
                    .iter()
                    .flat_map(|entry| wrap(entry, width, LOG_INDENT))
                    .collect();
                ("messages  (newest first; j/k scroll, M or Esc closes)", lines)
            }
            _ => ("pdfty keys  (j/k scroll, ? or Esc closes)", HELP.iter().map(|s| s.to_string()).collect()),
        }
    }

    fn draw_panel(&self, canvas: &mut Canvas, title: &str, lines: &[String], offset: usize) {
        if canvas.width < 20 || canvas.height < 6 {
            return;
        }
        let w = self.panel_width(canvas.width);
        let visible = lines.len().min(canvas.height.saturating_sub(4)).max(1);
        let h = visible + 3;
        let (x, y) = ((canvas.width - w) / 2, (canvas.height.saturating_sub(h)) / 2);
        canvas.fill(x, y, w, h, PANEL);
        canvas.text(x + 2, y + 1, &title.chars().take(w - 4).collect::<String>(), PANEL_TITLE);
        for (i, line) in lines.iter().skip(offset).take(visible).enumerate() {
            let text: String = line.chars().take(w - 4).collect();
            canvas.text(x + 2, y + 2 + i, &text, PANEL);
        }
    }

    /// Largest scroll offset of the open panel.
    pub(super) fn panel_max_offset(&self) -> usize {
        let (_, lines) = self.panel_content();
        lines.len().saturating_sub(self.view_size().1.saturating_sub(4) as usize)
    }
}

/// Log lines start with "HH:MM:SS  "; continuation lines align after it.
const LOG_INDENT: usize = 10;

/// Wrap at spaces where possible (hard-break long paths), indenting
/// continuation lines by `indent`.
fn wrap(text: &str, width: usize, indent: usize) -> Vec<String> {
    let width = width.max(indent + 10);
    let chars: Vec<char> = text.chars().collect();
    let mut lines = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let first = lines.is_empty();
        let avail = if first { width } else { width - indent };
        let end = (start + avail).min(chars.len());
        let mut cut = end;
        if end < chars.len() {
            if let Some(space) = chars[start..end].iter().rposition(|&c| c == ' ') {
                if space > avail / 3 {
                    cut = start + space + 1;
                }
            }
        }
        let piece: String = chars[start..cut].iter().collect::<String>().trim_end().to_string();
        lines.push(if first { piece } else { format!("{}{piece}", " ".repeat(indent)) });
        start = cut;
        while start < chars.len() && chars[start] == ' ' {
            start += 1;
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub(super) const HELP: &[&str] = &[
    "SCROLLING",
    "j / k, ↓ / ↑, wheel   Scroll 3 rows (single view: next / previous page)",
    "Ctrl-d / Ctrl-u       Half a screen down / up",
    "Space, PgDn / PgUp    A screen down / up",
    "h / l, ← / →          Previous / next page",
    "g / G, Home / End     First / last page;  10g jumps to page 10",
    ":10 Enter, 10 Enter   Jump to page 10",
    "H / L, J / K          Pan left / right, down / up",
    "Shift+wheel           Pan horizontally",
    "",
    "VIEW",
    "= / + / -             Zoom in / out (keeps the point under the cursor)",
    "0                     Reset zoom",
    "t                     Toggle continuous / single-page view",
    "[ / ]                 Previous / next PDF",
    "r                     Reload (also automatic when the PDF changes)",
    "c                     Center the point under the cursor",
    "",
    "CURSOR, TEXT AND SOURCE",
    "w / a / s / d         Move the cursor (scrolls at the edges)",
    "Left click            Move the cursor;  drag to select text",
    "v                     Start a selection at the cursor; move to extend",
    "y                     Copy the selection",
    "i, Enter, right click Inverse search: open the source in Neovim",
    "I, Ctrl+click         Inverse search and focus Neovim",
    "Esc                   Clear selection, highlight and pending count",
    "",
    "Counts repeat movements: 5j, 3l, 2=",
    "D                     Show / hide the debug lines under the page",
    "M                     Message log (full messages, wrapped)",
    "?                     This help",
    "q                     Quit",
];

#[cfg(test)]
mod tests {
    use super::wrap;

    #[test]
    fn wrap_keeps_everything_and_indents_continuations() {
        let text = "12:00:00  inverse page 2 (100.0, 200.0) → /Users/someone/a/very/long/path/to/the/document/source.tex:96";
        let lines = wrap(text, 40, 10);
        assert!(lines.len() > 2);
        assert!(lines.iter().all(|l| l.chars().count() <= 40));
        assert!(lines[1..].iter().all(|l| l.starts_with("          ")));
        let rejoined: String = lines.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
        assert_eq!(rejoined.replace(' ', ""), text.replace(' ', ""));
        assert_eq!(wrap("short", 40, 10), vec!["short"]);
    }
}

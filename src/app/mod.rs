//! Viewer state and the event loop.
//!
//! One thread owns all state. Terminal input, IPC, render results and a
//! periodic tick all arrive on a single channel; after draining it the loop
//! reschedules render work and draws at most one frame.

mod draw;
mod input;
mod search;
mod view;

use crate::{
    ipc::{self, Message, Socket},
    kitty::{Graphics, IdAllocator, Payload, Transfer},
    layout::{self, Layout, Mode},
    render::{self, DocRef, Job, Output, PageSize, Pool, TileBase, TileKey},
    synctex::Synctex,
    terminal,
    text::Line,
    watch::Watch,
};
use std::{
    collections::{HashMap, VecDeque},
    io::Write,
    path::PathBuf,
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};

/// Pixel bytes we allow Kitty to hold for us (its quota is 320 MB).
const IMAGE_BUDGET: u64 = 200 * 1024 * 1024;
pub const ZOOM_RANGE: (u32, u32) = (25, 500);
const DEBUG_ROWS: u16 = 3;

/// Wall-clock time as HH:MM:SS in the local time zone.
fn local_time() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
        return "--:--:--".into();
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

pub enum Event {
    Input(crossterm::event::Event),
    Ipc(Message),
    Render(Output),
    Synctex { doc: u64, version: u64, result: Result<Arc<Synctex>, String> },
    Note { text: String, error: bool },
    Tick,
}

struct Doc {
    id: u64,
    path: Arc<PathBuf>,
    version: u64,
    sizes: Vec<PageSize>,
    watch: Watch,
    _socket: Option<Socket>,
    zoom: u32,
    /// Viewport origin on the canvas, in cells.
    view: (i64, i64),
    /// Page shown in single view.
    page: usize,
    text: HashMap<usize, Arc<Vec<Line>>>,
    synctex: Option<Arc<Synctex>>,
}

impl Doc {
    fn reference(&self) -> DocRef {
        DocRef { id: self.id, version: self.version, path: Arc::clone(&self.path) }
    }
}

/// A selection between two page-space points; resolved to characters once
/// the page's text is available.
#[derive(Clone, Copy)]
struct Selection {
    doc: u64,
    page: usize,
    anchor: (f64, f64),
    head: (f64, f64),
}

/// A SyncTeX target; its paragraph is outlined.
#[derive(Clone, Copy)]
struct Marker {
    doc: u64,
    page: usize,
    x: f64,
    y: f64,
}

struct Uploaded {
    id: u32,
    bytes: u64,
    used: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    None,
    /// Scroll offsets in (wrapped) lines.
    Help(usize),
    Log(usize),
}

pub struct App {
    tx: mpsc::Sender<Event>,
    pool: Pool,
    gfx: Graphics,
    ids: IdAllocator,
    docs: Vec<Doc>,
    current: Option<usize>,
    next_doc_id: u64,
    mode: Mode,
    term: (u16, u16),
    cell: (u32, u32),
    /// Cursor in viewport cells.
    cursor: (u16, u16),
    selection: Option<Selection>,
    drag_from: Option<(u16, u16)>,
    marker: Option<Marker>,
    tiles: HashMap<TileKey, Uploaded>,
    latest: HashMap<TileBase, TileKey>,
    tile_bytes: u64,
    frame: u64,
    /// Graphics commands waiting for the next frame.
    pending: Vec<u8>,
    screen: Vec<Option<String>>,
    status_line: Option<String>,
    /// The debug lines last written below the page.
    footer: Option<String>,
    /// Show the three newest log lines below the page (toggled with `D`).
    debug_lines: bool,
    /// When frames that changed the page area were written (for the FPS counter).
    frames: VecDeque<Instant>,
    /// Tiles ready / tiles wanted around the current view.
    progress: (usize, usize),
    last_view: Option<(u64, i64, i64)>,
    message: (String, bool),
    log: VecDeque<String>,
    count: String,
    prompt: Option<String>,
    panel: Panel,
    nvim: Option<String>,
    synctex_enabled: bool,
    control_path: Option<PathBuf>,
    _control: Option<Socket>,
    temp_prefix: PathBuf,
    temp_files: VecDeque<(Instant, PathBuf)>,
    /// Images to delete once the terminal surely shows a newer frame. When
    /// Neovim or tmux redraw the screen they lag behind our output a little.
    doomed: VecDeque<(Instant, u32)>,
    dirty: bool,
    quit: bool,
}

pub fn run(pdfs: Vec<PathBuf>, synctex_enabled: bool, debug_lines: bool) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    let temp_prefix = std::env::temp_dir().join(format!("pdfty-{}-tty-graphics-protocol-", std::process::id()));
    let sink = tx.clone();
    let pool = Pool::new(Transfer::detect(), temp_prefix.clone(), move |output| {
        sink.send(Event::Render(output)).is_ok()
    });
    let control_path = synctex_enabled.then(ipc::control_socket_path);
    let control = match &control_path {
        Some(path) => {
            let tx = tx.clone();
            Some(Socket::start(path.clone(), None, move |m| tx.send(Event::Ipc(m)).is_ok())?)
        }
        None => None,
    };
    // Inside the pdfty panel, image commands go through the host Neovim
    // (nvim_ui_send) when the plugin says it supports that.
    let editor = std::env::var("NVIM")
        .ok()
        .filter(|s| !s.is_empty() && std::env::var("PDFTY_NVIM_UI").as_deref() == Ok("1"))
        .and_then(|server| crate::nvim::UiSender::connect(&server).ok())
        .map(Arc::new);
    let gfx = Graphics::new(terminal::in_tmux(), terminal::outer_tty().map(Arc::new)).with_editor(editor);
    let mut app = App {
        tx: tx.clone(),
        pool,
        gfx: gfx.clone(),
        ids: IdAllocator::new(),
        docs: Vec::new(),
        current: None,
        next_doc_id: 1,
        mode: Mode::Continuous,
        term: terminal::size(),
        cell: terminal::cell_pixels(gfx.tty()),
        cursor: (0, 0),
        selection: None,
        drag_from: None,
        marker: None,
        tiles: HashMap::new(),
        latest: HashMap::new(),
        tile_bytes: 0,
        frame: 0,
        pending: Vec::new(),
        screen: Vec::new(),
        status_line: None,
        footer: None,
        debug_lines,
        frames: VecDeque::new(),
        progress: (0, 0),
        last_view: None,
        message: (String::new(), false),
        log: VecDeque::new(),
        count: String::new(),
        prompt: None,
        panel: Panel::None,
        // Inside a Neovim terminal, $NVIM is the editor that hosts us.
        nvim: std::env::var("NVIM").ok().filter(|s| !s.is_empty()),
        doomed: VecDeque::new(),
        synctex_enabled,
        control_path,
        _control: control,
        temp_prefix,
        temp_files: VecDeque::new(),
        dirty: true,
        quit: false,
    };
    for pdf in pdfs {
        app.add_document(pdf)?;
    }
    if let Some(first) = app.docs.first().map(|d| d.path.as_ref().clone()) {
        app.switch_to(&first);
    }
    let _guard = terminal::Guard::enter()?;
    app.cursor = (app.view_size().0 / 2, app.view_size().1 / 2);

    let input = tx.clone();
    thread::spawn(move || {
        while let Ok(event) = crossterm::event::read() {
            if input.send(Event::Input(event)).is_err() {
                break;
            }
        }
    });
    let tick = tx.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(200));
        if tick.send(Event::Tick).is_err() {
            break;
        }
    });
    drop(tx);

    app.schedule();
    app.draw();
    while let Ok(event) = rx.recv() {
        app.handle(event);
        // Coalesce bursts (wheel scrolling, many finished tiles) into one frame.
        let deadline = Instant::now() + Duration::from_millis(4);
        while Instant::now() < deadline {
            match rx.try_recv() {
                Ok(event) => app.handle(event),
                Err(_) => break,
            }
        }
        if app.quit {
            break;
        }
        if app.dirty {
            app.schedule();
            app.draw();
        }
    }
    app.shutdown();
    Ok(())
}

impl App {
    fn handle(&mut self, event: Event) {
        match event {
            Event::Input(event) => self.input(event),
            Event::Ipc(message) => self.message(message),
            Event::Render(output) => self.receive(output),
            Event::Synctex { doc, version, result } => {
                if let Some(d) = self.docs.iter_mut().find(|d| d.id == doc && d.version == version) {
                    match result {
                        Ok(synctex) => d.synctex = Some(synctex),
                        Err(error) => self.log(format!("SyncTeX: {error}")),
                    }
                }
            }
            Event::Note { text, error } => self.note(text, error),
            Event::Tick => self.tick(),
        }
    }

    fn tick(&mut self) {
        let changed: Vec<usize> = self
            .docs
            .iter_mut()
            .enumerate()
            .filter_map(|(i, d)| d.watch.changed(&d.path).then_some(i))
            .collect();
        for index in changed {
            self.reload(index);
        }
        self.delete_doomed(Duration::from_millis(400));
        // Kitty deletes transferred files itself; this only catches files it
        // never received (for example while tmux was detached).
        while let Some((time, _)) = self.temp_files.front() {
            if time.elapsed() < Duration::from_secs(10) {
                break;
            }
            let (_, path) = self.temp_files.pop_front().unwrap();
            let _ = std::fs::remove_file(path);
        }
        // Keep the busy indicator in the status line current.
        let line = self.status_text();
        if self.status_line.as_deref() != Some(line.as_str()) {
            self.dirty = true;
        }
    }

    pub(super) fn note(&mut self, text: String, error: bool) {
        self.log(text.clone());
        self.message = (text, error);
        self.dirty = true;
    }

    pub(super) fn log(&mut self, text: String) {
        self.log.push_front(format!("{}  {text}", local_time()));
        self.log.truncate(200);
        if matches!(self.panel, Panel::Log(_)) || self.debug_lines {
            self.dirty = true;
        }
    }

    /// Rows under the page used by the debug lines.
    pub(super) fn debug_rows(&self) -> u16 {
        if self.debug_lines && self.term.1 >= 8 {
            DEBUG_ROWS
        } else {
            0
        }
    }

    fn doc(&self) -> Option<&Doc> {
        self.current.map(|i| &self.docs[i])
    }

    fn doc_mut(&mut self) -> Option<&mut Doc> {
        self.current.map(|i| &mut self.docs[i])
    }

    /// Document viewport: between the status line (top) and the debug lines.
    pub(super) fn view_size(&self) -> (u16, u16) {
        (self.term.0.max(1), self.term.1.saturating_sub(1 + self.debug_rows()).max(1))
    }

    pub(super) fn layout(&self) -> Layout {
        match self.doc() {
            Some(doc) => self.layout_for(doc, doc.page),
            None => Layout::default(),
        }
    }

    fn layout_for(&self, doc: &Doc, page: usize) -> Layout {
        layout::compute(
            &doc.sizes,
            &layout::Params { mode: self.mode, zoom: doc.zoom, view: self.view_size(), cell: self.cell, page },
        )
    }

    // ----- documents ---------------------------------------------------------

    pub(super) fn add_document(&mut self, pdf: PathBuf) -> Result<(), String> {
        if self.docs.iter().any(|d| *d.path == pdf) {
            return Ok(());
        }
        let sizes = render::page_sizes(&pdf).map_err(|e| format!("{}: {e}", pdf.display()))?;
        let socket = if self.synctex_enabled {
            let tx = self.tx.clone();
            Some(Socket::start(ipc::socket_path(&pdf), Some(pdf.clone()), move |m| {
                tx.send(Event::Ipc(m)).is_ok()
            })?)
        } else {
            None
        };
        let doc = Doc {
            id: self.next_doc_id,
            watch: Watch::new(&pdf),
            path: Arc::new(pdf),
            version: 1,
            sizes,
            _socket: socket,
            zoom: 100,
            view: (0, 0),
            page: 0,
            text: HashMap::new(),
            synctex: None,
        };
        self.next_doc_id += 1;
        self.warm_synctex(&doc);
        self.docs.push(doc);
        Ok(())
    }

    fn warm_synctex(&self, doc: &Doc) {
        if !self.synctex_enabled {
            return;
        }
        let (id, version, path, tx) = (doc.id, doc.version, Arc::clone(&doc.path), self.tx.clone());
        thread::spawn(move || {
            let result = Synctex::load(&path).map(Arc::new);
            let _ = tx.send(Event::Synctex { doc: id, version, result });
        });
    }

    pub(super) fn switch_to(&mut self, pdf: &std::path::Path) {
        let Some(index) = self.docs.iter().position(|d| *d.path == pdf) else { return };
        if self.current == Some(index) {
            return;
        }
        self.current = Some(index);
        self.selection = None;
        self.dirty = true;
    }

    pub(super) fn switch_relative(&mut self, delta: isize) {
        if self.docs.len() < 2 {
            return;
        }
        let current = self.current.unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(self.docs.len() as isize) as usize;
        let path = self.docs[next].path.as_ref().clone();
        self.switch_to(&path);
    }

    pub(super) fn reload_current(&mut self) {
        if let Some(index) = self.current {
            self.reload(index);
        }
    }

    fn reload(&mut self, index: usize) {
        let path = Arc::clone(&self.docs[index].path);
        let sizes = match render::page_sizes(&path) {
            Ok(sizes) if !sizes.is_empty() => sizes,
            Ok(_) => return self.note("reload: PDF has no pages".into(), true),
            Err(error) => return self.note(format!("reload: {error}"), true),
        };
        let doc = &mut self.docs[index];
        doc.version += 1;
        doc.sizes = sizes;
        doc.page = doc.page.min(doc.sizes.len() - 1);
        doc.text.clear();
        doc.synctex = None;
        doc.watch = Watch::new(&path);
        let id = doc.id;
        if self.marker.is_some_and(|m| m.doc == id) {
            self.marker = None;
        }
        if self.selection.is_some_and(|s| s.doc == id) {
            self.selection = None;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        self.log(format!("reloaded {name}"));
        self.warm_synctex(&self.docs[index]);
        if self.current == Some(index) {
            self.keep_view_in_bounds();
        }
        self.dirty = true;
    }

    // ----- render results and scheduling -------------------------------------

    fn receive(&mut self, output: Output) {
        match output {
            Output::Tile { key, result } => {
                let image = match result {
                    Ok(image) => image,
                    Err(error) => return self.note(format!("render page {}: {error}", key.base.page + 1), true),
                };
                let current = self.docs.iter().any(|d| d.id == key.base.doc && d.version == key.version);
                if !current || self.tiles.contains_key(&key) {
                    if let Payload::File(path) = image.payload {
                        let _ = std::fs::remove_file(path);
                    }
                    return;
                }
                let id = self.ids.next();
                self.gfx.upload(&mut self.pending, id, image.pixels, image.cells, &image.payload);
                if let Payload::File(path) = image.payload {
                    self.temp_files.push_back((Instant::now(), path));
                }
                let bytes = image.pixels.0 as u64 * image.pixels.1 as u64 * 4;
                self.tile_bytes += bytes;
                self.tiles.insert(key, Uploaded { id, bytes, used: self.frame });
                self.latest.insert(key.base, key);
                self.dirty = true;
            }
            Output::Text { doc, version, page, result } => {
                let Some(d) = self.docs.iter_mut().find(|d| d.id == doc && d.version == version) else {
                    return;
                };
                d.text.insert(page as usize, Arc::new(result.unwrap_or_default()));
                self.dirty = true;
            }
        }
    }

    /// Publish render work: visible tiles first (top to bottom), then text
    /// for overlays and selection, then the neighbourhood of the viewport.
    fn schedule(&mut self) {
        let Some(doc) = self.doc() else {
            self.pool.set_jobs(Vec::new());
            self.progress = (0, 0);
            return;
        };
        let (vw, vh) = self.view_size();
        let (vx, vy) = doc.view;
        let layout = self.layout();
        let mut jobs = Vec::new();
        let mut wanted = 0;
        let mut tiles_in = |layout: &Layout, x0: i64, y0: i64, x1: i64, y1: i64, jobs: &mut Vec<Job>| {
            for placed in &layout.pages {
                for (tx, ty) in view::tiles_in(placed, x0, y0, x1, y1) {
                    wanted += 1;
                    let key = self.tile_key(doc, placed, tx, ty);
                    if !self.tiles.contains_key(&key) {
                        let size = doc.sizes[placed.page];
                        let overlays = self.tile_overlays(doc, placed, tx, ty);
                        jobs.push(Job::Tile { doc: doc.reference(), key, size, overlays });
                    }
                }
            }
        };
        tiles_in(&layout, vx, vy, vx + vw as i64, vy + vh as i64, &mut jobs);
        let mut text_pages: Vec<usize> = Vec::new();
        text_pages.extend(self.selection.filter(|s| s.doc == doc.id).map(|s| s.page));
        text_pages.extend(self.marker.filter(|m| m.doc == doc.id).map(|m| m.page));
        for placed in &layout.pages {
            if placed.y < vy + vh as i64 && placed.y + placed.grid.rows as i64 > vy {
                text_pages.push(placed.page);
            }
        }
        for page in text_pages {
            if !doc.text.contains_key(&page) {
                jobs.push(Job::Text { doc: doc.reference(), page: page as u32 });
            }
        }
        // Prefetch one screen in each direction (or the neighbouring pages).
        match self.mode {
            Mode::Continuous => {
                let h = vh as i64;
                tiles_in(&layout, vx, vy + h, vx + vw as i64, vy + 2 * h, &mut jobs);
                tiles_in(&layout, vx, vy - h, vx + vw as i64, vy, &mut jobs);
            }
            Mode::Single => {
                for page in [doc.page + 1, doc.page.wrapping_sub(1)] {
                    if page < doc.sizes.len() {
                        let other = self.layout_for(doc, page);
                        if let Some(top) = other.page_top(page, vh) {
                            tiles_in(&other, vx, top, vx + vw as i64, top + vh as i64, &mut jobs);
                        }
                    }
                }
            }
        }
        let missing = jobs.iter().filter(|job| matches!(job, Job::Tile { .. })).count();
        self.progress = (wanted - missing, wanted);
        self.pool.set_jobs(jobs);
    }

    fn evict(&mut self, visible: &std::collections::HashSet<u32>) {
        // Tiles from older document versions are never shown again once off screen.
        let stale: Vec<TileKey> = self
            .tiles
            .iter()
            .filter(|(key, tile)| {
                !visible.contains(&tile.id)
                    && self
                        .docs
                        .iter()
                        .find(|d| d.id == key.base.doc)
                        .is_none_or(|d| d.version != key.version)
            })
            .map(|(key, _)| *key)
            .collect();
        for key in stale {
            self.drop_tile(key);
        }
        if self.tile_bytes <= IMAGE_BUDGET {
            return;
        }
        let mut candidates: Vec<(u64, TileKey)> = self
            .tiles
            .iter()
            .filter(|(_, tile)| !visible.contains(&tile.id))
            .map(|(key, tile)| (tile.used, *key))
            .collect();
        candidates.sort_by_key(|(used, _)| *used);
        for (_, key) in candidates {
            if self.tile_bytes <= IMAGE_BUDGET * 3 / 4 {
                break;
            }
            self.drop_tile(key);
        }
    }

    fn drop_tile(&mut self, key: TileKey) {
        if let Some(tile) = self.tiles.remove(&key) {
            self.doomed.push_back((Instant::now(), tile.id));
            self.tile_bytes -= tile.bytes;
            if self.latest.get(&key.base) == Some(&key) {
                self.latest.remove(&key.base);
            }
        }
    }

    fn delete_doomed(&mut self, age: Duration) {
        while self.doomed.front().is_some_and(|(time, _)| time.elapsed() >= age) {
            let (_, id) = self.doomed.pop_front().unwrap();
            self.gfx.delete(&mut self.pending, id);
            self.dirty = true;
        }
    }

    fn shutdown(&mut self) {
        self.pool.set_jobs(Vec::new());
        let keys: Vec<TileKey> = self.tiles.keys().copied().collect();
        for key in keys {
            self.drop_tile(key);
        }
        self.delete_doomed(Duration::ZERO);
        let mut bytes = Vec::new();
        self.gfx.deliver(&mut self.pending, &mut bytes);
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(&bytes);
        let _ = out.flush();
        for (_, path) in self.temp_files.drain(..) {
            let _ = std::fs::remove_file(path);
        }
        if let (Some(dir), Some(prefix)) = (self.temp_prefix.parent(), self.temp_prefix.file_name()) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                let prefix = prefix.to_string_lossy();
                for entry in entries.flatten() {
                    if entry.file_name().to_string_lossy().starts_with(prefix.as_ref()) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}

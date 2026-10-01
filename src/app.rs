//! Viewer state and the main event pump.
//! Domain modules operate on one App owned by this loop; worker threads only
//! return immutable results. Input/IPC stay responsive while PDF work runs.

mod cursor;
mod documents;
mod frames;
mod geometry;
mod help;
mod input;
mod integration;
mod navigation;
mod rendering;
mod selection;
mod status;
#[cfg(test)]
mod tests;

use documents::cleanup_old;
use input::InputOutcome;

use crate::{
    cache::{Cache, Key},
    image,
    ipc::{self, Message, Socket},
    reload::Watch,
    render::{self, Request},
    resolution::CellPixels,
    synctex, terminal, text,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    env, fs,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

/// Own the viewer state and coordinate terminal input, background rendering,
/// file reloads, and optional Neovim IPC on one main loop.
pub fn run(pdfs: Vec<PathBuf>, synctex_enabled: bool) -> Result<(), String> {
    let mut documents = Vec::new();
    for pdf in pdfs {
        if !documents.contains(&pdf) {
            documents.push(pdf);
        }
    }
    let pdf = documents.first().cloned().unwrap_or_default();
    let count = if pdf.as_os_str().is_empty() {
        0
    } else {
        render::page_count(&pdf)?
    };
    let dir = env::temp_dir().join(format!("pdfty-{}", std::process::id()));
    cleanup_old();
    fs::create_dir(&dir).map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel();
    let control_socket_path = synctex_enabled.then(ipc::control_socket_path);
    let _control_socket = if let Some(path) = &control_socket_path {
        Some(Socket::start(path.clone(), None, tx.clone())?)
    } else {
        None
    };
    let mut sockets = HashMap::new();
    if synctex_enabled {
        for document in &documents {
            let path = ipc::socket_path(document);
            sockets.insert(
                document.clone(),
                Socket::start(path, Some(document.clone()), tx.clone())?,
            );
        }
    }
    let socket_path = synctex_enabled
        .then(|| (!pdf.as_os_str().is_empty()).then(|| ipc::socket_path(&pdf)))
        .flatten()
        .or_else(|| control_socket_path.clone());
    // Rendering and scroll composition can block on subprocesses or PNG work;
    // their channels keep input handling independent of that work.
    let (requests, background, results, text_results) = render::worker();
    let (compose_requests, compose_results) = image::composition_worker();
    let _guard = terminal::Guard::enter()?;
    let mut app = App {
        pdf,
        count,
        documents,
        saved_documents: HashMap::new(),
        sockets,
        ipc_tx: tx,
        control_socket_path,
        page: 1,
        version: 1,
        generation: 0,
        dimensions: terminal::size(),
        cell_pixels: CellPixels::current(),
        cache: Cache::new(),
        dir,
        requests,
        background,
        results,
        text_results,
        compose_requests,
        compose_results,
        scroll_generation: 0,
        watch: Watch::new(&PathBuf::new()),
        status: String::new(),
        recent_calls: VecDeque::new(),
        displayed: None,
        marker: None,
        marker_image: None,
        marker_serial: 0,
        cursor: (0, 0),
        nvim: None,
        synctex_enabled,
        socket_path,
        page_input: String::new(),
        command_mode: false,
        command_input: String::new(),
        lines: Vec::new(),
        text_cache: HashMap::new(),
        base_image: None,
        selection: None,
        selection_serial: 0,
        prefetch_inflight: None,
        prefetch_failed: HashSet::new(),
        page_sizes: HashMap::new(),
        scroll_mode: false,
        scroll_offset: 0,
        scroll_serial: 0,
        scroll_files: Vec::new(),
        displayed_image_id: None,
        selection_image: None,
        zoom: 100,
        pan_x: 0,
        pan_y: 0,
        scroll_surface_initialized: false,
        scroll_dirty: false,
        scroll_last_draw: Instant::now(),
        single_surface_initialized: false,
        frame_rate: FrameRate::default(),
        help_open: false,
        help_offset: 0,
    };
    app.watch = Watch::new(&app.pdf);
    app.cursor = (app.view_width() / 2, app.view_height() / 2);
    terminal::clear();
    terminal::marker(app.cursor.0, app.cursor.1 + 1);
    if app.count > 0 {
        app.request();
    }
    let mut last_watch = Instant::now();
    let mut last_status_draw = Instant::now() - Duration::from_secs(1);
    loop {
        // Mouse rows include the top status bar. `cursor` and all page
        // rectangles use viewport rows beginning at zero, so subtract one at
        // input and add one only at the final Kitty/terminal placement.
        if let Some(event) = terminal::input() {
            match app.handle_input(event) {
                InputOutcome::Quit => break,
                InputOutcome::Skip => continue,
                InputOutcome::Handled => {}
            }
        }
        while let Ok(message) = rx.try_recv() {
            app.message(message);
        }
        app.receive_work();
        if last_watch.elapsed() >= Duration::from_millis(200) {
            last_watch = Instant::now();
            if app.count > 0 && app.watch.changed(&app.pdf) {
                app.reload();
            }
        }
        // Coalesce wheel events into at most one composition request per 16 ms.
        // The actual rate is bounded by PNG composition and Kitty transfer.
        if app.scroll_dirty && app.scroll_last_draw.elapsed() >= Duration::from_millis(16) {
            app.redraw();
            app.scroll_dirty = false;
            app.scroll_last_draw = Instant::now();
        }
        if app.help_open {
            continue;
        }
        // Repainting five terminal lines on every 35 ms input poll can flood
        // tmux while Kitty is transferring a page image.
        if last_status_draw.elapsed() < Duration::from_millis(250) {
            continue;
        }
        last_status_draw = Instant::now();
        app.draw_status();
    }
    terminal::clear();
    if let Some(id) = app.displayed_image_id {
        image::delete_image(id);
    }
    app.cache.clear();
    for state in app.saved_documents.values_mut() {
        state.cache.clear();
    }
    let _ = fs::remove_dir_all(&app.dir);
    Ok(())
}

/// State shared by single-page and continuous views. `version` invalidates
/// cached images after PDF reload; `generation` rejects obsolete visible-page
/// renders; `scroll_generation` rejects obsolete composed scroll frames.
struct App {
    pdf: PathBuf,
    count: u32,
    documents: Vec<PathBuf>,
    saved_documents: HashMap<PathBuf, SavedDocument>,
    sockets: HashMap<PathBuf, Socket>,
    ipc_tx: mpsc::Sender<Message>,
    control_socket_path: Option<PathBuf>,
    page: u32,
    version: u64,
    generation: u64,
    dimensions: (u16, u16),
    cell_pixels: CellPixels,
    cache: Cache,
    dir: PathBuf,
    requests: mpsc::Sender<Request>,
    background: mpsc::Sender<Request>,
    results: mpsc::Receiver<render::ResultPage>,
    text_results: mpsc::Receiver<render::TextResult>,
    compose_requests: mpsc::Sender<image::ComposeRequest>,
    compose_results: mpsc::Receiver<image::ComposeResult>,
    // A composition may finish after input; only this generation can be displayed.
    scroll_generation: u64,
    watch: Watch,
    status: String,
    recent_calls: VecDeque<String>,
    displayed: Option<(f64, f64)>,
    marker: Option<synctex::Position>,
    marker_image: Option<PathBuf>,
    marker_serial: u64,
    // Viewport cells, excluding the top status row; never PDF point units.
    cursor: (u16, u16),
    nvim: Option<String>,
    synctex_enabled: bool,
    socket_path: Option<PathBuf>,
    page_input: String,
    command_mode: bool,
    command_input: String,
    lines: Vec<text::Line>,
    text_cache: HashMap<u32, Vec<text::Line>>,
    // Clean cached pixels are retained separately from temporary highlighted PNGs.
    base_image: Option<PathBuf>,
    selection: Option<(usize, usize)>,
    selection_serial: u64,
    prefetch_inflight: Option<Key>,
    prefetch_failed: HashSet<u32>,
    page_sizes: HashMap<u32, (f64, f64)>,
    scroll_mode: bool,
    scroll_offset: i64,
    scroll_serial: u64,
    scroll_files: Vec<PathBuf>,
    displayed_image_id: Option<u32>,
    selection_image: Option<PathBuf>,
    zoom: u16,
    // Signed cell offsets permit centering near document edges with blank margins.
    pan_x: i64,
    pan_y: i64,
    scroll_surface_initialized: bool,
    scroll_dirty: bool,
    scroll_last_draw: Instant,
    single_surface_initialized: bool,
    frame_rate: FrameRate,
    help_open: bool,
    help_offset: usize,
}

struct SavedDocument {
    count: u32,
    page: u32,
    version: u64,
    zoom: u16,
    cache: Cache,
    text_cache: HashMap<u32, Vec<text::Line>>,
    page_sizes: HashMap<u32, (f64, f64)>,
    prefetch_failed: HashSet<u32>,
    watch: Watch,
}

#[derive(Default)]
/// Timestamps of successful image placements during the last second.
struct FrameRate {
    recent: VecDeque<Instant>,
}

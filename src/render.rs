//! In-process MuPDF rendering on a pool of worker threads.
//!
//! Pages are rendered as cell-aligned tiles at the terminal's native pixel
//! density. The main thread publishes a prioritized list of wanted jobs after
//! every view change; workers always take the most urgent job, so visible
//! tiles never wait behind prefetching.

use crate::{
    kitty::{Payload, Transfer},
    text::{Glyph, Line, Rect},
};
use mupdf::{Colorspace, Device, DisplayList, Document, Matrix, Pixmap, TextPageFlags};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
};

/// Tile size in cells. Kitty placeholders address at most 297 rows/columns.
pub const TILE_COLS: u32 = 128;
pub const TILE_ROWS: u32 = 24;

/// Page size in PDF points, with the page-space origin used by MuPDF.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageSize {
    pub x0: f64,
    pub y0: f64,
    pub w: f64,
    pub h: f64,
}

/// How a page maps onto terminal cells: `cols`×`rows` cells of `cw`×`ch`
/// pixels, with `scale` pixels per PDF point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Grid {
    pub cols: u32,
    pub rows: u32,
    pub cw: u32,
    pub ch: u32,
    scale_bits: u32,
}

impl Grid {
    pub fn new(cols: u32, rows: u32, cw: u32, ch: u32, scale: f32) -> Self {
        Self { cols, rows, cw, ch, scale_bits: scale.to_bits() }
    }
    pub fn scale(&self) -> f64 {
        f32::from_bits(self.scale_bits) as f64
    }
    #[cfg(test)]
    pub fn tiles_x(&self) -> u32 {
        self.cols.div_ceil(TILE_COLS)
    }
    #[cfg(test)]
    pub fn tiles_y(&self) -> u32 {
        self.rows.div_ceil(TILE_ROWS)
    }
    /// Cell rectangle (col, row, cols, rows) covered by a tile.
    pub fn tile_cells(&self, tx: u32, ty: u32) -> (u32, u32, u32, u32) {
        let c0 = tx * TILE_COLS;
        let r0 = ty * TILE_ROWS;
        (c0, r0, (self.cols - c0).min(TILE_COLS), (self.rows - r0).min(TILE_ROWS))
    }
    /// Page-space rectangle (points) covered by a tile.
    pub fn tile_rect(&self, size: &PageSize, tx: u32, ty: u32) -> Rect {
        let (c0, r0, c, r) = self.tile_cells(tx, ty);
        let s = self.scale();
        Rect::new(
            size.x0 + (c0 * self.cw) as f64 / s,
            size.y0 + (r0 * self.ch) as f64 / s,
            size.x0 + ((c0 + c) * self.cw) as f64 / s,
            size.y0 + ((r0 + r) * self.ch) as f64 / s,
        )
    }
}

/// Identifies a tile position independent of document version and overlays;
/// used to keep showing an older image while its replacement renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileBase {
    pub doc: u64,
    pub page: u32,
    pub grid: Grid,
    pub tx: u32,
    pub ty: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub base: TileBase,
    pub version: u64,
    /// Hash of the overlays drawn into this tile (0 when clean).
    pub overlay: u64,
}

/// Decorations drawn into page pixels, in page-space points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Overlay {
    Highlight(Rect),
    Outline(Rect),
}

impl Overlay {
    pub fn bounds(&self) -> Rect {
        match self {
            Overlay::Highlight(r) => *r,
            Overlay::Outline(r) => r.expand(6.0),
        }
    }
}

#[derive(Clone)]
pub struct DocRef {
    pub id: u64,
    pub version: u64,
    pub path: Arc<PathBuf>,
}

pub enum Job {
    Tile { doc: DocRef, key: TileKey, size: PageSize, overlays: Vec<Overlay> },
    Text { doc: DocRef, page: u32 },
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum JobId {
    Tile(TileKey),
    Text(u64, u64, u32),
}

impl Job {
    fn id(&self) -> JobId {
        match self {
            Job::Tile { key, .. } => JobId::Tile(*key),
            Job::Text { doc, page } => JobId::Text(doc.id, doc.version, *page),
        }
    }
}

pub struct TileImage {
    pub pixels: (u32, u32),
    pub cells: (u32, u32),
    pub payload: Payload,
}

pub enum Output {
    Tile { key: TileKey, result: Result<TileImage, String> },
    Text { doc: u64, version: u64, page: u32, result: Result<Vec<Line>, String> },
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    running: HashSet<JobId>,
    stop: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    lists: Mutex<ListCache>,
    transfer: Transfer,
    temp_prefix: PathBuf,
    serial: AtomicU64,
}

/// Display lists shared by all workers so each page is interpreted once.
#[derive(Default)]
struct ListCache {
    entries: HashMap<(u64, u64, u32), (u64, Arc<DisplayList>)>,
    clock: u64,
}

pub struct Pool {
    shared: Arc<Shared>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl Pool {
    pub fn new(
        transfer: Transfer,
        temp_prefix: PathBuf,
        sink: impl Fn(Output) -> bool + Send + Sync + 'static,
    ) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            ready: Condvar::new(),
            lists: Mutex::new(ListCache::default()),
            transfer,
            temp_prefix,
            serial: AtomicU64::new(0),
        });
        let sink = Arc::new(sink);
        let threads = thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 8);
        let workers = (0..threads)
            .map(|_| {
                let shared = Arc::clone(&shared);
                let sink = Arc::clone(&sink);
                thread::spawn(move || Worker::default().run(&shared, &*sink))
            })
            .collect();
        Self { shared, workers }
    }

    /// Replace the pending queue with `jobs` (most urgent first). Jobs already
    /// running are not duplicated.
    pub fn set_jobs(&self, jobs: Vec<Job>) {
        let mut queue = self.shared.queue.lock().unwrap();
        let running = &queue.running;
        let mut seen = HashSet::new();
        let jobs: VecDeque<Job> = jobs
            .into_iter()
            .filter(|job| {
                let id = job.id();
                !running.contains(&id) && seen.insert(id)
            })
            .collect();
        queue.jobs = jobs;
        drop(queue);
        self.shared.ready.notify_all();
    }

    pub fn is_busy(&self) -> bool {
        let queue = self.shared.queue.lock().unwrap();
        !queue.jobs.is_empty() || !queue.running.is_empty()
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.shared.queue.lock().unwrap().stop = true;
        self.shared.ready.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct Worker {
    /// Open documents keyed by (doc id, version). MuPDF documents are
    /// thread-affine, so every worker opens its own handle.
    docs: HashMap<(u64, u64), Document>,
}

impl Worker {
    fn run(mut self, shared: &Shared, sink: &(dyn Fn(Output) -> bool + Send + Sync)) {
        loop {
            let job = {
                let mut queue = shared.queue.lock().unwrap();
                loop {
                    if queue.stop {
                        return;
                    }
                    if let Some(job) = queue.jobs.pop_front() {
                        queue.running.insert(job.id());
                        break job;
                    }
                    queue = shared.ready.wait(queue).unwrap();
                }
            };
            let id = job.id();
            let output = match job {
                Job::Tile { doc, key, size, overlays } => Output::Tile {
                    key,
                    result: self.tile(shared, &doc, &key, &size, &overlays),
                },
                Job::Text { doc, page } => Output::Text {
                    doc: doc.id,
                    version: doc.version,
                    page,
                    result: self.text(shared, &doc, page),
                },
            };
            shared.queue.lock().unwrap().running.remove(&id);
            if !sink(output) {
                return;
            }
        }
    }

    fn document(&mut self, doc: &DocRef) -> Result<&Document, String> {
        let key = (doc.id, doc.version);
        if !self.docs.contains_key(&key) {
            // Drop older versions of this document; they are never needed again.
            self.docs.retain(|(id, version), _| *id != doc.id || *version > doc.version);
            let path = doc.path.to_str().ok_or("PDF path is not valid UTF-8")?;
            let opened = Document::open(path).map_err(|e| e.to_string())?;
            self.docs.insert(key, opened);
        }
        Ok(&self.docs[&key])
    }

    fn list(&mut self, shared: &Shared, doc: &DocRef, page: u32) -> Result<Arc<DisplayList>, String> {
        let key = (doc.id, doc.version, page);
        {
            let mut cache = shared.lists.lock().unwrap();
            cache.clock += 1;
            let clock = cache.clock;
            if let Some(entry) = cache.entries.get_mut(&key) {
                entry.0 = clock;
                return Ok(Arc::clone(&entry.1));
            }
        }
        let document = self.document(doc)?;
        let page_obj = document.load_page(page as i32).map_err(|e| e.to_string())?;
        let list = Arc::new(page_obj.to_display_list(true).map_err(|e| e.to_string())?);
        let mut cache = shared.lists.lock().unwrap();
        cache.clock += 1;
        let clock = cache.clock;
        cache.entries.insert(key, (clock, Arc::clone(&list)));
        cache.entries.retain(|(id, version, _), _| *id != doc.id || *version >= doc.version);
        while cache.entries.len() > 48 {
            let oldest = cache.entries.iter().min_by_key(|(_, (used, _))| *used).map(|(k, _)| *k);
            match oldest {
                Some(oldest) => cache.entries.remove(&oldest),
                None => break,
            };
        }
        Ok(list)
    }

    fn tile(
        &mut self,
        shared: &Shared,
        doc: &DocRef,
        key: &TileKey,
        size: &PageSize,
        overlays: &[Overlay],
    ) -> Result<TileImage, String> {
        let list = self.list(shared, doc, key.base.page)?;
        let grid = key.base.grid;
        let (c0, r0, cols, rows) = grid.tile_cells(key.base.tx, key.base.ty);
        let (x, y) = ((c0 * grid.cw) as i32, (r0 * grid.ch) as i32);
        let (w, h) = (cols * grid.cw, rows * grid.ch);
        let mut pixels = render_area(&list, size, grid.scale() as f32, (x, y, w, h))?;
        draw_overlays(&mut pixels, (w, h), (x, y), grid.scale(), size, overlays);
        let payload = match shared.transfer {
            Transfer::File => {
                let serial = shared.serial.fetch_add(1, Ordering::Relaxed);
                let mut path = shared.temp_prefix.clone().into_os_string();
                path.push(format!("{serial}.rgb"));
                let path = PathBuf::from(path);
                std::fs::write(&path, &pixels).map_err(|e| format!("{}: {e}", path.display()))?;
                Payload::File(path)
            }
            Transfer::Direct => {
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
                encoder.write_all(&pixels).map_err(|e| e.to_string())?;
                Payload::Direct(encoder.finish().map_err(|e| e.to_string())?)
            }
        };
        Ok(TileImage { pixels: (w, h), cells: (cols, rows), payload })
    }

    fn text(&mut self, shared: &Shared, doc: &DocRef, page: u32) -> Result<Vec<Line>, String> {
        let list = self.list(shared, doc, page)?;
        extract_text(&list)
    }
}

/// Render the device-space rectangle (x, y, w, h) of a page to packed RGB.
fn render_area(
    list: &DisplayList,
    size: &PageSize,
    scale: f32,
    (x, y, w, h): (i32, i32, u32, u32),
) -> Result<Vec<u8>, String> {
    let mut pixmap = Pixmap::new(&Colorspace::device_rgb(), x, y, w as i32, h as i32, false)
        .map_err(|e| e.to_string())?;
    pixmap.clear_with(255).map_err(|e| e.to_string())?;
    let ctm = Matrix::new(scale, 0.0, 0.0, scale, -size.x0 as f32 * scale, -size.y0 as f32 * scale);
    {
        let device = Device::from_pixmap(&pixmap).map_err(|e| e.to_string())?;
        let area = mupdf::Rect::new(x as f32, y as f32, (x + w as i32) as f32, (y + h as i32) as f32);
        list.run(&device, &ctm, area).map_err(|e| e.to_string())?;
    }
    let stride = pixmap.stride() as usize;
    let row = w as usize * 3;
    let samples = pixmap.samples();
    if stride == row {
        Ok(samples[..row * h as usize].to_vec())
    } else {
        let mut packed = Vec::with_capacity(row * h as usize);
        for line in 0..h as usize {
            packed.extend_from_slice(&samples[line * stride..line * stride + row]);
        }
        Ok(packed)
    }
}

fn draw_overlays(
    pixels: &mut [u8],
    (w, h): (u32, u32),
    (ox, oy): (i32, i32),
    scale: f64,
    size: &PageSize,
    overlays: &[Overlay],
) {
    if overlays.is_empty() {
        return;
    }
    let (w, h) = (w as i64, h as i64);
    // Page-space rectangle → clipped tile pixel range.
    let to_px = |r: Rect| {
        let x0 = ((r.x0 - size.x0) * scale).floor() as i64 - ox as i64;
        let y0 = ((r.y0 - size.y0) * scale).floor() as i64 - oy as i64;
        let x1 = ((r.x1 - size.x0) * scale).ceil() as i64 - ox as i64;
        let y1 = ((r.y1 - size.y0) * scale).ceil() as i64 - oy as i64;
        (x0, y0, x1, y1)
    };
    let span = |a: i64, b: i64, limit: i64| a.clamp(0, limit) as usize..b.clamp(0, limit) as usize;
    // Highlights are a union: overlapping line boxes must not tint twice.
    let mut mask: Option<Vec<bool>> = None;
    for overlay in overlays {
        if let Overlay::Highlight(rect) = overlay {
            let mask = mask.get_or_insert_with(|| vec![false; (w * h) as usize]);
            let (x0, y0, x1, y1) = to_px(*rect);
            for y in span(y0, y1, h) {
                mask[y * w as usize..][span(x0, x1, w)].fill(true);
            }
        }
    }
    if let Some(mask) = mask {
        // Multiply keeps text dark while tinting the paper.
        // Nightfox blue (#719cd6) halfway to white.
        const TINT: [u16; 3] = [184, 206, 235];
        for (pixel, _) in pixels.as_chunks_mut::<3>().0.iter_mut().zip(&mask).filter(|(_, &m)| m) {
            for c in 0..3 {
                pixel[c] = (pixel[c] as u16 * TINT[c] / 255) as u8;
            }
        }
    }
    for overlay in overlays {
        if let Overlay::Outline(rect) = overlay {
            let (x0, y0, x1, y1) = to_px(rect.expand(2.0));
            let stroke = (scale * 1.2).round().clamp(2.0, 6.0) as i64;
            for (a, b, c, d) in [
                (x0, y0, x1, y0 + stroke),
                (x0, y1 - stroke, x1, y1),
                (x0, y0, x0 + stroke, y1),
                (x1 - stroke, y0, x1, y1),
            ] {
                for y in span(b, d, h) {
                    for x in span(a, c, w) {
                        let i = (y * w as usize + x) * 3;
                        pixels[i..i + 3].copy_from_slice(&[0xc9, 0x4f, 0x6d]); // nightfox red
                    }
                }
            }
        }
    }
}

fn extract_text(list: &DisplayList) -> Result<Vec<Line>, String> {
    let page = list.to_text_page(TextPageFlags::empty()).map_err(|e| e.to_string())?;
    let mut lines = Vec::new();
    for block in page.blocks() {
        let b = block.bounds();
        let block_rect = Rect::new(b.x0 as f64, b.y0 as f64, b.x1 as f64, b.y1 as f64);
        let first = lines.len();
        for line in block.lines() {
            let l = line.bounds();
            let glyphs: Vec<Glyph> = line
                .chars()
                .filter_map(|c| {
                    let ch = c.char()?;
                    let q = c.quad();
                    Some(Glyph { ch, x0: q.ul.x.min(q.ll.x) as f64, x1: q.ur.x.max(q.lr.x) as f64 })
                })
                .collect();
            if glyphs.iter().all(|g| g.ch.is_whitespace()) {
                continue;
            }
            lines.push(Line {
                bbox: Rect::new(l.x0 as f64, l.y0 as f64, l.x1 as f64, l.y1 as f64),
                block: block_rect,
                glyphs,
            });
        }
        split_paragraphs(&mut lines[first..], &block_rect);
    }
    Ok(lines)
}

/// MuPDF merges consecutive TeX paragraphs into one block. Start a new
/// paragraph at an indented first line or a larger than usual vertical gap,
/// and give each line the bounds of its own paragraph.
fn split_paragraphs(lines: &mut [Line], block: &Rect) {
    if lines.len() < 2 {
        return;
    }
    let mut gaps: Vec<f64> = lines.windows(2).map(|w| w[1].bbox.y0 - w[0].bbox.y0).collect();
    gaps.sort_by(f64::total_cmp);
    let typical = gaps[gaps.len() / 2];
    let mut start = 0;
    for i in 0..=lines.len() {
        let boundary = i == lines.len()
            || (i > start && {
                let line = &lines[i].bbox;
                let previous = &lines[i - 1].bbox;
                let indented = line.x0 > block.x0 + 6.0 && previous.x0 <= block.x0 + 6.0;
                let gap = line.y0 - previous.y0 > typical * 1.4 + 1.0;
                indented || gap
            });
        if boundary {
            let rect = lines[start..i].iter().fold(lines[start].bbox, |r, l| {
                Rect::new(r.x0.min(l.bbox.x0), r.y0.min(l.bbox.y0), r.x1.max(l.bbox.x1), r.y1.max(l.bbox.y1))
            });
            for line in &mut lines[start..i] {
                line.block = rect;
            }
            start = i;
        }
    }
}

/// Page count and sizes, read on the calling thread.
pub fn page_sizes(path: &Path) -> Result<Vec<PageSize>, String> {
    let path = path.to_str().ok_or("PDF path is not valid UTF-8")?;
    let document = Document::open(path).map_err(|e| e.to_string())?;
    let count = document.page_count().map_err(|e| e.to_string())?;
    (0..count)
        .map(|index| {
            let b = document
                .load_page(index)
                .and_then(|page| page.bounds())
                .map_err(|e| e.to_string())?;
            Ok(PageSize {
                x0: b.x0 as f64,
                y0: b.y0 as f64,
                w: (b.x1 - b.x0).max(1.0) as f64,
                h: (b.y1 - b.y0).max(1.0) as f64,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn grid_tiles_cover_page_exactly() {
        let grid = Grid::new(300, 50, 10, 20, 2.0);
        assert_eq!((grid.tiles_x(), grid.tiles_y()), (3, 3));
        assert_eq!(grid.tile_cells(2, 2), (256, 48, 44, 2));
        let size = PageSize { x0: 0.0, y0: 0.0, w: 1500.0, h: 500.0 };
        let r = grid.tile_rect(&size, 1, 0);
        assert_eq!((r.x0, r.x1), (640.0, 1280.0));
    }

    #[test]
    fn sample_tiles_and_text_render() {
        let pdf = Path::new(env!("CARGO_MANIFEST_DIR")).join("sample/build/main.pdf");
        if !pdf.is_file() {
            eprintln!("skipping: build the sample first");
            return;
        }
        let sizes = page_sizes(&pdf).unwrap();
        assert!(!sizes.is_empty());
        let (tx, rx) = std::sync::mpsc::channel();
        let pool = Pool::new(Transfer::Direct, std::env::temp_dir().join("pdfty-test-"), move |o| {
            tx.send(o).is_ok()
        });
        let doc = DocRef { id: 1, version: 1, path: Arc::new(pdf) };
        let size = sizes[0];
        let scale = 1000.0 / size.w;
        let grid = Grid::new(100, (size.h * scale / 20.0).ceil() as u32, 10, 20, scale as f32);
        let key = TileKey { base: TileBase { doc: 1, page: 0, grid, tx: 0, ty: 0 }, version: 1, overlay: 0 };
        pool.set_jobs(vec![
            Job::Tile { doc: doc.clone(), key, size, overlays: vec![] },
            Job::Text { doc, page: 0 },
        ]);
        let mut got_tile = false;
        let mut got_text = false;
        while !(got_tile && got_text) {
            match rx.recv_timeout(Duration::from_secs(20)).unwrap() {
                Output::Tile { result, .. } => {
                    let image = result.unwrap();
                    assert_eq!(image.pixels, (1000, TILE_ROWS * 20));
                    let Payload::Direct(data) = image.payload else { panic!() };
                    let mut raw = Vec::new();
                    std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&data[..]), &mut raw).unwrap();
                    assert_eq!(raw.len(), 1000 * TILE_ROWS as usize * 20 * 3);
                    assert!(raw.iter().any(|&b| b < 128), "tile should contain dark text pixels");
                    got_tile = true;
                }
                Output::Text { result, .. } => {
                    let lines = result.unwrap();
                    assert!(lines.iter().any(|l| l.glyphs.iter().map(|g| g.ch).collect::<String>().contains("Lorem")));
                    got_text = true;
                }
            }
        }
    }
}

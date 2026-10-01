use crate::{
    cache::Key,
    resolution::CELL_WIDTH,
    text::{self, Line},
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

/// A page render request. `generation` identifies the user's latest request,
/// while `key` identifies reusable output across page visits.
pub struct Request {
    pub key: Key,
    pub pdf: PathBuf,
    pub dir: PathBuf,
    pub generation: u64,
}
pub struct ResultPage {
    pub pdf: PathBuf,
    pub key: Key,
    pub generation: u64,
    pub prefetch: bool,
    pub result: Result<(PathBuf, f64, f64, Vec<Line>), String>,
}
pub struct TextResult {
    pub pdf: PathBuf,
    pub key: Key,
    pub lines: Vec<Line>,
}

pub fn worker() -> (
    Sender<Request>,
    Sender<Request>,
    Receiver<ResultPage>,
    Receiver<TextResult>,
) {
    // A slow background page must not hold up a visible page.
    let (requests, incoming) = mpsc::channel::<Request>();
    let (background, background_incoming) = mpsc::channel::<Request>();
    let (outgoing, results) = mpsc::channel();
    let (text_requests, text_incoming) = mpsc::channel::<(PathBuf, Key)>();
    let (text_outgoing, text_results) = mpsc::channel();
    thread::spawn(move || {
        while let Ok((pdf, key)) = text_incoming.recv() {
            let lines = text::extract(&pdf, key.page).unwrap_or_default();
            if text_outgoing.send(TextResult { pdf, key, lines }).is_err() {
                break;
            }
        }
    });
    let urgent_outgoing = outgoing.clone();
    let urgent_text = text_requests.clone();
    thread::spawn(move || {
        while let Ok(mut request) = incoming.recv() {
            while let Ok(next) = incoming.try_recv() {
                request = next;
            }
            if !render_and_send(request, false, &urgent_outgoing, &urgent_text) {
                break;
            }
        }
    });
    thread::spawn(move || {
        while let Ok(request) = background_incoming.recv() {
            if !render_and_send(request, true, &outgoing, &text_requests) {
                break;
            }
        }
    });
    (requests, background, results, text_results)
}

fn render_and_send(
    request: Request,
    prefetch: bool,
    outgoing: &Sender<ResultPage>,
    text_requests: &Sender<(PathBuf, Key)>,
) -> bool {
    let result = render(&request.pdf, &request.dir, &request.key, request.generation);
    if result.is_ok() {
        let _ = text_requests.send((request.pdf.clone(), request.key.clone()));
    }
    outgoing
        .send(ResultPage {
            pdf: request.pdf,
            key: request.key,
            generation: request.generation,
            prefetch,
            result,
        })
        .is_ok()
}

pub fn page_count(pdf: &Path) -> Result<u32, String> {
    let output = Command::new("pdfinfo")
        .arg(pdf)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("Pages:")?.trim().parse().ok())
        .ok_or_else(|| "pdfinfo did not report a page count".into())
}

pub fn page_size(pdf: &Path, page: u32) -> Result<(f64, f64), String> {
    // pdfinfo reports page dimensions in points, used later for both image
    // scaling and terminal-to-PDF coordinate conversion.
    let output = Command::new("pdfinfo")
        .args(["-f", &page.to_string(), "-l", &page.to_string()])
        .arg(pdf)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text
        .lines()
        .find(|x| x.contains("size:"))
        .ok_or("missing page size")?;
    let tail = line.split_once("size:").unwrap().1.trim();
    let mut parts = tail.split_whitespace();
    let width = parts
        .next()
        .ok_or("missing width")?
        .parse()
        .map_err(|_| "invalid width")?;
    let _x = parts.next();
    let height = parts
        .next()
        .ok_or("missing height")?
        .parse()
        .map_err(|_| "invalid height")?;
    Ok((width, height))
}

fn render(
    pdf: &Path,
    dir: &Path,
    key: &Key,
    generation: u64,
) -> Result<(PathBuf, f64, f64, Vec<Line>), String> {
    static NEXT_IMAGE: AtomicU64 = AtomicU64::new(1);
    let (width, height) = page_size(pdf, key.page)?;
    // Render enough pixels for one bitmap target per terminal column. Single
    // view crops/downsizes this source; continuous view composites it. The
    // upper DPI bound prevents huge allocations at extreme zoom or pane sizes.
    let dpi = key.width as f64 * CELL_WIDTH as f64 * key.zoom as f64 / 100.0 / width * 72.0;
    let dpi = dpi.clamp(72.0, 540.0);
    let path = dir.join(format!(
        "{}-{}-{}-{}-{}-{generation}-{}.png",
        key.version,
        key.page,
        key.width,
        key.height,
        key.zoom,
        NEXT_IMAGE.fetch_add(1, Ordering::Relaxed)
    ));
    let output = Command::new("mutool")
        .args(["draw", "-q", "-F", "png", "-r", &format!("{dpi:.2}"), "-o"])
        .arg(&path)
        .arg(pdf)
        .arg(key.page.to_string())
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&path);
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    Ok((path, width, height, Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sample_page_renders_at_scroll_resolution() {
        let pdf = Path::new(env!("CARGO_MANIFEST_DIR")).join("sample/build/main.pdf");
        assert!(pdf.is_file(), "build the sample with `cd sample && latexmk -lualatex -synctex=1 -outdir=build main.tex`");
        assert!(page_count(&pdf).unwrap() >= 1);
        let dir = std::env::temp_dir().join(format!("pdfty-render-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = Key {
            version: 1,
            page: 1,
            width: 80,
            height: 23,
            zoom: 100,
        };
        let (path, _, _, _) = render(&pdf, &dir, &key, 1).unwrap();
        let bitmap = ::image::open(&path).unwrap();
        assert!(bitmap.width() >= 900);
        let lines = text::extract(&pdf, 1).unwrap();
        assert!(!lines.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}

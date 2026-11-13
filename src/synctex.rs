//! Native SyncTeX reader: parses `<name>.synctex(.gz)` beside the PDF and
//! answers forward (source → page point) and inverse (page point → source)
//! queries without the `synctex` command-line tool.
//!
//! All coordinates are PDF points measured from the page's top-left corner.

use flate2::read::GzDecoder;
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    VBox,
    HBox,
    VoidVBox,
    VoidHBox,
    Kern,
    Glue,
    Math,
    Current,
}

#[derive(Clone, Copy, Debug)]
struct Node {
    kind: Kind,
    tag: u32,
    line: u32,
    h: f64,
    v: f64,
    width: f64,
    height: f64,
    depth: f64,
    /// Index of the enclosing box on the same page, or usize::MAX.
    parent: usize,
}

impl Node {
    fn is_box(&self) -> bool {
        matches!(self.kind, Kind::HBox | Kind::VBox | Kind::VoidHBox | Kind::VoidVBox)
    }
    fn is_hbox(&self) -> bool {
        matches!(self.kind, Kind::HBox | Kind::VoidHBox)
    }
    fn rect(&self) -> (f64, f64, f64, f64) {
        let (x0, x1) = if self.width >= 0.0 {
            (self.h, self.h + self.width)
        } else {
            (self.h + self.width, self.h)
        };
        (x0, self.v - self.height, x1, self.v + self.depth.max(0.0))
    }
    fn distance2(&self, x: f64, y: f64) -> f64 {
        let (x0, y0, x1, y1) = self.rect();
        let dx = (x0 - x).max(0.0).max(x - x1);
        let dy = (y0 - y).max(0.0).max(y - y1);
        dx * dx + dy * dy
    }
}

/// Forward-search result.
#[derive(Clone, Copy, Debug)]
pub struct Position {
    /// 1-based page number.
    pub page: u32,
    pub x: f64,
    pub y: f64,
}

/// Inverse-search result.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub file: PathBuf,
    pub line: u32,
}

pub struct Synctex {
    inputs: HashMap<u32, PathBuf>,
    /// Nodes per page, index 0 is page 1.
    pages: Vec<Vec<Node>>,
}

/// Locate the SyncTeX file belonging to a PDF.
pub fn file_for(pdf: &Path) -> Option<PathBuf> {
    ["synctex.gz", "synctex"]
        .into_iter()
        .map(|extension| pdf.with_extension(extension))
        .find(|path| path.is_file())
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| {
        // Fall back to a lexical cleanup of `.` components.
        path.components()
            .filter(|c| !matches!(c, std::path::Component::CurDir))
            .collect()
    })
}

impl Synctex {
    pub fn load(pdf: &Path) -> Result<Self, String> {
        let path = file_for(pdf).ok_or_else(|| {
            format!("no SyncTeX file beside {}; compile with -synctex=1", pdf.display())
        })?;
        let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let text = if path.extension().is_some_and(|e| e == "gz") {
            let mut text = String::new();
            GzDecoder::new(&bytes[..])
                .read_to_string(&mut text)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            text
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let base = path.parent().unwrap_or(Path::new("."));
        Ok(Self::parse(&text, base))
    }

    /// Parse SyncTeX text. Relative input paths resolve against `base`.
    pub fn parse(text: &str, base: &Path) -> Self {
        let mut inputs = HashMap::new();
        let mut pages: Vec<Vec<Node>> = Vec::new();
        let mut magnification = 1000.0;
        let mut unit = 1.0;
        let mut x_offset = 0.0;
        let mut y_offset = 0.0;
        let mut content = false;
        let mut current: Option<usize> = None;
        let mut stack: Vec<usize> = Vec::new();
        // TeX scaled points to PDF big points.
        let mut factor = 1.0 / 65781.76;

        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("Input:") {
                if let Some((tag, file)) = rest.split_once(':') {
                    if let Ok(tag) = tag.parse() {
                        let path = Path::new(file);
                        let path = if path.is_absolute() { path.to_path_buf() } else { base.join(path) };
                        inputs.insert(tag, canonical(&path));
                    }
                }
                continue;
            }
            if !content {
                if let Some(value) = line.strip_prefix("Magnification:") {
                    magnification = value.trim().parse().unwrap_or(1000.0);
                } else if let Some(value) = line.strip_prefix("Unit:") {
                    unit = value.trim().parse().unwrap_or(1.0);
                } else if let Some(value) = line.strip_prefix("X Offset:") {
                    x_offset = value.trim().parse().unwrap_or(0.0);
                } else if let Some(value) = line.strip_prefix("Y Offset:") {
                    y_offset = value.trim().parse().unwrap_or(0.0);
                } else if line.starts_with("Content:") {
                    content = true;
                    factor = unit * magnification / 1000.0 / 65781.76;
                }
                continue;
            }
            if line.starts_with("Postamble:") {
                break;
            }
            let mut chars = line.chars();
            let Some(first) = chars.next() else { continue };
            let rest = chars.as_str();
            match first {
                '{' => {
                    let page: usize = rest.trim().parse().unwrap_or(pages.len() + 1);
                    if pages.len() < page {
                        pages.resize_with(page, Vec::new);
                    }
                    current = Some(page - 1);
                    stack.clear();
                }
                '}' => {
                    current = None;
                    stack.clear();
                }
                ']' | ')' => {
                    stack.pop();
                }
                '[' | '(' | 'v' | 'h' | 'k' | 'g' | '$' | 'x' => {
                    let Some(page) = current else { continue };
                    let Some(mut node) = parse_record(first, rest) else { continue };
                    node.h = node.h * factor + x_offset / 65781.76;
                    node.v = node.v * factor + y_offset / 65781.76;
                    node.width *= factor;
                    node.height *= factor;
                    node.depth *= factor;
                    node.parent = stack.last().copied().unwrap_or(usize::MAX);
                    let nodes = &mut pages[page];
                    nodes.push(node);
                    if first == '[' || first == '(' {
                        stack.push(nodes.len() - 1);
                    }
                }
                _ => {}
            }
        }
        Self { inputs, pages }
    }

    #[cfg(test)]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Whether the document was built from `source`.
    pub fn has_source(&self, source: &Path) -> bool {
        !self.tags_for(source).is_empty()
    }

    fn tags_for(&self, source: &Path) -> Vec<u32> {
        let source = canonical(source);
        let exact: Vec<u32> = self
            .inputs
            .iter()
            .filter(|(_, path)| **path == source)
            .map(|(tag, _)| *tag)
            .collect();
        if !exact.is_empty() {
            return exact;
        }
        // Builds from another directory may record a different prefix; accept
        // a unique file-name match like the reference implementation does.
        let name = source.file_name();
        let by_name: Vec<u32> = self
            .inputs
            .iter()
            .filter(|(_, path)| path.file_name() == name)
            .map(|(tag, _)| *tag)
            .collect();
        if by_name.len() == 1 {
            by_name
        } else {
            Vec::new()
        }
    }

    /// Find where `line` of `source` was typeset. Uses the nearest following
    /// line (or else preceding line) when the exact line produced no output.
    pub fn forward(&self, source: &Path, line: u32) -> Result<Position, String> {
        let tags = self.tags_for(source);
        if tags.is_empty() {
            return Err(format!("{} is not part of this PDF", source.display()));
        }
        let mut best_line: Option<u32> = None;
        let score = |l: u32| -> (u32, u32) {
            if l >= line {
                (0, l - line)
            } else {
                (1, line - l)
            }
        };
        for node in self.pages.iter().flatten() {
            if tags.contains(&node.tag) && node.line > 0 && best_line.is_none_or(|b| score(node.line) < score(b)) {
                best_line = Some(node.line);
            }
        }
        let target = best_line.ok_or("no SyncTeX records for this file")?;
        let order = |a: &&Node, b: &&Node| (a.v, a.h).partial_cmp(&(b.v, b.h)).unwrap_or(std::cmp::Ordering::Equal);
        for (index, nodes) in self.pages.iter().enumerate() {
            let matches: Vec<&Node> = nodes
                .iter()
                .filter(|n| tags.contains(&n.tag) && n.line == target && n.kind != Kind::VBox)
                .collect();
            // Glue records mark text read from this line: show where it starts.
            // Otherwise the line only closed something (a paragraph ends at the
            // following blank line), so show where that ends.
            let glue = matches.iter().copied().filter(|n| n.kind == Kind::Glue).min_by(|a, b| order(a, b));
            if let Some(node) = glue.or_else(|| matches.iter().copied().max_by(|a, b| order(a, b))) {
                return Ok(Position { page: index as u32 + 1, x: node.h, y: node.v });
            }
        }
        Err("no SyncTeX records for this line".into())
    }

    /// Find the source line typeset at a point on a 1-based page.
    pub fn inverse(&self, page: u32, x: f64, y: f64) -> Result<Source, String> {
        let nodes = self
            .pages
            .get(page.saturating_sub(1) as usize)
            .filter(|nodes| !nodes.is_empty())
            .ok_or_else(|| format!("no SyncTeX records for page {page}"))?;
        // Only boxes that typeset something from a source line are candidates;
        // TeX also emits empty boxes (headers, struts) at arbitrary places.
        let leaves = |index: usize| -> Vec<&Node> {
            nodes
                .iter()
                .filter(|n| n.parent == index && n.line > 0 && (!n.is_box() || n.kind == Kind::VoidHBox))
                .collect()
        };
        let candidates: Vec<(usize, &Node)> = nodes
            .iter()
            .enumerate()
            .filter(|(i, n)| n.is_hbox() && n.width.abs() > 0.0 && n.height + n.depth > 0.0 && !leaves(*i).is_empty())
            .collect();
        // The innermost line box containing the point, else the closest one.
        let hbox = candidates
            .iter()
            .filter(|(_, n)| n.distance2(x, y) == 0.0)
            .min_by(|(_, a), (_, b)| area(a).total_cmp(&area(b)))
            .or_else(|| {
                candidates
                    .iter()
                    .min_by(|(_, a), (_, b)| a.distance2(x, y).total_cmp(&b.distance2(x, y)))
            });
        let chosen = match hbox {
            Some(&(mut index, mut hbox)) => {
                // A box taller than a couple of text lines is a container (a
                // figure, a TikZ picture): use the closest box inside it.
                if hbox.height + hbox.depth > 24.0 {
                    let inside = |mut i: usize| {
                        while i != usize::MAX {
                            i = nodes[i].parent;
                            if i == index {
                                return true;
                            }
                        }
                        false
                    };
                    if let Some(&(i, n)) = candidates
                        .iter()
                        .filter(|(i, _)| *i != index && inside(*i))
                        .min_by(|(_, a), (_, b)| {
                            (a.distance2(x, y), area(a)).partial_cmp(&(b.distance2(x, y), area(b))).unwrap_or(std::cmp::Ordering::Equal)
                        })
                    {
                        (index, hbox) = (i, n);
                    }
                }
                // Inter-word glue carries the line the text was read from; the
                // box itself is tagged where TeX finished the paragraph, and
                // LuaTeX tags many kerns and glyph records unreliably.
                let leaves = leaves(index);
                let glue: Vec<&Node> = leaves.iter().copied().filter(|n| n.kind == Kind::Glue).collect();
                let pool = if glue.is_empty() { &leaves } else { &glue };
                let left = pool.iter().filter(|n| n.h <= x).max_by(|a, b| a.h.total_cmp(&b.h));
                left.or_else(|| pool.iter().min_by(|a, b| a.h.total_cmp(&b.h))).copied().unwrap_or(hbox)
            }
            None => nodes
                .iter()
                .filter(|n| n.line > 0)
                .min_by(|a, b| a.distance2(x, y).total_cmp(&b.distance2(x, y)))
                .ok_or("no SyncTeX records near this point")?,
        };
        let file = self
            .inputs
            .get(&chosen.tag)
            .cloned()
            .ok_or("SyncTeX record refers to an unknown input")?;
        Ok(Source { file, line: chosen.line })
    }
}

fn area(node: &Node) -> f64 {
    let (x0, y0, x1, y1) = node.rect();
    (x1 - x0) * (y1 - y0)
}

fn parse_record(kind: char, rest: &str) -> Option<Node> {
    let mut parts = rest.split(':');
    let mut ids = parts.next()?.split(',');
    let tag = ids.next()?.parse().ok()?;
    let line = ids.next()?.parse::<i64>().ok()?.max(0) as u32;
    let mut point = parts.next()?.split(',');
    let h: f64 = point.next()?.parse().ok()?;
    let v: f64 = point.next()?.parse().ok()?;
    let mut size = parts.next().map(|s| s.split(',')).into_iter().flatten();
    let mut next = || size.next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
    let (width, height, depth) = match kind {
        '[' | '(' | 'v' | 'h' => (next(), next(), next()),
        'k' => (next(), 0.0, 0.0),
        _ => (0.0, 0.0, 0.0),
    };
    let kind = match kind {
        '[' => Kind::VBox,
        '(' => Kind::HBox,
        'v' => Kind::VoidVBox,
        'h' => Kind::VoidHBox,
        'k' => Kind::Kern,
        'g' => Kind::Glue,
        '$' => Kind::Math,
        _ => Kind::Current,
    };
    Some(Node { kind, tag, line, h, v, width, height, depth, parent: usize::MAX })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "SyncTeX Version:1
Input:1:/doc/main.tex
Input:2:/doc/chapter.tex
Output:pdf
Magnification:1000
Unit:1
X Offset:0
Y Offset:0
Content:
{1
[1,5:4736286,4736286:29685704,45908418,0
(1,7:4736286,6578176:29685704,657817,0
x1,6:4736286,6578176
g1,6:9868800,6578176
g1,7:19000000,6578176
x1,7:19737600,6578176
)
(2,3:4736286,13156352:29685704,657817,0
x2,3:4736286,13156352
)
]
}1
{2
(1,20:4736286,6578176:29685704,657817,0
x1,20:4736286,6578176
)
}2
Postamble:
";

    #[test]
    fn forward_and_inverse_round_trip() {
        let s = Synctex::parse(SAMPLE, Path::new("/"));
        assert_eq!(s.page_count(), 2);
        let p = s.forward(Path::new("/doc/main.tex"), 6).unwrap();
        assert_eq!(p.page, 1);
        // The first inter-word glue read from line 6.
        assert!((p.x - 150.02).abs() < 0.01 && (p.y - 100.0).abs() < 0.1, "{p:?}");
        // A line without output snaps to the next line that has output.
        assert_eq!(s.forward(Path::new("/doc/main.tex"), 8).unwrap().page, 2);
        let src = s.inverse(1, 80.0, 99.0).unwrap();
        assert_eq!(src, Source { file: "/doc/main.tex".into(), line: 6 });
        assert_eq!(s.inverse(1, 350.0, 99.0).unwrap().line, 7);
        assert_eq!(s.inverse(1, 80.0, 199.0).unwrap().file, PathBuf::from("/doc/chapter.tex"));
        assert!(s.has_source(Path::new("/doc/chapter.tex")));
        assert!(!s.has_source(Path::new("/doc/other.tex")));
    }

    #[test]
    fn sample_document_round_trip() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let pdf = root.join("sample/build/main.pdf");
        let source = root.join("sample/main.tex");
        if !pdf.is_file() {
            eprintln!("skipping: build the sample first");
            return;
        }
        let s = Synctex::load(&pdf).unwrap();
        let tex = fs::read_to_string(&source).unwrap();
        for anchor in ["Lorem ipsum dolor sit amet", "\\section{Some tikz picture}"] {
            let line = tex.lines().position(|t| t.starts_with(anchor)).unwrap() as u32 + 1;
            let p = s.forward(&source, line).unwrap();
            let back = s.inverse(p.page, p.x + 1.0, p.y - 1.0).unwrap();
            assert_eq!(back.file, fs::canonicalize(&source).unwrap());
            assert_eq!(back.line, line, "{anchor}");
        }
    }
}


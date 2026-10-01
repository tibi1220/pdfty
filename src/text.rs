use serde::Deserialize;
use std::{path::Path, process::Command};

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Line {
    pub bbox: Rect,
    pub text: String,
    #[serde(skip)]
    pub paragraph: Rect,
}
#[derive(Deserialize)]
struct Page {
    blocks: Vec<Block>,
}
#[derive(Deserialize)]
struct Block {
    bbox: Rect,
    #[serde(default)]
    lines: Vec<Line>,
}
#[derive(Deserialize)]
struct Document {
    pages: Vec<Page>,
}

pub fn extract(pdf: &Path, page: u32) -> Result<Vec<Line>, String> {
    // Keep text and bounding boxes from mutool's structured output. The viewer
    // still displays a bitmap, but these boxes support selection and copying.
    // stext.json rounds coordinates to integer output pixels. Extracting at
    // 720 dpi gives ten times finer boxes than the 72 dpi default; convert
    // them back to PDF points before the app uses them.
    let output = Command::new("mutool")
        .args(["draw", "-q", "-r", "720", "-F", "stext.json"])
        .arg(pdf)
        .arg(page.to_string())
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    let document: Document = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    Ok(document
        .pages
        .into_iter()
        .flat_map(|p| p.blocks.into_iter())
        .flat_map(|b| {
            let paragraph = Rect {
                x: b.bbox.x / 10.0,
                y: b.bbox.y / 10.0,
                w: b.bbox.w / 10.0,
                h: b.bbox.h / 10.0,
            };
            b.lines.into_iter().map(move |mut line| {
                line.paragraph = paragraph.clone();
                line
            })
        })
        .filter(|line| !line.text.trim().is_empty())
        .map(|mut line| {
            line.bbox.x /= 10.0;
            line.bbox.y /= 10.0;
            line.bbox.w /= 10.0;
            line.bbox.h /= 10.0;
            line
        })
        .collect())
}

pub fn paragraph_at(lines: &[Line], x: f64, y: f64) -> Option<Rect> {
    let index = nearest(lines, x, y)?;
    // A SyncTeX target on a figure or in blank space should not outline an
    // unrelated text block elsewhere on the page.
    (distance(&lines[index].bbox, x, y) <= 15.0 * 15.0).then(|| lines[index].paragraph.clone())
}

pub fn nearest(lines: &[Line], x: f64, y: f64) -> Option<usize> {
    // Select the closest line even when the pointer falls in whitespace.
    lines
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| distance(&a.bbox, x, y).total_cmp(&distance(&b.bbox, x, y)))
        .map(|(index, _)| index)
}

fn distance(r: &Rect, x: f64, y: f64) -> f64 {
    // Squared distance to a rectangle is zero for a point inside it.
    let dx = (r.x - x).max(0.0).max(x - r.x - r.w);
    let dy = (r.y - y).max(0.0).max(y - r.y - r.h);
    dx * dx + dy * dy
}

pub fn selected(lines: &[Line], a: usize, b: usize) -> String {
    // Anchor and endpoint may be visited in either direction.
    let start = a.min(b);
    let end = a.max(b).min(lines.len().saturating_sub(1));
    lines[start..=end]
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selects_lines_in_either_direction() {
        let lines = vec![
            Line {
                bbox: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 10.0,
                    h: 2.0,
                },
                text: "one".into(),
                paragraph: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 10.0,
                    h: 2.0,
                },
            },
            Line {
                bbox: Rect {
                    x: 0.0,
                    y: 4.0,
                    w: 10.0,
                    h: 2.0,
                },
                text: "two".into(),
                paragraph: Rect {
                    x: 0.0,
                    y: 4.0,
                    w: 10.0,
                    h: 2.0,
                },
            },
        ];
        assert_eq!(nearest(&lines, 5.0, 5.0), Some(1));
        assert_eq!(selected(&lines, 1, 0), "one\ntwo");
        assert_eq!(paragraph_at(&lines, 5.0, 5.0).unwrap().y, 4.0);
    }
}

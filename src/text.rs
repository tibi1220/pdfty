//! Extracted page text in PDF points, plus character-level selection helpers.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self { x0, y0, x1, y1 }
    }
    pub fn expand(self, by: f64) -> Rect {
        Rect::new(self.x0 - by, self.y0 - by, self.x1 + by, self.y1 + by)
    }
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }
    /// Squared distance from a point; zero inside the rectangle.
    pub fn distance2(&self, x: f64, y: f64) -> f64 {
        let dx = (self.x0 - x).max(0.0).max(x - self.x1);
        let dy = (self.y0 - y).max(0.0).max(y - self.y1);
        dx * dx + dy * dy
    }
}

#[derive(Clone, Debug)]
pub struct Glyph {
    pub ch: char,
    pub x0: f64,
    pub x1: f64,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub bbox: Rect,
    /// The enclosing text block, used as the paragraph outline.
    pub block: Rect,
    pub glyphs: Vec<Glyph>,
}

/// A character position: line index and glyph index within that line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPos {
    pub line: usize,
    pub glyph: usize,
}

pub fn nearest_line(lines: &[Line], x: f64, y: f64) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| a.bbox.distance2(x, y).total_cmp(&b.bbox.distance2(x, y)))
        .map(|(index, _)| index)
}

/// The character under (or nearest to) a point.
pub fn position(lines: &[Line], x: f64, y: f64) -> Option<TextPos> {
    let line = nearest_line(lines, x, y)?;
    let glyphs = &lines[line].glyphs;
    if glyphs.is_empty() {
        return None;
    }
    let glyph = glyphs
        .iter()
        .position(|g| x < g.x1)
        .unwrap_or(glyphs.len() - 1);
    Some(TextPos { line, glyph })
}

fn ordered(a: TextPos, b: TextPos) -> (TextPos, TextPos) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Visit each selected line with its inclusive glyph range.
fn for_each_span(lines: &[Line], a: TextPos, b: TextPos, mut visit: impl FnMut(&Line, usize, usize)) {
    let (start, end) = ordered(a, b);
    let last_line = end.line.min(lines.len().saturating_sub(1));
    for (index, line) in lines.iter().enumerate().take(last_line + 1).skip(start.line) {
        if line.glyphs.is_empty() {
            continue;
        }
        let first = if index == start.line { start.glyph } else { 0 };
        let last = if index == end.line { end.glyph } else { line.glyphs.len() - 1 };
        let last = last.min(line.glyphs.len() - 1);
        if first <= last {
            visit(line, first, last);
        }
    }
}

pub fn selection_rects(lines: &[Line], a: TextPos, b: TextPos) -> Vec<Rect> {
    let mut rects = Vec::new();
    for_each_span(lines, a, b, |line, first, last| {
        rects.push(Rect::new(
            line.glyphs[first].x0,
            line.bbox.y0,
            line.glyphs[last].x1,
            line.bbox.y1,
        ))
    });
    rects
}

pub fn selection_text(lines: &[Line], a: TextPos, b: TextPos) -> String {
    let mut parts = Vec::new();
    for_each_span(lines, a, b, |line, first, last| {
        parts.push(line.glyphs[first..=last].iter().map(|g| g.ch).collect::<String>())
    });
    parts.join("\n")
}

/// The paragraph (text block) near a SyncTeX point. A target on a figure or in
/// blank space must not outline an unrelated block elsewhere on the page.
pub fn paragraph_at(lines: &[Line], x: f64, y: f64) -> Option<Rect> {
    let index = nearest_line(lines, x, y)?;
    (lines[index].bbox.distance2(x, y) <= 15.0 * 15.0).then_some(lines[index].block)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(y: f64, text: &str) -> Line {
        Line {
            bbox: Rect::new(0.0, y, text.len() as f64 * 10.0, y + 8.0),
            block: Rect::new(0.0, 0.0, 100.0, 30.0),
            glyphs: text
                .chars()
                .enumerate()
                .map(|(i, ch)| Glyph { ch, x0: i as f64 * 10.0, x1: i as f64 * 10.0 + 10.0 })
                .collect(),
        }
    }

    #[test]
    fn selects_characters_across_lines_in_either_direction() {
        let lines = vec![line(0.0, "hello world"), line(10.0, "second line")];
        let a = position(&lines, 61.0, 4.0).unwrap();
        assert_eq!(a, TextPos { line: 0, glyph: 6 });
        let b = position(&lines, 25.0, 14.0).unwrap();
        assert_eq!(selection_text(&lines, a, b), "world\nsec");
        assert_eq!(selection_text(&lines, b, a), "world\nsec");
        let rects = selection_rects(&lines, a, b);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].x0, 60.0);
        assert_eq!(rects[1].x1, 30.0);
    }

    #[test]
    fn paragraph_requires_nearby_text() {
        let lines = vec![line(0.0, "abc")];
        assert!(paragraph_at(&lines, 5.0, 5.0).is_some());
        assert!(paragraph_at(&lines, 5.0, 500.0).is_none());
    }
}

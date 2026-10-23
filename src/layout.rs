//! Block layout with inline formatting contexts. Produces a flat display
//! list, plus link hit boxes, in physical pixels.

use std::rc::Rc;

use crate::{
    dom::{self, Element, Node},
    lisp::Value,
    paint::Fonts,
    style::{Color, Style},
};

pub enum DisplayItem {
    Rect { x: f32, y: f32, w: f32, h: f32, color: Color },
    /// `y` is the text baseline.
    Text { x: f32, y: f32, text: String, size: f32, bold: bool, color: Color },
}

/// A clickable region produced by an `(a :href ...)` element.
pub struct Link {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub href: Rc<str>,
}

impl Link {
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// A run of text with a single style, from a text node in an inline context.
struct Segment {
    text: Rc<str>,
    style: Rc<Style>,
    href: Option<Rc<str>>,
}

/// A piece of a line: all of its text comes from one segment.
struct Piece {
    x: f32,
    w: f32,
    text: String,
    segment: usize,
}

/// An unbreakable unit. It may span several segments, e.g. `"x" (b "y")`.
#[derive(Default)]
struct Word {
    pieces: Vec<Piece>,
    width: f32,
    /// Segment holding the whitespace before this word; the space takes its style.
    space_segment: Option<usize>,
}

pub struct Layout<'a> {
    fonts: &'a Fonts,
    scale: f32,
    pub items: Vec<DisplayItem>,
    pub links: Vec<Link>,
}

impl<'a> Layout<'a> {
    pub fn new(fonts: &'a Fonts, scale: f32) -> Self {
        Layout { fonts, scale, items: Vec::new(), links: Vec::new() }
    }

    /// Lays out `el` in a box of width `w` at (`x`, `y`); returns the y
    /// just below it, including its bottom margin.
    pub fn block(&mut self, el: &Element, parent: &Style, x: f32, y: f32, w: f32) -> f32 {
        let style = Rc::new(Style::compute(el, parent));
        let pad = style.padding * self.scale;
        let (inner_x, inner_w) = (x + pad, (w - 2.0 * pad).max(0.0));
        let background_index = self.items.len();

        // Consecutive text and inline elements form one inline run; block
        // children end the run and stack below it.
        let mut cy = y + pad;
        let mut run = Vec::new();
        for child in &el.children {
            match child {
                Node::Text(text) => run.push(Segment { text: text.clone(), style: style.clone(), href: None }),
                Node::Element(child) if dom::is_inline(&child.tag) => collect_inline(child, &style, None, None, &mut run),
                Node::Element(child) => {
                    cy = self.inline_run(&mut run, inner_x, cy, inner_w);
                    cy = self.block(child, &style, inner_x, cy, inner_w);
                }
            }
        }
        cy = self.inline_run(&mut run, inner_x, cy, inner_w);

        let h = cy + pad - y;
        if let Some(color) = style.background {
            self.items.insert(background_index, DisplayItem::Rect { x, y, w, h, color });
        }
        y + h + style.margin * self.scale
    }

    /// Greedy word wrap of `run`, which is consumed. Returns the y below the last line.
    fn inline_run(&mut self, run: &mut Vec<Segment>, x: f32, y: f32, w: f32) -> f32 {
        let mut cy = y;
        let mut line: Vec<Piece> = Vec::new();
        let mut line_w = 0.0;
        for word in self.words(run) {
            let space_segment = word.space_segment.unwrap_or(word.pieces[0].segment);
            let mut space_w = self.measure(" ", &run[space_segment].style);
            if !line.is_empty() && line_w + space_w + word.width > w {
                cy = self.line(&line, run, x, cy);
                line.clear();
                line_w = 0.0;
            }
            if line.is_empty() {
                space_w = 0.0;
            } else {
                line.push(Piece { x: line_w, w: space_w, text: " ".into(), segment: space_segment });
            }
            line_w += space_w;
            for piece in word.pieces {
                line.push(Piece { x: line_w + piece.x, ..piece });
            }
            line_w += word.width;
        }
        if !line.is_empty() {
            cy = self.line(&line, run, x, cy);
        }
        run.clear();
        cy
    }

    /// Splits a run into words at whitespace, collapsing whitespace like HTML does.
    fn words(&self, run: &[Segment]) -> Vec<Word> {
        let mut words = Vec::new();
        let mut word = Word::default();
        let mut space_segment = None;
        for (i, segment) in run.iter().enumerate() {
            for c in segment.text.chars() {
                if c.is_whitespace() {
                    if !word.pieces.is_empty() {
                        words.push(std::mem::take(&mut word));
                    }
                    space_segment = Some(i);
                    continue;
                }
                if word.pieces.is_empty() {
                    word.space_segment = space_segment.take();
                }
                match word.pieces.last_mut() {
                    Some(piece) if piece.segment == i => piece.text.push(c),
                    _ => word.pieces.push(Piece { x: 0.0, w: 0.0, text: c.into(), segment: i }),
                }
            }
        }
        if !word.pieces.is_empty() {
            words.push(word);
        }

        for word in &mut words {
            for piece in &mut word.pieces {
                piece.x = word.width;
                piece.w = self.measure(&piece.text, &run[piece.segment].style);
                word.width += piece.w;
            }
        }
        words
    }

    /// Emits one line of pieces, aligned on a shared baseline. Returns the y below it.
    fn line(&mut self, line: &[Piece], run: &[Segment], x: f32, y: f32) -> f32 {
        let (mut above, mut below) = (0.0f32, 0.0f32);
        for piece in line {
            let (height, baseline) = self.line_box(&run[piece.segment].style);
            above = above.max(baseline);
            below = below.max(height - baseline);
        }
        let baseline_y = y + above;

        // Backgrounds first so neighbouring text is never painted over.
        for piece in line {
            let segment = &run[piece.segment];
            let (height, baseline) = self.line_box(&segment.style);
            let (px, py) = (x + piece.x, baseline_y - baseline);
            if let Some(color) = segment.style.background {
                self.items.push(DisplayItem::Rect { x: px, y: py, w: piece.w, h: height, color });
            }
            if let Some(href) = &segment.href {
                self.links.push(Link { x: px, y: py, w: piece.w, h: height, href: href.clone() });
            }
        }
        for piece in line {
            let style = &run[piece.segment].style;
            let size = style.font_size * self.scale;
            if piece.text != " " {
                let text = piece.text.clone();
                let (x, y) = (x + piece.x, baseline_y);
                self.items.push(DisplayItem::Text { x, y, text, size, bold: style.bold, color: style.color });
            }
            if style.underline {
                let thickness = (size / 14.0).max(1.0).round();
                let y = (baseline_y + size * 0.1).round();
                self.items.push(DisplayItem::Rect { x: x + piece.x, y, w: piece.w, h: thickness, color: style.color });
            }
        }
        y + above + below
    }

    /// The line height for `style`, and the baseline's offset from the top of the line.
    fn line_box(&self, style: &Style) -> (f32, f32) {
        let size = style.font_size * self.scale;
        let metrics = self.fonts.get(style.bold).horizontal_line_metrics(size).expect("horizontal font");
        let height = size * 1.3;
        (height, (height - (metrics.ascent - metrics.descent)) / 2.0 + metrics.ascent)
    }

    fn measure(&self, text: &str, style: &Style) -> f32 {
        self.fonts.measure(text, style.font_size * self.scale, style.bold)
    }
}

/// Flattens an inline element into styled segments. Inline backgrounds apply
/// to everything inside them; block elements nested in inline ones are
/// flattened too, as this engine has no anonymous block boxes.
fn collect_inline(el: &Element, parent: &Style, background: Option<Color>, href: Option<Rc<str>>, out: &mut Vec<Segment>) {
    let mut style = Style::compute(el, parent);
    style.background = style.background.or(background);
    let href = match el.attr("href") {
        Some(Value::Str(target)) if &*el.tag == "a" => Some(target.clone()),
        _ => href,
    };
    let style = Rc::new(style);
    for child in &el.children {
        match child {
            Node::Text(text) => out.push(Segment { text: text.clone(), style: style.clone(), href: href.clone() }),
            Node::Element(child) => collect_inline(child, &style, style.background, href.clone(), out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(src: &str, width: f32) -> (Vec<DisplayItem>, Vec<Link>) {
        let Ok(Value::Element(page)) = crate::lisp::run(src) else { panic!("not a page") };
        let fonts = Fonts::load();
        let mut layout = Layout::new(&fonts, 1.0);
        layout.block(&page, &Style::root(), 0.0, 0.0, width);
        (layout.items, layout.links)
    }

    fn texts(items: &[DisplayItem]) -> Vec<(&str, f32, f32)> {
        items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Text { x, y, text, .. } => Some((text.as_str(), *x, *y)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn styled_pieces_without_whitespace_stay_attached() {
        let (items, _) = layout(r#"(page (p "Hello, " (b "world") "!"))"#, 800.0);
        let fonts = Fonts::load();
        let texts = texts(&items);
        let [("Hello,", ..), ("world", world_x, y1), ("!", bang_x, y2)] = texts[..] else { panic!("{texts:?}") };
        assert_eq!(bang_x, world_x + fonts.measure("world", 16.0, true));
        assert_eq!(y1, y2);
    }

    #[test]
    fn inline_elements_wrap_onto_new_lines() {
        let (items, _) = layout(r#"(page :padding 0 (p "aaa " (b "bbb ccc ddd")))"#, 70.0);
        let lines: std::collections::BTreeSet<_> = texts(&items).iter().map(|(_, _, y)| y.to_bits()).collect();
        assert!(lines.len() >= 2);
    }

    #[test]
    fn links_record_hit_boxes_with_their_href() {
        let (_, links) = layout(r#"(page :padding 0 (p "see " (a :href "x.sx" "here")))"#, 800.0);
        assert!(!links.is_empty() && links.iter().all(|l| &*l.href == "x.sx"));
        assert!(links[0].contains(links[0].x + 1.0, links[0].y + 1.0));
        assert!(!links[0].contains(1.0, 1.0), "the unlinked 'see' is not clickable");
    }
}

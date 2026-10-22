//! Block layout with word-wrapped text. Produces a flat display list in
//! physical pixels.

use crate::{
    dom::{Element, Node},
    paint::Fonts,
    style::{Color, Style},
};

pub enum DisplayItem {
    Rect { x: f32, y: f32, w: f32, h: f32, color: Color },
    /// `y` is the text baseline.
    Text { x: f32, y: f32, text: String, size: f32, bold: bool, color: Color },
}

pub struct Layout<'a> {
    fonts: &'a Fonts,
    scale: f32,
    pub items: Vec<DisplayItem>,
}

impl<'a> Layout<'a> {
    pub fn new(fonts: &'a Fonts, scale: f32) -> Self {
        Layout { fonts, scale, items: Vec::new() }
    }

    /// Lays out `el` in a box of width `w` at (`x`, `y`); returns the y
    /// just below it, including its bottom margin.
    pub fn block(&mut self, el: &Element, parent: &Style, x: f32, y: f32, w: f32) -> f32 {
        let style = Style::compute(el, parent);
        let pad = style.padding * self.scale;
        let (inner_x, inner_w) = (x + pad, (w - 2.0 * pad).max(0.0));
        let background_index = self.items.len();

        // Adjacent text children form one inline run; elements are blocks.
        let mut cy = y + pad;
        let mut run = String::new();
        for child in &el.children {
            match child {
                Node::Text(text) => run.push_str(text),
                Node::Element(child) => {
                    cy = self.text(&mut run, &style, inner_x, cy, inner_w);
                    cy = self.block(child, &style, inner_x, cy, inner_w);
                }
            }
        }
        cy = self.text(&mut run, &style, inner_x, cy, inner_w);

        let h = cy + pad - y;
        if let Some(color) = style.background {
            self.items.insert(background_index, DisplayItem::Rect { x, y, w, h, color });
        }
        y + h + style.margin * self.scale
    }

    /// Greedy word wrap of `run`, which is consumed. Returns the y below the last line.
    fn text(&mut self, run: &mut String, style: &Style, x: f32, y: f32, w: f32) -> f32 {
        if run.trim().is_empty() {
            run.clear();
            return y;
        }
        let (size, bold) = (style.font_size * self.scale, style.bold);
        let metrics = self.fonts.get(bold).horizontal_line_metrics(size).expect("horizontal font");
        let line_height = size * 1.3;
        let baseline_offset = (line_height - (metrics.ascent - metrics.descent)) / 2.0 + metrics.ascent;
        let space = self.fonts.measure(" ", size, bold);

        let mut cy = y;
        let mut line = String::new();
        let mut line_w = 0.0;
        let emit = |items: &mut Vec<DisplayItem>, line: &mut String, cy: f32| {
            let text = std::mem::take(line);
            items.push(DisplayItem::Text { x, y: cy + baseline_offset, text, size, bold, color: style.color });
        };
        for word in run.split_whitespace() {
            let word_w = self.fonts.measure(word, size, bold);
            if !line.is_empty() && line_w + space + word_w > w {
                emit(&mut self.items, &mut line, cy);
                cy += line_height;
                line_w = 0.0;
            }
            if !line.is_empty() {
                line.push(' ');
                line_w += space;
            }
            line.push_str(word);
            line_w += word_w;
        }
        emit(&mut self.items, &mut line, cy);
        run.clear();
        cy + line_height
    }
}

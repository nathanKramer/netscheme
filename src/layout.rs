//! Block layout with inline formatting contexts. Produces a flat display
//! list, plus link hit boxes, in physical pixels.

use std::{collections::HashMap, rc::Rc};

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
    /// Items after this are only drawn inside `Clip`, until the next `Clip`.
    Clip(Clip),
}

/// A rectangle, as edges, that drawing is limited to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clip {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Clip {
    pub const NONE: Clip = Clip { x0: f32::NEG_INFINITY, y0: f32::NEG_INFINITY, x1: f32::INFINITY, y1: f32::INFINITY };

    pub fn rect(x: f32, y: f32, w: f32, h: f32) -> Clip {
        Clip { x0: x, y0: y, x1: x + w, y1: y + h }
    }

    pub fn intersect(self, other: Clip) -> Clip {
        Clip { x0: self.x0.max(other.x0), y0: self.y0.max(other.y0), x1: self.x1.min(other.x1), y1: self.y1.min(other.y1) }
    }

    pub fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }
}

/// A frame with a fixed height, which scrolls its page independently.
pub struct Scroller {
    /// Identifies the frame across renders of the same page: frames with a
    /// fixed height are numbered in layout order.
    pub id: usize,
    /// The visible part of the frame.
    pub area: Clip,
    /// The scroll offset used, after clamping.
    pub scroll: f32,
}

/// A clickable region: an `(a :href ...)`, or any element with `:on-click`.
pub struct Link {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub action: Action,
}

#[derive(Clone)]
pub enum Action {
    Go {
        href: Rc<str>,
        /// Location of the framed page the link is on, if it isn't the top-level page.
        base: Option<Rc<str>>,
    },
    /// Call this function with no arguments.
    Run(Value),
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
    action: Option<Action>,
    /// Padding on the left and right of each piece, for inline elements.
    padding: f32,
    /// Whether the text stays on one line, as in a button.
    nowrap: bool,
}

impl Segment {
    fn text(text: &Rc<str>, style: &Rc<Style>) -> Segment {
        Segment { text: text.clone(), style: style.clone(), action: None, padding: 0.0, nowrap: false }
    }
}

/// A piece of a line: all of its text comes from one segment.
struct Piece {
    x: f32,
    w: f32,
    text: String,
    segment: usize,
    /// Space on each side of the text, inside `w`.
    padding: f32,
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
    /// Height of the window, for `(frame :height "fill")`.
    viewport_height: f32,
    /// Requested scroll offsets of fixed-height frames, by `Scroller::id`.
    frame_scrolls: &'a HashMap<usize, f32>,
    next_frame_id: usize,
    clip: Clip,
    /// Location of the framed page being laid out, if any; see `Link::base`.
    base: Option<Rc<str>>,
    pub items: Vec<DisplayItem>,
    pub links: Vec<Link>,
    pub scrollers: Vec<Scroller>,
}

impl<'a> Layout<'a> {
    pub fn new(fonts: &'a Fonts, scale: f32, viewport_height: f32, frame_scrolls: &'a HashMap<usize, f32>) -> Self {
        Layout {
            fonts,
            scale,
            viewport_height,
            frame_scrolls,
            next_frame_id: 0,
            clip: Clip::NONE,
            base: None,
            items: Vec::new(),
            links: Vec::new(),
            scrollers: Vec::new(),
        }
    }

    /// Lays out `el` in a box of width `w` at (`x`, `y`); returns the y
    /// just below it, including its bottom margin.
    pub fn block(&mut self, el: &Element, parent: &Style, x: f32, y: f32, w: f32) -> f32 {
        let style = Rc::new(Style::compute(el, parent));
        let (pad, pad_left) = (style.padding * self.scale, style.padding_left * self.scale);
        let (inner_x, inner_w) = (x + pad_left, (w - pad - pad_left).max(0.0));
        let background_index = self.items.len();

        let top = y + pad;
        let bottom = match &*el.tag {
            "row" => self.row(&el.children, &style, inner_x, top, inner_w),
            "frame" => self.frame(el, inner_x, top, inner_w),
            _ => self.flow(&el.children, &style, inner_x, top, inner_w),
        };

        let h = bottom + pad - y;
        if let Some(color) = style.background {
            self.items.insert(background_index, DisplayItem::Rect { x, y, w, h, color });
        }
        // After the children's, so a click on a child's own handler or link wins.
        if let Some(handler) = el.attr("on-click") {
            self.push_link(Clip::rect(x, y, w, h), Action::Run(handler.clone()));
        }
        y + h + style.margin * self.scale
    }

    /// Normal flow: consecutive text and inline elements form one inline
    /// run; block children end the run and stack below it.
    fn flow(&mut self, children: &[Node], style: &Rc<Style>, x: f32, y: f32, w: f32) -> f32 {
        let mut cy = y;
        let mut run = Vec::new();
        for child in children {
            match child {
                Node::Text(text) => run.push(Segment::text(text, style)),
                Node::Element(child) if dom::is_inline(&child.tag) => self.collect_inline(child, style, None, None, &mut run),
                Node::Element(child) => {
                    cy = self.inline_run(&mut run, x, cy, w);
                    cy = self.block(child, style, x, cy, w);
                }
            }
        }
        self.inline_run(&mut run, x, cy, w)
    }

    /// Equal-width columns, one per child. Returns the bottom of the tallest.
    fn row(&mut self, children: &[Node], style: &Rc<Style>, x: f32, y: f32, w: f32) -> f32 {
        let gap = 16.0 * self.scale;
        let n = children.len().max(1) as f32;
        let column_w = ((w - gap * (n - 1.0)) / n).max(0.0);
        let mut bottom = y;
        for (i, child) in children.iter().enumerate() {
            let column_x = x + i as f32 * (column_w + gap);
            bottom = bottom.max(self.flow(std::slice::from_ref(child), style, column_x, y, column_w));
        }
        bottom
    }

    /// An embedded page, drawn at its `:scale` with a 1px border unless
    /// `:border` is false. With a `:height` (a number, or "fill" to reach the
    /// bottom of the window) the page is clipped to it and scrolls by itself.
    /// The page's styles start fresh rather than inheriting from outside.
    fn frame(&mut self, el: &Element, x: f32, y: f32, w: f32) -> f32 {
        let Some(Node::Element(page)) = el.children.first() else { return y };
        let zoom = match el.attr("scale") {
            Some(Value::Num(n)) => *n as f32,
            _ => 1.0,
        };
        let height = match el.attr("height") {
            Some(Value::Str(fill)) if &**fill == "fill" => Some((self.viewport_height - y).max(40.0 * self.scale)),
            Some(Value::Num(n)) => Some(*n as f32 * self.scale),
            _ => None,
        };
        let base = match el.attr("location") {
            Some(Value::Str(location)) => Some(location.clone()),
            _ => None,
        };
        let border = match el.attr("border") {
            Some(v) if !v.truthy() => 0.0,
            _ => self.scale.round().max(1.0),
        };

        let saved = (self.scale, std::mem::replace(&mut self.base, base));
        self.scale *= zoom;
        let (inner_x, inner_y, inner_w) = (x + border, y + border, w - 2.0 * border);
        let bottom = match height {
            None => self.block(page, &Style::root(), inner_x, inner_y, inner_w) + border,
            Some(h) => {
                let inner_h = (h - 2.0 * border).max(0.0);
                self.scrolling_page(page, Clip::rect(inner_x, inner_y, inner_w, inner_h));
                y + h
            }
        };
        (self.scale, self.base) = saved;

        if border > 0.0 {
            let color = Color(0xcc, 0xcc, 0xcc);
            let h = bottom - y;
            self.items.extend([
                DisplayItem::Rect { x, y, w, h: border, color },
                DisplayItem::Rect { x, y: bottom - border, w, h: border, color },
                DisplayItem::Rect { x, y, w: border, h, color },
                DisplayItem::Rect { x: x + w - border, y, w: border, h, color },
            ]);
        }
        bottom
    }

    /// Lays out `page` inside `area`, scrolled by its frame's offset and
    /// clipped to the area.
    fn scrolling_page(&mut self, page: &Element, area: Clip) {
        let id = self.next_frame_id;
        self.next_frame_id += 1;

        // Measure the page first, to know how far it can scroll.
        let marks = (self.items.len(), self.links.len(), self.scrollers.len());
        let content_bottom = self.block(page, &Style::root(), area.x0, area.y0, area.x1 - area.x0);
        self.items.truncate(marks.0);
        self.links.truncate(marks.1);
        self.scrollers.truncate(marks.2);
        self.next_frame_id = id + 1;

        let max_scroll = (content_bottom - area.y1).max(0.0);
        let scroll = self.frame_scrolls.get(&id).copied().unwrap_or(0.0).clamp(0.0, max_scroll).round();

        let outer = self.clip;
        self.clip = area.intersect(outer);
        self.items.push(DisplayItem::Clip(self.clip));
        self.block(page, &Style::root(), area.x0, area.y0 - scroll, area.x1 - area.x0);
        self.items.push(DisplayItem::Clip(outer));
        self.scrollers.push(Scroller { id, area: self.clip, scroll });
        self.clip = outer;
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
                line.push(Piece { x: line_w, w: space_w, text: " ".into(), segment: space_segment, padding: 0.0 });
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

    /// Splits a run into words at whitespace, collapsing whitespace like HTML
    /// does. Whitespace inside a `nowrap` segment doesn't split it.
    fn words(&self, run: &[Segment]) -> Vec<Word> {
        let mut words = Vec::new();
        let mut word = Word::default();
        let mut space_segment = None;
        for (i, segment) in run.iter().enumerate() {
            let mut inner_space = false;
            for c in segment.text.chars() {
                if c.is_whitespace() {
                    if segment.nowrap && word.pieces.last().is_some_and(|p| p.segment == i) {
                        inner_space = true;
                        continue;
                    }
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
                    Some(piece) if piece.segment == i => {
                        if inner_space {
                            piece.text.push(' ');
                        }
                        piece.text.push(c);
                    }
                    _ => word.pieces.push(Piece { x: 0.0, w: 0.0, text: c.into(), segment: i, padding: 0.0 }),
                }
                inner_space = false;
            }
            // Whitespace at the end of a nowrap segment still separates words.
            if inner_space {
                words.push(std::mem::take(&mut word));
                space_segment = Some(i);
            }
        }
        if !word.pieces.is_empty() {
            words.push(word);
        }

        for word in &mut words {
            for piece in &mut word.pieces {
                let segment = &run[piece.segment];
                piece.padding = segment.padding * self.scale;
                piece.x = word.width;
                piece.w = self.measure(&piece.text, &segment.style) + 2.0 * piece.padding;
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
            if let Some(action) = &segment.action {
                self.push_link(Clip::rect(px, py, piece.w, height), action.clone());
            }
        }
        for piece in line {
            let style = &run[piece.segment].style;
            let size = style.font_size * self.scale;
            if piece.text != " " {
                let text = piece.text.clone();
                let (x, y) = (x + piece.x + piece.padding, baseline_y);
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

    /// Records a clickable area, trimmed to what's visible.
    fn push_link(&mut self, area: Clip, action: Action) {
        let visible = area.intersect(self.clip);
        if !visible.is_empty() {
            let (x, y, w, h) = (visible.x0, visible.y0, visible.x1 - visible.x0, visible.y1 - visible.y0);
            self.links.push(Link { x, y, w, h, action });
        }
    }

    /// Flattens an inline element into styled segments. Inline backgrounds
    /// and actions apply to everything inside them; block elements nested in
    /// inline ones are flattened too, as this engine has no anonymous block boxes.
    fn collect_inline(&self, el: &Element, parent: &Style, background: Option<Color>, action: Option<Action>, out: &mut Vec<Segment>) {
        let mut style = Style::compute(el, parent);
        style.background = style.background.or(background);
        let action = match (el.attr("on-click"), el.attr("href")) {
            (Some(handler), _) => Some(Action::Run(handler.clone())),
            (None, Some(Value::Str(href))) if &*el.tag == "a" => Some(Action::Go { href: href.clone(), base: self.base.clone() }),
            _ => action,
        };
        let (padding, nowrap) = (style.padding, &*el.tag == "button");
        let style = Rc::new(style);
        for child in &el.children {
            match child {
                Node::Text(text) => {
                    out.push(Segment { text: text.clone(), style: style.clone(), action: action.clone(), padding, nowrap })
                }
                Node::Element(child) => self.collect_inline(child, &style, style.background, action.clone(), out),
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(src: &str, width: f32) -> (Vec<DisplayItem>, Vec<Link>) {
        let Ok(Value::Element(page)) = crate::lisp::run(src, &crate::loader::test_context()) else { panic!("not a page") };
        let fonts = Fonts::load();
        let scrolls = HashMap::new();
        let mut layout = Layout::new(&fonts, 1.0, 600.0, &scrolls);
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
        assert!(!links.is_empty() && links.iter().all(|l| matches!(&l.action, Action::Go { href, .. } if &**href == "x.sx")));
        assert!(links[0].contains(links[0].x + 1.0, links[0].y + 1.0));
        assert!(!links[0].contains(1.0, 1.0), "the unlinked 'see' is not clickable");
    }
}

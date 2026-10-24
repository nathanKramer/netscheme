//! Block layout with inline formatting contexts. Produces a flat display
//! list, plus link hit boxes, in physical pixels.

use std::{collections::HashMap, rc::Rc};

use crate::{
    dom::{self, Element, Node},
    image::Image,
    lisp::{self, Value},
    loader::Images,
    paint::Fonts,
    style::{Color, Face, Palette, Style},
};

pub enum DisplayItem {
    Rect { x: f32, y: f32, w: f32, h: f32, color: Color },
    /// An image scaled to `w` × `h`, or an empty box if it couldn't be loaded.
    Image { x: f32, y: f32, w: f32, h: f32, image: Option<Rc<Image>> },
    /// `y` is the text baseline.
    Text { x: f32, y: f32, text: String, size: f32, face: Face, color: Color },
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

/// Which text input has keyboard focus, and where its caret is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Focus {
    /// Inputs are numbered in layout order.
    pub id: usize,
    /// In characters from the start of the value.
    pub cursor: usize,
    /// The other end of the selection, which is empty when this is `cursor`.
    pub anchor: usize,
}

/// A text input, as laid out.
pub struct InputBox {
    pub id: usize,
    /// The visible part of the box.
    pub area: Clip,
    pub element: Rc<Element>,
    pub value: String,
    /// Window x of each caret position: before each character, and after the last.
    pub caret_xs: Vec<f32>,
}

impl InputBox {
    /// The caret position closest to window x.
    pub fn caret_at(&self, x: f32) -> usize {
        let distance = |i: usize| (self.caret_xs[i] - x).abs();
        (0..self.caret_xs.len()).min_by(|&a, &b| distance(a).total_cmp(&distance(b))).unwrap_or(0)
    }
}

/// A line of text, as laid out, for selecting text in. Every character on
/// the page has a position, counting in layout order, so a selection is a
/// range of positions.
pub struct TextLine {
    /// The line's box, and the part of it that's visible.
    pub area: Clip,
    pub visible: Clip,
    /// The innermost fixed-height frame the line is in, by `Scroller::id`.
    pub frame: Option<usize>,
    /// The position of the line's first character.
    pub start: usize,
    pub text: Vec<char>,
    /// Window x of each caret position: before each character, and after the last.
    pub caret_xs: Vec<f32>,
    /// Lines that wrapped from one paragraph share a number, so copying
    /// can join them with a space rather than a line break.
    pub paragraph: usize,
}

impl TextLine {
    /// The position closest to window x.
    pub fn position_at(&self, x: f32) -> usize {
        let distance = |i: usize| (self.caret_xs[i] - x).abs();
        self.start + (0..self.caret_xs.len()).min_by(|&a, &b| distance(a).total_cmp(&distance(b))).unwrap_or(0)
    }

    pub fn end(&self) -> usize {
        self.start + self.text.len()
    }
}

/// A scrollbar, for the page or a frame whose content is taller than the
/// area it's shown in. It's drawn over the right edge of that area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scrollbar {
    /// Where the thumb can move: pressing here scrolls.
    pub track: Clip,
    pub thumb: Clip,
    pub max_scroll: f32,
}

impl Scrollbar {
    /// The scrollbar for content `content_height` tall shown in `area` and
    /// scrolled by `scroll`, or `None` if it all fits.
    pub fn new(area: Clip, content_height: f32, scroll: f32, scale: f32) -> Option<Scrollbar> {
        let view = area.y1 - area.y0;
        let max_scroll = content_height - view;
        if max_scroll < 1.0 || view <= 0.0 {
            return None;
        }
        let (width, inset) = (8.0 * scale, 2.0 * scale);
        let track = Clip { x0: area.x1 - inset - width, y0: area.y0 + inset, x1: area.x1 - inset, y1: area.y1 - inset };
        let track_h = track.y1 - track.y0;
        // In proportion to how much of the content shows, but never too small to grab.
        let thumb_h = (track_h * view / content_height).max(24.0 * scale).min(track_h);
        let top = track.y0 + (track_h - thumb_h) * (scroll / max_scroll).clamp(0.0, 1.0);
        Some(Scrollbar { track, thumb: Clip { y0: top, y1: top + thumb_h, ..track }, max_scroll })
    }

    /// The scroll offset that puts the top of the thumb at `top`.
    pub fn scroll_for(&self, top: f32) -> f32 {
        let range = (self.track.y1 - self.track.y0) - (self.thumb.y1 - self.thumb.y0);
        if range <= 0.0 {
            return 0.0;
        }
        ((top - self.track.y0) / range * self.max_scroll).clamp(0.0, self.max_scroll)
    }

    /// Moved down by `dy`, as when the page around it scrolls.
    pub fn shifted(self, dy: f32) -> Scrollbar {
        let shift = |c: Clip| Clip { y0: c.y0 + dy, y1: c.y1 + dy, ..c };
        Scrollbar { track: shift(self.track), thumb: shift(self.thumb), ..self }
    }
}

/// Text to find in the page, and highlight.
#[derive(Clone, Copy)]
pub struct Search<'a> {
    pub query: &'a str,
    /// Which match is the current one, counting in layout order.
    pub current: usize,
    /// Only look inside frames, so that searching from the chrome finds text
    /// in the site rather than in the toolbar.
    pub frames_only: bool,
}

/// Where some text matched a `Search`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Match {
    pub area: Clip,
    /// The innermost fixed-height frame the match is in, by `Scroller::id`,
    /// or `None` if it's on the page itself.
    pub frame: Option<usize>,
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
    pub bar: Option<Scrollbar>,
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
    /// Set if this segment is something other than text.
    embed: Option<Embed>,
}

/// Something in a line of text that isn't text.
enum Embed {
    Input(Rc<Element>),
    /// An image, or `None` for one that couldn't be loaded, at this size.
    Image(Option<Rc<Image>>, f32, f32, Align),
    /// Takes up whatever room is left on its line.
    Spacer,
}

/// Shares `extra` width between the spacers in a line, moving everything
/// after each one along.
fn stretch(line: &mut [Piece], run: &[Segment], extra: f32) {
    let is_spacer = |piece: &Piece| matches!(run[piece.segment].embed, Some(Embed::Spacer));
    let spacers = line.iter().filter(|piece| is_spacer(piece)).count();
    if spacers == 0 || extra <= 0.0 {
        return;
    }
    let share = extra / spacers as f32;
    let mut shift = 0.0;
    for piece in line {
        piece.x += shift;
        if is_spacer(piece) {
            piece.w = share;
            shift += share;
        }
    }
}

/// Where an image sits on its line.
#[derive(Clone, Copy)]
enum Align {
    /// Its bottom on the baseline, like a letter.
    Baseline,
    /// Centred on the middle of the text's lowercase letters.
    Middle,
}

impl Segment {
    fn text(text: &Rc<str>, style: &Rc<Style>) -> Segment {
        Segment { text: text.clone(), style: style.clone(), action: None, padding: 0.0, nowrap: false, embed: None }
    }

    fn is_text(&self) -> bool {
        self.embed.is_none()
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
    palette: &'static Palette,
    scale: f32,
    /// Height of the window, for `(frame :height "fill")`.
    viewport_height: f32,
    /// Requested scroll offsets of fixed-height frames, by `Scroller::id`.
    frame_scrolls: &'a HashMap<usize, f32>,
    next_frame_id: usize,
    focus: Option<Focus>,
    caret_visible: bool,
    next_input_id: usize,
    clip: Clip,
    /// Location of the framed page being laid out, if any; see `Link::base`.
    base: Option<Rc<str>>,
    pub items: Vec<DisplayItem>,
    pub links: Vec<Link>,
    pub scrollers: Vec<Scroller>,
    pub inputs: Vec<InputBox>,
    /// What to find, and where it was found.
    search: Option<(Search<'a>, Vec<char>)>,
    /// Images that pages have loaded, by location.
    images: Option<&'a Images>,
    pub matches: Vec<Match>,
    /// The selected range of text positions, start first, and every line of text.
    selection: Option<(usize, usize)>,
    pub lines: Vec<TextLine>,
    /// The position of the next character laid out, and the current paragraph.
    next_char: usize,
    paragraph: usize,
    /// How many frames deep layout is, and the fixed-height frame it's in.
    frame_depth: usize,
    scroller: Option<usize>,
}

impl<'a> Layout<'a> {
    pub fn new(
        fonts: &'a Fonts,
        palette: &'static Palette,
        scale: f32,
        viewport_height: f32,
        frame_scrolls: &'a HashMap<usize, f32>,
        focus: Option<Focus>,
        caret_visible: bool,
    ) -> Self {
        Layout {
            fonts,
            palette,
            scale,
            viewport_height,
            frame_scrolls,
            next_frame_id: 0,
            focus,
            caret_visible,
            next_input_id: 0,
            clip: Clip::NONE,
            base: None,
            items: Vec::new(),
            links: Vec::new(),
            scrollers: Vec::new(),
            inputs: Vec::new(),
            search: None,
            images: None,
            matches: Vec::new(),
            selection: None,
            lines: Vec::new(),
            next_char: 0,
            paragraph: 0,
            frame_depth: 0,
            scroller: None,
        }
    }

    pub fn images(&mut self, images: &'a Images) {
        self.images = Some(images);
    }

    /// Highlights the text from position `start` up to `end`.
    pub fn select(&mut self, (start, end): (usize, usize)) {
        self.selection = Some((start, end));
    }

    /// Finds and highlights `search` as the page is laid out.
    pub fn search(&mut self, search: Search<'a>) {
        let query = search.query.chars().flat_map(char::to_lowercase).collect::<Vec<_>>();
        self.search = (!query.is_empty()).then_some((search, query));
    }

    /// Lays out `el` in a box of width `w` at (`x`, `y`); returns the y
    /// just below it, including its bottom margin.
    pub fn block(&mut self, el: &Element, parent: &Style, x: f32, y: f32, w: f32) -> f32 {
        lisp::grow_stack(|| self.block_on_stack(el, parent, x, y, w))
    }

    fn block_on_stack(&mut self, el: &Element, parent: &Style, x: f32, y: f32, w: f32) -> f32 {
        let style = Rc::new(Style::compute(el, parent));
        let (pad, pad_left) = (style.padding * self.scale, style.padding_left * self.scale);
        let (mut inner_x, mut inner_w) = (x + pad_left, (w - pad - pad_left).max(0.0));
        // `:max-width` narrows the content, centred, while the background keeps the full width.
        if let Some(Value::Num(max)) = el.attr("max-width") {
            let max = (*max as f32 * self.scale).max(0.0);
            if inner_w > max {
                inner_x += (inner_w - max) / 2.0;
                inner_w = max;
            }
        }
        let background_index = self.items.len();

        let top = y + pad;
        let bottom = match &*el.tag {
            "row" => self.row(&el.children, &style, inner_x, top, inner_w),
            "frame" => self.frame(el, inner_x, top, inner_w),
            "pre" => self.preformatted(&el.children, &style, inner_x, top),
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

    /// Text that keeps its spaces and line breaks, as code needs. Lines
    /// don't wrap. Inline elements inside can style parts of it.
    fn preformatted(&mut self, children: &[Node], style: &Rc<Style>, x: f32, y: f32) -> f32 {
        let mut run = Vec::new();
        for child in children {
            match child {
                Node::Text(text) => run.push(Segment::text(text, style)),
                Node::Element(child) => self.collect_inline(child, style, None, None, &mut run),
            }
        }
        let mut cy = y;
        let mut line: Vec<Piece> = Vec::new();
        let mut line_w = 0.0;
        for (i, segment) in run.iter().enumerate() {
            for (n, part) in segment.text.split('\n').enumerate() {
                if n > 0 {
                    if line.is_empty() {
                        // A blank line still takes up a line's height.
                        line.push(Piece { x: 0.0, w: 0.0, text: String::new(), segment: i, padding: 0.0 });
                    }
                    self.paragraph += 1;
                    cy = self.line(&line, &run, x, cy);
                    line.clear();
                    line_w = 0.0;
                }
                if !part.is_empty() {
                    let text = part.replace('\t', "    ");
                    let padding = segment.padding * self.scale;
                    let w = self.measure(&text, &segment.style) + 2.0 * padding;
                    line.push(Piece { x: line_w, w, text, segment: i, padding });
                    line_w += w;
                }
            }
        }
        if !line.is_empty() {
            self.paragraph += 1;
            cy = self.line(&line, &run, x, cy);
        }
        cy
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
        self.frame_depth += 1;
        let (inner_x, inner_y, inner_w) = (x + border, y + border, w - 2.0 * border);
        let bottom = match height {
            None => self.block(page, &Style::root(self.palette), inner_x, inner_y, inner_w) + border,
            Some(h) => {
                let inner_h = (h - 2.0 * border).max(0.0);
                self.scrolling_page(page, Clip::rect(inner_x, inner_y, inner_w, inner_h));
                y + h
            }
        };
        (self.scale, self.base) = saved;
        self.frame_depth -= 1;

        if border > 0.0 {
            let color = self.palette.border;
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
        let marks = (self.items.len(), self.links.len(), self.scrollers.len(), self.inputs.len(), self.next_input_id, self.matches.len());
        let text_marks = (self.lines.len(), self.next_char, self.paragraph);
        let content_bottom = self.block(page, &Style::root(self.palette), area.x0, area.y0, area.x1 - area.x0);
        self.items.truncate(marks.0);
        self.links.truncate(marks.1);
        self.scrollers.truncate(marks.2);
        self.inputs.truncate(marks.3);
        self.next_input_id = marks.4;
        self.matches.truncate(marks.5);
        self.lines.truncate(text_marks.0);
        (self.next_char, self.paragraph) = (text_marks.1, text_marks.2);
        self.next_frame_id = id + 1;

        let max_scroll = (content_bottom - area.y1).max(0.0);
        let scroll = self.frame_scrolls.get(&id).copied().unwrap_or(0.0).clamp(0.0, max_scroll).round();

        let outer = self.clip;
        self.clip = area.intersect(outer);
        self.items.push(DisplayItem::Clip(self.clip));
        // The page's background fills the frame, even below its content.
        let background = Style::compute(page, &Style::root(self.palette)).background.unwrap_or(self.palette.page);
        let (w, h) = (area.x1 - area.x0, area.y1 - area.y0);
        self.items.push(DisplayItem::Rect { x: area.x0, y: area.y0, w, h, color: background });
        let outer_scroller = self.scroller.replace(id);
        self.block(page, &Style::root(self.palette), area.x0, area.y0 - scroll, area.x1 - area.x0);
        self.scroller = outer_scroller;
        let bar = Scrollbar::new(area, content_bottom - area.y0, scroll, self.scale);
        if let Some(bar) = bar {
            self.items.push(DisplayItem::Rect { x: bar.thumb.x0, y: bar.thumb.y0, w: bar.thumb.x1 - bar.thumb.x0, h: bar.thumb.y1 - bar.thumb.y0, color: self.palette.scrollbar });
        }
        self.items.push(DisplayItem::Clip(outer));
        self.scrollers.push(Scroller { id, area: self.clip, scroll, bar });
        self.clip = outer;
    }

    /// Greedy word wrap of `run`, which is consumed. Returns the y below the last line.
    fn inline_run(&mut self, run: &mut Vec<Segment>, x: f32, y: f32, w: f32) -> f32 {
        self.paragraph += 1;
        let mut cy = y;
        let mut line: Vec<Piece> = Vec::new();
        let mut line_w = 0.0;
        for word in self.words(run) {
            let space_segment = word.space_segment.unwrap_or(word.pieces[0].segment);
            let mut space_w = self.measure(" ", &run[space_segment].style);
            if !line.is_empty() && line_w + space_w + word.width > w {
                stretch(&mut line, run, w - line_w);
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
            stretch(&mut line, run, w - line_w);
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
            if !segment.is_text() {
                if word.pieces.is_empty() {
                    word.space_segment = space_segment.take();
                }
                word.pieces.push(Piece { x: 0.0, w: 0.0, text: String::new(), segment: i, padding: 0.0 });
                continue;
            }
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
                piece.w = match &segment.embed {
                    Some(Embed::Input(input)) => self.input_width(input),
                    Some(Embed::Image(_, w, ..)) => *w,
                    Some(Embed::Spacer) => 0.0,
                    None => self.measure(&piece.text, &segment.style) + 2.0 * piece.padding,
                };
                word.width += piece.w;
            }
        }
        words
    }

    /// Emits one line of pieces, aligned on a shared baseline. Returns the y below it.
    fn line(&mut self, line: &[Piece], run: &[Segment], x: f32, y: f32) -> f32 {
        let (mut above, mut below) = (0.0f32, 0.0f32);
        for piece in line {
            let (height, baseline) = self.piece_box(&run[piece.segment]);
            above = above.max(baseline);
            below = below.max(height - baseline);
        }
        let baseline_y = y + above;

        // Backgrounds first so neighbouring text is never painted over.
        for piece in line {
            let segment = &run[piece.segment];
            let (height, baseline) = self.piece_box(segment);
            let (px, py) = (x + piece.x, baseline_y - baseline);
            match &segment.embed {
                Some(Embed::Input(input)) => {
                    self.input(input, &segment.style, Clip::rect(px, py, piece.w, height));
                    continue;
                }
                Some(Embed::Image(image, w, h, _)) => {
                    self.items.push(DisplayItem::Image { x: px, y: py, w: *w, h: *h, image: image.clone() });
                    if let Some(action) = &segment.action {
                        self.push_link(Clip::rect(px, py, *w, *h), action.clone());
                    }
                    continue;
                }
                Some(Embed::Spacer) => continue,
                None => {}
            }
            if let Some(color) = segment.style.background {
                self.items.push(DisplayItem::Rect { x: px, y: py, w: piece.w, h: height, color });
            }
            if let Some(action) = &segment.action {
                self.push_link(Clip::rect(px, py, piece.w, height), action.clone());
            }
        }
        // Highlights go over backgrounds but under the text.
        let chars = self.line_chars(line, run, x);
        self.select_in_line(&chars, x, y, above + below);
        self.find_in_line(&chars, y, above + below);
        for piece in line.iter().filter(|piece| run[piece.segment].is_text()) {
            let style = &run[piece.segment].style;
            let size = style.font_size * self.scale;
            if !piece.text.trim().is_empty() {
                let text = piece.text.clone();
                let (x, y) = (x + piece.x + piece.padding, baseline_y);
                self.items.push(DisplayItem::Text { x, y, text, size, face: style.face(), color: style.color });
            }
            if style.underline {
                let thickness = (size / 14.0).max(1.0).round();
                let y = (baseline_y + size * 0.1).round();
                self.items.push(DisplayItem::Rect { x: x + piece.x, y, w: piece.w, h: thickness, color: style.color });
            }
        }
        y + above + below
    }

    /// Each character of a line's text, with its left and right edges.
    fn line_chars(&self, line: &[Piece], run: &[Segment], x: f32) -> Vec<(char, f32, f32)> {
        let mut chars = Vec::new();
        for piece in line.iter().filter(|piece| run[piece.segment].is_text()) {
            let style = &run[piece.segment].style;
            let mut left = x + piece.x + piece.padding;
            for c in piece.text.chars() {
                let right = left + self.measure(c.encode_utf8(&mut [0; 4]), style);
                chars.push((c, left, right));
                left = right;
            }
        }
        chars
    }

    /// Records a line of `chars`, `h` high at (`x`, `y`), for selecting
    /// text in, and highlights the part of it that's selected.
    fn select_in_line(&mut self, chars: &[(char, f32, f32)], x: f32, y: f32, h: f32) {
        let start = self.next_char;
        self.next_char += chars.len();
        if let Some((from, to)) = self.selection.map(|(a, b)| (a.max(start), b.min(self.next_char)))
            && from < to
        {
            let (x0, x1) = (chars[from - start].1, chars[to - start - 1].2);
            self.items.push(DisplayItem::Rect { x: x0, y, w: x1 - x0, h, color: self.palette.selection });
        }
        let caret_xs: Vec<f32> = chars.iter().map(|c| c.1).chain(chars.last().map(|c| c.2)).collect();
        let (x0, x1) = (caret_xs.first().copied().unwrap_or(x), caret_xs.last().copied().unwrap_or(x));
        let area = Clip::rect(x0, y, x1 - x0, h);
        self.lines.push(TextLine {
            area,
            visible: area.intersect(self.clip),
            frame: self.scroller,
            start,
            text: chars.iter().map(|c| c.0).collect(),
            caret_xs,
            paragraph: self.paragraph,
        });
    }

    /// Highlights matches of the search in a line of `chars`, `h` high at
    /// `y`, and records where they are. A match can span pieces of the line,
    /// but not continue onto the next.
    fn find_in_line(&mut self, chars: &[(char, f32, f32)], y: f32, h: f32) {
        let Some((search, query)) = &self.search else { return };
        if search.frames_only && self.frame_depth == 0 {
            return;
        }
        let lower = |c: char| c.to_lowercase().next().unwrap_or(c);
        let n = query.len();
        let mut i = 0;
        while n > 0 && i + n <= chars.len() {
            if chars[i..i + n].iter().zip(query).all(|((c, ..), q)| lower(*c) == *q) {
                let (x0, x1) = (chars[i].1, chars[i + n - 1].2);
                let color = if self.matches.len() == search.current { self.palette.current_match } else { self.palette.found };
                self.items.push(DisplayItem::Rect { x: x0, y, w: x1 - x0, h, color });
                self.matches.push(Match { area: Clip::rect(x0, y, x1 - x0, h), frame: self.scroller });
                i += n;
            } else {
                i += 1;
            }
        }
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
    fn collect_inline(&self, el: &Rc<Element>, parent: &Style, background: Option<Color>, action: Option<Action>, out: &mut Vec<Segment>) {
        let mut style = Style::compute(el, parent);
        if &*el.tag == "input" {
            let style = Rc::new(style);
            out.push(Segment { text: "".into(), style, action: None, padding: 0.0, nowrap: true, embed: Some(Embed::Input(el.clone())) });
            return;
        }
        style.background = style.background.or(background);
        let action = match (el.attr("on-click"), el.attr("href")) {
            (Some(handler), _) => Some(Action::Run(handler.clone())),
            (None, Some(Value::Str(href))) if &*el.tag == "a" => Some(Action::Go { href: href.clone(), base: self.base.clone() }),
            _ => action,
        };
        if &*el.tag == "spacer" {
            out.push(Segment { text: "".into(), style: Rc::new(style), action: None, padding: 0.0, nowrap: true, embed: Some(Embed::Spacer) });
            return;
        }
        if &*el.tag == "img" {
            let image = self.image_box(el);
            out.push(Segment { text: "".into(), style: Rc::new(style), action, padding: 0.0, nowrap: true, embed: Some(image) });
            return;
        }
        let (padding, nowrap) = (style.padding, matches!(&*el.tag, "button" | "code"));
        let style = Rc::new(style);
        for child in &el.children {
            match child {
                Node::Text(text) => {
                    out.push(Segment { text: text.clone(), style: style.clone(), action: action.clone(), padding, nowrap, embed: None })
                }
                Node::Element(child) => self.collect_inline(child, &style, style.background, action.clone(), out),
            }
        }
    }

    /// An image's picture and size. Given a `:width` or a `:height`, it
    /// keeps the image's proportions; given neither, it's its natural size.
    fn image_box(&self, el: &Element) -> Embed {
        let location = match el.attr("location") {
            Some(Value::Str(location)) => location,
            _ => "",
        };
        let image = self.images.and_then(|images| images.get(location)).and_then(|image| image.as_ref().ok()).cloned();
        let attr = |name| match el.attr(name) {
            Some(Value::Num(n)) => Some((*n as f32).max(0.0)),
            _ => None,
        };
        // A missing image keeps its space, or a small box's if it has no size.
        let natural = image.as_ref().map_or((24.0, 24.0), |image| (image.width as f32, image.height as f32));
        let (w, h) = match (attr("width"), attr("height")) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, w * natural.1 / natural.0.max(1.0)),
            (None, Some(h)) => (h * natural.0 / natural.1.max(1.0), h),
            (None, None) => natural,
        };
        let align = match el.attr("align") {
            Some(Value::Str(align)) if &**align == "middle" => Align::Middle,
            _ => Align::Baseline,
        };
        Embed::Image(image, w * self.scale, h * self.scale, align)
    }

    /// Like `line_box`, but for any piece of a line.
    fn piece_box(&self, segment: &Segment) -> (f32, f32) {
        let (height, baseline) = self.line_box(&segment.style);
        match segment.embed {
            Some(Embed::Input(_)) => {
                let inset = self.input_inset(&segment.style);
                (height + 2.0 * inset, baseline + inset)
            }
            Some(Embed::Image(_, _, h, Align::Baseline)) => (h, h),
            // Lowercase letters are about half the font size tall.
            Some(Embed::Image(_, _, h, Align::Middle)) => (h, h / 2.0 + segment.style.font_size * self.scale * 0.25),
            Some(Embed::Spacer) => (0.0, 0.0),
            None => (height, baseline),
        }
    }

    /// Space between an input's edge and its text: padding plus a 1px border.
    fn input_inset(&self, style: &Style) -> f32 {
        (style.padding + 1.0) * self.scale
    }

    fn input_width(&self, input: &Element) -> f32 {
        let width = match input.attr("width") {
            Some(Value::Num(n)) => *n as f32,
            _ => 240.0,
        };
        width * self.scale
    }

    /// Draws a text input filling `area`: its value (or placeholder) clipped
    /// to the box and scrolled to keep the caret in view, plus the caret if
    /// the input has focus.
    fn input(&mut self, el: &Rc<Element>, style: &Style, area: Clip) {
        let id = self.next_input_id;
        self.next_input_id += 1;
        let focus = self.focus.filter(|focus| focus.id == id);

        let (x, y, w, h) = (area.x0, area.y0, area.x1 - area.x0, area.y1 - area.y0);
        let thin = self.scale.round().max(1.0);
        let (border, border_color) = match focus {
            Some(_) => (2.0 * thin, style.palette.focus),
            None => (thin, style.palette.border),
        };
        let background = style.background.unwrap_or(style.palette.input);
        self.items.push(DisplayItem::Rect { x, y, w, h, color: border_color });
        self.items.push(DisplayItem::Rect { x: x + border, y: y + border, w: w - 2.0 * border, h: h - 2.0 * border, color: background });

        let value = match el.attr("value") {
            None | Some(Value::Nil) => String::new(),
            Some(value) => value.to_string(),
        };
        let (size, face) = (style.font_size * self.scale, style.face());
        let inset = self.input_inset(style);
        let (inner_x, inner_w) = (x + inset, (w - 2.0 * inset).max(0.0));
        let (line_height, baseline) = self.line_box(style);

        let mut offsets = vec![0.0];
        for c in value.chars() {
            offsets.push(offsets[offsets.len() - 1] + self.fonts.measure(c.encode_utf8(&mut [0; 4]), size, face));
        }
        let len = value.chars().count();
        let cursor = focus.map(|focus| focus.cursor.min(len));
        let selection = focus.map(|focus| (focus.anchor.min(len), focus.cursor.min(len))).filter(|(a, c)| a != c);
        let scroll_x = cursor.map_or(0.0, |c| (offsets[c] - (inner_w - thin)).max(0.0));
        let text_x = inner_x - scroll_x;

        let outer = self.clip;
        let visible = area.intersect(outer);
        self.items.push(DisplayItem::Clip(Clip { x0: inner_x, x1: inner_x + inner_w, ..visible }));
        if let Some((a, c)) = selection {
            let (start, end) = (offsets[a.min(c)], offsets[a.max(c)]);
            let highlight = style.palette.selection;
            self.items.push(DisplayItem::Rect { x: text_x + start, y: y + inset, w: end - start, h: line_height, color: highlight });
        }
        let (text, color) = match el.attr("placeholder") {
            Some(Value::Str(placeholder)) if value.is_empty() => (placeholder.to_string(), style.palette.placeholder),
            _ => (value.clone(), style.color),
        };
        self.items.push(DisplayItem::Text { x: text_x, y: y + inset + baseline, text, size, face, color });
        // Like most text fields, no caret while there's a selection.
        if let Some(c) = cursor
            && self.caret_visible
            && selection.is_none()
        {
            let caret = DisplayItem::Rect { x: text_x + offsets[c], y: y + inset, w: thin, h: line_height, color: style.color };
            self.items.push(caret);
        }
        self.items.push(DisplayItem::Clip(outer));

        if !visible.is_empty() {
            let caret_xs = offsets.iter().map(|offset| text_x + offset).collect();
            self.inputs.push(InputBox { id, area: visible, element: el.clone(), value, caret_xs });
        }
    }

    /// The line height for `style`, and the baseline's offset from the top of the line.
    fn line_box(&self, style: &Style) -> (f32, f32) {
        let size = style.font_size * self.scale;
        let metrics = self.fonts.get(style.face()).horizontal_line_metrics(size).expect("horizontal font");
        let height = size * 1.3;
        (height, (height - (metrics.ascent - metrics.descent)) / 2.0 + metrics.ascent)
    }

    fn measure(&self, text: &str, style: &Style) -> f32 {
        self.fonts.measure(text, style.font_size * self.scale, style.face())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(src: &str, width: f32) -> (Vec<DisplayItem>, Vec<Link>) {
        let Ok(Value::Element(page)) = crate::lisp::run(src, &crate::loader::test_context()) else { panic!("not a page") };
        let fonts = Fonts::load();
        let scrolls = HashMap::new();
        let mut layout = Layout::new(&fonts, &Palette::LIGHT, 1.0, 600.0, &scrolls, None, true);
        layout.block(&page, &Style::root(&Palette::LIGHT), 0.0, 0.0, width);
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
        assert_eq!(bang_x, world_x + fonts.measure("world", 16.0, Face { bold: true, mono: false }));
        assert_eq!(y1, y2);
    }

    #[test]
    fn inline_elements_wrap_onto_new_lines() {
        let (items, _) = layout(r#"(page :padding 0 (p "aaa " (b "bbb ccc ddd")))"#, 70.0);
        let lines: std::collections::BTreeSet<_> = texts(&items).iter().map(|(_, _, y)| y.to_bits()).collect();
        assert!(lines.len() >= 2);
    }

    #[test]
    fn preformatted_text_keeps_spaces_and_line_breaks() {
        let (items, _) = layout(r#"(page :padding 0 (pre :padding 0 "a  b\n\n  c" (b "d")))"#, 800.0);
        let texts = texts(&items);
        assert_eq!(texts.iter().map(|(text, ..)| *text).collect::<Vec<_>>(), ["a  b", "  c", "d"]);
        let (first, third) = (texts[0].2, texts[1].2);
        assert!(third - first > 2.0 * 14.0, "a blank line between them");
        assert_eq!(texts[2].2, third, "styled text continues the line");
        let mono = Fonts::load().measure("  c", 14.0, Face { bold: false, mono: true });
        assert_eq!(texts[2].1, mono, "in the monospaced font");
    }

    #[test]
    fn max_width_centres_the_content() {
        let (items, _) = layout(r#"(page :padding 0 :max-width 100 (p "x"))"#, 300.0);
        assert_eq!(texts(&items)[0].1, 100.0);
        let (items, _) = layout(r#"(page :padding 0 :max-width 500 (p "x"))"#, 300.0);
        assert_eq!(texts(&items)[0].1, 0.0, "no effect when there's room");
    }

    #[test]
    fn a_spacer_pushes_the_rest_of_its_line_to_the_end() {
        let fonts = Fonts::load();
        let width = |text| fonts.measure(text, 16.0, Face::default());
        let (items, _) = layout(r#"(page :padding 0 (p "left" (spacer) "right"))"#, 300.0);
        let one = texts(&items);
        assert_eq!(one[0].1, 0.0);
        assert!((one[1].1 + width("right") - 300.0).abs() < 0.01, "right ends at the edge: {one:?}");

        let (items, _) = layout(r#"(page :padding 0 (p (spacer) "a" (spacer) "b"))"#, 300.0);
        let two = texts(&items);
        let gap = (300.0 - width("a") - width("b")) / 2.0;
        assert!((two[0].1 - gap).abs() < 0.01 && (two[1].1 + width("b") - 300.0).abs() < 0.01, "two share the room: {two:?}");
    }

    #[test]
    fn inline_code_stays_on_one_line() {
        let (items, _) = layout(r#"(page :padding 0 (p "aaa " (code "b c d")))"#, 60.0);
        assert!(texts(&items).iter().any(|(text, ..)| *text == "b c d"), "{:?}", texts(&items));
    }

    /// Lays out `src` with a 40 × 20 image at "pic.png".
    fn layout_with_image(src: &str) -> (Vec<DisplayItem>, Vec<Link>) {
        let Ok(Value::Element(page)) = crate::lisp::run(src, &crate::loader::test_context()) else { panic!("not a page") };
        let image = Image { id: 0, width: 40, height: 20, pixels: vec![255; 40 * 20 * 4] };
        let images = Images::from([("pic.png".to_string(), Ok(Rc::new(image)))]);
        let (fonts, scrolls) = (Fonts::load(), HashMap::new());
        let mut layout = Layout::new(&fonts, &Palette::LIGHT, 1.0, 600.0, &scrolls, None, true);
        layout.images(&images);
        layout.block(&page, &Style::root(&Palette::LIGHT), 0.0, 0.0, 800.0);
        (layout.items, layout.links)
    }

    fn image_boxes(items: &[DisplayItem]) -> Vec<(f32, f32, bool)> {
        items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Image { w, h, image, .. } => Some((*w, *h, image.is_some())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn images_keep_their_proportions() {
        let (items, _) = layout_with_image(
            r#"(page (p (img :src "pic.png") (img :src "pic.png" :width 80) (img :src "pic.png" :height 10)
                        (img :src "pic.png" :width 5 :height 50) (img :src "nope.png") (img :src "nope.png" :width 30)))"#,
        );
        let natural = (40.0, 20.0, true);
        assert_eq!(image_boxes(&items), [natural, (80.0, 40.0, true), (20.0, 10.0, true), (5.0, 50.0, true), (24.0, 24.0, false), (30.0, 30.0, false)]);
    }

    #[test]
    fn images_sit_on_the_line_and_can_be_links() {
        let (items, links) = layout_with_image(r#"(page :padding 0 (p "x" (a :href "big.sx" (img :src "pic.png" :width 80))))"#);
        let image_bottom = items.iter().find_map(|item| match item {
            DisplayItem::Image { y, h, .. } => Some(y + h),
            _ => None,
        });
        assert_eq!(image_bottom, Some(texts(&items)[0].2), "on the baseline");
        assert!(links.iter().any(|link| link.w == 80.0 && link.h == 40.0), "the image is the link");

        let (items, _) = layout_with_image(r#"(page :padding 0 (p "x" (img :src "pic.png" :width 80 :align "middle")))"#);
        let (image_top, baseline) = (items.iter().find_map(|item| match item {
            DisplayItem::Image { y, .. } => Some(*y),
            _ => None,
        }), texts(&items)[0].2);
        assert!(image_top.unwrap() + 40.0 > baseline, "centred on the text, so it reaches below the baseline");
    }

        fn search(src: &str, query: &str, frames_only: bool) -> Vec<Match> {
        let Ok(Value::Element(page)) = crate::lisp::run(src, &crate::loader::test_context()) else { panic!("not a page") };
        let fonts = Fonts::load();
        let scrolls = HashMap::new();
        let mut layout = Layout::new(&fonts, &Palette::LIGHT, 1.0, 600.0, &scrolls, None, true);
        layout.search(Search { query, current: 0, frames_only });
        layout.block(&page, &Style::root(&Palette::LIGHT), 0.0, 0.0, 800.0);
        layout.matches
    }

    #[test]
    fn searching_finds_text_across_pieces_of_a_line() {
        let src = r#"(page :padding 0 (p "Hello, " (b "wor") "ld. Hello!") (p "hello"))"#;
        assert_eq!(search(src, "hello", false).len(), 3, "ignoring case");
        let across = search(src, "o, WORLD", false);
        assert_eq!(across.len(), 1, "spanning a space and a bold piece");
        let fonts = Fonts::load();
        let start = fonts.measure("Hell", 16.0, Face::default());
        assert!((across[0].area.x0 - start).abs() < 0.01, "{:?} starts after \"Hell\"", across[0].area);
        assert_eq!(search(src, "hello", true).len(), 0, "outside frames, when only searching inside them");
        assert_eq!(search(src, "", false).len(), 0);
    }

    #[test]
    fn links_record_hit_boxes_with_their_href() {
        let (_, links) = layout(r#"(page :padding 0 (p "see " (a :href "x.sx" "here")))"#, 800.0);
        assert!(!links.is_empty() && links.iter().all(|l| matches!(&l.action, Action::Go { href, .. } if &**href == "x.sx")));
        assert!(links[0].contains(links[0].x + 1.0, links[0].y + 1.0));
        assert!(!links[0].contains(1.0, 1.0), "the unlinked 'see' is not clickable");
    }
}

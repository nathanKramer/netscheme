//! Block layout with inline formatting contexts. Produces a flat display
//! list, plus link hit boxes, in physical pixels.

use std::{collections::HashMap, ops::Range, rc::Rc};

use crate::{
    canvas::{self, Contour, Point},
    dom::{self, Element, Node},
    highlight,
    image::Image,
    lisp::{self, Value},
    loader::Images,
    paint::Fonts,
    style::{self, Color, Face, Palette, Style},
};

#[derive(Clone)]
pub enum DisplayItem {
    /// A rectangle whose corners are rounded by `radius`, at most half its shorter side.
    Rect { x: f32, y: f32, w: f32, h: f32, color: Color, radius: f32 },
    /// The edge of a box, `width` thick just inside it, with rounded corners like `Rect`'s.
    Border { x: f32, y: f32, w: f32, h: f32, width: f32, color: Color, radius: f32 },
    /// An image scaled to `w` × `h`, or an empty box if it couldn't be loaded.
    Image { x: f32, y: f32, w: f32, h: f32, image: Option<Rc<Image>>, radius: f32 },
    /// `y` is the text baseline.
    Text { x: f32, y: f32, text: String, size: f32, face: Face, color: Color },
    /// The area inside `contours`, by the nonzero rule, with smoothed edges.
    Polygon { contours: Vec<Contour>, color: Color },
    /// Items after this are only drawn inside `Clip`, until the next `Clip`.
    Clip(Clip),
    /// Items after this scroll with this fixed-height frame, by `Scroller::id`,
    /// or with the page if `None`, until the next `Layer`.
    Layer(Option<usize>),
}

/// Images are the same if they're the same picture.
impl PartialEq for DisplayItem {
    fn eq(&self, other: &DisplayItem) -> bool {
        use DisplayItem::*;
        let same_image = |a: &Option<Rc<crate::image::Image>>, b: &Option<Rc<crate::image::Image>>| match (a, b) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            (a, b) => a.is_none() && b.is_none(),
        };
        match (self, other) {
            (Rect { x, y, w, h, color, radius }, Rect { x: x2, y: y2, w: w2, h: h2, color: c2, radius: r2 }) => {
                (x, y, w, h, color, radius) == (x2, y2, w2, h2, c2, r2)
            }
            (Border { x, y, w, h, width, color, radius }, Border { x: x2, y: y2, w: w2, h: h2, width: t2, color: c2, radius: r2 }) => {
                (x, y, w, h, width, color, radius) == (x2, y2, w2, h2, t2, c2, r2)
            }
            (Image { x, y, w, h, image, radius }, Image { x: x2, y: y2, w: w2, h: h2, image: i2, radius: r2 }) => {
                (x, y, w, h, radius) == (x2, y2, w2, h2, r2) && same_image(image, i2)
            }
            (Text { x, y, text, size, face, color }, Text { x: x2, y: y2, text: t2, size: s2, face: f2, color: c2 }) => {
                (x, y, text, size, face, color) == (x2, y2, t2, s2, f2, c2)
            }
            (Polygon { contours, color }, Polygon { contours: c2, color: col2 }) => (contours, color) == (c2, col2),
            (Clip(a), Clip(b)) => a == b,
            (Layer(a), Layer(b)) => a == b,
            _ => false,
        }
    }
}

impl DisplayItem {
    /// The item with its y coordinates snapped to 64ths of a pixel, then
    /// moved down by `dy`, a whole number of pixels. Snapped, a coordinate
    /// moves exactly, so content painted at two scroll offsets lines up
    /// to the bit, as moving its pixels needs. Layout in browsers works
    /// in 64ths of a pixel for the same sort of reason.
    pub fn placed(self, dy: f32) -> DisplayItem {
        let snap = |y: f32| (y * 64.0).round() / 64.0 + dy;
        use DisplayItem::*;
        match self {
            Rect { x, y, w, h, color, radius } => Rect { x, y: snap(y), w, h, color, radius },
            Border { x, y, w, h, width, color, radius } => Border { x, y: snap(y), w, h, width, color, radius },
            Image { x, y, w, h, image, radius } => Image { x, y: snap(y), w, h, image, radius },
            Text { x, y, text, size, face, color } => Text { x, y: snap(y), text, size, face, color },
            Polygon { mut contours, color } => {
                contours.iter_mut().flatten().for_each(|point| point.1 = snap(point.1));
                Polygon { contours, color }
            }
            Clip(clip) => Clip(crate::layout::Clip { y0: snap(clip.y0), y1: snap(clip.y1), ..clip }),
            Layer(layer) => Layer(layer),
        }
    }

    /// The item moved down by `dy`.
    pub fn shifted(self, dy: f32) -> DisplayItem {
        use DisplayItem::*;
        match self {
            Rect { x, y, w, h, color, radius } => Rect { x, y: y + dy, w, h, color, radius },
            Border { x, y, w, h, width, color, radius } => Border { x, y: y + dy, w, h, width, color, radius },
            Image { x, y, w, h, image, radius } => Image { x, y: y + dy, w, h, image, radius },
            Text { x, y, text, size, face, color } => Text { x, y: y + dy, text, size, face, color },
            Polygon { mut contours, color } => {
                contours.iter_mut().flatten().for_each(|point| point.1 += dy);
                Polygon { contours, color }
            }
            Clip(clip) => Clip(clip.shifted(dy)),
            Layer(layer) => Layer(layer),
        }
    }
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

    /// Whether `other` is entirely inside this.
    pub fn covers(self, other: Clip) -> bool {
        self.x0 <= other.x0 && self.y0 <= other.y0 && self.x1 >= other.x1 && self.y1 >= other.y1
    }

    /// The rectangle moved down by `dy`.
    pub fn shifted(self, dy: f32) -> Clip {
        Clip { y0: self.y0 + dy, y1: self.y1 + dy, ..self }
    }

    /// The smallest rectangle containing both.
    pub fn union(self, other: Clip) -> Clip {
        Clip { x0: self.x0.min(other.x0), y0: self.y0.min(other.y0), x1: self.x1.max(other.x1), y1: self.y1.max(other.y1) }
    }

    /// Grown out to whole pixels.
    pub fn round_out(self) -> Clip {
        Clip { x0: self.x0.floor(), y0: self.y0.floor(), x1: self.x1.ceil(), y1: self.y1.ceil() }
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

/// What canvases' `:draw` functions returned, by the address of the canvas
/// element. A canvas draws these after its children.
pub type Drawings = HashMap<usize, Vec<Node>>;

/// A canvas that listens for keys, as laid out. Clicking it gives it the
/// keyboard.
pub struct KeyTarget {
    /// Key targets are numbered in layout order.
    pub id: usize,
    /// The visible part of the canvas.
    pub area: Clip,
    pub element: Rc<Element>,
}

/// Whether `el` has `:on-key-down` or `:on-key-up` handlers. `nil` is none.
pub fn listens_for_keys(el: &Element) -> bool {
    ["on-key-down", "on-key-up"].iter().any(|name| el.attr(name).is_some_and(Value::truthy))
}

/// A text input or textarea, as laid out.
pub struct InputBox {
    pub id: usize,
    /// The visible part of the box.
    pub area: Clip,
    pub element: Rc<Element>,
    pub value: String,
    /// Window x of each caret position: before each character, and after the last.
    pub caret_xs: Vec<f32>,
    /// Window y of the top of the line each caret position is on, and how
    /// tall lines are. An input's carets are all on one line.
    pub caret_ys: Vec<f32>,
    pub line_height: f32,
}

impl InputBox {
    /// The caret position closest to a point in the window: on the line
    /// nearest it, then nearest across.
    pub fn caret_at(&self, x: f32, y: f32) -> usize {
        let distance = |i: usize| {
            let top = self.caret_ys[i];
            ((top - y).max(y - top - self.line_height).max(0.0), (self.caret_xs[i] - x).abs())
        };
        (0..self.caret_xs.len()).min_by(|&a, &b| distance(a).partial_cmp(&distance(b)).expect("distances are numbers")).unwrap_or(0)
    }

    /// Whether it holds lines of text, as a textarea does.
    pub fn multiline(&self) -> bool {
        &*self.element.tag == "textarea"
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

/// Where an element with an `:id` was laid out, for a location that ends
/// in `#` and the id to scroll to.
pub struct Anchor {
    pub id: Rc<str>,
    pub area: Clip,
    /// The innermost fixed-height frame the element is in, as for `Match`.
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
        /// Whether only a Ctrl+click follows it: `(a :ctrl-click true ...)`.
        ctrl: bool,
    },
    /// Call this function with no arguments.
    Run(Value),
}

impl Link {
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    pub fn needs_ctrl(&self) -> bool {
        matches!(self.action, Action::Go { ctrl: true, .. })
    }
}

/// An `overlay`, drawn over the page rather than in its flow.
pub struct Overlay {
    /// What it covers, which clicks don't go through: its box, or with a
    /// `:backdrop`, the whole view.
    pub area: Clip,
    /// Its `:on-dismiss` handler.
    pub dismiss: Option<Value>,
    /// What it laid out, by index into the links, inputs, key targets and lines.
    pub links: Range<usize>,
    pub inputs: Range<usize>,
    pub key_targets: Range<usize>,
    pub lines: Range<usize>,
}

/// An overlay waiting for its page to finish, with the box it's anchored to
/// and the style and `:group-hover` it inherits from where it was written.
#[derive(Clone)]
struct PendingOverlay {
    el: Rc<Element>,
    parent: Rc<Style>,
    anchor: Clip,
    group_hover: usize,
}

/// How many lines of text tall an input is: a textarea's `:rows`, or 4.
fn input_rows(input: &Element) -> usize {
    match input.attr("rows") {
        _ if &*input.tag != "textarea" => 1,
        Some(Value::Num(n)) => (*n as usize).max(1),
        _ => 4,
    }
}

/// Whether `node` is an overlay, which takes no room where it's written.
fn is_overlay(node: &Node) -> bool {
    matches!(node, Node::Element(el) if &*el.tag == "overlay")
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
/// Space inside each edge of a box.
struct Insets {
    top: f32,
    right: f32,
    bottom: f32,
    left: f32,
}

enum Embed {
    /// A text input, this wide.
    Input(Rc<Element>, f32),
    /// An image, or `None` for one that couldn't be loaded, at this size.
    Image(Option<Rc<Image>>, f32, f32, Align),
    /// Takes up whatever room is left on its line.
    Spacer,
    /// A canvas and its shapes, at this size.
    Canvas(Rc<Element>, f32, f32),
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
    /// The key target that has the keyboard, if one does.
    key_focus: Option<usize>,
    next_key_target_id: usize,
    clip: Clip,
    /// Location of the framed page being laid out, if any; see `Link::base`.
    base: Option<Rc<str>>,
    pub items: Vec<DisplayItem>,
    pub links: Vec<Link>,
    pub scrollers: Vec<Scroller>,
    pub inputs: Vec<InputBox>,
    pub key_targets: Vec<KeyTarget>,
    /// What to find, and where it was found.
    search: Option<(Search<'a>, Vec<char>)>,
    /// Images that pages have loaded, by location.
    images: Option<&'a Images>,
    drawings: Option<&'a Drawings>,
    pub matches: Vec<Match>,
    pub anchors: Vec<Anchor>,
    /// The selected range of text positions, start first, and every line of text.
    selection: Option<(usize, usize)>,
    pub lines: Vec<TextLine>,
    /// The position of the next character laid out, and the current paragraph.
    next_char: usize,
    paragraph: usize,
    /// How many frames deep layout is, and the fixed-height frame it's in.
    frame_depth: usize,
    scroller: Option<usize>,
    /// Where the mouse pointer is, for `:hover` styles.
    pointer: Option<(f32, f32)>,
    /// Whether Ctrl is held, which underlines links with `:ctrl-click`.
    ctrl: bool,
    /// The element being laid out with its `:hover` style, by address, and
    /// how many hovered `:group`s layout is inside, for `:group-hover` styles.
    hovered: Option<usize>,
    group_hover: usize,
    /// The visible parts of elements with hover styles, so the browser can
    /// draw again when the pointer moves onto or off one.
    pub hover_areas: Vec<Clip>,
    /// Set while measuring how wide content is, with the right edge of
    /// the content so far: spacers don't stretch then.
    fitting: bool,
    right: f32,
    /// How far the frames layout is inside are scrolled, in all. Their
    /// content is laid out as if they weren't, and painting moves it.
    scrolled: f32,
    /// The visible part of the page being laid out, that overlays are kept
    /// inside and placed in. `None` is the page's own box.
    view: Option<Clip>,
    /// Overlays waiting for the page they're on to be laid out.
    pending: Vec<PendingOverlay>,
    pub overlays: Vec<Overlay>,
}

/// How much layout has produced, to go back to after laying something out
/// only to measure it.
struct Marks {
    items: usize,
    links: usize,
    scrollers: usize,
    inputs: usize,
    next_input_id: usize,
    matches: usize,
    anchors: usize,
    key_targets: usize,
    next_key_target_id: usize,
    lines: usize,
    next_char: usize,
    paragraph: usize,
    next_frame_id: usize,
    hover_areas: usize,
    pending: usize,
    overlays: usize,
}

/// How a row decides the width of one of its columns.
enum Column {
    /// The child's `:width`, or the width of its content.
    Sized(f32),
    /// A share of the room the sized columns leave.
    Fill,
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
            key_focus: None,
            next_key_target_id: 0,
            clip: Clip::NONE,
            base: None,
            items: Vec::new(),
            links: Vec::new(),
            scrollers: Vec::new(),
            inputs: Vec::new(),
            key_targets: Vec::new(),
            search: None,
            images: None,
            drawings: None,
            matches: Vec::new(),
            anchors: Vec::new(),
            selection: None,
            lines: Vec::new(),
            next_char: 0,
            paragraph: 0,
            frame_depth: 0,
            scroller: None,
            pointer: None,
            ctrl: false,
            hovered: None,
            group_hover: 0,
            hover_areas: Vec::new(),
            fitting: false,
            right: 0.0,
            scrolled: 0.0,
            view: None,
            pending: Vec::new(),
            overlays: Vec::new(),
        }
    }

    /// Lays out elements under (`x`, `y`) with their `:hover` styles.
    pub fn point(&mut self, x: f32, y: f32) {
        self.pointer = Some((x, y));
    }

    /// Places overlays in `view`, the part of the page the window shows.
    pub fn view(&mut self, view: Clip) {
        self.view = Some(view);
    }

    /// Lays out links with `:ctrl-click` as links, underlined, if `held`.
    pub fn ctrl(&mut self, held: bool) {
        self.ctrl = held;
    }

    fn marks(&self) -> Marks {
        Marks {
            items: self.items.len(),
            links: self.links.len(),
            scrollers: self.scrollers.len(),
            inputs: self.inputs.len(),
            next_input_id: self.next_input_id,
            matches: self.matches.len(),
            anchors: self.anchors.len(),
            key_targets: self.key_targets.len(),
            next_key_target_id: self.next_key_target_id,
            lines: self.lines.len(),
            next_char: self.next_char,
            paragraph: self.paragraph,
            next_frame_id: self.next_frame_id,
            hover_areas: self.hover_areas.len(),
            pending: self.pending.len(),
            overlays: self.overlays.len(),
        }
    }

    /// Forgets everything laid out since `marks`.
    fn rewind(&mut self, marks: Marks) {
        self.items.truncate(marks.items);
        self.links.truncate(marks.links);
        self.scrollers.truncate(marks.scrollers);
        self.inputs.truncate(marks.inputs);
        self.next_input_id = marks.next_input_id;
        self.matches.truncate(marks.matches);
        self.anchors.truncate(marks.anchors);
        self.key_targets.truncate(marks.key_targets);
        self.next_key_target_id = marks.next_key_target_id;
        self.lines.truncate(marks.lines);
        (self.next_char, self.paragraph) = (marks.next_char, marks.paragraph);
        self.next_frame_id = marks.next_frame_id;
        self.hover_areas.truncate(marks.hover_areas);
        self.pending.truncate(marks.pending);
        self.overlays.truncate(marks.overlays);
    }

    /// Moves the links, text lines and other areas recorded since `marks`
    /// down by `dy`. Display items stay where they are.
    fn shift_since(&mut self, marks: &Marks, dy: f32) {
        for link in &mut self.links[marks.links..] {
            link.y += dy;
        }
        for scroller in &mut self.scrollers[marks.scrollers..] {
            scroller.area = scroller.area.shifted(dy);
            scroller.bar = scroller.bar.map(|bar| bar.shifted(dy));
        }
        for input in &mut self.inputs[marks.inputs..] {
            input.caret_ys.iter_mut().for_each(|y| *y += dy);
        }
        let areas = self.inputs[marks.inputs..].iter_mut().map(|input| &mut input.area);
        let areas = areas.chain(self.matches[marks.matches..].iter_mut().map(|m| &mut m.area));
        let areas = areas.chain(self.anchors[marks.anchors..].iter_mut().map(|a| &mut a.area));
        let areas = areas.chain(self.key_targets[marks.key_targets..].iter_mut().map(|k| &mut k.area));
        let areas = areas.chain(self.hover_areas[marks.hover_areas..].iter_mut());
        let areas = areas.chain(self.pending[marks.pending..].iter_mut().map(|p| &mut p.anchor));
        let areas = areas.chain(self.overlays[marks.overlays..].iter_mut().map(|o| &mut o.area));
        for area in areas.chain(self.lines[marks.lines..].iter_mut().flat_map(|l| [&mut l.area, &mut l.visible])) {
            *area = area.shifted(dy);
        }
    }

    /// `el`'s style, with its `:hover` style if it's hovered and its
    /// `:group-hover` style if a group it's in is.
    fn style(&self, el: &Element, parent: &Style) -> Style {
        let mut style = Style::compute(el, parent);
        let mut extra = |name| {
            if let Some(Value::List(attrs)) = el.attr(name) {
                let pairs = attrs.as_chunks::<2>().0.iter().filter_map(|[name, value]| match name {
                    Value::Keyword(name) => Some((&**name, value)),
                    _ => None,
                });
                style.apply(pairs);
            }
        };
        if self.group_hover > 0 {
            extra("group-hover");
        }
        if self.hovered == Some(el as *const Element as usize) {
            extra("hover");
        }
        style
    }

    pub fn images(&mut self, images: &'a Images) {
        self.images = Some(images);
    }

    pub fn drawings(&mut self, drawings: &'a Drawings) {
        self.drawings = Some(drawings);
    }

    /// Outlines key target `id`, which has the keyboard.
    pub fn focus_keys(&mut self, id: usize) {
        self.key_focus = Some(id);
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

    /// Lays out `el` once, or for an element with a `:hover` style or that
    /// is a `:group`, again with its hover styles if the pointer is over it.
    fn block_on_stack(&mut self, el: &Element, parent: &Style, x: f32, y: f32, w: f32) -> f32 {
        let group = el.attr("group").is_some_and(Value::truthy);
        if !group && el.attr("hover").is_none() {
            return self.block_box(el, self.style(el, parent), x, y, w).0;
        }
        let marks = self.marks();
        let (bottom, area) = self.block_box(el, self.style(el, parent), x, y, w);
        let visible = area.intersect(self.clip);
        if !self.pointer.is_some_and(|(px, py)| visible.contains(px, py)) {
            self.hover_areas.push(visible);
            return bottom;
        }
        self.rewind(marks);
        let outer = self.hovered.replace(el as *const Element as usize);
        self.group_hover += group as usize;
        let (bottom, area) = self.block_box(el, self.style(el, parent), x, y, w);
        self.group_hover -= group as usize;
        self.hovered = outer;
        self.hover_areas.push(area.intersect(self.clip));
        bottom
    }

    /// Lays out `el`, in `style`, in a box of width `w` at (`x`, `y`).
    /// Given a `:width`, a number or "fit" to shrink to its content, the box
    /// is that wide, and sits at the left, or as `:align` says. Returns the
    /// y below it, including its bottom margin, and the box.
    fn block_box(&mut self, el: &Element, style: Style, x: f32, y: f32, w: f32) -> (f32, Clip) {
        let style = Rc::new(style);
        let page_overlays = self.pending.len();
        let y = y + style.margin_top * self.scale;
        let width = match el.attr("width") {
            Some(Value::Num(n)) => Some((*n as f32 * self.scale).max(0.0)),
            Some(Value::Str(fit)) if &**fit == "fit" => Some(self.fit_width(el, &style, w)),
            _ => None,
        };
        let (x, w) = match width {
            Some(width) => {
                let room = (w - width).max(0.0);
                let shift = match el.attr("align") {
                    Some(Value::Str(align)) if &**align == "right" => room,
                    Some(Value::Str(align)) if &**align == "center" => room / 2.0,
                    _ => 0.0,
                };
                (x + shift, width)
            }
            None => (x, w),
        };
        let (top, right, bottom, left) = style.insets();
        let (top, right, bottom, left) = (top * self.scale, right * self.scale, bottom * self.scale, left * self.scale);
        let (mut inner_x, mut inner_w) = (x + left, (w - left - right).max(0.0));
        // `:max-width` narrows the content, centred, while the background keeps the full width.
        if let Some(Value::Num(max)) = el.attr("max-width") {
            let max = (*max as f32 * self.scale).max(0.0);
            if inner_w > max {
                inner_x += (inner_w - max) / 2.0;
                inner_w = max;
            }
        }
        let background_index = self.items.len();

        let outer_right = std::mem::replace(&mut self.right, inner_x);
        let content_bottom = self.contents(el, &style, inner_x, y + top, inner_w);
        let content_right = if width.is_some() { x + w } else { self.right + right };
        self.right = outer_right.max(content_right);

        let h = content_bottom + bottom - y;
        let radius = style.radius * self.scale;
        if let Some(color) = style.border
            && style.border_width > 0.0
        {
            let width = style.border_width * self.scale;
            self.items.insert(background_index, DisplayItem::Border { x, y, w, h, width, color, radius });
        }
        if let Some(color) = style.background {
            self.items.insert(background_index, DisplayItem::Rect { x, y, w, h, color, radius });
        }
        // After the children's, so a click on a child's own handler or link wins.
        if let Some(action) = self.action(el) {
            self.push_link(Clip::rect(x, y, w, h), action);
        }
        if let Some(Value::Str(id)) = el.attr("id") {
            self.anchors.push(Anchor { id: id.clone(), area: Clip::rect(x, y, w, h), frame: self.scroller });
        }
        self.queue_overlays(el, &style, Clip::rect(x, y, w, h));
        // A page's overlays go over all of it, so they're laid out last.
        if &*el.tag == "page" {
            self.lay_out_overlays(page_overlays, Clip::rect(x, y, w, h));
        }
        (y + h + style.margin * self.scale, Clip::rect(x, y, w, h))
    }

    /// Queues the overlays in `el`, including those inside its inline
    /// elements, anchored to its box.
    fn queue_overlays(&mut self, el: &Element, style: &Rc<Style>, anchor: Clip) {
        for child in &el.children {
            let Node::Element(child) = child else { continue };
            if &*child.tag == "overlay" {
                let group_hover = self.group_hover;
                self.pending.push(PendingOverlay { el: child.clone(), parent: style.clone(), anchor, group_hover });
            } else if dom::is_inline(child) {
                self.queue_overlays(child, &Rc::new(self.style(child, style)), anchor);
            }
        }
    }

    /// Lays out the overlays queued since `start`, in order, and those
    /// they queue in turn, over the page whose box is `page`.
    fn lay_out_overlays(&mut self, start: usize, page: Clip) {
        let mut i = start;
        while i < self.pending.len() {
            let pending = self.pending[i].clone();
            self.overlay(&pending, self.view.unwrap_or(page));
            i += 1;
        }
        self.pending.truncate(start);
    }

    /// Lays out an overlay where its `:at` says, keeping it in `view` where
    /// it can. Next to its anchor, it's "below" (the default), "above",
    /// "left" or "right" of it, moving to the other side if there's more room
    /// there. In the view, it's at the "center", "top", "bottom",
    /// "top-left", "top-right", "bottom-left" or "bottom-right". It's as
    /// wide as its content unless it has a `:width`. A `:backdrop` colour
    /// covers the view beneath it.
    fn overlay(&mut self, pending: &PendingOverlay, view: Clip) {
        let PendingOverlay { el, parent, anchor, group_hover } = pending;
        let at = match el.attr("at") {
            Some(Value::Str(at)) => at.clone(),
            _ => "below".into(),
        };
        let style = Rc::new(Style::compute(el, parent));
        let (margin_top, margin) = (style.margin_top * self.scale, style.margin * self.scale);
        let in_view = matches!(&*at, "center" | "top" | "bottom" | "top-left" | "top-right" | "bottom-left" | "bottom-right");
        let room = view.x1 - view.x0 - if in_view { 2.0 * margin } else { 0.0 };
        let w = match el.attr("width") {
            Some(Value::Num(n)) => (*n as f32 * self.scale).max(0.0),
            _ => self.fit_width(el, &style, room.max(0.0)),
        };
        let outer_group_hover = std::mem::replace(&mut self.group_hover, *group_hover);
        // Measured first, to know where it fits.
        let marks = self.marks();
        let h = self.block(el, parent, 0.0, 0.0, w);
        self.rewind(marks);

        let fits_x = |x: f32| x.min(view.x1 - w).max(view.x0);
        let fits_y = |y: f32| y.min(view.y1 - h).max(view.y0);
        let (x, y) = match &*at {
            "above" | "below" => {
                let (above, below) = (anchor.y0 - h, anchor.y1);
                let more_room_above = anchor.y0 - view.y0 > view.y1 - anchor.y1;
                let y = match &*at {
                    "above" if above < view.y0 && !more_room_above => below,
                    "above" => above,
                    _ if below + h > view.y1 && more_room_above => above,
                    _ => below,
                };
                (fits_x(anchor.x0), y)
            }
            "left" | "right" => {
                let (left, right) = (anchor.x0 - margin - w, anchor.x1 + margin);
                let more_room_left = anchor.x0 - view.x0 > view.x1 - anchor.x1;
                let x = match &*at {
                    "left" if left < view.x0 && !more_room_left => right,
                    "left" => left,
                    _ if right + w > view.x1 && more_room_left => left,
                    _ => right,
                };
                (x, fits_y(anchor.y0 - margin_top))
            }
            _ => {
                let x = match &*at {
                    "top-left" | "bottom-left" => view.x0 + margin,
                    "top-right" | "bottom-right" => view.x1 - margin - w,
                    _ => view.x0 + (view.x1 - view.x0 - w) / 2.0,
                };
                let y = match &*at {
                    "top" | "top-left" | "top-right" => view.y0,
                    "bottom" | "bottom-left" | "bottom-right" => view.y1 - h,
                    _ => view.y0 + (view.y1 - view.y0 - h) / 2.0,
                };
                (x, y)
            }
        };
        // Whole pixels, so one fixed in the view stays exactly put as the page scrolls.
        let (x, y) = (x.round(), y.round());

        let backdrop = match el.attr("backdrop") {
            Some(Value::Str(color)) => Some(style::parse_color(color, self.palette.page)),
            _ => None,
        };
        if let Some(color) = backdrop {
            let (vw, vh) = (view.x1 - view.x0, view.y1 - view.y0);
            self.items.push(DisplayItem::Rect { x: view.x0, y: view.y0, w: vw, h: vh, color, radius: 0.0 });
        }
        let starts = (self.links.len(), self.inputs.len(), self.key_targets.len(), self.lines.len());
        let bottom = self.block(el, parent, x, y, w);
        self.group_hover = outer_group_hover;
        let area = match backdrop {
            Some(_) => view,
            None => Clip { x0: x, y0: y + margin_top, x1: x + w, y1: bottom - margin },
        };
        self.overlays.push(Overlay {
            area: area.intersect(self.clip),
            dismiss: el.attr("on-dismiss").filter(|handler| handler.truthy()).cloned(),
            links: starts.0..self.links.len(),
            inputs: starts.1..self.inputs.len(),
            key_targets: starts.2..self.key_targets.len(),
            lines: starts.3..self.lines.len(),
        });
    }

    /// Lays out what's inside `el`'s box.
    fn contents(&mut self, el: &Element, style: &Rc<Style>, x: f32, y: f32, w: f32) -> f32 {
        match &*el.tag {
            "row" => self.row(el, style, x, y, w),
            "frame" => self.frame(el, x, y, w),
            "pre" => self.preformatted(&el.children, style, x, y, w),
            _ => self.flow(&el.children, style, x, y, w),
        }
    }

    /// How wide `el`'s box needs to be for its content, up to `w`.
    fn fit_width(&mut self, el: &Element, style: &Rc<Style>, w: f32) -> f32 {
        let (_, right, _, left) = style.insets();
        let (right, left) = (right * self.scale, left * self.scale);
        self.measure_width(w, |layout| {
            layout.right = left;
            layout.contents(el, style, left, 0.0, (w - left - right).max(0.0));
            layout.right + right
        })
    }

    /// How wide what `lay_out` lays out is, at most `w`; it returns the right
    /// edge of its content. Then forgets it.
    fn measure_width(&mut self, w: f32, lay_out: impl FnOnce(&mut Self) -> f32) -> f32 {
        let marks = self.marks();
        let outer = (std::mem::replace(&mut self.fitting, true), self.right);
        let width = lay_out(self);
        self.rewind(marks);
        (self.fitting, self.right) = outer;
        width.clamp(0.0, w)
    }

    /// Normal flow: consecutive text and inline elements form one inline
    /// run; block children end the run and stack below it.
    fn flow(&mut self, children: &[Node], style: &Rc<Style>, x: f32, y: f32, w: f32) -> f32 {
        let mut cy = y;
        let mut run = Vec::new();
        for child in children {
            match child {
                Node::Text(text) => run.push(Segment::text(text, style)),
                Node::Element(child) if &*child.tag == "overlay" => {}
                Node::Element(child) if dom::is_inline(child) => self.collect_inline(child, style, None, None, w, &mut run),
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
    fn preformatted(&mut self, children: &[Node], style: &Rc<Style>, x: f32, y: f32, w: f32) -> f32 {
        let mut run = Vec::new();
        for child in children {
            match child {
                Node::Text(text) => run.push(Segment::text(text, style)),
                Node::Element(child) if &*child.tag == "overlay" => {}
                Node::Element(child) => self.collect_inline(child, style, None, None, w, &mut run),
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

    /// Columns side by side, one per child, `:gap` apart. A child with a
    /// numeric `:width` keeps it, and inline children and those with
    /// `:width "fit"` are as wide as their content. The other children,
    /// including inline ones with `:width :fill`, and spacers, share the room left equally. `:align` lines the columns up
    /// at the "top", "middle" or "bottom". Returns the bottom of the tallest.
    fn row(&mut self, el: &Element, style: &Rc<Style>, x: f32, y: f32, w: f32) -> f32 {
        let children: Vec<&Node> = el.children.iter().filter(|child| !is_overlay(child)).collect();
        let gap = match el.attr("gap") {
            Some(Value::Num(n)) => (*n as f32).max(0.0) * self.scale,
            _ => 16.0 * self.scale,
        };
        let n = children.len().max(1) as f32;
        let gaps = gap * (n - 1.0);
        let columns: Vec<Column> = children.iter().map(|child| self.column(child, style, w)).collect();
        let sized: f32 = columns.iter().map(|c| if let Column::Sized(w) = c { *w } else { 0.0 }).sum();
        let fills = columns.iter().filter(|c| matches!(c, Column::Fill)).count().max(1) as f32;
        let share = ((w - gaps - sized) / fills).max(0.0);
        let widths: Vec<f32> = columns.iter().map(|c| if let Column::Sized(w) = c { *w } else { share }).collect();
        let xs: Vec<f32> = widths.iter().scan(x, |next, w| Some(std::mem::replace(next, *next + w + gap))).collect();

        let column = |layout: &mut Self, i: usize, y: f32| layout.flow(std::slice::from_ref(children[i]), style, xs[i], y, widths[i]);
        let align = match el.attr("align") {
            Some(Value::Str(align)) if matches!(&**align, "middle" | "center") => 0.5,
            Some(Value::Str(align)) if &**align == "bottom" => 1.0,
            _ => 0.0,
        };
        // Lined up other than at the top, the columns are measured first.
        let mut heights = vec![0.0; children.len()];
        if align > 0.0 {
            let marks = self.marks();
            for (i, height) in heights.iter_mut().enumerate() {
                *height = column(self, i, y) - y;
            }
            self.rewind(marks);
        }
        let tallest = heights.iter().copied().fold(0.0, f32::max);
        let mut bottom = y;
        for (i, height) in heights.iter().enumerate() {
            bottom = bottom.max(column(self, i, y + (tallest - height) * align));
        }
        bottom
    }

    /// How a row sizes the column for `child`, in a row `w` wide. While
    /// fitting, every column is as wide as its content.
    fn column(&mut self, child: &Node, style: &Rc<Style>, w: f32) -> Column {
        let Node::Element(el) = child else {
            return if self.fitting { self.content_column(child, style, w) } else { Column::Fill };
        };
        match el.attr("width") {
            Some(Value::Num(n)) => return Column::Sized((*n as f32).max(0.0) * self.scale),
            Some(Value::Str(fit)) if &**fit == "fit" => return self.content_column(child, style, w),
            Some(Value::Keyword(fill)) if &**fill == "fill" && !self.fitting => return Column::Fill,
            _ => {}
        }
        match (&*el.tag, dom::is_inline(el)) {
            ("spacer", _) if !self.fitting => Column::Fill,
            (_, true) => self.content_column(child, style, w),
            _ if self.fitting => self.content_column(child, style, w),
            _ => Column::Fill,
        }
    }

    fn content_column(&mut self, child: &Node, style: &Rc<Style>, w: f32) -> Column {
        Column::Sized(self.measure_width(w, |layout| {
            layout.right = 0.0;
            layout.flow(std::slice::from_ref(child), style, 0.0, 0.0, w);
            layout.right
        }))
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
            Some(Value::Str(fill)) if &**fill == "fill" => Some((self.viewport_height - (y - self.scrolled)).max(40.0 * self.scale)),
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
                DisplayItem::Rect { x, y, w, h: border, color, radius: 0.0 },
                DisplayItem::Rect { x, y: bottom - border, w, h: border, color, radius: 0.0 },
                DisplayItem::Rect { x, y, w: border, h, color, radius: 0.0 },
                DisplayItem::Rect { x: x + w - border, y, w: border, h, color, radius: 0.0 },
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
        let marks = self.marks();
        let content_bottom = self.block(page, &Style::root(self.palette), area.x0, area.y0, area.x1 - area.x0);
        self.rewind(marks);

        let max_scroll = (content_bottom - area.y1).max(0.0);
        let scroll = self.frame_scrolls.get(&id).copied().unwrap_or(0.0).clamp(0.0, max_scroll).round();

        let outer = self.clip;
        self.clip = area.intersect(outer);
        self.items.push(DisplayItem::Clip(self.clip));
        // The page's background fills the frame, even below its content.
        let background = self.style(page, &Style::root(self.palette)).background.unwrap_or(self.palette.page);
        let (w, h) = (area.x1 - area.x0, area.y1 - area.y0);
        self.items.push(DisplayItem::Rect { x: area.x0, y: area.y0, w, h, color: background, radius: 0.0 });
        // The content is laid out unscrolled, in the frame's layer, which
        // painting moves; so the clip and pointer move the other way.
        let outer_scroller = self.scroller.replace(id);
        let marks = self.marks();
        let saved = (self.clip, self.pointer, self.scrolled, self.view);
        self.clip = self.clip.shifted(scroll);
        self.view = Some(area.shifted(scroll));
        self.pointer = self.pointer.map(|(x, y)| (x, y + scroll));
        self.scrolled += scroll;
        self.items.push(DisplayItem::Layer(Some(id)));
        self.block(page, &Style::root(self.palette), area.x0, area.y0, area.x1 - area.x0);
        self.items.push(DisplayItem::Layer(outer_scroller));
        (self.clip, self.pointer, self.scrolled, self.view) = saved;
        self.scroller = outer_scroller;
        self.shift_since(&marks, -scroll);
        let bar = Scrollbar::new(area, content_bottom - area.y0, scroll, self.scale);
        if let Some(bar) = bar {
            self.items.push(DisplayItem::Rect {
                x: bar.thumb.x0,
                y: bar.thumb.y0,
                w: bar.thumb.x1 - bar.thumb.x0,
                h: bar.thumb.y1 - bar.thumb.y0,
                color: self.palette.scrollbar,
                radius: 0.0,
            });
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
                if !self.fitting {
                    stretch(&mut line, run, w - line_w);
                }
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
            if !self.fitting {
                stretch(&mut line, run, w - line_w);
            }
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
                    Some(Embed::Input(_, w)) => *w,
                    Some(Embed::Image(_, w, ..)) | Some(Embed::Canvas(_, w, _)) => *w,
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
        let content = line.iter().filter(|piece| !matches!(run[piece.segment].embed, Some(Embed::Spacer)));
        self.right = content.map(|piece| x + piece.x + piece.w).fold(self.right, f32::max);

        // Backgrounds first so neighbouring text is never painted over.
        // Consecutive pieces of one segment share a background, so rounded
        // corners go round a whole phrase rather than each word.
        let mut last_background: Option<(usize, usize)> = None;
        for piece in line {
            let segment = &run[piece.segment];
            let (height, baseline) = self.piece_box(segment);
            let (px, py) = (x + piece.x, baseline_y - baseline);
            match &segment.embed {
                Some(Embed::Input(input, _)) => {
                    self.input(input, &segment.style, Clip::rect(px, py, piece.w, height));
                    continue;
                }
                Some(Embed::Image(image, w, h, _)) => {
                    let radius = segment.style.radius * self.scale;
                    self.items.push(DisplayItem::Image { x: px, y: py, w: *w, h: *h, image: image.clone(), radius });
                    if let Some(action) = &segment.action {
                        self.push_link(Clip::rect(px, py, *w, *h), action.clone());
                    }
                    continue;
                }
                Some(Embed::Canvas(el, w, h)) => {
                    self.canvas(el, &segment.style, Clip::rect(px, py, *w, *h));
                    if let Some(action) = &segment.action {
                        self.push_link(Clip::rect(px, py, *w, *h), action.clone());
                    }
                    continue;
                }
                Some(Embed::Spacer) => continue,
                None => {}
            }
            if let Some(color) = segment.style.background {
                match last_background.and_then(|(i, item)| (i == piece.segment).then_some(item)) {
                    Some(item) => {
                        if let DisplayItem::Rect { x: start, w, .. } = &mut self.items[item] {
                            *w = px + piece.w - *start;
                        }
                    }
                    None => {
                        let radius = segment.style.radius * self.scale;
                        last_background = Some((piece.segment, self.items.len()));
                        self.items.push(DisplayItem::Rect { x: px, y: py, w: piece.w, h: height, color, radius });
                    }
                }
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
                self.items.push(DisplayItem::Rect { x: x + piece.x, y, w: piece.w, h: thickness, color: style.color, radius: 0.0 });
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
            self.items.push(DisplayItem::Rect { x: x0, y, w: x1 - x0, h, color: self.palette.selection, radius: 0.0 });
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
                self.items.push(DisplayItem::Rect { x: x0, y, w: x1 - x0, h, color, radius: 0.0 });
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
    /// `width` is the content width of the enclosing block.
    fn collect_inline(
        &self,
        el: &Rc<Element>,
        parent: &Style,
        background: Option<Color>,
        action: Option<Action>,
        width: f32,
        out: &mut Vec<Segment>,
    ) {
        let mut style = self.style(el, parent);
        if matches!(&*el.tag, "input" | "textarea") {
            let style = Rc::new(style);
            let embed = Embed::Input(el.clone(), self.input_width(el, width));
            out.push(Segment { text: "".into(), style, action: None, padding: 0.0, nowrap: true, embed: Some(embed) });
            return;
        }
        // A link with `:ctrl-click` is underlined only while it's a link.
        if &*el.tag == "a" && el.attr("ctrl-click").is_some_and(Value::truthy) {
            style.underline = self.ctrl;
        }
        let action = self.action(el).or(action);
        if &*el.tag == "spacer" {
            out.push(Segment {
                text: "".into(),
                style: Rc::new(style),
                action: None,
                padding: 0.0,
                nowrap: true,
                embed: Some(Embed::Spacer),
            });
            return;
        }
        if &*el.tag == "img" {
            let image = self.image_box(el, width);
            out.push(Segment { text: "".into(), style: Rc::new(style), action, padding: 0.0, nowrap: true, embed: Some(image) });
            return;
        }
        if &*el.tag == "canvas" {
            let size = |name, default| match el.attr(name) {
                Some(Value::Num(n)) => (*n as f32).max(0.0) * self.scale,
                _ => default * self.scale,
            };
            let embed = Embed::Canvas(el.clone(), size("width", 300.0), size("height", 150.0));
            out.push(Segment { text: "".into(), style: Rc::new(style), action, padding: 0.0, nowrap: true, embed: Some(embed) });
            return;
        }
        style.background = style.background.or(background);
        let (padding, nowrap) = (style.padding_left, matches!(&*el.tag, "button" | "code"));
        let style = Rc::new(style);
        for child in &el.children {
            match child {
                Node::Text(text) => {
                    out.push(Segment { text: text.clone(), style: style.clone(), action: action.clone(), padding, nowrap, embed: None })
                }
                Node::Element(child) if &*child.tag == "overlay" => {}
                Node::Element(child) => self.collect_inline(child, &style, style.background, action.clone(), width, out),
            }
        }
    }

    /// What clicking `el` does: run its `:on-click` handler, or for a link,
    /// go to its `:href`.
    fn action(&self, el: &Element) -> Option<Action> {
        match (el.attr("on-click"), el.attr("href")) {
            (Some(handler), _) => Some(Action::Run(handler.clone())),
            (None, Some(Value::Str(href))) if &*el.tag == "a" => {
                let ctrl = el.attr("ctrl-click").is_some_and(Value::truthy);
                Some(Action::Go { href: href.clone(), base: self.base.clone(), ctrl })
            }
            _ => None,
        }
    }

    /// An image's picture and size. Given a `:width` or a `:height`, it
    /// keeps the image's proportions; given neither, it's its natural size.
    /// `:width :fill` takes the containing block's content width, `width`.
    fn image_box(&self, el: &Element, width: f32) -> Embed {
        let location = match el.attr("location") {
            Some(Value::Str(location)) => location,
            _ => "",
        };
        let image = self.images.and_then(|images| images.get(location)).and_then(|image| image.as_ref().ok()).cloned();
        let attr = |name| match el.attr(name) {
            Some(Value::Num(n)) => Some((*n as f32).max(0.0) * self.scale),
            Some(Value::Keyword(fill)) if name == "width" && &**fill == "fill" => Some(width),
            _ => None,
        };
        // A missing image keeps its space, or a small box's if it has no size.
        let natural = image.as_ref().map_or((24.0, 24.0), |image| (image.width as f32, image.height as f32));
        let natural = (natural.0 * self.scale, natural.1 * self.scale);
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
        Embed::Image(image, w, h, align)
    }

    /// Like `line_box`, but for any piece of a line.
    fn piece_box(&self, segment: &Segment) -> (f32, f32) {
        let (height, baseline) = self.line_box(&segment.style);
        match segment.embed {
            Some(Embed::Input(ref input, _)) => {
                let inset = self.input_inset(&segment.style);
                let text_height = height * input_rows(input) as f32;
                (text_height + inset.top + inset.bottom, baseline + inset.top)
            }
            Some(Embed::Image(_, _, h, Align::Baseline)) | Some(Embed::Canvas(_, _, h)) => (h, h),
            // Lowercase letters are about half the font size tall.
            Some(Embed::Image(_, _, h, Align::Middle)) => (h, h / 2.0 + segment.style.font_size * self.scale * 0.25),
            Some(Embed::Spacer) => (0.0, 0.0),
            None => (height, baseline),
        }
    }

    /// Space between an input's edges and its text, on each side: padding plus a 1px border.
    fn input_inset(&self, style: &Style) -> Insets {
        let side = |padding: f32| (padding + 1.0) * self.scale;
        Insets {
            top: side(style.padding_top),
            right: side(style.padding_right),
            bottom: side(style.padding_bottom),
            left: side(style.padding_left),
        }
    }

    /// An input's `:width`, or 240, or a textarea's, or 400. `:width :fill`
    /// takes the containing block's content width, `width`, as an image's does.
    fn input_width(&self, input: &Element, width: f32) -> f32 {
        match input.attr("width") {
            Some(Value::Num(n)) => *n as f32 * self.scale,
            Some(Value::Keyword(fill)) if &**fill == "fill" => width,
            _ if &*input.tag == "textarea" => 400.0 * self.scale,
            _ => 240.0 * self.scale,
        }
    }

    /// Draws a text input or textarea filling `area`: its value (or
    /// placeholder) clipped to the box and scrolled to keep the caret in
    /// view, plus the caret if it has focus. A textarea's value is lines of
    /// text. Either is highlighted as code if its `:code` names a language
    /// the browser knows, as `:code :netscheme` does.
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
        // The background's corners follow the border's curve, inside it.
        let radius = style.radius * self.scale;
        self.items.push(DisplayItem::Rect { x, y, w, h, color: border_color, radius });
        self.items.push(DisplayItem::Rect {
            x: x + border,
            y: y + border,
            w: w - 2.0 * border,
            h: h - 2.0 * border,
            color: background,
            radius: (radius - border).max(0.0),
        });

        let value = match el.attr("value") {
            None | Some(Value::Nil) => String::new(),
            Some(value) => value.to_string(),
        };
        let multiline = &*el.tag == "textarea";
        let (size, face) = (style.font_size * self.scale, style.face());
        let inset = self.input_inset(style);
        let (inner_x, inner_w) = (x + inset.left, (w - inset.left - inset.right).max(0.0));
        let (inner_y, inner_h) = (y + inset.top, (h - inset.top - inset.bottom).max(0.0));
        let (line_height, baseline) = self.line_box(style);

        // How far each caret position is from the start of its line, and which line that is.
        let chars: Vec<char> = value.chars().collect();
        let (mut offsets, mut rows) = (vec![0.0], vec![0]);
        for &c in &chars {
            let (offset, row) = (offsets[offsets.len() - 1], rows[rows.len() - 1]);
            if multiline && c == '\n' {
                offsets.push(0.0);
                rows.push(row + 1);
            } else {
                offsets.push(offset + self.fonts.measure(c.encode_utf8(&mut [0; 4]), size, face));
                rows.push(row);
            }
        }
        let len = chars.len();
        let cursor = focus.map(|focus| focus.cursor.min(len));
        let selection = focus.map(|focus| (focus.anchor.min(len), focus.cursor.min(len))).filter(|(a, c)| a != c);
        let scroll_x = cursor.map_or(0.0, |c| (offsets[c] - (inner_w - thin)).max(0.0));
        let scroll_y = cursor.filter(|_| multiline).map_or(0.0, |c| ((rows[c] + 1) as f32 * line_height - inner_h).max(0.0));
        let (text_x, text_y) = (inner_x - scroll_x, inner_y - scroll_y);
        let row_y = |row: usize| text_y + row as f32 * line_height;

        let outer = self.clip;
        let visible = area.intersect(outer);
        // An input's text is clipped at the sides; a textarea's lines inside its border too.
        let text_clip = match multiline {
            true => Clip { x0: inner_x, x1: inner_x + inner_w, y0: y + border, y1: y + h - border }.intersect(visible),
            false => Clip { x0: inner_x, x1: inner_x + inner_w, ..visible },
        };
        self.items.push(DisplayItem::Clip(text_clip));
        if let Some((a, c)) = selection {
            // A selected line break shows as a space at the end of its line.
            let space = self.fonts.measure(" ", size, face);
            let (mut from, end) = (a.min(c), a.max(c));
            while from < end {
                let line_break = (from..end).find(|&i| multiline && chars[i] == '\n');
                let (x0, x1) = match line_break {
                    Some(i) => (offsets[from], offsets[i] + space),
                    None => (offsets[from], offsets[end]),
                };
                let (x, y) = (text_x + x0, row_y(rows[from]));
                self.items.push(DisplayItem::Rect { x, y, w: x1 - x0, h: line_height, color: style.palette.selection, radius: 0.0 });
                from = line_break.map_or(end, |i| i + 1);
            }
        }
        match el.attr("placeholder") {
            Some(Value::Str(placeholder)) if value.is_empty() => {
                let (text, color) = (placeholder.to_string(), style.palette.placeholder);
                self.items.push(DisplayItem::Text { x: text_x, y: text_y + baseline, text, size, face, color });
            }
            _ => {
                // Each character's colour, then runs of one colour on one line, drawn if they're in view.
                let mut colors = vec![style.color; len];
                for (range, token) in highlight::language(el.attr("code")).map_or(Vec::new(), |tokens| tokens(&chars)) {
                    colors[range].fill(token.color(style.palette));
                }
                let breaks = |i: usize| multiline && chars[i] == '\n';
                let mut i = 0;
                while i < len {
                    let start = i;
                    while i < len && !breaks(i) && colors[i] == colors[start] {
                        i += 1;
                    }
                    let top = row_y(rows[start]);
                    if i > start && top < text_clip.y1 && top + line_height > text_clip.y0 {
                        let text = chars[start..i].iter().collect();
                        let (x, y, color) = (text_x + offsets[start], top + baseline, colors[start]);
                        self.items.push(DisplayItem::Text { x, y, text, size, face, color });
                    }
                    if i < len && breaks(i) {
                        i += 1;
                    }
                }
            }
        }
        // Like most text fields, no caret while there's a selection.
        if let Some(c) = cursor
            && self.caret_visible
            && selection.is_none()
        {
            let caret =
                DisplayItem::Rect { x: text_x + offsets[c], y: row_y(rows[c]), w: thin, h: line_height, color: style.color, radius: 0.0 };
            self.items.push(caret);
        }
        self.items.push(DisplayItem::Clip(outer));

        if !visible.is_empty() {
            let caret_xs = offsets.iter().map(|offset| text_x + offset).collect();
            let caret_ys = rows.iter().map(|&row| row_y(row)).collect();
            self.inputs.push(InputBox { id, area: visible, element: el.clone(), value, caret_xs, caret_ys, line_height });
        }
    }

    /// Draws a canvas's background and its shapes, clipped to `area`: its
    /// children, then what its `:draw` function returned. Other children of
    /// a canvas are left out.
    fn canvas(&mut self, el: &Rc<Element>, style: &Style, area: Clip) {
        let outer = self.clip;
        self.clip = area.intersect(outer);
        self.items.push(DisplayItem::Clip(self.clip));
        if let Some(color) = style.background {
            let radius = style.radius * self.scale;
            self.items.push(DisplayItem::Rect { x: area.x0, y: area.y0, w: area.x1 - area.x0, h: area.y1 - area.y0, color, radius });
        }
        let first_link = self.links.len();
        let drawn = self.drawings.and_then(|drawings| drawings.get(&(Rc::as_ptr(el) as usize)));
        for child in el.children.iter().chain(drawn.into_iter().flatten()) {
            if let Node::Element(shape) = child {
                self.shape(shape, style, (area.x0, area.y0));
            }
        }
        // Later shapes are drawn over earlier ones, so they're clicked first.
        self.links[first_link..].reverse();
        if listens_for_keys(el) {
            let id = self.next_key_target_id;
            self.next_key_target_id += 1;
            if self.key_focus == Some(id) {
                let (x, y, w, h) = (area.x0, area.y0, area.x1 - area.x0, area.y1 - area.y0);
                let (thick, color) = (2.0 * self.scale.round().max(1.0), self.palette.focus);
                self.items.extend([
                    DisplayItem::Rect { x, y, w, h: thick, color, radius: 0.0 },
                    DisplayItem::Rect { x, y: y + h - thick, w, h: thick, color, radius: 0.0 },
                    DisplayItem::Rect { x, y, w: thick, h, color, radius: 0.0 },
                    DisplayItem::Rect { x: x + w - thick, y, w: thick, h, color, radius: 0.0 },
                ]);
            }
            if !self.clip.is_empty() {
                self.key_targets.push(KeyTarget { id, area: self.clip, element: el.clone() });
            }
        }
        self.items.push(DisplayItem::Clip(outer));
        self.clip = outer;
    }

    /// One shape in a canvas whose top-left corner is at `origin`. Shapes
    /// are filled with `:fill` and outlined with `:stroke`, and given
    /// neither, filled in the text colour; lines are always stroked.
    fn shape(&mut self, el: &Element, parent: &Style, origin: Point) {
        let style = self.style(el, parent);
        let scale = self.scale;
        let num = |name| match el.attr(name) {
            Some(Value::Num(n)) => *n as f32,
            _ => 0.0,
        };
        let at = |x: f32, y: f32| (origin.0 + x * scale, origin.1 + y * scale);
        let colour = |name| match el.attr(name) {
            Some(Value::Str(c)) => Some(style::parse_color(c, style.color)),
            _ => None,
        };
        let (mut fill, stroke) = (colour("fill"), colour("stroke"));
        if fill.is_none() && stroke.is_none() {
            fill = Some(style.color);
        }
        let width = match el.attr("stroke-width") {
            Some(Value::Num(n)) => (*n as f32).max(0.0) * scale,
            _ => scale,
        };

        let (outline, edge): (Option<Vec<Contour>>, Vec<Contour>) = match &*el.tag {
            "rect" => {
                let (a, b) = (at(num("x"), num("y")), at(num("x") + num("w"), num("y") + num("h")));
                (Some(vec![canvas::rect(a, b)]), canvas::rect_outline(a, b, width))
            }
            "circle" => {
                let (centre, r) = (at(num("x"), num("y")), num("r").max(0.0) * scale);
                (Some(vec![canvas::circle(centre, r)]), canvas::circle_outline(centre, r, width))
            }
            "line" => {
                fill = None;
                let ends = [at(num("x1"), num("y1")), at(num("x2"), num("y2"))];
                (None, canvas::stroke(&ends, false, width))
            }
            "polyline" | "polygon" => {
                let closed = &*el.tag == "polygon";
                let points: Vec<Point> = match el.attr("points") {
                    Some(Value::List(points)) => points
                        .iter()
                        .filter_map(|point| match point {
                            Value::List(xy) => match &xy[..] {
                                [Value::Num(x), Value::Num(y)] => Some(at(*x as f32, *y as f32)),
                                _ => None,
                            },
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                if !closed {
                    fill = None;
                }
                let edge = canvas::stroke(&points, closed, width);
                (closed.then(|| vec![points]), edge)
            }
            "path" => {
                let d = match el.attr("d") {
                    Some(Value::Str(d)) => d.clone(),
                    _ => "".into(),
                };
                let zoom = match el.attr("scale") {
                    Some(Value::Num(n)) => *n as f32,
                    _ => 1.0,
                };
                let (dx, dy) = (num("x"), num("y"));
                let subpaths = canvas::path(&d, zoom * scale, |(x, y)| at(dx + x * zoom, dy + y * zoom));
                let edge = subpaths.iter().flat_map(|(points, closed)| canvas::stroke(points, *closed, width)).collect();
                (Some(subpaths.into_iter().map(|(points, _)| points).collect()), edge)
            }
            "text" => {
                let text: String = el
                    .children
                    .iter()
                    .filter_map(|child| match child {
                        Node::Text(text) => Some(&**text),
                        Node::Element(_) => None,
                    })
                    .collect();
                let (x, y) = at(num("x"), num("y"));
                let (size, face) = (style.font_size * scale, style.face());
                if let Some(handler) = el.attr("on-click") {
                    let (height, baseline) = self.line_box(&style);
                    self.push_link(Clip::rect(x, y - baseline, self.measure(&text, &style), height), Action::Run(handler.clone()));
                }
                self.items.push(DisplayItem::Text { x, y, text, size, face, color: style.color });
                return;
            }
            _ => return,
        };
        // Lines, which are only stroked, take their colour from `:stroke` or the text.
        let stroke = stroke.or(match outline {
            None => Some(style.color),
            Some(_) => None,
        });

        let mut bounds: Option<Clip> = None;
        let mut draw = |items: &mut Vec<DisplayItem>, contours: Vec<Contour>, color| {
            for &(x, y) in contours.iter().flatten() {
                let point = Clip { x0: x, y0: y, x1: x, y1: y };
                bounds = Some(bounds.map_or(point, |b| Clip { x0: b.x0.min(x), y0: b.y0.min(y), x1: b.x1.max(x), y1: b.y1.max(y) }));
            }
            items.push(DisplayItem::Polygon { contours, color });
        };
        if let (Some(outline), Some(color)) = (outline, fill) {
            draw(&mut self.items, outline, color);
        }
        if let Some(color) = stroke
            && width > 0.0
        {
            draw(&mut self.items, edge, color);
        }
        if let (Some(handler), Some(bounds)) = (el.attr("on-click"), bounds) {
            self.push_link(bounds, Action::Run(handler.clone()));
        }
    }

    /// The line height for `style`, and the baseline's offset from the top of the line.
    fn line_box(&self, style: &Style) -> (f32, f32) {
        let size = style.font_size * self.scale;
        let metrics = self.fonts.get(style.face()).horizontal_line_metrics(size).expect("horizontal font");
        let height = size * style.line_height;
        (height, (height - (metrics.ascent - metrics.descent)) / 2.0 + metrics.ascent)
    }

    fn measure(&self, text: &str, style: &Style) -> f32 {
        self.fonts.measure(text, style.font_size * self.scale, style.face())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Family;

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
        assert_eq!(bang_x, world_x + fonts.measure("world", 16.0, Face { family: Family::Sans, weight: 700 }));
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
        let mono = Fonts::load().measure("  c", 14.0, Face { family: Family::Mono, weight: 400 });
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
        assert_eq!(
            image_boxes(&items),
            [natural, (80.0, 40.0, true), (20.0, 10.0, true), (5.0, 50.0, true), (24.0, 24.0, false), (30.0, 30.0, false)]
        );
    }

    #[test]
    fn filled_images_take_the_content_width() {
        let (items, _) = layout_with_image(r#"(page :padding 0 (img :src "pic.png" :width :fill))"#);
        assert_eq!(image_boxes(&items), [(800.0, 400.0, true)], "the whole layout width");

        let (items, _) = layout_with_image(
            r#"(page :padding 0 (div :max-width 300 (img :src "pic.png" :width :fill)) (div :padding 50 (img :src "pic.png" :width :fill)))"#,
        );
        assert_eq!(image_boxes(&items), [(300.0, 150.0, true), (700.0, 350.0, true)], "the narrower content width");

        let (items, _) = layout_with_image(r#"(page :padding 0 (img :src "pic.png" :width :fill :height 10))"#);
        assert_eq!(image_boxes(&items), [(800.0, 10.0, true)], "stretched to the given height");
    }

    #[test]
    fn radius_is_scaled_onto_boxes_and_images() {
        let src = r##"(page :padding 0 (div :radius 6 :background "#eee" (span :radius 3 :background "#ccc" "tag")))"##;
        let Ok(Value::Element(page)) = crate::lisp::run(src, &crate::loader::test_context()) else { panic!("not a page") };
        let (fonts, scrolls) = (Fonts::load(), HashMap::new());
        let mut layout = Layout::new(&fonts, &Palette::LIGHT, 2.0, 600.0, &scrolls, None, true);
        layout.block(&page, &Style::root(&Palette::LIGHT), 0.0, 0.0, 400.0);
        let radii: Vec<f32> = layout
            .items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Rect { radius, .. } => Some(*radius),
                _ => None,
            })
            .collect();
        assert_eq!(radii, [0.0, 12.0, 6.0], "the page, the div and the span");

        let (items, _) = layout_with_image(r#"(page (img :src "pic.png" :radius 10) (img :src "pic.png"))"#);
        let radii: Vec<f32> = items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Image { radius, .. } => Some(*radius),
                _ => None,
            })
            .collect();
        assert_eq!(radii, [10.0, 0.0]);
    }

    #[test]
    fn a_phrase_shares_one_background() {
        let (items, _) = layout(r##"(page :padding 0 (p (span :background "#ccc" :radius 99 "two words")))"##, 400.0);
        let rects = items.iter().filter(|item| matches!(item, DisplayItem::Rect { .. })).count();
        assert_eq!(rects, 2, "the page's and the span's");
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
        let (image_top, baseline) = (
            items.iter().find_map(|item| match item {
                DisplayItem::Image { y, .. } => Some(*y),
                _ => None,
            }),
            texts(&items)[0].2,
        );
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

    fn polygons(items: &[DisplayItem]) -> Vec<(&[Contour], Color)> {
        items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Polygon { contours, color } => Some((&contours[..], *color)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn canvases_draw_their_shapes_from_their_corner() {
        let src = r##"(page :padding 0 (p (canvas :width 100 :height 50 :background "#fff" :color "#123"
                         (rect :x 10 :y 5 :w 20 :h 10)
                         (circle :x 50 :y 25 :r 10 :fill "red" :stroke "blue" :stroke-width 2)
                         (line :x1 0 :y1 0 :x2 100 :y2 50)
                         (text :x 5 :y 40 "hi"))))"##;
        let (items, _) = layout(src, 300.0);
        let shapes = polygons(&items);
        assert_eq!(shapes.len(), 4, "the rect, the circle's inside and edge, and the line");
        assert_eq!(
            shapes[0],
            (&[vec![(10.0, 5.0), (30.0, 5.0), (30.0, 15.0), (10.0, 15.0)]][..], Color::parse("#123").unwrap()),
            "filled in the text colour"
        );
        assert_eq!((shapes[1].1, shapes[2].1), (Color::parse("red").unwrap(), Color::parse("blue").unwrap()));
        assert_eq!(shapes[2].0.len(), 2, "the edge is a ring");
        assert_eq!(shapes[3].1, Color::parse("#123").unwrap(), "a line is stroked in the text colour");
        assert!(texts(&items).contains(&("hi", 5.0, 40.0)), "text sits on its baseline at :y");

        let clips: Vec<Clip> = items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Clip(c) => Some(*c),
                _ => None,
            })
            .collect();
        assert_eq!(clips, [Clip::rect(0.0, 0.0, 100.0, 50.0), Clip::NONE], "shapes are clipped to the canvas");
    }

    #[test]
    fn canvas_shapes_can_be_clicked_and_the_top_one_wins() {
        let src = r#"(page :padding 0 (p (canvas :width 100 :height 50 :on-click (fn () "canvas")
                        (rect :x 0 :y 0 :w 60 :h 50 :on-click (fn () "under"))
                        (circle :x 50 :y 25 :r 10 :on-click (fn () "over")))))"#;
        let (_, links) = layout(src, 300.0);
        let clicked = |x, y| match &links.iter().find(|link| link.contains(x, y)).unwrap().action {
            Action::Run(handler) => crate::lisp::apply(handler, &[]).unwrap().to_string(),
            Action::Go { .. } => panic!("not a handler"),
        };
        assert_eq!(clicked(50.0, 25.0), "over");
        assert_eq!(clicked(5.0, 5.0), "under");
        assert_eq!(clicked(90.0, 5.0), "canvas");
    }

    #[test]
    fn ctrl_click_links_are_underlined_while_ctrl_is_held() {
        let src = r#"(page :padding 0 (p (a :href "x.sx" :ctrl-click true "def")))"#;
        let Ok(Value::Element(page)) = crate::lisp::run(src, &crate::loader::test_context()) else { panic!("not a page") };
        let fonts = Fonts::load();
        let scrolls = HashMap::new();
        let rects = |ctrl| {
            let mut layout = Layout::new(&fonts, &Palette::LIGHT, 1.0, 600.0, &scrolls, None, true);
            layout.ctrl(ctrl);
            layout.block(&page, &Style::root(&Palette::LIGHT), 0.0, 0.0, 800.0);
            layout.items.iter().filter(|item| matches!(item, DisplayItem::Rect { .. })).count()
        };
        assert_eq!(rects(true), rects(false) + 1, "an underline");
    }

    #[test]
    fn links_record_hit_boxes_with_their_href() {
        let (_, links) = layout(r#"(page :padding 0 (p "see " (a :href "x.sx" "here")))"#, 800.0);
        assert!(!links.is_empty() && links.iter().all(|l| matches!(&l.action, Action::Go { href, .. } if &**href == "x.sx")));
        assert!(links[0].contains(links[0].x + 1.0, links[0].y + 1.0));
        assert!(!links[0].contains(1.0, 1.0), "the unlinked 'see' is not clickable");
    }

    fn rects(items: &[DisplayItem]) -> Vec<(f32, f32, f32, f32)> {
        items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Rect { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn fonts_and_weights_are_inherited() {
        let (items, _) =
            layout(r#"(page :font "Open Sans" (p "a" (b "b") (span :weight 500 "c")) (h1 :weight 800 "d") (p :mono true "e"))"#, 800.0);
        let faces: Vec<Face> = items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Text { face, .. } => Some(*face),
                _ => None,
            })
            .collect();
        let open = |weight| Face { family: Family::OpenSans, weight };
        assert_eq!(faces, [open(400), open(700), open(500), open(800), Face { family: Family::Mono, weight: 400 }]);
    }

    #[test]
    fn fonts_fall_back_to_the_nearest_weight() {
        let fonts = Fonts::load();
        let width = |family, weight| fonts.measure("Wide words", 20.0, Face { family, weight });
        assert_eq!(width(Family::Sans, 800), width(Family::Sans, 700));
        assert_eq!(width(Family::Sans, 500), width(Family::Sans, 400), "500 takes the lighter of two as near");
        assert_eq!(width(Family::Sans, 600), width(Family::Sans, 700));
        assert_ne!(width(Family::OpenSans, 500), width(Family::OpenSans, 400), "Open Sans has a medium weight");
    }

    #[test]
    fn line_height_spaces_lines() {
        let (items, _) = layout(r#"(page :padding 0 (p :margin 0 :line-height 2 "a") (p "b"))"#, 800.0);
        let texts = texts(&items);
        // The extra 32 - 20.8 is shared above and below the first line's text.
        assert!((texts[1].2 - texts[0].2 - (20.8 + 11.2 / 2.0)).abs() < 0.01, "{texts:?}");
    }

    #[test]
    fn rows_give_widths_and_gaps_and_share_the_rest() {
        let (items, _) = layout(
            r##"(page :padding 0 (row :gap 10 (div :width 100 :background "#111" "a") (div :background "#222" "b") (div :background "#333" "c")))"##,
            410.0,
        );
        let rects = rects(&items);
        let xs_ws: Vec<(f32, f32)> = rects[1..].iter().filter(|r| r.2 > 50.0).map(|r| (r.0, r.2)).collect();
        assert_eq!(xs_ws, [(0.0, 100.0), (110.0, 145.0), (265.0, 145.0)]);
    }

    #[test]
    fn rows_fit_inline_children_and_spacers_fill() {
        let fonts = Fonts::load();
        let (items, _) = layout(r#"(page :padding 0 (row :gap 0 (b "left") (spacer) (b "right")))"#, 400.0);
        let texts = texts(&items);
        let right = fonts.measure("right", 16.0, Face { family: Family::Sans, weight: 700 });
        assert_eq!(texts[0].1, 0.0);
        assert!((texts[1].1 + right - 400.0).abs() < 0.01, "pushed to the end: {texts:?}");
    }

    #[test]
    fn inputs_can_have_rounded_corners() {
        let (items, _) = layout(r#"(page :padding 0 (p (input :radius 8)))"#, 300.0);
        let radii: Vec<f32> = items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Rect { radius, .. } => Some(*radius),
                _ => None,
            })
            .collect();
        assert!(radii.ends_with(&[8.0, 7.0]), "the border's corners, and the background's just inside them: {radii:?}");
    }

    #[test]
    fn textareas_show_lines_and_can_highlight_netscheme() {
        let (items, _) = layout(r#"(page :padding 0 (p :margin 0 (textarea :rows 3 :value "a\n(p :x 1)")))"#, 500.0);
        let texts = texts(&items);
        let [("a", ax, ay), ("(p :x 1)", px, py)] = texts[..] else { panic!("{texts:?}") };
        assert_eq!((ax, px), (6.0, 6.0));
        assert!((py - ay - 16.0 * 1.3).abs() < 0.01, "one line below");
        let box_height = items.iter().find_map(|item| match item {
            DisplayItem::Rect { h, .. } => Some(*h),
            _ => None,
        });
        assert_eq!(box_height, Some(16.0 * 1.3 * 3.0 + 12.0), "three lines tall, with padding and a border");

        let (items, _) = layout(r#"(page :padding 0 (textarea :code :netscheme :value "(p :x 1) ; hi"))"#, 500.0);
        let colors: Vec<(&str, Color)> = items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Text { text, color, .. } => Some((text.as_str(), *color)),
                _ => None,
            })
            .collect();
        let p = &Palette::LIGHT;
        assert_eq!(
            colors,
            vec![
                ("(", p.text),
                ("p", p.syntax_head),
                (" ", p.text),
                (":x", p.syntax_keyword),
                (" ", p.text),
                ("1", p.syntax_constant),
                (") ", p.text),
                ("; hi", p.syntax_comment)
            ]
        );
    }

    #[test]
    fn inputs_are_padded_on_each_side() {
        let (plain, _) = layout(r#"(page :padding 0 (p :margin 0 (input :value "x")))"#, 300.0);
        let (padded, _) = layout(r#"(page :padding 0 (p :margin 0 (input :value "x" :padding-left 12)))"#, 300.0);
        let text_x = |items: &[DisplayItem]| texts(items)[0].1;
        assert_eq!(text_x(&plain), 6.0, "5px of padding and a 1px border");
        assert_eq!(text_x(&padded), 13.0, "12px of padding on the left");
        // The input's box is the last rectangle drawn, as it has no selection or caret.
        let height = |items: &[DisplayItem]| {
            items.iter().rev().find_map(|item| match item {
                DisplayItem::Rect { h, .. } => Some(*h),
                _ => None,
            })
        };
        assert_eq!(height(&plain), height(&padded), "and no taller");
    }

    #[test]
    fn inputs_can_fill_the_room_in_a_row() {
        let page = r#"(page :padding 0 (row :gap 0 (div :width 100) (input :width :fill) (div :width 100)))"#;
        let Ok(Value::Element(page)) = crate::lisp::run(page, &crate::loader::test_context()) else { panic!("not a page") };
        let fonts = Fonts::load();
        let scrolls = HashMap::new();
        let mut layout = Layout::new(&fonts, &Palette::LIGHT, 1.0, 600.0, &scrolls, None, true);
        layout.block(&page, &Style::root(&Palette::LIGHT), 0.0, 0.0, 500.0);
        let area = layout.inputs[0].area;
        assert_eq!((area.x0, area.x1), (100.0, 400.0), "between the columns either side");
    }

    #[test]
    fn rows_can_centre_their_columns() {
        let (items, _) =
            layout_with_image(r#"(page :padding 0 (row :align "middle" (img :src "pic.png" :width 120) (div (p :margin 0 "title"))))"#);
        let image = items.iter().find_map(|item| match item {
            DisplayItem::Image { x, y, h, .. } => Some((*x, *y, *h)),
            _ => None,
        });
        assert_eq!(image, Some((0.0, 0.0, 60.0)));
        let (_, x, baseline) = texts(&items)[0];
        assert_eq!(x, 136.0, "after the image and the gap");
        assert!(baseline > 20.0 && baseline < 45.0, "the text is centred against the image: {baseline}");
    }

    #[test]
    fn blocks_can_shrink_to_fit_and_sit_on_the_right() {
        let fonts = Fonts::load();
        let text = fonts.measure("Home", 16.0, Face::default());
        let (items, _) = layout(r##"(page :padding 0 (div :width "fit" :align "right" :padding 10 :background "#222" "Home"))"##, 400.0);
        let (x, _, w, h) = rects(&items)[1];
        assert!((w - (text + 20.0)).abs() < 0.01, "as wide as its text and padding: {w}");
        assert!((x + w - 400.0).abs() < 0.01, "at the right edge: {x}");
        assert!((h - (16.0 * 1.3 + 20.0)).abs() < 0.01, "with padding above and below");

        let (items, _) = layout(r##"(page :padding 0 (div :width 100 :align "center" :background "#222" "x"))"##, 400.0);
        assert_eq!(rects(&items)[1].0, 150.0);
    }

    #[test]
    fn fitting_a_row_adds_up_its_columns() {
        let fonts = Fonts::load();
        let words = fonts.measure("Home", 14.0, Face::default()) + fonts.measure("Blog", 14.0, Face::default());
        let (items, _) =
            layout(r##"(page :padding 0 (div :width "fit" :background "#222" :size 14 (row :gap 32 (a "Home") (a "Blog"))))"##, 400.0);
        let w = rects(&items)[1].2;
        assert!((w - (words + 32.0)).abs() < 0.01, "{w} vs {}", words + 32.0);
        let texts = texts(&items);
        assert_eq!(texts.len(), 2);
        assert_eq!(texts[0].2, texts[1].2, "on one line");
    }

    #[test]
    fn margins_and_padding_on_each_side() {
        let (items, _) = layout(
            r##"(page :padding 0 (div :margin-top 20 :padding-top 5 :padding-bottom 7 :padding-left 3 :background "#222" (p :margin 0 "x")))"##,
            400.0,
        );
        let (x, y, _, h) = rects(&items)[1];
        assert_eq!((x, y), (0.0, 20.0));
        assert!((h - (5.0 + 16.0 * 1.3 + 7.0)).abs() < 0.01);
        assert_eq!(texts(&items)[0].1, 3.0);
    }

    #[test]
    fn borders_are_drawn_inside_the_box_and_push_the_content_in() {
        let (items, _) = layout(r##"(page :padding 0 (div :border "#f15125" :border-width 2 :radius 8 "x"))"##, 400.0);
        let border = items.iter().find_map(|item| match item {
            DisplayItem::Border { x, y, w, width, color, radius, .. } => Some((*x, *y, *w, *width, *color, *radius)),
            _ => None,
        });
        assert_eq!(border, Some((0.0, 0.0, 400.0, 2.0, Color(0xf1, 0x51, 0x25, 255), 8.0)));
        assert_eq!(texts(&items)[0].1, 2.0);
    }

    fn layout_pointing(src: &str, pointer: (f32, f32)) -> Layout<'static> {
        let Ok(Value::Element(page)) = crate::lisp::run(src, &crate::loader::test_context()) else { panic!("not a page") };
        let fonts = Box::leak(Box::new(Fonts::load()));
        let scrolls = Box::leak(Box::new(HashMap::new()));
        let mut layout = Layout::new(fonts, &Palette::LIGHT, 1.0, 600.0, scrolls, None, true);
        layout.point(pointer.0, pointer.1);
        layout.block(&page, &Style::root(&Palette::LIGHT), 0.0, 0.0, 400.0);
        layout
    }

    /// The colours of the border, the title, and the text below.
    fn colours(items: &[DisplayItem]) -> Vec<Color> {
        items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Text { color, text, .. } if text == "title" || text == "below" => Some(*color),
                DisplayItem::Border { color, .. } => Some(*color),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn hover_styles_apply_under_the_pointer() {
        let src = r##"(page :padding 0
                        (div :group true :border "#333" :hover (list :border "#f15125")
                          (p :margin 0 "not the heading")
                          (p :margin 0 :group-hover (list :color "#f15125") "title"))
                        (p "below"))"##;
        let (red, grey, ink) = (Color::parse("#f15125").unwrap(), Color::parse("#333").unwrap(), Palette::LIGHT.text);
        let away = layout_pointing(src, (10.0, 500.0));
        assert_eq!(colours(&away.items), [grey, ink, ink]);
        assert_eq!(away.hover_areas.len(), 1, "the div can be hovered");

        let over = layout_pointing(src, (10.0, 5.0));
        assert_eq!(colours(&over.items), [red, red, ink], "the border and the title change");
        assert_eq!(over.hover_areas.len(), 1);
        assert_eq!(over.lines.len(), away.lines.len(), "laid out once, in the end");
    }

    #[test]
    fn links_that_hold_blocks_are_cards() {
        let src = r##"(page :padding 0
                        (a :href "about.sx" :group true :padding 16 :margin 12 :border "#333" :hover (list :border "#f15125")
                          (p :margin 0 :group-hover (list :color "#f15125") "title")
                          (p :margin 0 "text"))
                        (p (a :href "inline.sx" "below")))"##;
        let away = layout_pointing(src, (10.0, 500.0));
        let go = |link: &Link| match &link.action {
            Action::Go { href, .. } => href.to_string(),
            Action::Run(_) => panic!("not a link"),
        };
        let [card, inline] = &away.links[..] else { panic!("two links") };
        assert_eq!((go(card), go(inline)), ("about.sx".into(), "inline.sx".into()));
        assert_eq!((card.x, card.y, card.w), (0.0, 0.0, 400.0), "the card's whole box, padding and all");
        assert!((card.h - (2.0 * 17.0 + 2.0 * 16.0 * 1.3)).abs() < 0.01, "two lines inside 16px of padding and a border: {}", card.h);
        let (grey, ink, link) = (Color::parse("#333").unwrap(), Palette::LIGHT.text, Palette::LIGHT.link);
        assert_eq!(colours(&away.items), [grey, ink, link], "the card's text keeps its colour; the inline link's doesn't");
        let underlines = away.items.iter().filter(|item| matches!(item, DisplayItem::Rect { h, .. } if *h == 1.0)).count();
        assert_eq!(underlines, 1, "only the inline link is underlined");

        let red = Color::parse("#f15125").unwrap();
        let over = layout_pointing(src, (10.0, 5.0));
        assert_eq!(colours(&over.items), [red, red, link], "it's a group, and can be hovered");
    }

    #[test]
    fn paths_draw_svg_path_data() {
        let src = r##"(page :padding 0 (p (canvas :width 48 :height 48 (path :scale 2 :x 1 :d "M0 0 H10 V10 H0 Z" :fill "#123"))))"##;
        let (items, _) = layout(src, 300.0);
        let shapes = polygons(&items);
        assert_eq!(shapes.len(), 1);
        assert_eq!(shapes[0].0, [vec![(1.0, 0.0), (21.0, 0.0), (21.0, 20.0), (1.0, 20.0)]], "scaled, at :x");
    }
}

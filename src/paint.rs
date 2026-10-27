//! Rasterizes a display list into a 0RGB pixel buffer.

use std::{
    collections::{HashMap, VecDeque},
    rc::Rc,
};

use fontdue::{Font, FontSettings, Metrics};

use crate::{
    canvas::Contour,
    damage::{self, Blit, Changes, Frame},
    dom::Element,
    image::Image,
    layout::{
        Anchor, Clip, DisplayItem, Drawings, Focus, FramedPage, InputBox, KeyTarget, Layout, Link, Magnify, Match, Overlay, Scrollbar,
        Scroller, Search, Site, TextLine,
    },
    loader::Images,
    style::{Color, Face, Family, Palette, Style},
};

/// How many scaled copies of images to keep before starting afresh.
const MAX_SCALED_IMAGES: usize = 64;

/// The bundled fonts, by family and weight.
pub struct Fonts {
    faces: Vec<(Family, u16, Font)>,
}

impl Fonts {
    pub fn load() -> Fonts {
        let load = |bytes: &[u8]| Font::from_bytes(bytes, FontSettings::default()).expect("bundled font");
        let faces = vec![
            (Family::Sans, 400, load(include_bytes!("../assets/fonts/LiberationSans-Regular.ttf"))),
            (Family::Sans, 700, load(include_bytes!("../assets/fonts/LiberationSans-Bold.ttf"))),
            (Family::Mono, 400, load(include_bytes!("../assets/fonts/LiberationMono-Regular.ttf"))),
            (Family::Mono, 700, load(include_bytes!("../assets/fonts/LiberationMono-Bold.ttf"))),
            (Family::OpenSans, 400, load(include_bytes!("../assets/fonts/OpenSans-Regular.ttf"))),
            (Family::OpenSans, 500, load(include_bytes!("../assets/fonts/OpenSans-Medium.ttf"))),
            (Family::OpenSans, 600, load(include_bytes!("../assets/fonts/OpenSans-SemiBold.ttf"))),
            (Family::OpenSans, 700, load(include_bytes!("../assets/fonts/OpenSans-Bold.ttf"))),
            (Family::OpenSans, 800, load(include_bytes!("../assets/fonts/OpenSans-ExtraBold.ttf"))),
        ];
        Fonts { faces }
    }

    /// The font in `face`'s family nearest its weight. Between two as near,
    /// a weight under 500 takes the lighter one, as CSS does.
    pub fn get(&self, face: Face) -> &Font {
        let distance = |weight: u16| {
            let d = (weight as i32 - face.weight as i32).abs() * 2;
            // Break ties: the side CSS prefers is a little nearer.
            if (weight < face.weight) == (face.weight < 500) { d - 1 } else { d }
        };
        let family = self.faces.iter().filter(|(family, ..)| *family == face.family);
        family.min_by_key(|(_, weight, _)| distance(*weight)).map(|(.., font)| font).expect("every family has a font")
    }

    pub fn measure(&self, text: &str, size: f32, face: Face) -> f32 {
        let font = self.get(face);
        text.chars().map(|c| font.metrics(c, size).advance_width).sum()
    }
}

/// How far the page, and each fixed-height frame in it, is scrolled.
#[derive(Clone, Default)]
pub struct Scroll {
    pub page: f32,
    /// By `Scroller::id`.
    pub frames: HashMap<usize, f32>,
}

/// What the user is doing, as far as drawing the page is concerned.
pub struct View<'a> {
    pub scale: f32,
    pub scroll: &'a Scroll,
    pub focus: Option<Focus>,
    /// The canvas that has the keyboard, by `KeyTarget::id`.
    pub key_focus: Option<usize>,
    /// Whether a blinking caret is in its visible phase.
    pub caret_visible: bool,
    /// Whether to use the browser's dark colours.
    pub dark: bool,
    pub search: Option<Search<'a>>,
    /// The selected text, as a range of positions; see `TextLine`.
    pub selection: Option<(usize, usize)>,
    /// The images pages have loaded, for `img` elements to show.
    pub images: Option<&'a Images>,
    /// What canvases' `:draw` functions returned.
    pub drawings: Option<&'a Drawings>,
    /// Where the mouse pointer is in the window, for `:hover` styles.
    pub pointer: Option<(f32, f32)>,
    /// Whether Ctrl is held, which underlines links with `:ctrl-click`.
    pub ctrl: bool,
    /// Where the link under the pointer goes, shown in a box at the bottom of the window.
    pub status: Option<&'a str>,
    /// How the site is zoomed.
    pub magnify: Magnify,
}

pub struct Rendered {
    /// Link hit boxes, in window coordinates.
    pub links: Vec<Link>,
    /// Scrollable frames, in window coordinates.
    pub scrollers: Vec<Scroller>,
    /// Text inputs, in window coordinates.
    pub inputs: Vec<InputBox>,
    /// Canvases that listen for keys, in window coordinates.
    pub key_targets: Vec<KeyTarget>,
    /// The offsets actually used, after clamping.
    pub scroll: Scroll,
    /// The page's own scrollbar, if it's taller than the window.
    pub bar: Option<Scrollbar>,
    /// Where the search matched, in window coordinates.
    pub matches: Vec<Match>,
    /// Elements with an `:id`, in window coordinates.
    pub anchors: Vec<Anchor>,
    /// Every line of text, in window coordinates.
    pub lines: Vec<TextLine>,
    /// The visible parts of elements with hover styles, in window coordinates.
    pub hover_areas: Vec<Clip>,
    /// Overlays, topmost last, in window coordinates.
    pub overlays: Vec<Overlay>,
    /// Where the site was drawn, in window coordinates, and the frames on it.
    pub site: Option<Site>,
    pub framed: Vec<FramedPage>,
    /// The parts of the window that were painted, or `None` for all of it.
    pub damage: Option<Vec<Clip>>,
}

pub struct Renderer {
    pub fonts: Fonts,
    glyphs: HashMap<(char, u32, Face), (Metrics, Vec<u8>)>,
    /// Images scaled to the sizes they're drawn at, by `Image::id`, width and height.
    scaled: HashMap<(u64, u32, u32), Vec<u8>>,
    /// The last frame painted, and the colour behind it.
    last: Option<(Frame, Color)>,
    /// What changed in each recent frame, latest first, or `None` for a
    /// frame that was painted in full or blitted.
    history: VecDeque<Option<Vec<Clip>>>,
}

impl Renderer {
    pub fn new() -> Renderer {
        Renderer { fonts: Fonts::load(), glyphs: HashMap::new(), scaled: HashMap::new(), last: None, history: VecDeque::new() }
    }

    /// Lays out `page` and paints it into `buf` (`w` × `h` physical pixels),
    /// scrolled by `view.scroll`. Offsets are first clamped so nothing
    /// scrolls past its end. `age` is how many frames ago `buf` was last
    /// painted, or 0 if it's new; only what changed since then is painted.
    pub fn render(&mut self, page: &Element, buf: &mut [u32], w: usize, h: usize, view: &View, age: u8) -> Rendered {
        let palette = Palette::new(view.dark);
        let root = Style::root(palette);
        let page_background = Style::compute(page, &root).background.unwrap_or(palette.page);
        let mut layout = Layout::new(&self.fonts, palette, view.scale, h as f32, &view.scroll.frames, view.focus, view.caret_visible);
        if let Some(search) = view.search {
            layout.search(search);
        }
        if let Some(images) = view.images {
            layout.images(images);
        }
        if let Some(drawings) = view.drawings {
            layout.drawings(drawings);
        }
        if let Some(selection) = view.selection {
            layout.select(selection);
        }
        if let Some(id) = view.key_focus {
            layout.focus_keys(id);
        }
        // The page is laid out before it's scrolled.
        if let Some((x, y)) = view.pointer {
            layout.point(x, y + view.scroll.page.max(0.0));
        }
        layout.ctrl(view.ctrl);
        layout.magnify(view.magnify);
        // Overlays are placed in the part of the page the window shows. The
        // page can't be measured first, so they go by the scroll asked for.
        layout.view(Clip::rect(0.0, view.scroll.page.max(0.0).round(), w as f32, h as f32));
        let content_height = layout.block(page, &root, 0.0, 0.0, w as f32);
        let dy = view.scroll.page.clamp(0.0, (content_height - h as f32).max(0.0)).round();
        let (items, mut links, mut scrollers, mut inputs, mut matches) =
            (layout.items, layout.links, layout.scrollers, layout.inputs, layout.matches);
        let (mut key_targets, mut anchors) = (layout.key_targets, layout.anchors);
        let (mut lines, mut hover_areas, mut overlays) = (layout.lines, layout.hover_areas, layout.overlays);
        let (mut site, framed) = (layout.site, layout.framed);

        let screen = Clip::rect(0.0, 0.0, w as f32, h as f32);
        // Each frame's content moves up by how far it's scrolled, and by how
        // far the frames and the page it's in are.
        let scrolls: HashMap<usize, f32> = scrollers.iter().map(|s| (s.id, s.scroll)).collect();
        let mut offsets = HashMap::from([(None, dy)]);
        let mut layer = None;
        let mut items: Vec<DisplayItem> = items
            .into_iter()
            .map(|item| {
                if let DisplayItem::Layer(next) = item {
                    if let Some(id) = next {
                        let offset = offsets[&layer] + scrolls.get(&id).copied().unwrap_or(0.0);
                        offsets.entry(next).or_insert(offset);
                    }
                    layer = next;
                }
                item.placed(-offsets[&layer])
            })
            .collect();
        let bar = Scrollbar::new(screen, content_height, dy, view.scale);
        if let Some(bar) = bar {
            let (x, y, w, h) = (bar.thumb.x0, bar.thumb.y0, bar.thumb.x1 - bar.thumb.x0, bar.thumb.y1 - bar.thumb.y0);
            items.push(DisplayItem::Rect { x, y, w, h, color: palette.scrollbar, radius: 0.0 });
        }
        if let Some(status) = view.status {
            items.extend(self.status_box(status, screen, view, palette));
        }
        for link in &mut links {
            link.y -= dy;
        }
        let areas = scrollers.iter_mut().map(|s| &mut s.area).chain(inputs.iter_mut().map(|i| &mut i.area));
        let areas = areas.chain(matches.iter_mut().map(|m| &mut m.area)).chain(key_targets.iter_mut().map(|k| &mut k.area));
        let areas = areas.chain(hover_areas.iter_mut()).chain(anchors.iter_mut().map(|a| &mut a.area));
        let areas = areas.chain(overlays.iter_mut().map(|o| &mut o.area)).chain(site.iter_mut().map(|s| &mut s.area));
        for area in areas.chain(lines.iter_mut().flat_map(|l| [&mut l.area, &mut l.visible])) {
            area.y0 -= dy;
            area.y1 -= dy;
        }
        for scroller in &mut scrollers {
            scroller.bar = scroller.bar.map(|bar| bar.shifted(-dy));
        }
        for input in &mut inputs {
            input.caret_ys.iter_mut().for_each(|y| *y -= dy);
        }
        let frames = scrollers.iter().map(|s| (s.id, s.scroll)).collect();

        let mut layers: HashMap<_, _> = scrollers.iter().map(|s| (Some(s.id), (s.scroll, s.area))).collect();
        layers.insert(None, (dy, screen));
        let background = blend(palette.page.to_u32(), page_background, 255);
        let background = Color((background >> 16) as u8, (background >> 8) as u8, background as u8, 255);
        let damage = self.paint(buf, w, Frame { items, layers, screen }, background, age, palette);
        Rendered {
            links,
            scrollers,
            inputs,
            key_targets,
            scroll: Scroll { page: dy, frames },
            bar,
            matches,
            anchors,
            lines,
            hover_areas,
            overlays,
            site,
            framed,
            damage,
        }
    }

    /// A box at the bottom-left of the window showing `text`, as browsers
    /// show where the link under the pointer goes. It moves to the right if
    /// the pointer is over it, and long text is cut short to half the window.
    fn status_box(&self, text: &str, screen: Clip, view: &View, palette: &Palette) -> Vec<DisplayItem> {
        let (size, face) = (13.0 * view.scale, Face { family: Family::Sans, weight: 400 });
        let (padding, height) = ((8.0 * view.scale).round(), (22.0 * view.scale).round());
        let room = (screen.x1 / 2.0 - 2.0 * padding).max(0.0);
        let mut text = text.to_string();
        if self.fonts.measure(&text, size, face) > room {
            while !text.is_empty() && self.fonts.measure(&text, size, face) + self.fonts.measure("…", size, face) > room {
                text.pop();
            }
            text.push('…');
        }
        let w = (self.fonts.measure(&text, size, face) + 2.0 * padding).ceil();
        let y = screen.y1 - height;
        let left = Clip::rect(0.0, y, w, height);
        let x = match view.pointer {
            Some((px, py)) if left.contains(px, py) => screen.x1 - w,
            _ => 0.0,
        };
        let border = view.scale.round().max(1.0);
        vec![
            DisplayItem::Clip(screen),
            DisplayItem::Layer(None),
            DisplayItem::Rect { x, y, w, h: height, color: palette.button, radius: 0.0 },
            DisplayItem::Border { x, y, w, h: height, width: border, color: palette.border, radius: 0.0 },
            DisplayItem::Text { x: x + padding, y: y + (height + size * 0.7) / 2.0, text, size, face, color: palette.text },
        ]
    }

    /// Paints `frame` over `background`, or just the parts of it that differ
    /// from what `buf` showed `age` frames ago. Returns the parts painted, or
    /// `None` if it all was.
    fn paint(&mut self, buf: &mut [u32], w: usize, frame: Frame, background: Color, age: u8, palette: &Palette) -> Option<Vec<Clip>> {
        let changes = match &self.last {
            Some((last, last_background)) if *last_background == background && age > 0 => {
                damage::changes(last, &frame, |item| self.bounds(item))
            }
            _ => None,
        };
        let since_last = changes.as_ref().and_then(|changes| changes.blit.is_none().then(|| changes.areas.clone()));
        // An older buffer also missed what changed in the frames since it, and
        // can't be blitted, as the blit is from the last frame's pixels.
        let changes = changes.and_then(|mut changes| {
            if age > 1 {
                if changes.blit.is_some() {
                    return None;
                }
                for missed in self.history.iter().take(age as usize - 1) {
                    changes.areas.extend(missed.as_ref()?);
                }
                if self.history.len() < age as usize - 1 {
                    return None;
                }
                changes.areas = damage::merge(changes.areas);
            }
            Some(changes)
        });
        self.history.push_front(since_last);
        self.history.truncate(4);

        let painted = match changes {
            None => {
                self.paint_area(buf, w, &frame.items, frame.screen, background, palette);
                None
            }
            Some(Changes { blit, areas }) => {
                if let Some(blit) = blit {
                    blit_pixels(buf, w, blit);
                }
                for &area in &areas {
                    self.paint_area(buf, w, &frame.items, area, background, palette);
                }
                Some(areas.into_iter().chain(blit.map(|blit| blit.area)).collect())
            }
        };
        self.last = Some((frame, background));
        painted
    }

    /// Paints the part of `items` inside `area` over `background`.
    fn paint_area(&mut self, buf: &mut [u32], w: usize, items: &[DisplayItem], area: Clip, background: Color, palette: &Palette) {
        fill_rect(buf, w, area, background);
        let mut clip = area;
        for item in items {
            match item {
                DisplayItem::Rect { x, y, w: rw, h: rh, color, radius } => {
                    let rect = Clip::rect(*x, *y, *rw, *rh);
                    if *radius > 0.0 {
                        fill_rounded_rect(buf, w, clip, rect, *radius, *color)
                    } else {
                        fill_rect(buf, w, rect.intersect(clip), *color)
                    }
                }
                DisplayItem::Border { x, y, w: bw, h: bh, width, color, radius } => {
                    stroke_rounded_rect(buf, w, clip, Clip::rect(*x, *y, *bw, *bh), *radius, *width, *color)
                }
                DisplayItem::Text { x, y, text, size, face, color } => {
                    if y + size >= clip.y0 && y - size < clip.y1 && *x < clip.x1 {
                        self.draw_text(buf, w, clip, *x, *y, text, *size, *face, *color)
                    }
                }
                DisplayItem::Image { x, y, w: iw, h: ih, image, radius } => {
                    let rect = Clip::rect(*x, *y, *iw, *ih);
                    match image {
                        Some(image) => self.draw_image(buf, w, clip, rect, *radius, image),
                        None => fill_rounded_rect(buf, w, clip, rect, *radius, palette.button),
                    }
                }
                DisplayItem::Polygon { contours, color } => fill_polygon(buf, w, clip, contours, 0.0, *color),
                DisplayItem::Clip(c) => clip = c.intersect(area),
                DisplayItem::Layer(_) => {}
            }
        }
    }

    /// The area `item` may paint, with a pixel to spare for smoothed edges.
    fn bounds(&self, item: &DisplayItem) -> Clip {
        let grown = |c: Clip| Clip { x0: c.x0 - 1.0, y0: c.y0 - 1.0, x1: c.x1 + 1.0, y1: c.y1 + 1.0 };
        match item {
            DisplayItem::Rect { x, y, w, h, .. } | DisplayItem::Border { x, y, w, h, .. } | DisplayItem::Image { x, y, w, h, .. } => {
                grown(Clip::rect(*x, *y, *w, *h))
            }
            // Glyphs can reach a little past their advance, and above the font size.
            DisplayItem::Text { x, y, text, size, face, .. } => {
                let width = self.fonts.measure(text, *size, *face);
                grown(Clip { x0: x - size * 0.25, y0: y - size * 1.25, x1: x + width + size * 0.25, y1: y + size * 0.5 })
            }
            DisplayItem::Polygon { contours, .. } => {
                let points = contours.iter().flatten();
                let start = Clip { x0: f32::INFINITY, y0: f32::INFINITY, x1: f32::NEG_INFINITY, y1: f32::NEG_INFINITY };
                grown(points.fold(start, |b, &(x, y)| Clip { x0: b.x0.min(x), y0: b.y0.min(y), x1: b.x1.max(x), y1: b.y1.max(y) }))
            }
            DisplayItem::Clip(_) | DisplayItem::Layer(_) => Clip::NONE,
        }
    }

    /// Draws `image` scaled to fill `area`, blending it by its transparency,
    /// with its corners rounded by `radius`.
    fn draw_image(&mut self, buf: &mut [u32], w: usize, clip: Clip, area: Clip, radius: f32, image: &Rc<Image>) {
        let (x0, y0) = (area.x0.round() as i64, area.y0.round() as i64);
        let (iw, ih) = ((area.x1 - area.x0).round().max(1.0) as u32, (area.y1 - area.y0).round().max(1.0) as u32);
        let visible = Clip::rect(x0 as f32, y0 as f32, iw as f32, ih as f32).intersect(clip);
        if visible.is_empty() {
            return;
        }
        if self.scaled.len() >= MAX_SCALED_IMAGES {
            self.scaled.clear();
        }
        let pixels = self.scaled.entry((image.id, iw, ih)).or_insert_with(|| image.scaled(iw, ih));
        let shape = Clip::rect(x0 as f32, y0 as f32, iw as f32, ih as f32);
        for py in visible.y0.round() as i64..visible.y1.round() as i64 {
            for px in visible.x0.round() as i64..visible.x1.round() as i64 {
                let i = (((py - y0) * iw as i64 + (px - x0)) * 4) as usize;
                let [mut r, mut g, mut b, mut a] = [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]];
                let coverage = corner_coverage(shape, radius, px, py);
                if coverage < 1.0 {
                    // Premultiplied, so the colours fade along with the alpha.
                    [r, g, b, a] = [r, g, b, a].map(|c| (c as f32 * coverage).round() as u8);
                }
                if a == 0 {
                    continue;
                }
                let dst = &mut buf[py as usize * w + px as usize];
                // The colours are premultiplied, so they're added to what's left showing through.
                let over = |s: u8, shift: u32| (s as u32 + ((*dst >> shift) & 0xff) * (255 - a as u32) / 255).min(255);
                *dst = over(r, 16) << 16 | over(g, 8) << 8 | over(b, 0);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text(&mut self, buf: &mut [u32], w: usize, clip: Clip, x: f32, baseline: f32, text: &str, size: f32, face: Face, color: Color) {
        let mut pen = x;
        for c in text.chars() {
            let (m, bitmap) = self.glyphs.entry((c, size.to_bits(), face)).or_insert_with(|| self.fonts.get(face).rasterize(c, size));
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (baseline - m.height as f32 - m.ymin as f32).round() as i32;
            for row in 0..m.height {
                for col in 0..m.width {
                    let alpha = bitmap[row * m.width + col];
                    let (px, py) = (gx + col as i32, gy + row as i32);
                    if alpha > 0 && clip.contains(px as f32, py as f32) {
                        let dst = &mut buf[py as usize * w + px as usize];
                        *dst = blend(*dst, color, alpha);
                    }
                }
            }
            pen += m.advance_width;
        }
    }
}

/// Moves the pixels in `blit.area` down by `blit.dy`, or up if it's
/// negative. What moves out of the area is lost, and what it uncovers is
/// left as it was, to be painted.
fn blit_pixels(buf: &mut [u32], w: usize, blit: Blit) {
    let Blit { area, dy } = blit;
    let (x0, x1, y0, y1) = (area.x0 as usize, area.x1 as usize, area.y0 as i64, area.y1 as i64);
    let rows = (y0..y1).filter(|row| (y0..y1).contains(&(row - dy as i64)));
    let mut copy = |row: i64| {
        let from = (row - dy as i64) as usize * w;
        buf.copy_within(from + x0..from + x1, row as usize * w + x0);
    };
    // Rows are copied in the order that doesn't overwrite ones still to move.
    if dy > 0 { rows.rev().for_each(&mut copy) } else { rows.for_each(&mut copy) }
}

/// Fills `area`, which must already be clipped to the buffer.
fn fill_rect(buf: &mut [u32], w: usize, area: Clip, color: Color) {
    if area.is_empty() {
        return;
    }
    let (x0, x1) = (area.x0.round() as usize, area.x1.round() as usize);
    let (y0, y1) = (area.y0.round() as usize, area.y1.round() as usize);
    for row in y0..y1 {
        let pixels = &mut buf[row * w + x0..row * w + x1];
        if color.is_opaque() {
            pixels.fill(color.to_u32());
        } else {
            pixels.iter_mut().for_each(|dst| *dst = blend(*dst, color, 255));
        }
    }
}

/// Fills `area` with its corners rounded by `radius`, blending the pixels
/// along each curve by how much of them it covers.
fn fill_rounded_rect(buf: &mut [u32], w: usize, clip: Clip, area: Clip, radius: f32, color: Color) {
    let visible = area.intersect(clip);
    if visible.is_empty() {
        return;
    }
    let r = radius.min((area.x1 - area.x0) / 2.0).min((area.y1 - area.y0) / 2.0);
    // The rows between the corners are solid.
    let (top, bottom) = ((area.y0 + r).round(), (area.y1 - r).round());
    fill_rect(buf, w, Clip { y0: top, y1: bottom, ..area }.intersect(visible), color);
    let (x0, x1) = (visible.x0.round() as i64, visible.x1.round() as i64);
    let rows = (visible.y0.round() as i64..visible.y1.round() as i64).filter(|&py| (py as f32) < top || (py as f32) >= bottom);
    for py in rows {
        for px in x0..x1 {
            let alpha = (corner_coverage(area, radius, px, py) * 255.0).round() as u8;
            if alpha > 0 {
                let dst = &mut buf[py as usize * w + px as usize];
                *dst = blend(*dst, color, alpha);
            }
        }
    }
}

/// Draws the edge of `area`, `width` thick just inside it, with its
/// corners rounded by `radius`.
fn stroke_rounded_rect(buf: &mut [u32], w: usize, clip: Clip, area: Clip, radius: f32, width: f32, color: Color) {
    let visible = area.intersect(clip);
    if visible.is_empty() || width <= 0.0 {
        return;
    }
    let inner = Clip { x0: area.x0 + width, y0: area.y0 + width, x1: area.x1 - width, y1: area.y1 - width };
    let inner_radius = (radius - width).max(0.0);
    // Well inside the inner edge, where there's nothing to draw.
    let hollow = Clip {
        x0: inner.x0 + inner_radius + 1.0,
        y0: inner.y0 + inner_radius + 1.0,
        x1: inner.x1 - inner_radius - 1.0,
        y1: inner.y1 - inner_radius - 1.0,
    };
    for py in visible.y0.round() as i64..visible.y1.round() as i64 {
        for px in visible.x0.round() as i64..visible.x1.round() as i64 {
            let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
            if hollow.contains(cx, cy) {
                continue;
            }
            let inside = match inner.contains(cx, cy) {
                true => corner_coverage(inner, inner_radius, px, py),
                false => 0.0,
            };
            let alpha = ((corner_coverage(area, radius, px, py) - inside).max(0.0) * 255.0).round() as u8;
            if alpha > 0 {
                let dst = &mut buf[py as usize * w + px as usize];
                *dst = blend(*dst, color, alpha);
            }
        }
    }
}

/// How much of pixel (`px`, `py`) is inside `area` with its corners rounded
/// by `radius`, at most half its shorter side: 1 away from the corners, and
/// fading out over a pixel across each curve.
fn corner_coverage(area: Clip, radius: f32, px: i64, py: i64) -> f32 {
    let r = radius.min((area.x1 - area.x0) / 2.0).min((area.y1 - area.y0) / 2.0);
    if r <= 0.0 {
        return 1.0;
    }
    let (x, y) = (px as f32 + 0.5, py as f32 + 0.5);
    // The centre of the nearest corner's circle; on a straight edge, the nearest point on the inner box.
    let (cx, cy) = (x.clamp(area.x0 + r, area.x1 - r), y.clamp(area.y0 + r, area.y1 - r));
    if cx == x || cy == y {
        return 1.0;
    }
    (0.5 - ((x - cx).hypot(y - cy) - r)).clamp(0.0, 1.0)
}

/// Rows of samples taken in each row of pixels, to smooth a polygon's edges.
const SAMPLES: usize = 4;

/// An edge of a polygon, from its top to its bottom.
struct Edge {
    top: f32,
    bottom: f32,
    /// Where the edge is at `top`, and how far it moves along for each pixel down.
    x: f32,
    slope: f32,
    /// 1 for an edge that went down, -1 for one that went up.
    winding: i32,
}

/// Fills the area inside `contours`, moved up by `dy`, by the nonzero rule.
/// Each row of pixels is sampled along `SAMPLES` lines across it, and each
/// pixel is blended by how much of those lines inside the area cover it.
fn fill_polygon(buf: &mut [u32], w: usize, clip: Clip, contours: &[Contour], dy: f32, color: Color) {
    let mut edges = Vec::new();
    let mut bounds = Clip { x0: f32::INFINITY, y0: f32::INFINITY, x1: f32::NEG_INFINITY, y1: f32::NEG_INFINITY };
    for contour in contours {
        for (i, &(ax, ay)) in contour.iter().enumerate() {
            let (bx, by) = contour[(i + 1) % contour.len()];
            let (ay, by) = (ay - dy, by - dy);
            bounds = Clip { x0: bounds.x0.min(ax), y0: bounds.y0.min(ay), x1: bounds.x1.max(ax), y1: bounds.y1.max(ay) };
            if ay != by {
                let slope = (bx - ax) / (by - ay);
                edges.push(match ay < by {
                    true => Edge { top: ay, bottom: by, x: ax, slope, winding: 1 },
                    false => Edge { top: by, bottom: ay, x: bx, slope, winding: -1 },
                });
            }
        }
    }
    let pixels = Clip { x0: clip.x0.round(), y0: clip.y0.round(), x1: clip.x1.round(), y1: clip.y1.round() };
    let area = Clip { x0: bounds.x0.floor(), y0: bounds.y0.floor(), x1: bounds.x1.ceil(), y1: bounds.y1.ceil() }.intersect(pixels);
    if area.is_empty() {
        return;
    }
    let (x0, x1, y0, y1) = (area.x0 as usize, area.x1 as usize, area.y0 as usize, area.y1 as usize);
    edges.sort_by(|a, b| a.top.total_cmp(&b.top));

    let mut coverage = vec![0.0f32; x1 - x0];
    let (mut next, mut active, mut crossings) = (0, Vec::new(), Vec::new());
    for py in y0..y1 {
        coverage.fill(0.0);
        for sample in 0..SAMPLES {
            let sy = py as f32 + (sample as f32 + 0.5) / SAMPLES as f32;
            while next < edges.len() && edges[next].top <= sy {
                active.push(next);
                next += 1;
            }
            active.retain(|&i| edges[i].bottom > sy);
            crossings.clear();
            crossings.extend(active.iter().map(|&i| {
                let edge = &edges[i];
                (edge.x + (sy - edge.top) * edge.slope - x0 as f32, edge.winding)
            }));
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
            let (mut winding, mut start) = (0, 0.0);
            for &(x, change) in &crossings {
                if winding == 0 {
                    start = x;
                }
                winding += change;
                if winding == 0 {
                    cover(&mut coverage, start, x);
                }
            }
        }
        let row = &mut buf[py * w + x0..py * w + x1];
        for (dst, &covered) in row.iter_mut().zip(&coverage) {
            let alpha = (covered / SAMPLES as f32 * 255.0).round().min(255.0) as u8;
            match alpha {
                0 => {}
                255 if color.is_opaque() => *dst = color.to_u32(),
                _ => *dst = blend(*dst, color, alpha),
            }
        }
    }
}

/// Adds the part of each pixel between `a` and `b` to `coverage`, a row of
/// pixels starting at 0.
fn cover(coverage: &mut [f32], a: f32, b: f32) {
    let len = coverage.len();
    let (a, b) = (a.max(0.0), b.min(len as f32));
    if b <= a {
        return;
    }
    let (first, last) = (a.floor() as usize, b.floor() as usize);
    if first == last {
        coverage[first] += b - a;
        return;
    }
    coverage[first] += (first + 1) as f32 - a;
    for pixel in &mut coverage[first + 1..last] {
        *pixel += 1.0;
    }
    if last < len {
        coverage[last] += b - last as f32;
    }
}

/// Mixes `src` into `dst` by `alpha` and by `src`'s own opacity.
fn blend(dst: u32, src: Color, alpha: u8) -> u32 {
    let a = alpha as u32 * src.3 as u32 / 255;
    let mix = |s: u8, shift: u32| (s as u32 * a + ((dst >> shift) & 0xff) * (255 - a)) / 255;
    mix(src.0, 16) << 16 | mix(src.1, 8) << 8 | mix(src.2, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lisp::{self, Value};

    fn render(src: &str, h: usize, scroll: f32) -> Rendered {
        let Ok(Value::Element(page)) = lisp::run(src, &crate::loader::test_context()) else { panic!("not a page") };
        let mut buf = vec![0; 200 * h];
        let scroll = Scroll { page: scroll, frames: HashMap::new() };
        Renderer::new().render(
            &page,
            &mut buf,
            200,
            h,
            &View {
                scale: 1.0,
                scroll: &scroll,
                focus: None,
                key_focus: None,
                caret_visible: true,
                dark: false,
                search: None,
                selection: None,
                images: None,
                drawings: None,
                pointer: None,
                ctrl: false,
                status: None,
                magnify: Magnify::default(),
            },
            0,
        )
    }

    /// Paints `src` into a 200 × `h` buffer, starting from black.
    fn pixels(src: &str, h: usize) -> Vec<u32> {
        let Ok(Value::Element(page)) = lisp::run(src, &crate::loader::test_context()) else { panic!("not a page") };
        let mut buf = vec![0; 200 * h];
        let scroll = Scroll { page: 0.0, frames: HashMap::new() };
        Renderer::new().render(
            &page,
            &mut buf,
            200,
            h,
            &View {
                scale: 1.0,
                scroll: &scroll,
                focus: None,
                key_focus: None,
                caret_visible: true,
                dark: false,
                search: None,
                selection: None,
                images: None,
                drawings: None,
                pointer: None,
                ctrl: false,
                status: None,
                magnify: Magnify::default(),
            },
            0,
        );
        buf
    }

    #[test]
    fn polygons_are_filled_with_smooth_edges_and_holes() {
        let src = r##"(page :padding 0 :background "#000"
                        (canvas :width 200 :height 60 (circle :x 30.5 :y 30 :r 20 :fill "#fff")
                                (circle :x 100 :y 30 :r 20 :stroke "#fff" :stroke-width 4)
                                (rect :x 150 :y 10 :w 80 :h 10 :fill "#fff")))"##;
        let buf = pixels(src, 60);
        let at = |x: usize, y: usize| buf[y * 200 + x] & 0xff;
        assert_eq!(at(30, 30), 0xff, "inside");
        assert_eq!(at(5, 30), 0, "outside");
        let edge = at(10, 30);
        assert!(edge > 0x40 && edge < 0xc0, "half covered at the edge: {edge:#x}");
        assert_eq!(at(100, 30), 0, "a ring has a hole");
        assert_eq!(at(100, 11), 0xff, "and an edge");
        assert_eq!(at(199, 15), 0xff, "a shape reaches the canvas's edge");
        assert_eq!(at(150, 30), 0, "and stops there");
    }

    #[test]
    fn the_status_box_is_cut_short_and_keeps_out_of_the_pointers_way() {
        let renderer = Renderer::new();
        let scroll = Scroll { page: 0.0, frames: HashMap::new() };
        let view = |pointer| View {
            scale: 1.0,
            scroll: &scroll,
            focus: None,
            key_focus: None,
            caret_visible: true,
            dark: false,
            search: None,
            selection: None,
            images: None,
            drawings: None,
            pointer,
            ctrl: false,
            status: None,
            magnify: Magnify::default(),
        };
        let screen = Clip::rect(0.0, 0.0, 400.0, 300.0);
        let placed = |items: &[DisplayItem]| {
            items.iter().find_map(|item| match item {
                DisplayItem::Rect { x, y, w, .. } => Some((*x, *y, *w)),
                _ => None,
            })
        };
        let text = |items: &[DisplayItem]| {
            items.iter().find_map(|item| match item {
                DisplayItem::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
        };

        let items = renderer.status_box("pages/tour.sx", screen, &view(None), &Palette::LIGHT);
        let (x, y, w) = placed(&items).unwrap();
        assert_eq!((x, y + 22.0), (0.0, 300.0), "at the bottom-left");
        assert_eq!(text(&items).unwrap(), "pages/tour.sx");

        let long = "https://example.com/".to_string() + &"a".repeat(200);
        let items = renderer.status_box(&long, screen, &view(None), &Palette::LIGHT);
        let (_, _, long_w) = placed(&items).unwrap();
        assert!(long_w <= 200.0, "no wider than half the window: {long_w}");
        assert!(text(&items).unwrap().ends_with('…'));

        let items = renderer.status_box("pages/tour.sx", screen, &view(Some((5.0, 290.0))), &Palette::LIGHT);
        assert_eq!(placed(&items).unwrap().0, 400.0 - w, "moved to the right, out from under the pointer");
    }

    #[test]
    fn rounded_boxes_blend_their_corners() {
        let src = r##"(page :padding 0 :background "#000" (div :radius 20 :background "#fff" :padding 40))"##;
        let buf = pixels(src, 100);
        let at = |x: usize, y: usize| buf[y * 200 + x] & 0xff;
        assert_eq!(at(0, 0), 0, "the corner shows the background");
        assert_eq!(at(100, 40), 0xff, "the middle is filled");
        assert_eq!(at(100, 0), 0xff, "and the straight edges");
        // Its centre, (1.5, 12.5), is 19.96 px from the corner circle's, (20, 20).
        let edge = at(1, 12);
        assert!(edge > 0x20 && edge < 0xe0, "blended on the curve: {edge:#x}");

        let square = pixels(r##"(page :padding 0 :background "#000" (div :background "#fff" :padding 40))"##, 100);
        assert_eq!(square[0] & 0xff, 0xff, "no radius, square corners");
        let zero = pixels(r##"(page :padding 0 :background "#000" (div :radius 0 :background "#fff" :padding 40))"##, 100);
        assert!(zero == square, "radius 0 is the same as none");
    }

    #[test]
    fn rounded_images_have_clear_corners() {
        let Ok(Value::Element(page)) = lisp::run(
            r##"(page :padding 0 :background "#000" (img :src "pic.png" :width 40 :radius 999))"##,
            &crate::loader::test_context(),
        ) else {
            panic!("not a page")
        };
        let image = Image { id: 0, width: 4, height: 4, pixels: vec![255; 4 * 4 * 4] };
        let images = crate::loader::Images::from([("pic.png".to_string(), Ok(Rc::new(image)))]);
        let mut buf = vec![0; 200 * 60];
        let scroll = Scroll { page: 0.0, frames: HashMap::new() };
        let view = View {
            scale: 1.0,
            scroll: &scroll,
            focus: None,
            key_focus: None,
            caret_visible: true,
            dark: false,
            search: None,
            selection: None,
            images: Some(&images),
            drawings: None,
            pointer: None,
            ctrl: false,
            status: None,
            magnify: Magnify::default(),
        };
        Renderer::new().render(&page, &mut buf, 200, 60, &view, 0);
        let top = (0..60).find(|&y| buf[y * 200 + 20] & 0xff != 0).expect("the image is drawn");
        let at = |x: usize, y: usize| buf[(top + y) * 200 + x] & 0xff;
        assert_eq!(at(0, 0), 0, "a circle's corner is clear");
        assert_eq!(at(20, 20), 0xff, "its middle is the image");
        let edge = at(1, 12);
        assert!(edge > 0x20 && edge < 0xe0, "blended on the curve: {edge:#x}");
    }

    const TALL: &str = r#"(page :padding 0 (div :padding 500 (a :href "x.sx" "link")))"#;

    #[test]
    fn scroll_is_clamped_to_the_content() {
        assert_eq!(render(TALL, 100, -50.0).scroll.page, 0.0);
        let bottom = render(TALL, 100, 1e9).scroll.page;
        assert!(bottom > 900.0 && bottom < 1100.0 - 100.0, "{bottom}");
        assert_eq!(render(r#"(page "short")"#, 100, 50.0).scroll.page, 0.0, "nothing to scroll");
    }

    #[test]
    fn overlays_go_over_the_page_without_taking_room() {
        let page = r##"(page :padding 0
                         (div :padding 10 (p :margin 0 "anchor") (overlay :at "below" :width 50 :background "#c33" "menu"))
                         (p :margin 0 "after"))"##;
        let rendered = render(page, 100, 0.0);
        assert_eq!(rendered.overlays.len(), 1);
        let menu = rendered.overlays[0].area;
        assert_eq!((menu.x0, menu.x1), (0.0, 50.0), "as wide as it says");
        assert!(menu.y0 > 30.0, "below its anchor: {menu:?}");
        assert_eq!(
            rendered.lines[1].area,
            render(r#"(page :padding 0 (div :padding 10 (p :margin 0 "anchor")) (p :margin 0 "after"))"#, 100, 0.0).lines[1].area
        );
    }

    #[test]
    fn overlays_stay_in_the_window() {
        // Near the bottom, a menu opens upwards instead.
        let low = r##"(page :padding 0 (div :padding-top 80 (div (p :margin 0 "anchor") (overlay :background "#c33" (p :margin 0 "a") (p :margin 0 "b")))))"##;
        let menu = render(low, 100, 0.0).overlays[0].area;
        assert!(menu.y1 <= 80.5, "above its anchor: {menu:?}");
        // One fixed in the window stays put as the page scrolls.
        let fixed = |scroll| render(&TALL.replace("(page", r#"(page (overlay :at "bottom-right" "toast")"#), 100, scroll).overlays[0].area;
        let (top, scrolled) = (fixed(0.0), fixed(300.0));
        let edges = |c: Clip| [c.x0, c.y0, c.x1, c.y1];
        assert!(edges(top).iter().zip(edges(scrolled)).all(|(a, b)| (a - b).abs() < 0.01), "{top:?} {scrolled:?}");
        assert!(fixed(0.0).y1 <= 100.0 && fixed(0.0).x1 <= 200.0);
        // A backdrop covers the whole window.
        let modal = render(r##"(page (overlay :at "center" :backdrop "#0008" "sure?"))"##, 100, 0.0).overlays[0].area;
        assert_eq!(modal, Clip::rect(0.0, 0.0, 200.0, 100.0));
    }

    #[test]
    fn links_move_with_the_page() {
        let (top, scrolled) = (render(TALL, 100, 0.0), render(TALL, 100, 300.0));
        assert_eq!(scrolled.links[0].y, top.links[0].y - 300.0);
    }

    #[test]
    fn fill_frames_clip_their_page_and_scroll_by_themselves() {
        let dir = std::env::temp_dir().join(format!("toy-browser-frame-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("long.sx"), r#"(page (div :margin 400) (p (a :href "x.sx" "far down")) (div :margin 400))"#).unwrap();
        let ctx = std::rc::Rc::new(crate::loader::PageContext {
            location: dir.join("outer.sx").to_string_lossy().into(),
            params: Vec::new(),
            depth: 0,
            scale: 1.0,
            state_key: String::new(),
            store: Default::default(),
            host: Default::default(),
            frames_created: Default::default(),
        });
        let src = r#"(page :padding 0 (p "toolbar") (frame :src "long.sx" :height "fill" :margin 0))"#;
        let Ok(Value::Element(page)) = lisp::run(src, &ctx) else { panic!("not a page") };
        let mut renderer = Renderer::new();
        let mut buf = vec![0; 200 * 300];
        let mut render = |frames: HashMap<usize, f32>| {
            let scroll = Scroll { page: 0.0, frames };
            renderer.render(
                &page,
                &mut buf,
                200,
                300,
                &View {
                    scale: 1.0,
                    scroll: &scroll,
                    focus: None,
                    key_focus: None,
                    caret_visible: true,
                    dark: false,
                    search: None,
                    selection: None,
                    images: None,
                    drawings: None,
                    pointer: None,
                    ctrl: false,
                    status: None,
                    magnify: Magnify::default(),
                },
                0,
            )
        };

        let top = render(HashMap::new());
        assert_eq!(top.scrollers.len(), 1);
        assert!(top.links.is_empty(), "the link is below the frame's visible area");
        assert_eq!(top.scroll.page, 0.0, "the outer page fits the window");

        let bottom = render(HashMap::from([(0, 1e9)]));
        assert!(bottom.scrollers[0].scroll > 0.0 && bottom.scrollers[0].scroll < 1e9, "clamped to the content");

        let middle = render(HashMap::from([(0, 300.0)]));
        let (frame, link) = (&middle.scrollers[0], &middle.links[0]);
        assert_eq!(frame.scroll, 300.0);
        assert!(frame.area.contains(link.x, link.y), "the link scrolled into view");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// What changes from frame to frame in `painting_only_what_changed_matches_painting_everything`.
    #[derive(Clone, Copy, Default)]
    struct Moment {
        pointer: Option<(f32, f32)>,
        page: f32,
        frame: f32,
        caret: bool,
        selection: Option<(usize, usize)>,
        dark: bool,
    }

    #[test]
    fn painting_only_what_changed_matches_painting_everything() {
        let dir = std::env::temp_dir().join(format!("toy-browser-damage-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("long.sx"),
            r##"(page :padding 6
                  (div :group true :border "#888" :radius 4 :padding 4 :hover (style :border "#f15125" :background "#ffffff20")
                    (p :margin 0 :group-hover (style :color "#f15125") "a card in the frame"))
                  (map (fn (i) (p :margin 4 "line " i " of the framed page")) (range 30))
                  (overlay :at "top-right" :background "#c33" :padding 2 "pinned"))"##,
        )
        .unwrap();
        let ctx = std::rc::Rc::new(crate::loader::PageContext {
            location: dir.join("outer.sx").to_string_lossy().into(),
            params: Vec::new(),
            depth: 0,
            scale: 1.0,
            state_key: String::new(),
            store: Default::default(),
            host: Default::default(),
            frames_created: Default::default(),
        });
        let src = r##"(page :padding 10
                        (p "address " (input :value "netscheme"))
                        (div :group true :border "#333" :radius 6 :padding 8 :background "#00000010"
                             :hover (style :border "#f15125")
                          (p :margin 0 :group-hover (style :color "#f15125" :bold true) "card title")
                          (p :margin 0 (canvas :width 60 :height 30 (rect :x 5 :y 5 :w 30 :h 20 :fill "#36c") (circle :x 40 :y 15 :r 12 :fill "#c63"))))
                        (frame :src "long.sx" :height 160 :margin 0)
                        (map (fn (i) (p "paragraph " i " below the frame")) (range 40))
                        (overlay :at "bottom-right" :background "#333" :color "#fff" :padding 4 "toast"))"##;
        let Ok(Value::Element(page)) = lisp::run(src, &ctx) else { panic!("not a page") };
        let (w, h) = (300, 400);
        let render = |renderer: &mut Renderer, buf: &mut [u32], moment: Moment, age: u8| {
            let scroll = Scroll { page: moment.page, frames: HashMap::from([(0, moment.frame)]) };
            let view = View {
                scale: 1.0,
                scroll: &scroll,
                focus: Some(Focus { id: 0, cursor: 3, anchor: 3 }),
                key_focus: None,
                caret_visible: moment.caret,
                dark: moment.dark,
                search: None,
                selection: moment.selection,
                images: None,
                drawings: None,
                pointer: moment.pointer,
                ctrl: false,
                status: None,
                magnify: Magnify::default(),
            };
            renderer.render(&page, buf, w, h, &view, age)
        };
        // Where the cards are, to point at them.
        let areas = render(&mut Renderer::new(), &mut vec![0; w * h], Moment::default(), 0).hover_areas;
        let middle = |c: Clip| Some(((c.x0 + c.x1) / 2.0, (c.y0 + c.y1) / 2.0));
        let (card, framed_card) = (middle(areas[0]), middle(areas[1]));
        let start = Moment { caret: true, ..Moment::default() };
        let moments = [
            start,
            Moment { pointer: card, ..start },
            Moment { pointer: Some((5.0, 395.0)), ..start },
            Moment { caret: false, ..start },
            Moment { frame: 40.0, ..start },
            Moment { frame: 10.0, pointer: framed_card, ..start },
            Moment { frame: 300.0, ..start },
            Moment { page: 60.0, frame: 300.0, ..start },
            Moment { page: 20.0, frame: 300.0, pointer: card, ..start },
            Moment { page: 20.0, frame: 250.0, ..start },
            Moment { selection: Some((2, 12)), ..start },
            Moment { dark: true, ..start },
            start,
        ];
        let full = |moment| {
            let mut buf = vec![0; w * h];
            render(&mut Renderer::new(), &mut buf, moment, 0);
            buf
        };
        let expected: Vec<Vec<u32>> = moments.iter().map(|&m| full(m)).collect();

        // A window with one buffer, which always holds the last frame.
        let (mut renderer, mut buf) = (Renderer::new(), vec![0; w * h]);
        let mut partial = 0;
        for (i, &moment) in moments.iter().enumerate() {
            let damage = render(&mut renderer, &mut buf, moment, if i == 0 { 0 } else { 1 }).damage;
            partial += damage.is_some() as usize;
            assert!(buf == expected[i], "frame {i} with one buffer differs from painting it all");
        }
        // All but the first, the one that scrolls two things at once, and the two that change the background.
        assert_eq!(partial, moments.len() - 4, "the rest were painted in part");

        // A window with two buffers, which take turns.
        let (mut renderer, mut bufs) = (Renderer::new(), [vec![0; w * h], vec![0; w * h]]);
        for (i, &moment) in moments.iter().enumerate() {
            render(&mut renderer, &mut bufs[i % 2], moment, if i < 2 { 0 } else { 2 });
            assert!(bufs[i % 2] == expected[i], "frame {i} with two buffers differs from painting it all");
        }
    }

    #[test]
    fn a_hover_paints_only_the_card() {
        let src = r##"(page :padding 10 (p "above") (div :hover (style :background "#eee") :padding 8 "card") (p "below"))"##;
        let Ok(Value::Element(page)) = lisp::run(src, &crate::loader::test_context()) else { panic!("not a page") };
        let mut renderer = Renderer::new();
        let mut buf = vec![0; 200 * 200];
        let scroll = Scroll::default();
        let mut render = |pointer, age| {
            let view = View {
                scale: 1.0,
                scroll: &scroll,
                focus: None,
                key_focus: None,
                caret_visible: true,
                dark: false,
                search: None,
                selection: None,
                images: None,
                drawings: None,
                pointer,
                ctrl: false,
                status: None,
                magnify: Magnify::default(),
            };
            renderer.render(&page, &mut buf, 200, 200, &view, age)
        };
        let first = render(None, 0);
        assert_eq!(first.damage, None, "a new buffer is painted in full");
        let card = first.hover_areas[0];
        let hovered = render(Some((50.0, (card.y0 + card.y1) / 2.0)), 1);
        let grown = Clip { x0: card.x0 - 1.0, y0: card.y0 - 1.0, x1: card.x1 + 1.0, y1: card.y1 + 1.0 };
        assert_eq!(hovered.damage, Some(vec![grown.round_out()]), "just the card, and a pixel around it");
        assert_eq!(render(Some((50.0, (card.y0 + card.y1) / 2.0)), 1).damage, Some(vec![]), "nothing changed");
    }
}

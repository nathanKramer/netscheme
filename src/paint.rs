//! Rasterizes a display list into a 0RGB pixel buffer.

use std::{collections::HashMap, rc::Rc};

use fontdue::{Font, FontSettings, Metrics};

use crate::{
    canvas::Contour,
    dom::Element,
    image::Image,
    layout::{Clip, DisplayItem, Drawings, Focus, InputBox, KeyTarget, Layout, Link, Match, Scrollbar, Scroller, Search, TextLine},
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
    /// Every line of text, in window coordinates.
    pub lines: Vec<TextLine>,
    /// The visible parts of elements with hover styles, in window coordinates.
    pub hover_areas: Vec<Clip>,
}

pub struct Renderer {
    pub fonts: Fonts,
    glyphs: HashMap<(char, u32, Face), (Metrics, Vec<u8>)>,
    /// Images scaled to the sizes they're drawn at, by `Image::id`, width and height.
    scaled: HashMap<(u64, u32, u32), Vec<u8>>,
}

impl Renderer {
    pub fn new() -> Renderer {
        Renderer { fonts: Fonts::load(), glyphs: HashMap::new(), scaled: HashMap::new() }
    }

    /// Lays out `page` and paints it into `buf` (`w` × `h` physical pixels),
    /// scrolled by `view.scroll`. Offsets are first clamped so nothing
    /// scrolls past its end.
    pub fn render(&mut self, page: &Element, buf: &mut [u32], w: usize, h: usize, view: &View) -> Rendered {
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
        let content_height = layout.block(page, &root, 0.0, 0.0, w as f32);
        let dy = view.scroll.page.clamp(0.0, (content_height - h as f32).max(0.0)).round();
        let (items, mut links, mut scrollers, mut inputs, mut matches) =
            (layout.items, layout.links, layout.scrollers, layout.inputs, layout.matches);
        let mut key_targets = layout.key_targets;
        let (mut lines, mut hover_areas) = (layout.lines, layout.hover_areas);

        buf.fill(blend(palette.page.to_u32(), page_background, 255));
        let screen = Clip::rect(0.0, 0.0, w as f32, h as f32);
        let mut clip = screen;
        for item in &items {
            match item {
                DisplayItem::Rect { x, y, w: rw, h: rh, color, radius } => {
                    let area = Clip::rect(*x, *y - dy, *rw, *rh);
                    if *radius > 0.0 {
                        fill_rounded_rect(buf, w, clip, area, *radius, *color)
                    } else {
                        fill_rect(buf, w, area.intersect(clip), *color)
                    }
                }
                DisplayItem::Border { x, y, w: bw, h: bh, width, color, radius } => {
                    stroke_rounded_rect(buf, w, clip, Clip::rect(*x, *y - dy, *bw, *bh), *radius, *width, *color)
                }
                DisplayItem::Text { x, y, text, size, face, color } => {
                    let baseline = *y - dy;
                    if baseline + size >= clip.y0 && baseline - size < clip.y1 {
                        self.draw_text(buf, w, clip, *x, baseline, text, *size, *face, *color)
                    }
                }
                DisplayItem::Image { x, y, w: iw, h: ih, image, radius } => {
                    let area = Clip::rect(*x, *y - dy, *iw, *ih);
                    match image {
                        Some(image) => self.draw_image(buf, w, clip, area, *radius, image),
                        None => fill_rounded_rect(buf, w, clip, area, *radius, palette.button),
                    }
                }
                DisplayItem::Polygon { contours, color } => fill_polygon(buf, w, clip, contours, dy, *color),
                DisplayItem::Clip(c) => clip = Clip { y0: c.y0 - dy, y1: c.y1 - dy, ..*c }.intersect(screen),
            }
        }
        let bar = Scrollbar::new(screen, content_height, dy, view.scale);
        if let Some(bar) = bar {
            fill_rect(buf, w, bar.thumb, palette.scrollbar);
        }
        for link in &mut links {
            link.y -= dy;
        }
        let areas = scrollers.iter_mut().map(|s| &mut s.area).chain(inputs.iter_mut().map(|i| &mut i.area));
        let areas = areas.chain(matches.iter_mut().map(|m| &mut m.area)).chain(key_targets.iter_mut().map(|k| &mut k.area));
        let areas = areas.chain(hover_areas.iter_mut());
        for area in areas.chain(lines.iter_mut().flat_map(|l| [&mut l.area, &mut l.visible])) {
            area.y0 -= dy;
            area.y1 -= dy;
        }
        for scroller in &mut scrollers {
            scroller.bar = scroller.bar.map(|bar| bar.shifted(-dy));
        }
        let frames = scrollers.iter().map(|s| (s.id, s.scroll)).collect();
        Rendered { links, scrollers, inputs, key_targets, scroll: Scroll { page: dy, frames }, bar, matches, lines, hover_areas }
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
            },
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
            },
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
        };
        Renderer::new().render(&page, &mut buf, 200, 60, &view);
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
                },
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
}

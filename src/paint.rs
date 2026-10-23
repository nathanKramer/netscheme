//! Rasterizes a display list into a 0RGB pixel buffer.

use std::collections::HashMap;

use fontdue::{Font, FontSettings, Metrics};

use crate::{
    dom::Element,
    layout::{Clip, DisplayItem, Focus, InputBox, Layout, Link, Scroller},
    style::{Color, Style},
};

pub struct Fonts {
    regular: Font,
    bold: Font,
}

impl Fonts {
    pub fn load() -> Fonts {
        let load = |bytes: &[u8]| Font::from_bytes(bytes, FontSettings::default()).expect("bundled font");
        Fonts {
            regular: load(include_bytes!("../assets/fonts/LiberationSans-Regular.ttf")),
            bold: load(include_bytes!("../assets/fonts/LiberationSans-Bold.ttf")),
        }
    }

    pub fn get(&self, bold: bool) -> &Font {
        if bold { &self.bold } else { &self.regular }
    }

    pub fn measure(&self, text: &str, size: f32, bold: bool) -> f32 {
        let font = self.get(bold);
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
    /// Whether a blinking caret is in its visible phase.
    pub caret_visible: bool,
}

pub struct Rendered {
    /// Link hit boxes, in window coordinates.
    pub links: Vec<Link>,
    /// Scrollable frames, in window coordinates.
    pub scrollers: Vec<Scroller>,
    /// Text inputs, in window coordinates.
    pub inputs: Vec<InputBox>,
    /// The offsets actually used, after clamping.
    pub scroll: Scroll,
}

pub struct Renderer {
    pub fonts: Fonts,
    glyphs: HashMap<(char, u32, bool), (Metrics, Vec<u8>)>,
}

impl Renderer {
    pub fn new() -> Renderer {
        Renderer { fonts: Fonts::load(), glyphs: HashMap::new() }
    }

    /// Lays out `page` and paints it into `buf` (`w` × `h` physical pixels),
    /// scrolled by `view.scroll`. Offsets are first clamped so nothing
    /// scrolls past its end.
    pub fn render(&mut self, page: &Element, buf: &mut [u32], w: usize, h: usize, view: &View) -> Rendered {
        let root = Style::root();
        let page_background = Style::compute(page, &root).background.unwrap_or(Color::WHITE);
        let mut layout = Layout::new(&self.fonts, view.scale, h as f32, &view.scroll.frames, view.focus, view.caret_visible);
        let content_height = layout.block(page, &root, 0.0, 0.0, w as f32);
        let dy = view.scroll.page.clamp(0.0, (content_height - h as f32).max(0.0)).round();
        let (items, mut links, mut scrollers, mut inputs) = (layout.items, layout.links, layout.scrollers, layout.inputs);

        buf.fill(page_background.to_u32());
        let screen = Clip::rect(0.0, 0.0, w as f32, h as f32);
        let mut clip = screen;
        for item in &items {
            match item {
                DisplayItem::Rect { x, y, w: rw, h: rh, color } => {
                    fill_rect(buf, w, Clip::rect(*x, *y - dy, *rw, *rh).intersect(clip), *color)
                }
                DisplayItem::Text { x, y, text, size, bold, color } => {
                    let baseline = *y - dy;
                    if baseline + size >= clip.y0 && baseline - size < clip.y1 {
                        self.draw_text(buf, w, clip, *x, baseline, text, *size, *bold, *color)
                    }
                }
                DisplayItem::Clip(c) => clip = Clip { y0: c.y0 - dy, y1: c.y1 - dy, ..*c }.intersect(screen),
            }
        }
        for link in &mut links {
            link.y -= dy;
        }
        for area in scrollers.iter_mut().map(|s| &mut s.area).chain(inputs.iter_mut().map(|i| &mut i.area)) {
            area.y0 -= dy;
            area.y1 -= dy;
        }
        let frames = scrollers.iter().map(|s| (s.id, s.scroll)).collect();
        Rendered { links, scrollers, inputs, scroll: Scroll { page: dy, frames } }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text(&mut self, buf: &mut [u32], w: usize, clip: Clip, x: f32, baseline: f32, text: &str, size: f32, bold: bool, color: Color) {
        let mut pen = x;
        for c in text.chars() {
            let (m, bitmap) = self
                .glyphs
                .entry((c, size.to_bits(), bold))
                .or_insert_with(|| self.fonts.get(bold).rasterize(c, size));
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
        buf[row * w + x0..row * w + x1].fill(color.to_u32());
    }
}

fn blend(dst: u32, src: Color, alpha: u8) -> u32 {
    let a = alpha as u32;
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
        Renderer::new().render(&page, &mut buf, 200, h, &View { scale: 1.0, scroll: &scroll, focus: None, caret_visible: true })
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
            frames_created: Default::default(),
        });
        let src = r#"(page :padding 0 (p "toolbar") (frame :src "long.sx" :height "fill" :margin 0))"#;
        let Ok(Value::Element(page)) = lisp::run(src, &ctx) else { panic!("not a page") };
        let mut renderer = Renderer::new();
        let mut buf = vec![0; 200 * 300];
        let mut render = |frames: HashMap<usize, f32>| {
            let scroll = Scroll { page: 0.0, frames };
            renderer.render(&page, &mut buf, 200, 300, &View { scale: 1.0, scroll: &scroll, focus: None, caret_visible: true })
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


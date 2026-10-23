//! Rasterizes a display list into a 0RGB pixel buffer.

use std::collections::HashMap;

use fontdue::{Font, FontSettings, Metrics};

use crate::{
    dom::Element,
    layout::{DisplayItem, Layout, Link},
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

pub struct Rendered {
    /// Link hit boxes, in window coordinates.
    pub links: Vec<Link>,
    /// The scroll offset actually used, after clamping.
    pub scroll: f32,
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
    /// scrolled down by `scroll` pixels, which is first clamped so the page
    /// can't scroll past its end.
    pub fn render(&mut self, page: &Element, buf: &mut [u32], w: usize, h: usize, scale: f32, scroll: f32) -> Rendered {
        let root = Style::root();
        let page_background = Style::compute(page, &root).background.unwrap_or(Color::WHITE);
        let mut layout = Layout::new(&self.fonts, scale);
        let content_height = layout.block(page, &root, 0.0, 0.0, w as f32);
        let scroll = scroll.clamp(0.0, (content_height - h as f32).max(0.0)).round();
        let (items, mut links) = (layout.items, layout.links);

        buf.fill(page_background.to_u32());
        for item in &items {
            match item {
                DisplayItem::Rect { x, y, w: rw, h: rh, color } => fill_rect(buf, w, h, *x, *y - scroll, *rw, *rh, *color),
                DisplayItem::Text { x, y, text, size, bold, color } => {
                    let baseline = *y - scroll;
                    if baseline + size >= 0.0 && baseline - size < h as f32 {
                        self.draw_text(buf, w, h, *x, baseline, text, *size, *bold, *color)
                    }
                }
            }
        }
        for link in &mut links {
            link.y -= scroll;
        }
        Rendered { links, scroll }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text(&mut self, buf: &mut [u32], w: usize, h: usize, x: f32, baseline: f32, text: &str, size: f32, bold: bool, color: Color) {
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
                    if alpha > 0 && px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                        let dst = &mut buf[py as usize * w + px as usize];
                        *dst = blend(*dst, color, alpha);
                    }
                }
            }
            pen += m.advance_width;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_rect(buf: &mut [u32], w: usize, h: usize, x: f32, y: f32, rw: f32, rh: f32, color: Color) {
    let clamp = |v: f32, max: usize| (v.round().max(0.0) as usize).min(max);
    let (x0, x1) = (clamp(x, w), clamp(x + rw, w));
    let (y0, y1) = (clamp(y, h), clamp(y + rh, h));
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
        Renderer::new().render(&page, &mut buf, 200, h, 1.0, scroll)
    }

    const TALL: &str = r#"(page :padding 0 (div :padding 500 (a :href "x.sx" "link")))"#;

    #[test]
    fn scroll_is_clamped_to_the_content() {
        assert_eq!(render(TALL, 100, -50.0).scroll, 0.0);
        let bottom = render(TALL, 100, 1e9).scroll;
        assert!(bottom > 900.0 && bottom < 1100.0 - 100.0, "{bottom}");
        assert_eq!(render(r#"(page "short")"#, 100, 50.0).scroll, 0.0, "nothing to scroll");
    }

    #[test]
    fn links_move_with_the_page() {
        let (top, scrolled) = (render(TALL, 100, 0.0), render(TALL, 100, 300.0));
        assert_eq!(scrolled.links[0].y, top.links[0].y - 300.0);
    }
}

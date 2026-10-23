//! Computed styles: tag defaults, then inline attributes, with text
//! properties inherited from the parent.

use crate::{dom::Element, lisp::Value};

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Color(pub u8, pub u8, pub u8);

impl Color {
    pub const WHITE: Color = Color(255, 255, 255);
    pub const TEXT: Color = Color(0x1a, 0x1a, 0x1a);
    pub const LINK: Color = Color(0x1a, 0x5f, 0xb4);

    pub fn to_u32(self) -> u32 {
        (self.0 as u32) << 16 | (self.1 as u32) << 8 | self.2 as u32
    }

    /// Accepts `#rgb`, `#rrggbb`, or a handful of names.
    pub fn parse(s: &str) -> Option<Color> {
        let named = match s {
            "black" => Some("#000"),
            "white" => Some("#fff"),
            "red" => Some("#d00"),
            "green" => Some("#080"),
            "blue" => Some("#00d"),
            "gray" | "grey" => Some("#888"),
            _ => None,
        };
        let hex = named.unwrap_or(s).strip_prefix('#')?;
        let digit = |i: usize, len: usize| u8::from_str_radix(hex.get(i..i + len)?, 16).ok();
        match hex.len() {
            3 => Some(Color(digit(0, 1)? * 17, digit(1, 1)? * 17, digit(2, 1)? * 17)),
            6 => Some(Color(digit(0, 2)?, digit(2, 2)?, digit(4, 2)?)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Style {
    // Inherited
    pub font_size: f32,
    pub bold: bool,
    pub color: Color,
    pub underline: bool,
    // Not inherited
    pub background: Option<Color>,
    pub padding: f32,
    pub padding_left: f32,
    pub margin: f32,
}

impl Style {
    pub fn root() -> Style {
        Style { font_size: 16.0, bold: false, color: Color::TEXT, underline: false, background: None, padding: 0.0, padding_left: 0.0, margin: 0.0 }
    }

    pub fn compute(el: &Element, parent: &Style) -> Style {
        let mut s = Style { background: None, padding: 0.0, padding_left: 0.0, margin: 0.0, ..parent.clone() };
        let mut padding_left = None;

        // The "user-agent stylesheet".
        match &*el.tag {
            "page" => {
                s.padding = 24.0;
                s.background = Some(Color::WHITE);
            }
            "h1" => (s.font_size, s.bold, s.margin) = (32.0, true, 16.0),
            "h2" => (s.font_size, s.bold, s.margin) = (24.0, true, 12.0),
            "h3" => (s.font_size, s.bold, s.margin) = (19.0, true, 10.0),
            "p" => s.margin = 12.0,
            "b" => s.bold = true,
            "a" => (s.color, s.underline) = (Color::LINK, true),
            "frame" => s.margin = 12.0,
            _ => {}
        }

        for (name, value) in &el.attrs {
            match (&**name, value) {
                ("color", Value::Str(c)) => s.color = parse_color(c, s.color),
                ("background", Value::Str(c)) => s.background = Some(parse_color(c, Color::WHITE)),
                ("size", Value::Num(n)) => s.font_size = *n as f32,
                ("bold", v) => s.bold = v.truthy(),
                ("underline", v) => s.underline = v.truthy(),
                ("padding", Value::Num(n)) => s.padding = *n as f32,
                ("padding-left", Value::Num(n)) => padding_left = Some(*n as f32),
                ("margin", Value::Num(n)) => s.margin = *n as f32,
                _ => {}
            }
        }
        s.padding_left = padding_left.unwrap_or(s.padding);
        s
    }
}

fn parse_color(s: &str, fallback: Color) -> Color {
    Color::parse(s).unwrap_or_else(|| {
        eprintln!("warning: unknown color {s:?}");
        fallback
    })
}

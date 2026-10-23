//! Computed styles: tag defaults, then inline attributes, with text
//! properties inherited from the parent.

use crate::{dom::Element, lisp::Value};

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Color(pub u8, pub u8, pub u8);

impl Color {
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

/// The browser's own colours: for pages that don't choose their own, and
/// for parts of elements that pages can't style, like an input's border.
#[derive(Debug, PartialEq)]
pub struct Palette {
    pub page: Color,
    pub text: Color,
    pub link: Color,
    pub button: Color,
    pub input: Color,
    pub code: Color,
    pub border: Color,
    pub scrollbar: Color,
    pub focus: Color,
    pub placeholder: Color,
    pub selection: Color,
}

impl Palette {
    pub const LIGHT: Palette = Palette {
        page: Color(0xff, 0xff, 0xff),
        text: Color(0x1a, 0x1a, 0x1a),
        link: Color(0x1a, 0x5f, 0xb4),
        button: Color(0xe4, 0xdf, 0xeb),
        input: Color(0xff, 0xff, 0xff),
        code: Color(0xf4, 0xf1, 0xf7),
        border: Color(0xb9, 0xb2, 0xc3),
        scrollbar: Color(0xc4, 0xbd, 0xcd),
        focus: Color(0x5b, 0x3a, 0xb8),
        placeholder: Color(0x9a, 0x94, 0xa3),
        selection: Color(0xd8, 0xcc, 0xf2),
    };

    pub const DARK: Palette = Palette {
        page: Color(0x1d, 0x1a, 0x22),
        text: Color(0xe8, 0xe4, 0xee),
        link: Color(0x8a, 0xb4, 0xf8),
        button: Color(0x3a, 0x35, 0x44),
        input: Color(0x26, 0x22, 0x2d),
        code: Color(0x29, 0x25, 0x30),
        border: Color(0x5a, 0x53, 0x66),
        scrollbar: Color(0x55, 0x4e, 0x60),
        focus: Color(0xb8, 0x9c, 0xff),
        placeholder: Color(0x82, 0x7b, 0x8d),
        selection: Color(0x4e, 0x3d, 0x7a),
    };

    pub fn new(dark: bool) -> &'static Palette {
        if dark { &Palette::DARK } else { &Palette::LIGHT }
    }
}

/// Which of the bundled fonts text is drawn in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Face {
    pub bold: bool,
    pub mono: bool,
}

#[derive(Clone, Debug)]
pub struct Style {
    // Inherited
    pub palette: &'static Palette,
    pub font_size: f32,
    pub bold: bool,
    pub mono: bool,
    pub color: Color,
    pub underline: bool,
    // Not inherited
    pub background: Option<Color>,
    pub padding: f32,
    pub padding_left: f32,
    pub margin: f32,
}

impl Style {
    pub fn root(palette: &'static Palette) -> Style {
        Style {
            palette,
            font_size: 16.0,
            bold: false,
            mono: false,
            color: palette.text,
            underline: false,
            background: None,
            padding: 0.0,
            padding_left: 0.0,
            margin: 0.0,
        }
    }

    pub fn face(&self) -> Face {
        Face { bold: self.bold, mono: self.mono }
    }

    pub fn compute(el: &Element, parent: &Style) -> Style {
        let mut s = Style { background: None, padding: 0.0, padding_left: 0.0, margin: 0.0, ..parent.clone() };
        let mut padding_left = None;

        // The "user-agent stylesheet".
        let palette = s.palette;
        match &*el.tag {
            "page" => {
                s.padding = 24.0;
                s.background = Some(palette.page);
            }
            "h1" => (s.font_size, s.bold, s.margin) = (32.0, true, 16.0),
            "h2" => (s.font_size, s.bold, s.margin) = (24.0, true, 12.0),
            "h3" => (s.font_size, s.bold, s.margin) = (19.0, true, 10.0),
            "p" => s.margin = 12.0,
            "b" => s.bold = true,
            "code" => (s.mono, s.font_size, s.background, s.padding) = (true, s.font_size * 0.9, Some(palette.code), 2.0),
            "pre" => (s.mono, s.font_size, s.background, s.padding, s.margin) = (true, 14.0, Some(palette.code), 14.0, 12.0),
            "a" => (s.color, s.underline) = (palette.link, true),
            "frame" => s.margin = 12.0,
            "button" => (s.background, s.padding) = (Some(palette.button), 8.0),
            "input" => (s.background, s.padding, s.bold, s.underline) = (Some(palette.input), 5.0, false, false),
            _ => {}
        }

        for (name, value) in &el.attrs {
            match (&**name, value) {
                ("color", Value::Str(c)) => s.color = parse_color(c, s.color),
                ("background", Value::Str(c)) => s.background = Some(parse_color(c, palette.page)),
                ("size", Value::Num(n)) => s.font_size = *n as f32,
                ("bold", v) => s.bold = v.truthy(),
                ("mono", v) => s.mono = v.truthy(),
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

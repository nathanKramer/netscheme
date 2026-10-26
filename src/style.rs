//! Computed styles: tag defaults, then inline attributes, with text
//! properties inherited from the parent.

use crate::{dom::Element, lisp::Value};

/// Red, green, blue, and opacity: 255 is opaque.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Color(pub u8, pub u8, pub u8, pub u8);

impl Color {
    /// The colour as 0RGB, ignoring its opacity.
    pub fn to_u32(self) -> u32 {
        (self.0 as u32) << 16 | (self.1 as u32) << 8 | self.2 as u32
    }

    pub fn is_opaque(self) -> bool {
        self.3 == 255
    }

    /// Accepts `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, or a handful of names.
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
            3 => Some(Color(digit(0, 1)? * 17, digit(1, 1)? * 17, digit(2, 1)? * 17, 255)),
            4 => Some(Color(digit(0, 1)? * 17, digit(1, 1)? * 17, digit(2, 1)? * 17, digit(3, 1)? * 17)),
            6 => Some(Color(digit(0, 2)?, digit(2, 2)?, digit(4, 2)?, 255)),
            8 => Some(Color(digit(0, 2)?, digit(2, 2)?, digit(4, 2)?, digit(6, 2)?)),
            _ => None,
        }
    }
}

/// The bundled typefaces. Monospaced text is always Liberation Mono.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Family {
    /// Liberation Sans, the default, which stands in for `system-ui` too.
    #[default]
    Sans,
    OpenSans,
    Mono,
}

impl Family {
    /// The family `name` picks out, as `:font` names it.
    pub fn parse(name: &str) -> Option<Family> {
        match &*name.to_ascii_lowercase() {
            "sans" | "sans-serif" | "system-ui" | "liberation sans" => Some(Family::Sans),
            "open sans" => Some(Family::OpenSans),
            "mono" | "monospace" | "liberation mono" => Some(Family::Mono),
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
    /// Text that matches a search, and the current match.
    pub found: Color,
    pub current_match: Color,
    pub focus: Color,
    pub placeholder: Color,
    pub selection: Color,
}

impl Palette {
    pub const LIGHT: Palette = Palette {
        page: Color(0xff, 0xff, 0xff, 0xff),
        text: Color(0x1a, 0x1a, 0x1a, 0xff),
        link: Color(0x1a, 0x5f, 0xb4, 0xff),
        button: Color(0xe4, 0xdf, 0xeb, 0xff),
        input: Color(0xff, 0xff, 0xff, 0xff),
        code: Color(0xf4, 0xf1, 0xf7, 0xff),
        border: Color(0xb9, 0xb2, 0xc3, 0xff),
        scrollbar: Color(0xc4, 0xbd, 0xcd, 0xff),
        found: Color(0xff, 0xe8, 0x8a, 0xff),
        current_match: Color(0xff, 0xb3, 0x47, 0xff),
        focus: Color(0x5b, 0x3a, 0xb8, 0xff),
        placeholder: Color(0x9a, 0x94, 0xa3, 0xff),
        selection: Color(0xd8, 0xcc, 0xf2, 0xff),
    };

    pub const DARK: Palette = Palette {
        page: Color(0x1d, 0x1a, 0x22, 0xff),
        text: Color(0xe8, 0xe4, 0xee, 0xff),
        link: Color(0x8a, 0xb4, 0xf8, 0xff),
        button: Color(0x3a, 0x35, 0x44, 0xff),
        input: Color(0x26, 0x22, 0x2d, 0xff),
        code: Color(0x29, 0x25, 0x30, 0xff),
        border: Color(0x5a, 0x53, 0x66, 0xff),
        scrollbar: Color(0x55, 0x4e, 0x60, 0xff),
        found: Color(0x6b, 0x5a, 0x1f, 0xff),
        current_match: Color(0xb3, 0x6b, 0x12, 0xff),
        focus: Color(0xb8, 0x9c, 0xff, 0xff),
        placeholder: Color(0x82, 0x7b, 0x8d, 0xff),
        selection: Color(0x4e, 0x3d, 0x7a, 0xff),
    };

    pub fn new(dark: bool) -> &'static Palette {
        if dark { &Palette::DARK } else { &Palette::LIGHT }
    }
}

/// Which of the bundled fonts text is drawn in: a family, and a weight
/// from 100 to 900, where 400 is regular and 700 bold.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Face {
    pub family: Family,
    pub weight: u16,
}

impl Default for Face {
    fn default() -> Face {
        Face { family: Family::Sans, weight: 400 }
    }
}

#[derive(Clone, Debug)]
pub struct Style {
    // Inherited
    pub palette: &'static Palette,
    pub font_size: f32,
    pub font: Family,
    pub weight: u16,
    pub mono: bool,
    /// The height of a line of text, as a multiple of the font size.
    pub line_height: f32,
    pub color: Color,
    pub underline: bool,
    // Not inherited
    pub background: Option<Color>,
    pub padding_top: f32,
    pub padding_right: f32,
    pub padding_bottom: f32,
    pub padding_left: f32,
    /// Space above and below a block.
    pub margin_top: f32,
    pub margin: f32,
    /// How round the corners of its box are.
    pub radius: f32,
    /// A line drawn just inside the edge of the box, `border_width` thick.
    pub border: Option<Color>,
    pub border_width: f32,
}

impl Style {
    pub fn root(palette: &'static Palette) -> Style {
        Style {
            palette,
            font_size: 16.0,
            font: Family::Sans,
            weight: 400,
            mono: false,
            line_height: 1.3,
            color: palette.text,
            underline: false,
            background: None,
            padding_top: 0.0,
            padding_right: 0.0,
            padding_bottom: 0.0,
            padding_left: 0.0,
            margin_top: 0.0,
            margin: 0.0,
            radius: 0.0,
            border: None,
            border_width: 0.0,
        }
    }

    pub fn face(&self) -> Face {
        Face { family: if self.mono { Family::Mono } else { self.font }, weight: self.weight }
    }

    fn set_padding(&mut self, padding: f32) {
        (self.padding_top, self.padding_right, self.padding_bottom, self.padding_left) = (padding, padding, padding, padding);
    }

    pub fn compute(el: &Element, parent: &Style) -> Style {
        let mut s = Style {
            background: None,
            padding_top: 0.0,
            padding_right: 0.0,
            padding_bottom: 0.0,
            padding_left: 0.0,
            margin_top: 0.0,
            margin: 0.0,
            radius: 0.0,
            border: None,
            border_width: 0.0,
            ..parent.clone()
        };

        // The "user-agent stylesheet".
        let palette = s.palette;
        let bolder = s.weight.max(700);
        match &*el.tag {
            "page" => {
                s.set_padding(24.0);
                s.background = Some(palette.page);
            }
            "h1" => (s.font_size, s.weight, s.margin) = (32.0, bolder, 16.0),
            "h2" => (s.font_size, s.weight, s.margin) = (24.0, bolder, 12.0),
            "h3" => (s.font_size, s.weight, s.margin) = (19.0, bolder, 10.0),
            "p" => s.margin = 12.0,
            "b" => s.weight = bolder,
            "code" => {
                (s.mono, s.font_size, s.background) = (true, s.font_size * 0.9, Some(palette.code));
                s.set_padding(2.0);
            }
            "pre" => {
                (s.mono, s.font_size, s.background, s.margin) = (true, 14.0, Some(palette.code), 12.0);
                s.set_padding(14.0);
            }
            "a" => (s.color, s.underline) = (palette.link, true),
            "frame" => s.margin = 12.0,
            "button" => {
                s.background = Some(palette.button);
                s.set_padding(8.0);
            }
            "input" => {
                (s.background, s.weight, s.underline) = (Some(palette.input), 400, false);
                s.set_padding(5.0);
            }
            _ => {}
        }
        s.apply(el.attrs.iter().map(|(name, value)| (&**name, value)));
        s
    }

    /// Sets the style attributes among `attrs`, in order, ignoring the rest.
    /// `:padding` sets every side, and a side given after it wins.
    pub fn apply<'v>(&mut self, attrs: impl IntoIterator<Item = (&'v str, &'v Value)>) {
        let palette = self.palette;
        for (name, value) in attrs {
            match (name, value) {
                ("color", Value::Str(c)) => self.color = parse_color(c, self.color),
                ("background", Value::Str(c)) => self.background = Some(parse_color(c, palette.page)),
                ("size", Value::Num(n)) => self.font_size = *n as f32,
                ("font", Value::Str(name)) => match Family::parse(name) {
                    Some(Family::Mono) => self.mono = true,
                    Some(family) => (self.font, self.mono) = (family, false),
                    None => eprintln!("warning: unknown font {name:?}"),
                },
                ("weight", Value::Num(n)) => self.weight = (*n as f32).clamp(100.0, 900.0) as u16,
                ("bold", v) => self.weight = if v.truthy() { 700 } else { 400 },
                ("mono", v) => self.mono = v.truthy(),
                ("line-height", Value::Num(n)) => self.line_height = (*n as f32).max(0.0),
                ("underline", v) => self.underline = v.truthy(),
                ("padding", Value::Num(n)) => self.set_padding(*n as f32),
                ("padding-top", Value::Num(n)) => self.padding_top = *n as f32,
                ("padding-right", Value::Num(n)) => self.padding_right = *n as f32,
                ("padding-bottom", Value::Num(n)) => self.padding_bottom = *n as f32,
                ("padding-left", Value::Num(n)) => self.padding_left = *n as f32,
                ("margin" | "margin-bottom", Value::Num(n)) => self.margin = *n as f32,
                ("margin-top", Value::Num(n)) => self.margin_top = *n as f32,
                ("radius", Value::Num(n)) => self.radius = (*n as f32).max(0.0),
                ("border", Value::Str(c)) => {
                    self.border = Some(parse_color(c, self.color));
                    if self.border_width == 0.0 {
                        self.border_width = 1.0;
                    }
                }
                ("border", v) if !v.truthy() => self.border = None,
                ("border-width", Value::Num(n)) => self.border_width = (*n as f32).max(0.0),
                _ => {}
            }
        }
    }

    /// The space between the edge of the box and its content, on each side:
    /// padding plus any border.
    pub fn insets(&self) -> (f32, f32, f32, f32) {
        let border = if self.border.is_some() { self.border_width } else { 0.0 };
        (self.padding_top + border, self.padding_right + border, self.padding_bottom + border, self.padding_left + border)
    }
}

pub fn parse_color(s: &str, fallback: Color) -> Color {
    Color::parse(s).unwrap_or_else(|| {
        eprintln!("warning: unknown color {s:?}");
        fallback
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lisp::{self, Value};

    #[test]
    fn radius_is_read_but_not_inherited() {
        let Ok(Value::Element(page)) = lisp::run(r#"(div :radius 8 (span "x"))"#, &crate::loader::test_context()) else {
            panic!("not an element")
        };
        let style = Style::compute(&page, &Style::root(&Palette::LIGHT));
        assert_eq!(style.radius, 8.0);
        let crate::dom::Node::Element(span) = &page.children[0] else { panic!("no span") };
        assert_eq!(Style::compute(span, &style).radius, 0.0);
    }

    #[test]
    fn colours_can_be_translucent() {
        assert_eq!(Color::parse("#ffffff05"), Some(Color(255, 255, 255, 5)));
        assert_eq!(Color::parse("#f008"), Some(Color(255, 0, 0, 0x88)));
        assert_eq!(Color::parse("#123"), Some(Color(0x11, 0x22, 0x33, 255)));
        assert_eq!(Color::parse("#12345"), None);
    }

    #[test]
    fn padding_sides_win_when_given_after_padding() {
        let Ok(Value::Element(el)) =
            lisp::run(r#"(div :padding 4 :padding-top 9 :font "Open Sans" :bold true)"#, &crate::loader::test_context())
        else {
            panic!("not an element")
        };
        let style = Style::compute(&el, &Style::root(&Palette::LIGHT));
        assert_eq!((style.padding_top, style.padding_right, style.padding_bottom, style.padding_left), (9.0, 4.0, 4.0, 4.0));
        assert_eq!(style.face(), Face { family: Family::OpenSans, weight: 700 });
    }
}

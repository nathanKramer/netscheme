//! Reads CSS, so that HTML pages translated into Netscheme can keep
//! roughly the look their stylesheets give them. This covers the parts of
//! CSS that the translation can use: rules, selectors and their
//! specificity, `@media` queries, `@import`, custom properties and the
//! values of the properties Netscheme has a counterpart for. Everything
//! else is skipped over.

use std::collections::HashMap;

/// The width and height of the window that media queries and viewport
/// units are measured against, as a new window opens at.
pub const VIEWPORT: (f32, f32) = (900.0, 700.0);
/// Stylesheets imported more deeply than this are left out.
const MAX_IMPORT_DEPTH: u32 = 4;

/// A property and its value, as written.
#[derive(Clone, Debug, PartialEq)]
pub struct Declaration {
    pub name: String,
    pub value: String,
    pub important: bool,
}

/// A rule's selector, the declarations it applies, and its place among
/// the rules, which breaks ties of specificity.
#[derive(Debug)]
pub struct Rule {
    pub selector: Complex,
    pub specificity: (u32, u32, u32),
    pub order: usize,
    pub declarations: std::rc::Rc<Vec<Declaration>>,
}

/// The rules of a page's stylesheets, in the order they apply, filed by
/// what their selectors most narrowly need so that an element is only
/// checked against rules that could match it.
#[derive(Default, Debug)]
pub struct Rules {
    by_id: HashMap<String, Vec<Rule>>,
    by_class: HashMap<String, Vec<Rule>>,
    by_tag: HashMap<String, Vec<Rule>>,
    other: Vec<Rule>,
    count: usize,
}

impl Rules {
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Adds the stylesheet `text`. `import` reads the stylesheet an
    /// `@import` names, relative to this one.
    pub fn add(&mut self, text: &str, import: &mut dyn FnMut(&str) -> Option<String>) {
        self.add_nested(&strip_comments(text), import, 0);
    }

    fn add_nested(&mut self, text: &str, import: &mut dyn FnMut(&str) -> Option<String>, depth: u32) {
        let mut rest = text;
        loop {
            rest = rest.trim_start();
            if rest.is_empty() {
                return;
            }
            if let Some(at) = rest.strip_prefix('@') {
                let name_end = at.find(|c: char| !(c.is_alphanumeric() || c == '-')).unwrap_or(at.len());
                let name = at[..name_end].to_ascii_lowercase();
                let after = &at[name_end..];
                let Some(end) = find_unnested(after, &['{', ';']) else { return };
                let prelude = after[..end].trim();
                if after[end..].starts_with(';') {
                    if name == "import" && depth < MAX_IMPORT_DEPTH {
                        self.import(prelude, import, depth);
                    }
                    rest = &after[end + 1..];
                    continue;
                }
                let block_end = matching_brace(after, end).unwrap_or(after.len());
                let block = &after[(end + 1).min(block_end)..block_end];
                match &*name {
                    "media" if media_matches(prelude) => self.add_nested(block, import, depth),
                    "supports" | "layer" | "container" | "document" | "scope" | "starting-style" => self.add_nested(block, import, depth),
                    _ => {}
                }
                rest = after.get(block_end + 1..).unwrap_or("");
                continue;
            }
            let Some(open) = find_unnested(rest, &['{']) else { return };
            let block_end = matching_brace(rest, open).unwrap_or(rest.len());
            self.add_rule(&rest[..open], &rest[open + 1..block_end]);
            rest = rest.get(block_end + 1..).unwrap_or("");
        }
    }

    fn import(&mut self, prelude: &str, import: &mut dyn FnMut(&str) -> Option<String>, depth: u32) {
        let (href, media) = match prelude.strip_prefix("url(") {
            Some(url) => match url.split_once(')') {
                Some((href, media)) => (href, media),
                None => return,
            },
            None => match prelude.chars().next() {
                Some(quote @ ('"' | '\'')) => match prelude[1..].split_once(quote) {
                    Some((href, media)) => (href, media),
                    None => return,
                },
                _ => return,
            },
        };
        let href = href.trim().trim_matches(['"', '\'']);
        let media = media.trim();
        if !media.is_empty() && !media.starts_with("layer") && !media.starts_with("supports") && !media_matches(media) {
            return;
        }
        if let Some(text) = import(href) {
            self.add_nested(&strip_comments(&text), import, depth + 1);
        }
    }

    fn add_rule(&mut self, selectors: &str, block: &str) {
        // Nested rules, as in CSS nesting, are left out of the declarations.
        let declarations = std::rc::Rc::new(parse_declarations(block));
        if declarations.is_empty() {
            return;
        }
        for selector in split_unnested(selectors, ',') {
            let Some(selector) = parse_complex(selector.trim()) else { continue };
            let specificity = selector.specificity();
            let rule = Rule { specificity, order: self.count, declarations: declarations.clone(), selector };
            self.count += 1;
            let subject = &rule.selector.subject;
            let bucket = if let Some(id) = &subject.id {
                self.by_id.entry(id.clone()).or_default()
            } else if let Some(class) = subject.classes.first() {
                self.by_class.entry(class.clone()).or_default()
            } else if let Some(tag) = &subject.tag {
                self.by_tag.entry(tag.clone()).or_default()
            } else {
                &mut self.other
            };
            bucket.push(rule);
        }
    }

    /// The rules whose selectors match the last element of `path`.
    pub fn matching<'r, E: Element>(&'r self, path: &[Place<E>]) -> Vec<&'r Rule> {
        let Some(place) = path.last() else { return Vec::new() };
        let el = place.element();
        let mut candidates: Vec<&Rule> = Vec::new();
        if let Some(id) = el.attr("id") {
            candidates.extend(self.by_id.get(id.trim()).into_iter().flatten());
        }
        for class in el.attr("class").unwrap_or("").split_whitespace() {
            candidates.extend(self.by_class.get(class).into_iter().flatten());
        }
        candidates.extend(self.by_tag.get(el.name()).into_iter().flatten());
        candidates.extend(&self.other);
        candidates.retain(|rule| rule.selector.matches(path));
        candidates.sort_by_key(|rule| rule.order);
        candidates.dedup_by_key(|rule| rule.order);
        candidates
    }
}

/// An element that selectors can match.
pub trait Element {
    fn name(&self) -> &str;
    fn attr(&self, name: &str) -> Option<&str>;
}

/// An element in the document, as one of its parent's element children.
pub struct Place<'a, E> {
    pub siblings: std::rc::Rc<Vec<&'a E>>,
    pub index: usize,
}

impl<'a, E> Place<'a, E> {
    pub fn element(&self) -> &'a E {
        self.siblings[self.index]
    }
}

/// The declarations in a block, or a `style` attribute, in order.
pub fn parse_declarations(block: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    for part in split_unnested(&strip_comments(block), ';') {
        // A nested rule, which the next declaration runs into.
        if part.contains('{') || part.contains('}') {
            continue;
        }
        let Some((name, value)) = part.split_once(':') else { continue };
        let name = name.trim();
        let name = if name.starts_with("--") { name.to_string() } else { name.to_ascii_lowercase() };
        let mut value = value.trim();
        let mut important = false;
        if let Some(i) = value.to_ascii_lowercase().rfind("!important") {
            important = true;
            value = value[..i].trim_end();
        }
        if name.is_empty() || value.is_empty() && !name.starts_with("--") {
            continue;
        }
        for (name, value) in expand(&name, value) {
            out.push(Declaration { name, value, important });
        }
    }
    out
}

/// A shorthand property as the longhands it sets; other properties as they are.
fn expand(name: &str, value: &str) -> Vec<(String, String)> {
    let one = |name: &str| vec![(name.to_string(), value.to_string())];
    // A shorthand given a variable can't be split until the variable's known.
    if value.contains("var(") {
        return match name {
            "margin" | "padding" => sides(name, "", &[value]),
            "background" => one("background-color"),
            _ => one(name),
        };
    }
    let words = split_words(value);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    match name {
        "margin" | "padding" => sides(name, "", &words),
        "border-width" | "border-color" | "border-style" => {
            let (_, what) = name.split_once('-').unwrap();
            sides("border", what, &words)
        }
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" => {
            let mut width = "medium".to_string();
            let mut style = "none".to_string();
            let mut color = "currentcolor".to_string();
            for word in &words {
                let lower = word.to_ascii_lowercase();
                if is_border_style(&lower) {
                    style = lower;
                } else if matches!(&*lower, "thin" | "medium" | "thick") || length(word, 16.0).is_some() {
                    width = word.to_string();
                } else {
                    color = word.to_string();
                }
            }
            let names: Vec<&str> = match name.strip_prefix("border-") {
                Some(side) => vec![side],
                None => vec!["top", "right", "bottom", "left"],
            };
            names
                .into_iter()
                .flat_map(|side| {
                    [
                        (format!("border-{side}-width"), width.clone()),
                        (format!("border-{side}-style"), style.clone()),
                        (format!("border-{side}-color"), color.clone()),
                    ]
                })
                .collect()
        }
        "background" => {
            // The colour is whatever word is one, except inside an image.
            let color = words.iter().rev().find(|word| !word.contains('(') || is_color_function(word)).filter(|word| color(word).is_some());
            vec![("background-color".into(), color.map_or("transparent".into(), |c| c.to_string()))]
        }
        "font" => font(&words),
        "text-decoration" => one("text-decoration-line"),
        "list-style" => {
            let none = words.iter().any(|word| word.eq_ignore_ascii_case("none"));
            vec![("list-style-type".into(), if none { "none".into() } else { "disc".into() })]
        }
        "flex" => {
            let grow = match words.as_slice() {
                [word] if word.eq_ignore_ascii_case("none") || word.eq_ignore_ascii_case("initial") => "0",
                [word] if word.eq_ignore_ascii_case("auto") => "1",
                [first, ..] if first.parse::<f32>().is_ok() => first,
                _ => "0",
            };
            vec![("flex-grow".into(), grow.into())]
        }
        "gap" | "grid-gap" => {
            let row = words.first().copied().unwrap_or("0");
            let column = words.get(1).copied().unwrap_or(row);
            vec![("row-gap".into(), row.into()), ("column-gap".into(), column.into())]
        }
        "inset" | "place-items" => Vec::new(),
        _ => one(name),
    }
}

/// The four longhands of a property like `margin` given one to four values.
fn sides(name: &str, what: &str, words: &[&str]) -> Vec<(String, String)> {
    let [top, right, bottom, left] = match *words {
        [all] => [all, all, all, all],
        [vertical, horizontal] => [vertical, horizontal, vertical, horizontal],
        [top, horizontal, bottom] => [top, horizontal, bottom, horizontal],
        [top, right, bottom, left, ..] => [top, right, bottom, left],
        [] => return Vec::new(),
    };
    let suffix = if what.is_empty() { String::new() } else { format!("-{what}") };
    [("top", top), ("right", right), ("bottom", bottom), ("left", left)]
        .into_iter()
        .map(|(side, value)| (format!("{name}-{side}{suffix}"), value.to_string()))
        .collect()
}

fn is_border_style(word: &str) -> bool {
    matches!(word, "none" | "hidden" | "dotted" | "dashed" | "solid" | "double" | "groove" | "ridge" | "inset" | "outset")
}

/// The longhands of the `font` shorthand: `[style] [weight] size[/line-height] family`.
fn font(words: &[&str]) -> Vec<(String, String)> {
    let Some(size_at) = words.iter().position(|word| {
        let size = word.split('/').next().unwrap_or(word);
        size.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.') || font_size_keyword(size).is_some()
    }) else {
        return Vec::new();
    };
    let mut out = vec![("font-weight".to_string(), "normal".to_string()), ("line-height".to_string(), "normal".to_string())];
    for word in &words[..size_at] {
        if word.eq_ignore_ascii_case("bold") || word.parse::<f32>().is_ok() || word.eq_ignore_ascii_case("bolder") {
            out[0].1 = word.to_string();
        }
    }
    let (size, line_height) = words[size_at].split_once('/').unwrap_or((words[size_at], ""));
    out.push(("font-size".into(), size.into()));
    if !line_height.is_empty() {
        out[1].1 = line_height.into();
    }
    out.push(("font-family".into(), words[size_at + 1..].join(" ")));
    out
}

/// `value` split at spaces outside brackets and quotes.
pub fn split_words(value: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut depth = 0;
    let mut quote = None;
    for c in value.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, c) if c.is_whitespace() && depth == 0 => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                continue;
            }
            _ => {}
        }
        word.push(c);
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        rest = rest[start + 2..].find("*/").map_or("", |end| &rest[start + 2 + end + 2..]);
    }
    out.push_str(rest);
    out
}

/// Where the first of `chars` is in `text`, outside brackets and quotes.
fn find_unnested(text: &str, chars: &[char]) -> Option<usize> {
    let mut depth = 0i32;
    let mut quote = None;
    for (i, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, c) if depth == 0 && chars.contains(&c) => return Some(i),
            (None, '(' | '[') => depth += 1,
            (None, ')' | ']') => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Where the `}` is that closes the `{` at `open`.
fn matching_brace(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut quote = None;
    for (i, c) in text[open..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '{') => depth += 1,
            (None, '}') => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// `text` split at `separator`, outside brackets and quotes.
fn split_unnested(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(i) = find_unnested(rest, &[separator]) {
        parts.push(&rest[..i]);
        rest = &rest[i + separator.len_utf8()..];
    }
    parts.push(rest);
    parts
}

// Media queries

/// Whether the media query list `query` matches a screen the size of
/// `VIEWPORT`, in light mode, with a mouse.
pub fn media_matches(query: &str) -> bool {
    let query = query.trim();
    query.is_empty() || split_unnested(query, ',').into_iter().any(media_query_matches)
}

fn media_query_matches(query: &str) -> bool {
    let query = query.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase();
    let (negated, query) = match query.strip_prefix("not ") {
        Some(rest) => (true, rest.trim()),
        None => (false, query.strip_prefix("only ").unwrap_or(&query).trim()),
    };
    let matched = query.split(" and ").all(|part| {
        let part = part.trim();
        match part.strip_prefix('(').and_then(|p| p.strip_suffix(')')) {
            Some(feature) => media_feature(feature.trim()),
            None => matches!(part, "screen" | "all"),
        }
    });
    matched != negated
}

fn media_feature(feature: &str) -> bool {
    let (width, height) = VIEWPORT;
    if let Some((name, value)) = feature.split_once(':') {
        let (name, value) = (name.trim(), value.trim());
        let size = || length(value, 16.0);
        return match name {
            "min-width" => size().is_some_and(|v| width >= v),
            "max-width" => size().is_some_and(|v| width <= v),
            "min-height" => size().is_some_and(|v| height >= v),
            "max-height" => size().is_some_and(|v| height <= v),
            "prefers-color-scheme" => value == "light",
            "prefers-reduced-motion" | "prefers-reduced-transparency" => value == "no-preference",
            "prefers-contrast" => value == "no-preference",
            "hover" | "any-hover" => value == "hover",
            "pointer" | "any-pointer" => value == "fine",
            "orientation" => value == "landscape",
            "min-resolution" => value.trim_end_matches("dppx").trim_end_matches('x').parse::<f32>().is_ok_and(|v| v <= 1.0),
            "max-resolution" => true,
            _ => false,
        };
    }
    // Ranges, like `width >= 600px` or `400px < width <= 800px`.
    let tokens: Vec<&str> = feature.split_whitespace().collect();
    let value_of = |token: &str| match token {
        "width" => Some(width),
        "height" => Some(height),
        _ => length(token, 16.0),
    };
    if tokens.len().is_multiple_of(2) || tokens.len() < 3 || !tokens.iter().any(|t| matches!(*t, "width" | "height")) {
        return matches!(feature, "color" | "hover" | "pointer");
    }
    tokens.windows(3).step_by(2).all(|window| {
        let (Some(a), Some(b)) = (value_of(window[0]), value_of(window[2])) else { return false };
        match window[1] {
            "<" => a < b,
            "<=" => a <= b,
            ">" => a > b,
            ">=" => a >= b,
            "=" => a == b,
            _ => false,
        }
    })
}

// Selectors

/// A selector like `nav > ul li.active`: its last compound, and the
/// compounds before it with how each relates to the one after.
#[derive(Debug, Clone)]
pub struct Complex {
    pub subject: Compound,
    ancestors: Vec<(Compound, Combinator)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Combinator {
    Descendant,
    Child,
    Next,
    Later,
}

/// A selector without combinators, like `a.button[href]:first-child`.
#[derive(Debug, Clone, Default)]
pub struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<AttrTest>,
    pseudo: Vec<Pseudo>,
}

#[derive(Debug, Clone)]
struct AttrTest {
    name: String,
    test: Option<(char, String)>,
    ignore_case: bool,
}

#[derive(Debug, Clone)]
enum Pseudo {
    /// Like `:hover`, which a translated page is never in.
    Never,
    Root,
    Link,
    Empty,
    /// `:nth-child(an+b)`, counting from the end if `last`, and only
    /// elements of the same tag if `of_type`.
    Nth {
        a: i32,
        b: i32,
        last: bool,
        of_type: bool,
    },
    Not(Vec<Complex>),
    Is(Vec<Complex>),
    /// `:where`, which matches like `:is` but adds no specificity.
    Where(Vec<Complex>),
    Has,
}

fn parse_complex(text: &str) -> Option<Complex> {
    let mut compounds: Vec<(Compound, Combinator)> = Vec::new();
    let mut combinator = Combinator::Descendant;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut first = true;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '>' => combinator = Combinator::Child,
            '+' => combinator = Combinator::Next,
            '~' => combinator = Combinator::Later,
            _ => {
                let (compound, end) = parse_compound(&chars, i)?;
                if !first {
                    compounds.last_mut()?.1 = combinator;
                }
                compounds.push((compound, Combinator::Descendant));
                combinator = Combinator::Descendant;
                first = false;
                i = end;
                continue;
            }
        }
        if first {
            return None;
        }
        i += 1;
    }
    let (subject, _) = compounds.pop()?;
    Some(Complex { subject, ancestors: compounds })
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_' || c == '\\' || !c.is_ascii()
}

fn read_ident(chars: &[char], mut i: usize) -> (String, usize) {
    let mut name = String::new();
    while let Some(&c) = chars.get(i) {
        if c == '\\'
            && let Some(&next) = chars.get(i + 1)
        {
            name.push(next);
            i += 2;
            continue;
        }
        if !is_name_char(c) {
            break;
        }
        name.push(c);
        i += 1;
    }
    (name, i)
}

/// The contents of the brackets opening at `i`, and where they end.
fn read_bracketed(chars: &[char], i: usize, open: char, close: char) -> Option<(String, usize)> {
    let mut depth = 0;
    for (j, &c) in chars.iter().enumerate().skip(i) {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some((chars[i + 1..j].iter().collect(), j + 1));
            }
        }
    }
    None
}

fn parse_compound(chars: &[char], mut i: usize) -> Option<(Compound, usize)> {
    let mut compound = Compound::default();
    let start = i;
    while let Some(&c) = chars.get(i) {
        match c {
            '*' => i += 1,
            '#' => {
                let (id, end) = read_ident(chars, i + 1);
                compound.id = Some(id);
                i = end;
            }
            '.' => {
                let (class, end) = read_ident(chars, i + 1);
                compound.classes.push(class);
                i = end;
            }
            '[' => {
                let (inside, end) = read_bracketed(chars, i, '[', ']')?;
                compound.attrs.push(parse_attr_test(&inside)?);
                i = end;
            }
            ':' => {
                let element = chars.get(i + 1) == Some(&':');
                let (name, end) = read_ident(chars, if element { i + 2 } else { i + 1 });
                let name = name.to_ascii_lowercase();
                let (argument, end) = match chars.get(end) {
                    Some('(') => {
                        let (inside, end) = read_bracketed(chars, end, '(', ')')?;
                        (Some(inside), end)
                    }
                    _ => (None, end),
                };
                // Pseudo-elements, like ::before, are content the translation doesn't have.
                if element || matches!(&*name, "before" | "after" | "first-line" | "first-letter" | "selection" | "placeholder" | "marker")
                {
                    return None;
                }
                compound.pseudo.push(parse_pseudo(&name, argument.as_deref())?);
                i = end;
            }
            c if i == start && is_name_char(c) => {
                let (tag, end) = read_ident(chars, i);
                compound.tag = Some(tag.to_ascii_lowercase());
                i = end;
            }
            _ => break,
        }
    }
    (i > start).then_some((compound, i))
}

fn parse_attr_test(inside: &str) -> Option<AttrTest> {
    let inside = inside.trim();
    let operator = inside.find(['=', '~', '|', '^', '$', '*']);
    let Some(at) = operator else {
        return Some(AttrTest { name: inside.to_ascii_lowercase(), test: None, ignore_case: false });
    };
    let name = inside[..at].trim().to_ascii_lowercase();
    let (op, rest) = match inside[at..].strip_prefix('=') {
        Some(rest) => ('=', rest),
        None => (inside[at..].chars().next()?, inside[at + 1..].strip_prefix('=')?),
    };
    let rest = rest.trim();
    let (value, flags) = match rest.chars().next() {
        Some(quote @ ('"' | '\'')) => rest[1..].split_once(quote)?,
        _ => rest.split_once(' ').unwrap_or((rest, "")),
    };
    let ignore_case = flags.trim().eq_ignore_ascii_case("i");
    Some(AttrTest { name, test: Some((op, value.to_string())), ignore_case })
}

fn parse_pseudo(name: &str, argument: Option<&str>) -> Option<Pseudo> {
    let list = |argument: Option<&str>| -> Option<Vec<Complex>> {
        // A selector in the list that can't match, like one with ::before, is left out.
        Some(split_unnested(argument?, ',').into_iter().filter_map(|s| parse_complex(s.trim())).collect())
    };
    Some(match (name, argument) {
        ("root", _) => Pseudo::Root,
        ("link" | "any-link", _) => Pseudo::Link,
        ("empty", _) => Pseudo::Empty,
        ("first-child", _) => Pseudo::Nth { a: 0, b: 1, last: false, of_type: false },
        ("last-child", _) => Pseudo::Nth { a: 0, b: 1, last: true, of_type: false },
        ("first-of-type", _) => Pseudo::Nth { a: 0, b: 1, last: false, of_type: true },
        ("last-of-type", _) => Pseudo::Nth { a: 0, b: 1, last: true, of_type: true },
        ("only-child", _) => Pseudo::Is(vec![parse_complex(":first-child:last-child")?]),
        ("only-of-type", _) => Pseudo::Is(vec![parse_complex(":first-of-type:last-of-type")?]),
        ("nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type", Some(argument)) => {
            let (a, b) = parse_nth(argument)?;
            Pseudo::Nth { a, b, last: name.contains("last"), of_type: name.ends_with("of-type") }
        }
        ("not", argument) => Pseudo::Not(list(argument)?),
        ("is" | "matches" | "-webkit-any" | "-moz-any", argument) => Pseudo::Is(list(argument)?),
        ("where", argument) => Pseudo::Where(list(argument)?),
        ("has", _) => Pseudo::Has,
        _ => Pseudo::Never,
    })
}

/// The `a` and `b` of `an+b`, `odd` or `even`.
fn parse_nth(text: &str) -> Option<(i32, i32)> {
    // Any `of S` is ignored.
    let text: String = text.split(" of ").next()?.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    match &*text {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    match text.split_once('n') {
        Some((a, b)) => {
            let a = match a {
                "" | "+" => 1,
                "-" => -1,
                a => a.parse().ok()?,
            };
            let b = if b.is_empty() { 0 } else { b.trim_start_matches('+').parse().ok()? };
            Some((a, b))
        }
        None => Some((0, text.parse().ok()?)),
    }
}

impl Complex {
    pub fn specificity(&self) -> (u32, u32, u32) {
        std::iter::once(&self.subject).chain(self.ancestors.iter().map(|(compound, _)| compound)).fold((0, 0, 0), |sum, compound| {
            let (a, b, c) = compound.specificity();
            (sum.0 + a, sum.1 + b, sum.2 + c)
        })
    }

    /// Whether this matches the last element of `path`, whose ancestors
    /// are the elements before it.
    pub fn matches<E: Element>(&self, path: &[Place<E>]) -> bool {
        let Some(place) = path.last() else { return false };
        self.subject.matches(path, place) && self.ancestors_match(self.ancestors.len(), path, place)
    }

    /// Whether the first `n` compounds before the subject match, given
    /// that the one after them matched `place`, at the end of `path`.
    fn ancestors_match<E: Element>(&self, n: usize, path: &[Place<E>], place: &Place<E>) -> bool {
        if n == 0 {
            return true;
        }
        let (compound, combinator) = &self.ancestors[n - 1];
        let parent_path = &path[..path.len() - 1];
        match combinator {
            Combinator::Child => parent_path
                .last()
                .is_some_and(|parent| compound.matches(parent_path, parent) && self.ancestors_match(n - 1, parent_path, parent)),
            Combinator::Descendant => (0..parent_path.len()).rev().any(|i| {
                let ancestors = &path[..=i];
                compound.matches(ancestors, &path[i]) && self.ancestors_match(n - 1, ancestors, &path[i])
            }),
            Combinator::Next | Combinator::Later => {
                let earlier = 0..place.index;
                let earlier: Vec<usize> = if *combinator == Combinator::Next { earlier.rev().take(1).collect() } else { earlier.collect() };
                earlier.into_iter().any(|index| {
                    let sibling = Place { siblings: place.siblings.clone(), index };
                    let sibling_path = with_last(path, &sibling);
                    compound.matches(&sibling_path, &sibling) && self.ancestors_match(n - 1, &sibling_path, &sibling)
                })
            }
        }
    }
}

/// `path` with its last element's place swapped for `place`, a sibling of it.
fn with_last<'a, E>(path: &[Place<'a, E>], place: &Place<'a, E>) -> Vec<Place<'a, E>> {
    let mut path: Vec<Place<E>> = path.iter().map(|p| Place { siblings: p.siblings.clone(), index: p.index }).collect();
    if let Some(last) = path.last_mut() {
        last.index = place.index;
    }
    path
}

impl Compound {
    fn specificity(&self) -> (u32, u32, u32) {
        let mut sum = (self.id.is_some() as u32, (self.classes.len() + self.attrs.len()) as u32, self.tag.is_some() as u32);
        for pseudo in &self.pseudo {
            let most = |list: &Vec<Complex>| list.iter().map(Complex::specificity).max().unwrap_or((0, 0, 0));
            let (a, b, c) = match pseudo {
                Pseudo::Not(list) | Pseudo::Is(list) => most(list),
                Pseudo::Where(_) => (0, 0, 0),
                _ => (0, 1, 0),
            };
            sum = (sum.0 + a, sum.1 + b, sum.2 + c);
        }
        sum
    }

    /// Whether this matches the element at `place`, the last of `path`
    /// but for which of its siblings it is.
    fn matches<E: Element>(&self, path: &[Place<E>], place: &Place<E>) -> bool {
        let el = place.element();
        if self.tag.as_ref().is_some_and(|tag| tag != el.name()) {
            return false;
        }
        if self.id.as_ref().is_some_and(|id| el.attr("id").map(str::trim) != Some(id)) {
            return false;
        }
        if !self.classes.is_empty() {
            let classes: Vec<&str> = el.attr("class").unwrap_or("").split_whitespace().collect();
            if !self.classes.iter().all(|class| classes.contains(&class.as_str())) {
                return false;
            }
        }
        if !self.attrs.iter().all(|test| test.matches(el.attr(&test.name))) {
            return false;
        }
        let here = with_last(path, place);
        self.pseudo.iter().all(|pseudo| match pseudo {
            Pseudo::Never | Pseudo::Has => false,
            Pseudo::Root => el.name() == "html",
            Pseudo::Link => matches!(el.name(), "a" | "area") && el.attr("href").is_some(),
            Pseudo::Empty => false,
            Pseudo::Nth { a, b, last, of_type } => {
                let same = |i: &usize| !of_type || place.siblings[*i].name() == el.name();
                let position = if *last {
                    (place.index + 1..place.siblings.len()).filter(same).count() as i32 + 1
                } else {
                    (0..place.index).filter(same).count() as i32 + 1
                };
                match a {
                    0 => position == *b,
                    a => (position - b) % a == 0 && (position - b) / a >= 0,
                }
            }
            Pseudo::Not(list) => !list.iter().any(|selector| selector.matches(&here)),
            Pseudo::Is(list) | Pseudo::Where(list) => list.iter().any(|selector| selector.matches(&here)),
        })
    }
}

impl AttrTest {
    fn matches(&self, value: Option<&str>) -> bool {
        let Some(value) = value else { return false };
        let Some((op, wanted)) = &self.test else { return true };
        let (value, wanted) = match self.ignore_case {
            true => (value.to_lowercase(), wanted.to_lowercase()),
            false => (value.to_string(), wanted.clone()),
        };
        match op {
            '=' => value == wanted,
            '~' => value.split_whitespace().any(|word| word == wanted),
            '|' => value == wanted || value.starts_with(&format!("{wanted}-")),
            '^' => !wanted.is_empty() && value.starts_with(&wanted),
            '$' => !wanted.is_empty() && value.ends_with(&wanted),
            '*' => !wanted.is_empty() && value.contains(&wanted),
            _ => false,
        }
    }
}

// Values

/// `value` with each `var(--name, fallback)` replaced by the variable's
/// value from `vars`, or the fallback. `None` if one is missing and has
/// no fallback.
pub fn substitute(value: &str, vars: &HashMap<String, String>) -> Option<String> {
    substitute_nested(value, vars, 0)
}

fn substitute_nested(value: &str, vars: &HashMap<String, String>, depth: u32) -> Option<String> {
    if !value.contains("var(") {
        return Some(value.to_string());
    }
    if depth > 16 {
        return None;
    }
    let start = value.find("var(")?;
    let chars: Vec<char> = value[start + 3..].chars().collect();
    let (inside, end) = read_bracketed(&chars, 0, '(', ')')?;
    let after: String = chars[end..].iter().collect();
    let (name, fallback) = match inside.split_once(',') {
        Some((name, fallback)) => (name.trim(), Some(fallback.trim())),
        None => (inside.trim(), None),
    };
    let replacement = match vars.get(name).filter(|v| !v.trim().is_empty()) {
        Some(found) => substitute_nested(found, vars, depth + 1)?,
        None => substitute_nested(fallback?, vars, depth + 1)?,
    };
    substitute_nested(&format!("{}{replacement}{after}", &value[..start]), vars, depth + 1)
}

/// The font size a keyword like `large` stands for, in pixels.
pub fn font_size_keyword(word: &str) -> Option<f32> {
    Some(match &*word.to_ascii_lowercase() {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "xxx-large" => 48.0,
        _ => return None,
    })
}

/// A length in pixels, with `em` measured against `font_size` and
/// percentages left out, as what they're of isn't known.
pub fn length(value: &str, font_size: f32) -> Option<f32> {
    length_of(value, font_size, None)
}

/// A length in pixels, with percentages of `whole`.
pub fn length_of(value: &str, font_size: f32, whole: Option<f32>) -> Option<f32> {
    let value = value.trim().to_ascii_lowercase();
    if let Some(inside) = value.strip_prefix("calc(").and_then(|v| v.strip_suffix(')')) {
        return calc(inside, font_size, whole);
    }
    for (function, pick) in [("min(", f32::min as fn(f32, f32) -> f32), ("max(", f32::max)] {
        if let Some(inside) = value.strip_prefix(function).and_then(|v| v.strip_suffix(')')) {
            return split_unnested(inside, ',').into_iter().filter_map(|v| length_of(v, font_size, whole)).reduce(pick);
        }
    }
    if let Some(inside) = value.strip_prefix("clamp(").and_then(|v| v.strip_suffix(')')) {
        let parts: Vec<Option<f32>> = split_unnested(inside, ',').into_iter().map(|v| length_of(v, font_size, whole)).collect();
        return match parts.as_slice() {
            [Some(low), Some(preferred), Some(high)] => Some(preferred.clamp(*low, high.max(*low))),
            [Some(low), None, _] => Some(*low),
            _ => None,
        };
    }
    if let Some(inside) = value.strip_prefix('(').and_then(|v| v.strip_suffix(')')) {
        return calc(inside, font_size, whole);
    }
    let split = value.find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e'))).unwrap_or(value.len());
    // `e` is a digit only when an exponent follows, not in `em`.
    let split = if value[..split].ends_with('e') { split - 1 } else { split };
    let (number, unit) = value.split_at(split);
    let n: f32 = number.parse().ok()?;
    let (vw, vh) = (VIEWPORT.0 / 100.0, VIEWPORT.1 / 100.0);
    Some(match unit {
        "px" => n,
        "" if n == 0.0 => 0.0,
        "em" => n * font_size,
        "rem" => n * 16.0,
        "ex" | "ch" => n * font_size * 0.5,
        "pt" => n * 4.0 / 3.0,
        "pc" => n * 16.0,
        "in" => n * 96.0,
        "cm" => n * 96.0 / 2.54,
        "mm" => n * 96.0 / 25.4,
        "q" => n * 96.0 / 101.6,
        "vw" | "svw" | "lvw" | "dvw" => n * vw,
        "vh" | "svh" | "lvh" | "dvh" => n * vh,
        "vmin" => n * vw.min(vh),
        "vmax" => n * vw.max(vh),
        "%" => whole? * n / 100.0,
        _ => return None,
    })
}

/// A `calc()` expression of lengths and numbers, added, subtracted,
/// multiplied and divided.
fn calc(expression: &str, font_size: f32, whole: Option<f32>) -> Option<f32> {
    let words = split_words(expression);
    // Sums first: `+` and `-` must have spaces around them.
    if let Some(i) = words.iter().rposition(|w| w == "+" || w == "-") {
        let left = calc(&words[..i].join(" "), font_size, whole)?;
        let right = calc(&words[i + 1..].join(" "), font_size, whole)?;
        return Some(if words[i] == "+" { left + right } else { left - right });
    }
    let product = expression.replace('*', " * ").replace('/', " / ");
    let words = split_words(&product);
    let mut total: Option<f32> = None;
    let mut operator = '*';
    for word in &words {
        match word.as_str() {
            "*" => operator = '*',
            "/" => operator = '/',
            word => {
                let value = word.parse::<f32>().ok().or_else(|| length_of(word, font_size, whole))?;
                total = Some(match (total, operator) {
                    (None, _) => value,
                    (Some(t), '*') => t * value,
                    (Some(t), _) if value != 0.0 => t / value,
                    _ => return None,
                });
            }
        }
    }
    total
}

/// Red, green, blue and opacity, each 0 to 255.
pub type Rgba = [u8; 4];

fn is_color_function(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    ["rgb(", "rgba(", "hsl(", "hsla(", "light-dark("].iter().any(|f| lower.starts_with(f))
}

/// A colour, or `None` for `currentcolor`, keywords like `inherit`, and
/// colours this doesn't read.
pub fn color(value: &str) -> Option<Rgba> {
    let value = value.trim().to_ascii_lowercase();
    if let Some(hex) = value.strip_prefix('#') {
        let digit = |i: usize, len: usize| u8::from_str_radix(hex.get(i..i + len)?, 16).ok();
        return match hex.len() {
            3 => Some([digit(0, 1)? * 17, digit(1, 1)? * 17, digit(2, 1)? * 17, 255]),
            4 => Some([digit(0, 1)? * 17, digit(1, 1)? * 17, digit(2, 1)? * 17, digit(3, 1)? * 17]),
            6 => Some([digit(0, 2)?, digit(2, 2)?, digit(4, 2)?, 255]),
            8 => Some([digit(0, 2)?, digit(2, 2)?, digit(4, 2)?, digit(6, 2)?]),
            _ => None,
        };
    }
    // The light one, as translated pages are in light mode.
    if let Some(inside) = value.strip_prefix("light-dark(").and_then(|v| v.strip_suffix(')')) {
        return color(split_unnested(inside, ',').first()?);
    }
    if let Some((function, rest)) = value.split_once('(') {
        let inside = rest.strip_suffix(')')?;
        let parts: Vec<&str> = inside.split([',', ' ', '/']).map(str::trim).filter(|p| !p.is_empty()).collect();
        let alpha = |part: Option<&&str>| -> Option<u8> {
            let Some(part) = part else { return Some(255) };
            let a = match part.strip_suffix('%') {
                Some(percent) => percent.parse::<f32>().ok()? / 100.0,
                None => part.parse::<f32>().ok()?,
            };
            Some((a.clamp(0.0, 1.0) * 255.0).round() as u8)
        };
        return match function {
            "rgb" | "rgba" => {
                let channel = |part: &str| -> Option<u8> {
                    let v = match part.strip_suffix('%') {
                        Some(percent) => percent.parse::<f32>().ok()? * 2.55,
                        None => part.parse::<f32>().ok()?,
                    };
                    Some(v.round().clamp(0.0, 255.0) as u8)
                };
                Some([channel(parts.first()?)?, channel(parts.get(1)?)?, channel(parts.get(2)?)?, alpha(parts.get(3))?])
            }
            "hsl" | "hsla" => {
                let h = parts.first()?.trim_end_matches("deg").parse::<f32>().ok()?;
                let s = parts.get(1)?.trim_end_matches('%').parse::<f32>().ok()? / 100.0;
                let l = parts.get(2)?.trim_end_matches('%').parse::<f32>().ok()? / 100.0;
                let [r, g, b] = hsl_to_rgb(h, s, l);
                Some([r, g, b, alpha(parts.get(3))?])
            }
            _ => None,
        };
    }
    if value == "transparent" {
        return Some([0, 0, 0, 0]);
    }
    let hex = NAMED_COLORS.split_whitespace().collect::<Vec<_>>().chunks(2).find(|pair| pair[0] == value).map(|pair| pair[1])?;
    color(&format!("#{hex}"))
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s.clamp(0.0, 1.0);
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [r, g, b].map(|v| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8)
}

/// `rgba` as Netscheme writes colours: `#rrggbb`, or `#rrggbbaa` if it's translucent.
pub fn hex(rgba: Rgba) -> String {
    let [r, g, b, a] = rgba;
    if a == 255 { format!("#{r:02x}{g:02x}{b:02x}") } else { format!("#{r:02x}{g:02x}{b:02x}{a:02x}") }
}

const NAMED_COLORS: &str = "aliceblue f0f8ff antiquewhite faebd7 aqua 00ffff aquamarine 7fffd4 azure f0ffff beige f5f5dc \
bisque ffe4c4 black 000000 blanchedalmond ffebcd blue 0000ff blueviolet 8a2be2 brown a52a2a burlywood deb887 \
cadetblue 5f9ea0 chartreuse 7fff00 chocolate d2691e coral ff7f50 cornflowerblue 6495ed cornsilk fff8dc crimson dc143c \
cyan 00ffff darkblue 00008b darkcyan 008b8b darkgoldenrod b8860b darkgray a9a9a9 darkgreen 006400 darkgrey a9a9a9 \
darkkhaki bdb76b darkmagenta 8b008b darkolivegreen 556b2f darkorange ff8c00 darkorchid 9932cc darkred 8b0000 \
darksalmon e9967a darkseagreen 8fbc8f darkslateblue 483d8b darkslategray 2f4f4f darkslategrey 2f4f4f \
darkturquoise 00ced1 darkviolet 9400d3 deeppink ff1493 deepskyblue 00bfff dimgray 696969 dimgrey 696969 \
dodgerblue 1e90ff firebrick b22222 floralwhite fffaf0 forestgreen 228b22 fuchsia ff00ff gainsboro dcdcdc \
ghostwhite f8f8ff gold ffd700 goldenrod daa520 gray 808080 green 008000 greenyellow adff2f grey 808080 \
honeydew f0fff0 hotpink ff69b4 indianred cd5c5c indigo 4b0082 ivory fffff0 khaki f0e68c lavender e6e6fa \
lavenderblush fff0f5 lawngreen 7cfc00 lemonchiffon fffacd lightblue add8e6 lightcoral f08080 lightcyan e0ffff \
lightgoldenrodyellow fafad2 lightgray d3d3d3 lightgreen 90ee90 lightgrey d3d3d3 lightpink ffb6c1 lightsalmon ffa07a \
lightseagreen 20b2aa lightskyblue 87cefa lightslategray 778899 lightslategrey 778899 lightsteelblue b0c4de \
lightyellow ffffe0 lime 00ff00 limegreen 32cd32 linen faf0e6 magenta ff00ff maroon 800000 mediumaquamarine 66cdaa \
mediumblue 0000cd mediumorchid ba55d3 mediumpurple 9370db mediumseagreen 3cb371 mediumslateblue 7b68ee \
mediumspringgreen 00fa9a mediumturquoise 48d1cc mediumvioletred c71585 midnightblue 191970 mintcream f5fffa \
mistyrose ffe4e1 moccasin ffe4b5 navajowhite ffdead navy 000080 oldlace fdf5e6 olive 808000 olivedrab 6b8e23 \
orange ffa500 orangered ff4500 orchid da70d6 palegoldenrod eee8aa palegreen 98fb98 paleturquoise afeeee \
palevioletred db7093 papayawhip ffefd5 peachpuff ffdab9 peru cd853f pink ffc0cb plum dda0dd powderblue b0e0e6 \
purple 800080 rebeccapurple 663399 red ff0000 rosybrown bc8f8f royalblue 4169e1 saddlebrown 8b4513 salmon fa8072 \
sandybrown f4a460 seagreen 2e8b57 seashell fff5ee sienna a0522d silver c0c0c0 skyblue 87ceeb slateblue 6a5acd \
slategray 708090 slategrey 708090 snow fffafa springgreen 00ff7f steelblue 4682b4 tan d2b48c teal 008080 \
thistle d8bfd8 tomato ff6347 turquoise 40e0d0 violet ee82ee wheat f5deb3 white ffffff whitesmoke f5f5f5 \
yellow ffff00 yellowgreen 9acd32";

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    struct El(&'static str, Vec<(&'static str, &'static str)>);

    impl Element for El {
        fn name(&self) -> &str {
            self.0
        }
        fn attr(&self, name: &str) -> Option<&str> {
            self.1.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
        }
    }

    /// The path to the last element of `chain`, each the only child of the one before.
    fn path(chain: &[El]) -> Vec<Place<'_, El>> {
        chain.iter().map(|el| Place { siblings: Rc::new(vec![el]), index: 0 }).collect()
    }

    fn matches(selector: &str, path: &[Place<El>]) -> bool {
        parse_complex(selector).expect("a selector").matches(path)
    }

    #[test]
    fn selectors_match_by_tag_class_id_attribute_and_ancestry() {
        let chain =
            [El("html", vec![]), El("nav", vec![("id", "top")]), El("ul", vec![]), El("a", vec![("class", "item active"), ("href", "/x")])];
        let path = path(&chain);
        for selector in ["a", ".item", "a.item.active", "#top a", "nav > ul > a", "ul a[href^='/']", ":root a", "a:first-child", "*"] {
            assert!(matches(selector, &path), "{selector}");
        }
        for selector in ["p", ".other", "#top > a", "a:hover", "a:not(.active)", "nav + a"] {
            assert!(!matches(selector, &path), "{selector}");
        }
        assert!(parse_complex("a::before").is_none(), "pseudo-elements are content the translation lacks");
    }

    #[test]
    fn sibling_combinators_and_nth_child_count_the_parent_s_children() {
        let (a, b, c) = (El("h2", vec![]), El("p", vec![]), El("p", vec![("class", "x")]));
        let siblings = Rc::new(vec![&a, &b, &c]);
        let place = |index| vec![Place { siblings: siblings.clone(), index }];
        assert!(matches("h2 + p", &place(1)));
        assert!(!matches("h2 + p", &place(2)));
        assert!(matches("h2 ~ .x", &place(2)));
        assert!(matches("p:nth-child(odd)", &place(2)));
        assert!(matches("p:last-child", &place(2)));
        assert!(matches("p:first-of-type", &place(1)));
        assert!(!matches("p:nth-child(2n)", &place(2)));
    }

    #[test]
    fn specificity_counts_ids_then_classes_then_tags() {
        assert_eq!(parse_complex("#a .b c:hover").unwrap().specificity(), (1, 2, 1));
        assert_eq!(parse_complex(":where(#a) p:is(.x, #y)").unwrap().specificity(), (1, 0, 1));
    }

    #[test]
    fn rules_keep_matching_media_and_skip_the_rest() {
        let mut rules = Rules::default();
        let css = "/* x */ p { color: red } @media print { p { color: blue } } @media (min-width: 600px) { p { margin: 0 } }
                   @media (max-width: 400px) { p { margin: 9px } } @font-face { font-family: x } @import 'more.css';";
        let mut imported = Vec::new();
        rules.add(css, &mut |href| {
            imported.push(href.to_string());
            Some("p { padding: 1px 2px }".into())
        });
        assert_eq!(imported, ["more.css"]);
        let p = El("p", vec![]);
        let found: Vec<String> =
            rules.matching(&path(std::slice::from_ref(&p))).iter().flat_map(|r| r.declarations.iter().map(|d| d.name.clone())).collect();
        assert_eq!(
            found,
            [
                "color",
                "margin-top",
                "margin-right",
                "margin-bottom",
                "margin-left",
                "padding-top",
                "padding-right",
                "padding-bottom",
                "padding-left"
            ]
        );
    }

    #[test]
    fn shorthands_become_longhands() {
        let d = parse_declarations("border: 1px solid #ccc; background: url(x.png) no-repeat #fff; font: bold 14px/1.5 Arial, sans-serif");
        let get = |name: &str| d.iter().find(|d| d.name == name).map(|d| d.value.as_str());
        assert_eq!(get("border-left-color"), Some("#ccc"));
        assert_eq!(get("border-top-width"), Some("1px"));
        assert_eq!(get("background-color"), Some("#fff"));
        assert_eq!((get("font-weight"), get("font-size"), get("line-height")), (Some("bold"), Some("14px"), Some("1.5")));
        assert_eq!(get("font-family"), Some("Arial, sans-serif"));
    }

    #[test]
    fn values_are_read() {
        assert_eq!(color("rgb(255 0 0 / 50%)"), Some([255, 0, 0, 128]));
        assert_eq!(color("rgba(0, 128, 255, 1)"), Some([0, 128, 255, 255]));
        assert_eq!(color("hsl(120, 100%, 25%)"), Some([0, 128, 0, 255]));
        assert_eq!(color("CornflowerBlue"), Some([0x64, 0x95, 0xed, 255]));
        assert_eq!(color("currentColor"), None);
        assert_eq!(color("light-dark(#eee, #222)"), Some([0xee, 0xee, 0xee, 255]));
        assert_eq!(length("1.5em", 10.0), Some(15.0));
        assert_eq!(length("calc(1rem + 2px)", 10.0), Some(18.0));
        assert_eq!(length("clamp(1rem, 10vw, 2rem)", 10.0), Some(32.0));
        assert_eq!(length("50%", 10.0), None);
        let vars = HashMap::from([("--a".to_string(), "var(--b)".to_string()), ("--b".to_string(), "#123".to_string())]);
        assert_eq!(substitute("1px solid var(--a)", &vars).as_deref(), Some("1px solid #123"));
        assert_eq!(substitute("var(--missing, red)", &vars).as_deref(), Some("red"));
        assert_eq!(substitute("var(--missing)", &vars), None);
    }

    #[test]
    fn media_queries_are_for_a_window_s_width_in_light_mode() {
        assert!(media_matches("screen and (min-width: 768px)"));
        assert!(!media_matches("(max-width: 600px)"));
        assert!(media_matches("print, (prefers-color-scheme: light)"));
        assert!(!media_matches("(prefers-color-scheme: dark)"));
        assert!(media_matches("(width >= 40em)"));
        assert!(!media_matches("not all and (min-width: 100px)"));
        assert!(!media_matches("only screen\nand (min-width : 300px)\nand (max-width : 750px)"));
        assert!(media_matches("only screen\nand (min-width : 300px)\nand (max-width : 950px)"));
    }
}

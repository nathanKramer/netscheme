//! Reads HTML pages by translating them into Netscheme programs. Netscheme
//! has no HTML, CSS or JavaScript, so an HTML page is parsed into a tree,
//! each element is turned into the nearest Netscheme element, and the
//! result is printed as the source of a page. Scripts are left out, and so
//! is anything Netscheme can't draw, like video.
//!
//! The page's stylesheets are read and applied to the tree, and each
//! element gets the Netscheme style attributes nearest to the CSS that
//! applies to it, so the page looks roughly as it would in other browsers.
//!
//! The parser is forgiving, as browsers' are: it closes elements that HTML
//! lets authors leave open, like `p` and `li`, and ignores end tags that
//! match nothing.

use std::{collections::HashMap, rc::Rc};

use crate::css::{self, Declaration, Place, Rgba, Rules};

/// A page reads at most this many stylesheets, counting imported ones.
const MAX_STYLESHEETS: usize = 24;

/// Whether the file at `location`, holding `text`, is an HTML page.
pub fn is_html(location: &str, text: &str) -> bool {
    let file = location.split_once('?').map_or(location, |(file, _)| file).to_ascii_lowercase();
    file.ends_with(".html") || file.ends_with(".htm") || text.trim_start_matches('\u{feff}').trim_start().starts_with('<')
}

/// The Netscheme program for the HTML page `html`, without the
/// stylesheets it links to.
#[cfg(test)]
pub fn to_lisp(html: &str) -> String {
    to_lisp_with(html, "", &mut |_, _| None)
}

/// The Netscheme program for the HTML page `html`, from `location`.
/// `load(base, href)` reads the stylesheet at `href`, relative to `base`.
pub fn to_lisp_with(html: &str, location: &str, load: &mut dyn FnMut(&str, &str) -> Option<String>) -> String {
    let root = parse(&html.replace("\r\n", "\n").replace('\r', "\n"));
    let rules = stylesheets(&root, location, load);
    let styles = Styles::compute(&root, &rules);
    let title = find(&root, "title").map(|title| collapse(&text_of(title)).trim().to_string());
    let body = find(&root, "body").unwrap_or(&root);
    let ctx = Context { styles: &styles, pre: false, depth: 0 };
    let mut children = vec![banner()];
    children.extend(convert_children(body, &ctx));

    let mut page = Out::element("page", page_style(&root, body, &styles), finish_block(children));
    if let (Out::Element { attrs, .. }, Some(title)) = (&mut page, title.filter(|t| !t.is_empty())) {
        attrs.insert(0, ("title", Attr::Str(title)));
    }
    let mut out = String::from(
        "; Translated from HTML by Netscheme, which doesn't support HTML, CSS or\n\
         ; JavaScript. Scripts were left out, and styles are approximate.\n\n",
    );
    write(&page, 0, &mut out);
    out.push('\n');
    out
}

/// The style of the page itself, from the CSS for `html` and `body`. A
/// page with stylesheets is drawn in the colours other browsers would give
/// it, in light mode, rather than the browser's own.
fn page_style(root: &Html, body: &Html, styles: &Styles) -> Vec<(&'static str, Attr)> {
    if styles.rules_empty {
        return Vec::new();
    }
    let body_style = styles.get(body);
    let html_style = find(root, "html").and_then(|html| styles.get(html));
    let background = [body_style, html_style]
        .into_iter()
        .flatten()
        .find_map(|style| style.decls.get("background-color").and_then(|c| css::color(c)).filter(|c| c[3] > 0))
        .unwrap_or([255, 255, 255, 255]);
    let color = body_style.and_then(|style| style.color).unwrap_or([0, 0, 0, 255]);
    let mut attrs = vec![("background", Attr::Str(css::hex(background))), ("color", Attr::Str(css::hex(color)))];
    let Some(style) = body_style else {
        attrs.push(("padding", Attr::Num(8.0)));
        return attrs;
    };
    // Other browsers give `body` a margin, which is the page's padding here.
    for (side, name) in [("top", "padding-top"), ("right", "padding-right"), ("bottom", "padding-bottom"), ("left", "padding-left")] {
        let margin = style.decls.get(&format!("margin-{side}")).map_or(Some(8.0), |m| css::length(m, style.font_size));
        let padding = style.decls.get(&format!("padding-{side}")).and_then(|p| css::length(p, style.font_size)).unwrap_or(0.0);
        attrs.push((name, Attr::Num(round(margin.unwrap_or(0.0).max(0.0) + padding.max(0.0)))));
    }
    let mut own = styles.css(body, "page");
    own.attrs.retain(|(name, _)| !name.starts_with("padding") && !matches!(*name, "background" | "color" | "margin" | "margin-top"));
    attrs.extend(own.attrs);
    attrs
}

/// The rules of the page's stylesheets, from `style` elements and `link`s
/// to stylesheets, in the order they appear.
fn stylesheets(root: &Html, location: &str, load: &mut dyn FnMut(&str, &str) -> Option<String>) -> Rules {
    let mut sheets = Vec::new();
    collect_stylesheets(root, &mut sheets);
    let mut rules = Rules::default();
    let mut loaded = 0;
    for sheet in sheets {
        match sheet {
            Sheet::Inline(text) => rules.add(&text, &mut |href| {
                loaded += 1;
                (loaded <= MAX_STYLESHEETS).then(|| load(location, href)).flatten()
            }),
            Sheet::Linked(href) => {
                loaded += 1;
                if loaded > MAX_STYLESHEETS {
                    continue;
                }
                let Some(text) = load(location, &href) else { continue };
                // Imports are relative to the stylesheet that has them.
                let base = crate::loader::resolve(location, &href);
                rules.add(&text, &mut |href| {
                    loaded += 1;
                    (loaded <= MAX_STYLESHEETS).then(|| load(&base, href)).flatten()
                });
            }
        }
    }
    rules
}

enum Sheet {
    Inline(String),
    Linked(String),
}

fn collect_stylesheets(el: &Html, out: &mut Vec<Sheet>) {
    for child in &el.children {
        let Node::Element(child) = child else { continue };
        let media = child.attr("media").is_none_or(css::media_matches);
        match child.name.as_str() {
            "svg" | "template" | "math" => {}
            "style" if media && child.attr("type").is_none_or(|t| t.trim().eq_ignore_ascii_case("text/css")) => {
                out.push(Sheet::Inline(text_of(child)))
            }
            "link" if media => {
                let rel = child.attr("rel").unwrap_or("").to_ascii_lowercase();
                let rel: Vec<&str> = rel.split_whitespace().collect();
                if let Some(href) = child.attr("href").filter(|_| rel.contains(&"stylesheet") && !rel.contains(&"alternate")) {
                    out.push(Sheet::Linked(href.trim().to_string()));
                }
            }
            _ => collect_stylesheets(child, out),
        }
    }
}

/// What the page's CSS gives an element: the values of the properties set
/// on it, after the cascade, and the inherited properties the translation
/// needs to work out the others.
struct Styled {
    decls: HashMap<String, String>,
    /// In pixels.
    font_size: f32,
    /// What the `color` property is, if the CSS gave it one here or above.
    color: Option<Rgba>,
    text_align: Option<String>,
    list_none: bool,
    vars: Rc<HashMap<String, String>>,
}

/// The styles of each element in a page, by its address in the tree.
struct Styles {
    by_element: HashMap<*const Html, Styled>,
    rules_empty: bool,
}

impl Styles {
    fn compute(root: &Html, rules: &Rules) -> Styles {
        let mut styles = Styles { by_element: HashMap::new(), rules_empty: rules.is_empty() };
        let top = Styled { decls: HashMap::new(), font_size: 16.0, color: None, text_align: None, list_none: false, vars: Rc::default() };
        styles.compute_children(root, &mut Vec::new(), &top, rules);
        styles
    }

    fn compute_children<'a>(&mut self, el: &'a Html, path: &mut Vec<Place<'a, Html>>, parent: &Styled, rules: &Rules) {
        let kids: Rc<Vec<&Html>> = Rc::new(
            el.children
                .iter()
                .filter_map(|child| match child {
                    Node::Element(child) => Some(child),
                    Node::Text(_) => None,
                })
                .collect(),
        );
        for index in 0..kids.len() {
            let kid = kids[index];
            if SKIPPED.contains(&kid.name.as_str()) {
                continue;
            }
            path.push(Place { siblings: kids.clone(), index });
            let styled = cascade(kid, path, parent, rules);
            self.compute_children(kid, path, &styled, rules);
            self.by_element.insert(kid as *const Html, styled);
            path.pop();
        }
    }

    fn get(&self, el: &Html) -> Option<&Styled> {
        self.by_element.get(&(el as *const Html))
    }

    fn float(&self, el: &Html) -> Option<Float> {
        match self.get(el)?.decls.get("float")?.to_ascii_lowercase().as_str() {
            "left" | "inline-start" => Some(Float::Left),
            "right" | "inline-end" => Some(Float::Right),
            _ => None,
        }
    }
}

impl css::Element for Html {
    fn name(&self) -> &str {
        &self.name
    }

    fn attr(&self, name: &str) -> Option<&str> {
        Html::attr(self, name)
    }
}

/// The style the rules, and its `style` attribute, give the element at the
/// end of `path`, whose parent's is `parent`.
fn cascade(el: &Html, path: &[Place<Html>], parent: &Styled, rules: &Rules) -> Styled {
    let matching = rules.matching(path);
    let inline = el.attr("style").map(css::parse_declarations).unwrap_or_default();
    let hints = presentational_hints(el);
    // Ordered by importance, then whether it's from the `style` attribute,
    // then specificity, then which rule comes later.
    type Precedence = (bool, u32, (u32, u32, u32), usize);
    let mut applied: Vec<(Precedence, &Declaration)> = hints.iter().map(|d| ((false, 0, (0, 0, 0), 0), d)).collect();
    for rule in &matching {
        applied.extend(rule.declarations.iter().map(|d| ((d.important, 0, rule.specificity, rule.order), d)));
    }
    applied.extend(inline.iter().map(|d| ((d.important, 1, (0, 0, 0), usize::MAX), d)));
    // Stable, so that later declarations in a rule win.
    applied.sort_by_key(|(key, _)| *key);

    let mut raw: HashMap<&str, &str> = HashMap::new();
    for (_, d) in &applied {
        raw.insert(&d.name, &d.value);
    }
    let vars = if raw.keys().any(|name| name.starts_with("--")) {
        let mut vars = (*parent.vars).clone();
        vars.extend(raw.iter().filter(|(name, _)| name.starts_with("--")).map(|(name, value)| (name.to_string(), value.to_string())));
        Rc::new(vars)
    } else {
        parent.vars.clone()
    };
    let decls: HashMap<String, String> = raw
        .into_iter()
        .filter(|(name, _)| !name.starts_with("--"))
        .filter_map(|(name, value)| Some((name.to_string(), css::substitute(value, &vars)?.trim().to_string())))
        .filter(|(_, value)| !matches!(&*value.to_ascii_lowercase(), "initial" | "unset" | "revert" | "revert-layer"))
        .collect();

    // As other browsers size them, relative to the parent's text.
    let scale = match el.name.as_str() {
        "h1" => 2.0,
        "h2" => 1.5,
        "h3" => 1.17,
        "h5" | "small" | "sub" | "sup" => 0.83,
        "h6" => 0.67,
        _ => 1.0,
    };
    let font_size = decls.get("font-size").and_then(|size| font_size(size, parent.font_size)).unwrap_or(parent.font_size * scale);
    let color = match decls.get("color").map(|c| c.to_ascii_lowercase()) {
        Some(c) if matches!(&*c, "inherit" | "currentcolor") => parent.color,
        Some(c) => css::color(&c).or(parent.color),
        None => parent.color,
    };
    let text_align = match decls.get("text-align").map(|a| a.to_ascii_lowercase()) {
        Some(a) if a == "inherit" => parent.text_align.clone(),
        Some(a) => Some(a),
        None => parent.text_align.clone(),
    };
    let list_none = match decls.get("list-style-type") {
        Some(t) => t.eq_ignore_ascii_case("none"),
        None => parent.list_none,
    };
    Styled { decls, font_size, color, text_align, list_none, vars }
}

/// The styles that old HTML attributes like `bgcolor` stand for, which
/// any CSS for the element overrides.
fn presentational_hints(el: &Html) -> Vec<Declaration> {
    let mut hints = Vec::new();
    let mut hint = |name: &str, value: &str| hints.push(Declaration { name: name.into(), value: value.trim().into(), important: false });
    if let Some(color) = el.attr("bgcolor") {
        hint("background-color", color);
    }
    if let Some(color) = el.attr("color").filter(|_| el.name == "font") {
        hint("color", color);
    }
    if let Some(color) = el.attr("text").filter(|_| el.name == "body") {
        hint("color", color);
    }
    if let Some(width) = el.attr("width").filter(|_| matches!(el.name.as_str(), "td" | "th" | "table" | "col" | "hr")) {
        // A number of pixels; percentages can't be followed.
        if width.trim().parse::<f32>().is_ok() {
            hint("width", &format!("{}px", width.trim()));
        }
    }
    hints
}

/// A `font-size` value in pixels, where the parent's is `parent`.
fn font_size(value: &str, parent: f32) -> Option<f32> {
    let value = value.to_ascii_lowercase();
    match &*value {
        "inherit" => Some(parent),
        "smaller" => Some(parent / 1.2),
        "larger" => Some(parent * 1.2),
        _ => css::font_size_keyword(&value).or_else(|| css::length_of(&value, parent, Some(parent))),
    }
}

fn round(n: f32) -> f64 {
    (n as f64 * 10.0).round() / 10.0
}

/// The notice at the top of every translated page.
fn banner() -> Out {
    let attrs = vec![
        ("background", Attr::Code(r##"(light-dark "#fff1d6" "#3d2f1c")"##)),
        ("border", Attr::Code(r##"(light-dark "#e8c98a" "#5c4626")"##)),
        ("radius", Attr::Num(6.0)),
        ("padding", Attr::Num(10.0)),
        ("margin", Attr::Num(16.0)),
    ];
    let text = "Netscheme does not natively support HTML, CSS or JavaScript. This page has been converted to Lisp.";
    let p = Out::element(
        "p",
        vec![("margin", Attr::Num(0.0)), ("size", Attr::Num(14.0)), ("color", Attr::Code(r##"(light-dark "#6b4a12" "#f0d49a")"##))],
        vec![Out::Text(text.into())],
    );
    Out::element("div", attrs, vec![p])
}

// Parsing

#[derive(Debug)]
struct Html {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}

#[derive(Debug)]
enum Node {
    Element(Html),
    Text(String),
}

impl Html {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// Elements that never have children or an end tag.
const VOID: &[&str] = &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track", "wbr"];
/// Elements whose content is text up to their end tag, not more HTML.
const RAW: &[&str] = &["script", "style", "title", "textarea", "xmp", "noembed", "noframes"];
/// Elements that end an open `p` when they start.
const CLOSES_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "details",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "ul",
];

/// Parses `html` into a tree, under an element with no name.
fn parse(html: &str) -> Html {
    let chars: Vec<char> = html.chars().collect();
    let mut tree = Tree { open: vec![Html { name: String::new(), attrs: Vec::new(), children: Vec::new() }] };
    let mut i = 0;
    let mut text = String::new();
    let starts = |i: usize, s: &str| s.chars().enumerate().all(|(n, c)| chars.get(i + n).is_some_and(|d| d.eq_ignore_ascii_case(&c)));
    while i < chars.len() {
        if chars[i] != '<' {
            text.push(chars[i]);
            i += 1;
            continue;
        }
        let next = chars.get(i + 1).copied().unwrap_or(' ');
        if starts(i, "<!--") {
            i = find_from(&chars, i + 4, "-->").map_or(chars.len(), |end| end + 3);
        } else if next == '!' || next == '?' {
            i = find_from(&chars, i, ">").map_or(chars.len(), |end| end + 1);
        } else if next == '/' && chars.get(i + 2).is_some_and(char::is_ascii_alphabetic) {
            tree.text(&text);
            text.clear();
            let (name, end) = read_name(&chars, i + 2);
            i = find_from(&chars, end, ">").map_or(chars.len(), |end| end + 1);
            tree.close(&name);
        } else if next.is_ascii_alphabetic() {
            tree.text(&text);
            text.clear();
            let (name, attrs, end) = read_tag(&chars, i + 1);
            i = end;
            if RAW.contains(&name.as_str()) {
                let close = format!("</{name}");
                let mut end = i;
                while end < chars.len() && !starts(end, &close) {
                    end += 1;
                }
                let raw: String = chars[i..end].iter().collect();
                let content = if name == "title" || name == "textarea" { decode_entities(&raw) } else { raw };
                tree.open(&name, attrs);
                tree.open.last_mut().unwrap().children.push(Node::Text(content));
                tree.close(&name);
                i = find_from(&chars, end, ">").map_or(chars.len(), |end| end + 1);
            } else {
                tree.open(&name, attrs);
            }
        } else {
            text.push('<');
            i += 1;
        }
    }
    tree.text(&text);
    tree.close_all()
}

/// The elements being parsed: the outermost first, the one that new
/// content goes into last.
struct Tree {
    open: Vec<Html>,
}

impl Tree {
    fn text(&mut self, raw: &str) {
        if !raw.is_empty() {
            self.open.last_mut().unwrap().children.push(Node::Text(decode_entities(raw)));
        }
    }

    /// The position of the innermost open `name`, unless one of `fences`
    /// is open inside it.
    fn position(&self, names: &[&str], fences: &[&str]) -> Option<usize> {
        for (i, el) in self.open.iter().enumerate().rev().take_while(|(i, _)| *i > 0) {
            if names.contains(&el.name.as_str()) {
                return Some(i);
            }
            if fences.contains(&el.name.as_str()) {
                return None;
            }
        }
        None
    }

    fn open(&mut self, name: &str, attrs: Vec<(String, String)>) {
        let implied: Option<usize> = match name {
            _ if CLOSES_P.contains(&name) => self.position(&["p"], &["button", "table", "td", "th"]),
            "li" => self.position(&["li"], &["ul", "ol", "menu"]),
            "dt" | "dd" => self.position(&["dt", "dd"], &["dl"]),
            "tr" => self.position(&["tr"], &["table", "thead", "tbody", "tfoot"]),
            "td" | "th" => self.position(&["td", "th"], &["tr", "table"]),
            "thead" | "tbody" | "tfoot" => self.position(&["thead", "tbody", "tfoot"], &["table"]),
            "option" => self.position(&["option"], &["select", "datalist"]),
            _ => None,
        };
        if let Some(i) = implied {
            self.pop_to(i);
        }
        self.open.push(Html { name: name.into(), attrs, children: Vec::new() });
        if VOID.contains(&name) {
            self.pop_to(self.open.len() - 1);
        }
    }

    fn close(&mut self, name: &str) {
        if name == "br" {
            return self.open("br", Vec::new());
        }
        if let Some(i) = self.open.iter().rposition(|el| el.name == name).filter(|&i| i > 0) {
            self.pop_to(i);
        }
    }

    /// Closes the element at `i` and everything inside it.
    fn pop_to(&mut self, i: usize) {
        while self.open.len() > i {
            let el = self.open.pop().unwrap();
            self.open.last_mut().unwrap().children.push(Node::Element(el));
        }
    }

    fn close_all(mut self) -> Html {
        self.pop_to(1);
        self.open.pop().unwrap()
    }
}

fn find_from(chars: &[char], from: usize, needle: &str) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    (from..chars.len()).find(|&i| chars[i..].starts_with(&needle))
}

/// Reads a tag or attribute name at `i`, in lower case, and where it ends.
fn read_name(chars: &[char], mut i: usize) -> (String, usize) {
    let mut name = String::new();
    while let Some(&c) = chars.get(i) {
        if c.is_whitespace() || matches!(c, '>' | '/' | '=') {
            break;
        }
        name.push(c.to_ascii_lowercase());
        i += 1;
    }
    (name, i)
}

/// Reads the name and attributes of a start tag whose name begins at `i`,
/// and where the tag ends.
fn read_tag(chars: &[char], i: usize) -> (String, Vec<(String, String)>, usize) {
    let (name, mut i) = read_name(chars, i);
    let mut attrs = Vec::new();
    loop {
        while chars.get(i).is_some_and(|c| c.is_whitespace() || *c == '/') {
            i += 1;
        }
        match chars.get(i) {
            None => return (name, attrs, i),
            Some('>') => return (name, attrs, i + 1),
            Some('=') => i += 1,
            Some(_) => {
                let (attr, end) = read_name(chars, i);
                i = end;
                while chars.get(i).is_some_and(|c| c.is_whitespace()) {
                    i += 1;
                }
                let mut value = String::new();
                if chars.get(i) == Some(&'=') {
                    i += 1;
                    while chars.get(i).is_some_and(|c| c.is_whitespace()) {
                        i += 1;
                    }
                    match chars.get(i) {
                        Some(&quote @ ('"' | '\'')) => {
                            let end = (i + 1..chars.len()).find(|&j| chars[j] == quote).unwrap_or(chars.len());
                            value = chars[i + 1..end].iter().collect();
                            i = end + 1;
                        }
                        _ => {
                            while chars.get(i).is_some_and(|c| !c.is_whitespace() && *c != '>') {
                                value.push(chars[i]);
                                i += 1;
                            }
                        }
                    }
                }
                if !attr.is_empty() && !attrs.iter().any(|(k, _)| *k == attr) {
                    attrs.push((attr, decode_entities(&value)));
                }
            }
        }
    }
}

/// Replaces character references like `&amp;` and `&#8212;` with the
/// characters they stand for. Unknown names are left as they are.
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let end = rest.char_indices().skip(1).take(32).find(|(_, c)| !c.is_ascii_alphanumeric() && *c != '#').map(|(i, _)| i);
        let (name, len) = match end {
            Some(end) if rest[end..].starts_with(';') => (&rest[1..end], end + 1),
            Some(end) => (&rest[1..end], end),
            None => (&rest[1..], rest.len()),
        };
        match entity(name) {
            Some(c) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(code).or(Some('\u{fffd}'));
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "middot" => '·',
        "bull" => '•',
        "times" => '×',
        "divide" => '÷',
        "deg" => '°',
        "plusmn" => '±',
        "para" => '¶',
        "sect" => '§',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        "shy" => '\u{ad}',
        "zwj" => '\u{200d}',
        "zwnj" => '\u{200c}',
        _ => return None,
    })
}

/// The first element named `name` in `el`, at any depth.
fn find<'h>(el: &'h Html, name: &str) -> Option<&'h Html> {
    el.children.iter().find_map(|child| match child {
        Node::Element(child) if child.name == name => Some(child),
        Node::Element(child) => find(child, name),
        Node::Text(_) => None,
    })
}

/// All of the text in `el`, as it's written.
fn text_of(el: &Html) -> String {
    el.children
        .iter()
        .map(|child| match child {
            Node::Element(child) => text_of(child),
            Node::Text(text) => text.clone(),
        })
        .collect()
}

/// `text` with each run of whitespace made a single space, as HTML shows it.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        // A non-breaking space is kept, as it's there to be seen.
        if c.is_whitespace() && c != '\u{a0}' {
            space = true;
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    if space {
        out.push(' ');
    }
    out
}

// Translating

/// A Netscheme element or text, as it will be printed.
#[derive(Debug)]
enum Out {
    Element {
        tag: &'static str,
        attrs: Vec<(&'static str, Attr)>,
        children: Vec<Out>,
    },
    Text(String),
    /// A `br`, which ends the line of text in the block it's in.
    Break,
}

#[derive(Debug)]
enum Attr {
    Str(String),
    Num(f64),
    Bool(bool),
    /// Netscheme code, printed as it is.
    Code(&'static str),
}

impl Out {
    /// An element, with each run of text among `children` joined into one.
    fn element(tag: &'static str, attrs: Vec<(&'static str, Attr)>, children: Vec<Out>) -> Out {
        let mut joined: Vec<Out> = Vec::with_capacity(children.len());
        for child in children {
            match (joined.last_mut(), child) {
                (Some(Out::Text(text)), Out::Text(more)) => text.push_str(&more),
                (_, child) => joined.push(child),
            }
        }
        Out::Element { tag, attrs, children: joined }
    }

    fn is_block(&self) -> bool {
        match self {
            Out::Element { tag, children, .. } => {
                matches!(*tag, "page" | "div" | "row" | "p" | "h1" | "h2" | "h3" | "pre")
                    || *tag == "a" && children.iter().any(Out::is_block)
            }
            _ => false,
        }
    }
}

#[derive(Clone)]
struct Context<'s> {
    styles: &'s Styles,
    /// Inside a `pre`, where whitespace is kept.
    pre: bool,
    /// How many Netscheme elements deep this is.
    depth: u32,
}

/// Elements nested deeper than this lose their wrappers, as Netscheme
/// refuses documents nested more than 512 deep.
const MAX_DEPTH: u32 = 400;

/// Elements that are left out, as Netscheme can't show them.
const SKIPPED: &[&str] = &[
    "head", "script", "style", "template", "svg", "math", "iframe", "object", "embed", "video", "audio", "canvas", "select", "datalist",
    "map", "link", "meta", "base", "title", "noembed", "noframes", "dialog", "area",
];

fn convert_children(el: &Html, ctx: &Context) -> Vec<Out> {
    // What each child became, and which side it floats to, if it does.
    let mut pieces: Vec<(Option<Float>, Vec<Out>)> = Vec::new();
    let mut number = el.attr("start").and_then(|n| n.trim().parse::<i64>().ok()).unwrap_or(1);
    for child in &el.children {
        let piece = match child {
            Node::Text(text) if ctx.pre => vec![Out::Text(text.clone())],
            Node::Text(text) => vec![Out::Text(collapse(text))],
            Node::Element(child) if child.name == "li" && el.name == "ol" => {
                if let Some(value) = child.attr("value").and_then(|n| n.trim().parse().ok()) {
                    number = value;
                }
                let css = ctx.styles.css(child, "li");
                number += 1;
                if css.display == Display::None {
                    continue;
                }
                css.apply(vec![list_item(child, &format!("{}.", number - 1), ctx)])
            }
            Node::Element(child) => convert(child, ctx),
        };
        let float = match child {
            Node::Element(child) => ctx.styles.float(child),
            Node::Text(_) => None,
        };
        pieces.push((float, piece));
    }
    float(pieces)
}

fn list_item(el: &Html, marker: &str, ctx: &Context) -> Out {
    let inner = Context { depth: ctx.depth + 2, ..ctx.clone() };
    let body = Out::element("div", Vec::new(), finish_block(convert_children(el, &inner)));
    let attrs = vec![("gap", Attr::Num(8.0)), ("margin", Attr::Num(4.0))];
    let unmarked = ctx.styles.get(el).is_some_and(|style| style.list_none) || ctx.styles.css(el, "li").display == Display::Inline;
    match unmarked {
        true => body,
        false => Out::element("row", attrs, vec![Out::element("span", Vec::new(), vec![Out::Text(marker.into())]), body]),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Float {
    Left,
    Right,
}

/// The children of an element, from `pieces`, what each child became and
/// how it floats. Netscheme has no floats, so from the first floated child
/// on, they're a row: what floats left, then the rest, then what floats right.
fn float(pieces: Vec<(Option<Float>, Vec<Out>)>) -> Vec<Out> {
    let Some(first) = pieces.iter().position(|(float, _)| float.is_some()) else {
        return pieces.into_iter().flat_map(|(_, piece)| piece).collect();
    };
    let mut pieces = pieces.into_iter();
    let mut out: Vec<Out> = pieces.by_ref().take(first).flat_map(|(_, piece)| piece).collect();
    let (mut left, mut rest, mut right) = (Vec::new(), Vec::new(), Vec::new());
    for (float, piece) in pieces {
        match float {
            Some(Float::Left) => left.extend(piece),
            Some(Float::Right) => right.extend(piece),
            None => rest.extend(piece),
        }
    }
    let rest = finish_block(rest);
    let mut columns = row_children(left, None);
    match rest.is_empty() {
        true => columns.push(Out::element("spacer", Vec::new(), Vec::new())),
        false => columns.push(Out::element("div", vec![("width", Attr::Code(":fill"))], rest)),
    }
    columns.extend(row_children(right, None));
    out.push(Out::element("row", vec![("gap", Attr::Num(8.0))], columns));
    out
}

/// The Netscheme for `el`: usually one element, but nothing for what's
/// left out, and its children alone for elements that only group them.
fn convert(el: &Html, ctx: &Context) -> Vec<Out> {
    let name = el.name.as_str();
    if SKIPPED.contains(&name) || el.attr("hidden").is_some() {
        return Vec::new();
    }
    let css = ctx.styles.css(el, name);
    if css.display == Display::None {
        return Vec::new();
    }
    let keep_wrapper = !css.is_empty();
    css.apply(convert_element(el, ctx, keep_wrapper))
}

/// `convert`, before the element's CSS is applied. Elements that only
/// group others are kept if `keep_wrapper`, to hold the style.
fn convert_element(el: &Html, ctx: &Context, keep_wrapper: bool) -> Vec<Out> {
    let name = el.name.as_str();
    let inner = Context { depth: ctx.depth + 1, pre: ctx.pre || name == "pre", styles: ctx.styles };
    let children = || convert_children(el, &inner);
    let block = |tag, attrs| vec![Out::element(tag, attrs, finish_block(children()))];
    let inline = |tag, attrs| vec![inline_element(tag, attrs, children())];
    let muted = || Attr::Code(r##"(light-dark "#666" "#999")"##);
    match name {
        "br" => vec![Out::Break],
        "wbr" => Vec::new(),
        // Closed, as it starts out, it shows only its summary.
        "details" if el.attr("open").is_none() => {
            let summary = el.children.iter().find_map(|child| match child {
                Node::Element(summary) if summary.name == "summary" => Some(summary),
                _ => None,
            });
            summary.map_or_else(
                || vec![Out::element("div", Vec::new(), vec![Out::Text("Details".into())])],
                |summary| convert(summary, &inner),
            )
        }
        "h1" | "h2" | "h3" => block(
            match name {
                "h1" => "h1",
                "h2" => "h2",
                _ => "h3",
            },
            Vec::new(),
        ),
        "h4" => block("p", vec![("bold", Attr::Bool(true)), ("margin", Attr::Num(10.0))]),
        "h5" => block("p", vec![("bold", Attr::Bool(true)), ("size", Attr::Num(14.0)), ("margin", Attr::Num(10.0))]),
        "h6" => block("p", vec![("bold", Attr::Bool(true)), ("size", Attr::Num(13.0)), ("margin", Attr::Num(10.0))]),
        "p" => block("p", Vec::new()),
        "pre" | "listing" | "plaintext" | "xmp" => {
            // A line break straight after the start tag isn't shown.
            let mut children = children();
            if let Some(Out::Text(text)) = children.first_mut() {
                *text = text.strip_prefix('\n').unwrap_or(text).into();
            }
            vec![Out::element("pre", Vec::new(), children)]
        }
        "blockquote" => block("div", vec![("padding-left", Attr::Num(24.0)), ("margin", Attr::Num(12.0))]),
        "ul" | "ol" | "menu" | "dir" => block("div", vec![("padding-left", Attr::Num(16.0)), ("margin", Attr::Num(12.0))]),
        "li" => vec![list_item(el, "•", ctx)],
        "dl" => block("div", vec![("margin", Attr::Num(12.0))]),
        "dt" => block("p", vec![("bold", Attr::Bool(true)), ("margin", Attr::Num(2.0))]),
        "dd" => block("div", vec![("padding-left", Attr::Num(24.0)), ("margin", Attr::Num(8.0))]),
        "hr" => {
            let attrs = vec![
                ("border", Attr::Code(r##"(light-dark "#ccc" "#555")"##)),
                ("margin-top", Attr::Num(8.0)),
                ("margin", Attr::Num(12.0)),
            ];
            vec![Out::element("div", attrs, Vec::new())]
        }
        "table" => block("div", vec![("margin", Attr::Num(12.0))]),
        "thead" | "tbody" | "tfoot" => children(),
        "caption" => block("p", vec![("bold", Attr::Bool(true))]),
        "tr" => {
            let mut cells: Vec<Out> = children().into_iter().filter(|cell| matches!(cell, Out::Element { .. })).collect();
            // Columns are as wide as their content in other browsers, which
            // rows can't do across rows, but a row's short cells can be.
            let last = cells.len().saturating_sub(1);
            for cell in &mut cells[..last] {
                let short = text_length(cell) <= SHORT_CELL;
                if let Out::Element { attrs, .. } = cell
                    && short
                    && !attrs.iter().any(|(name, _)| *name == "width")
                {
                    attrs.push(("width", Attr::Str("fit".into())));
                }
            }
            vec![Out::element("row", vec![("gap", Attr::Num(12.0)), ("margin", Attr::Num(4.0))], cells)]
        }
        "td" => block("div", Vec::new()),
        "th" => block("div", vec![("bold", Attr::Bool(true))]),
        "a" => match el.attr("href").filter(|href| !href.trim_start().to_ascii_lowercase().starts_with("javascript:")) {
            Some(href) => {
                let children = children();
                let attrs = vec![("href", Attr::Str(href.trim().into()))];
                if children.iter().any(Out::is_block) {
                    vec![Out::element("a", attrs, finish_block(children))]
                } else {
                    vec![inline_element("a", attrs, children)]
                }
            }
            None => children(),
        },
        "b" | "strong" => inline("b", Vec::new()),
        "code" | "kbd" | "samp" | "tt" => inline("code", Vec::new()),
        "u" | "ins" => inline("span", vec![("underline", Attr::Bool(true))]),
        "small" | "sub" | "sup" => inline("span", vec![("size", Attr::Num(13.0))]),
        "mark" => inline("span", vec![("background", Attr::Code(r##"(light-dark "#fff3a3" "#5c5320")"##))]),
        "s" | "del" | "strike" => inline("span", vec![("color", muted())]),
        "q" => {
            let mut quoted = vec![Out::Text("“".into())];
            quoted.extend(children());
            quoted.push(Out::Text("”".into()));
            vec![inline_element("span", Vec::new(), quoted)]
        }
        "img" => image(el),
        "button" => vec![Out::element("button", Vec::new(), vec![Out::Text(collapse(&text_of(el)).trim().into())])],
        "input" => match el.attr("type").unwrap_or("text").to_ascii_lowercase().as_str() {
            "submit" | "button" | "reset" => {
                let label = el.attr("value").unwrap_or(if el.attr("type") == Some("reset") { "Reset" } else { "Submit" });
                vec![Out::element("button", Vec::new(), vec![Out::Text(label.into())])]
            }
            "text" | "search" | "email" | "url" | "tel" | "password" | "number" => {
                let mut attrs = vec![("value", Attr::Str(el.attr("value").unwrap_or("").into()))];
                if let Some(placeholder) = el.attr("placeholder") {
                    attrs.push(("placeholder", Attr::Str(placeholder.into())));
                }
                vec![Out::element("input", attrs, Vec::new())]
            }
            _ => Vec::new(),
        },
        "textarea" => {
            let mut attrs = vec![("value", Attr::Str(text_of(el).trim_start_matches('\n').into()))];
            if let Some(rows) = el.attr("rows").and_then(|n| n.trim().parse().ok()) {
                attrs.push(("rows", Attr::Num(rows)));
            }
            vec![Out::element("textarea", attrs, Vec::new())]
        }
        "div" | "section" | "article" | "main" | "header" | "footer" | "nav" | "aside" | "figure" | "figcaption" | "address" | "form"
        | "fieldset" | "legend" | "details" | "summary" | "center" | "hgroup" | "search" | "body" | "html" => {
            if inner.depth > MAX_DEPTH {
                return finish_block(children());
            }
            let mut out = finish_block(children());
            // A wrapper around a single block adds nothing in Netscheme.
            if out.len() == 1 && out[0].is_block() && !keep_wrapper {
                return out;
            }
            vec![Out::element("div", Vec::new(), std::mem::take(&mut out))]
        }
        // `span`, `em`, `label`, unknown elements and the rest only group
        // their children, as far as Netscheme can show.
        _ => children(),
    }
}

// Styling

/// How CSS lays an element out, as far as the translation follows it.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Display {
    /// As its tag is laid out.
    Normal,
    None,
    Block,
    /// Laid out in a line of text, from `display: inline` or `inline-block`.
    Inline,
    /// A row of its children, from `display: flex`.
    Row,
}

/// The Netscheme attributes for an element's CSS.
struct Css {
    attrs: Vec<(&'static str, Attr)>,
    display: Display,
    /// For a row: how its children are spread along it, from `justify-content`.
    justify: Option<String>,
    /// From `text-align`, for a block of text.
    text_align: Option<&'static str>,
    /// Whether it has `flex-grow`, so it fills the row it's in.
    grows: bool,
}

impl Styles {
    /// The attributes for the CSS of `el`, which becomes a Netscheme `tag`.
    fn css(&self, el: &Html, tag: &str) -> Css {
        let mut css = Css { attrs: Vec::new(), display: Display::Normal, justify: None, text_align: None, grows: false };
        let Some(style) = self.get(el) else { return css };
        let decls = &style.decls;
        let get = |name: &str| decls.get(name).map(|v| v.to_ascii_lowercase());
        let size = style.font_size;
        let len = |name: &str| decls.get(name).and_then(|v| css::length(v, size));
        let attrs = &mut css.attrs;

        css.display = match get("display").as_deref() {
            Some("none") => Display::None,
            Some("flex" | "inline-flex") if get("flex-direction").is_none_or(|d| d.starts_with("row")) => Display::Row,
            Some("block" | "flex" | "grid" | "list-item" | "table" | "flow-root") => Display::Block,
            Some("inline" | "inline-block" | "inline-table") => Display::Inline,
            _ => Display::Normal,
        };
        // Hidden, or only there for screen readers: clipped down to a pixel, or moved far off the page.
        let tiny = |name: &str| len(name).is_some_and(|v| v <= 1.0);
        let clipped =
            get("overflow").is_some_and(|o| o.contains("hidden")) || decls.contains_key("clip") || decls.contains_key("clip-path");
        let far_off = |name: &str| len(name).is_some_and(|v| v <= -999.0);
        let positioned = get("position").is_some_and(|p| p == "absolute" || p == "fixed");
        if get("visibility").as_deref() == Some("hidden")
            || clipped && (tiny("width") || tiny("height"))
            || positioned && (far_off("left") || far_off("top"))
            || far_off("text-indent")
        {
            css.display = Display::None;
        }

        if decls.contains_key("color")
            && let Some(color) = style.color
        {
            attrs.push(("color", Attr::Str(css::hex(color))));
        }
        if let Some(background) = decls.get("background-color").and_then(|c| css::color(c)) {
            // Clearing the background matters only where Netscheme gives one.
            if background[3] > 0 || matches!(tag, "code" | "pre" | "button" | "input" | "textarea" | "mark" | "kbd" | "samp" | "tt") {
                attrs.push(("background", Attr::Str(css::hex(background))));
            }
        }
        let heading = matches!(tag, "h1" | "h2" | "h3" | "h4" | "h5" | "h6");
        if decls.contains_key("font-size") || heading && !self.rules_empty {
            attrs.push(("size", Attr::Num(round(size))));
        }
        match get("font-weight").as_deref() {
            Some("bold" | "bolder") => attrs.push(("weight", Attr::Num(700.0))),
            Some("normal") => attrs.push(("weight", Attr::Num(400.0))),
            Some("lighter") => attrs.push(("weight", Attr::Num(300.0))),
            Some(n) => {
                if let Ok(n) = n.parse::<f64>() {
                    attrs.push(("weight", Attr::Num(n.clamp(100.0, 900.0))));
                }
            }
            None => {}
        }
        if let Some(family) = get("font-family") {
            let first = family.split(',').next().unwrap_or("").trim().trim_matches(['"', '\'']).to_string();
            if ["mono", "courier", "consolas", "menlo", "monaco", "code"].iter().any(|m| first.contains(m)) {
                attrs.push(("mono", Attr::Bool(true)));
            } else if first == "open sans" {
                attrs.push(("font", Attr::Str("Open Sans".into())));
            } else if matches!(tag, "code" | "pre") && first != "inherit" {
                attrs.push(("mono", Attr::Bool(false)));
            }
        }
        if let Some(height) = get("line-height") {
            let ratio = match height.parse::<f32>() {
                Ok(n) => Some(n),
                Err(_) => css::length_of(&height, size, Some(size)).map(|px| px / size.max(1.0)),
            };
            if let Some(ratio) = ratio.filter(|r| *r > 0.0) {
                attrs.push(("line-height", Attr::Num(round(ratio * 100.0) / 100.0)));
            }
        }
        if let Some(line) = get("text-decoration-line") {
            if line.contains("underline") {
                attrs.push(("underline", Attr::Bool(true)));
            } else if line.contains("none") {
                attrs.push(("underline", Attr::Bool(false)));
            }
        }
        for side in ["top", "right", "bottom", "left"] {
            if let Some(padding) = len(&format!("padding-{side}")) {
                attrs.push((padding_name(side), Attr::Num(round(padding.max(0.0)))));
            }
        }
        if let Some(margin) = len("margin-top") {
            attrs.push(("margin-top", Attr::Num(round(margin.max(0.0)))));
        }
        if let Some(margin) = len("margin-bottom") {
            attrs.push(("margin", Attr::Num(round(margin.max(0.0)))));
        }
        let width = match get("width").as_deref() {
            Some("fit-content" | "max-content" | "min-content") => Some(Attr::Str("fit".into())),
            Some(_) => len("width").filter(|w| *w > 0.0).map(|w| Attr::Num(round(w))),
            None => None,
        };
        let has_width = width.is_some();
        if let Some(width) = width {
            attrs.push(("width", width));
        }
        let max_width = len("max-width").filter(|w| *w > 0.0);
        if let Some(max) = max_width.filter(|_| tag != "img") {
            attrs.push(("max-width", Attr::Num(round(max))));
        }
        if tag == "img"
            && let Some(height) = len("height").filter(|h| *h > 0.0)
        {
            attrs.push(("height", Attr::Num(round(height))));
        }
        let auto = |side: &str| get(&format!("margin-{side}")).as_deref() == Some("auto");
        if has_width && auto("left") {
            attrs.push(("align", Attr::Str(if auto("right") { "center" } else { "right" }.into())));
        }
        if let Some((color, width)) = border(style) {
            attrs.push(("border", Attr::Str(css::hex(color))));
            attrs.push(("border-width", Attr::Num(round(width))));
        }
        let radius = decls.get("border-top-left-radius").or(decls.get("border-radius"));
        if let Some(radius) = radius.and_then(|r| css::split_words(r).into_iter().next()) {
            match radius.ends_with('%') {
                true => attrs.push(("radius", Attr::Num(999.0))),
                false => {
                    if let Some(r) = css::length(&radius, size).filter(|r| *r > 0.0) {
                        attrs.push(("radius", Attr::Num(round(r))));
                    }
                }
            }
        }
        if css.display == Display::Row {
            let gap = len("column-gap").unwrap_or(0.0);
            attrs.push(("gap", Attr::Num(round(gap.max(0.0)))));
            match get("align-items").as_deref() {
                Some("center") => attrs.push(("align", Attr::Str("middle".into()))),
                Some("flex-end" | "end") => attrs.push(("align", Attr::Str("bottom".into()))),
                _ => {}
            }
            css.justify = get("justify-content");
        }
        css.grows = get("flex-grow").and_then(|g| g.parse::<f32>().ok()).is_some_and(|g| g > 0.0);
        if css.grows {
            attrs.push(("width", Attr::Code(":fill")));
        }
        // Not in table cells and list items, which are columns of rows,
        // where being as wide as their content would squeeze the others.
        css.text_align = match style.text_align.as_deref() {
            _ if matches!(tag, "td" | "th" | "li") => None,
            Some("center" | "-webkit-center") => Some("center"),
            Some("right" | "end" | "-webkit-right") => Some("right"),
            _ => None,
        };
        css
    }
}

fn padding_name(side: &str) -> &'static str {
    match side {
        "top" => "padding-top",
        "right" => "padding-right",
        "bottom" => "padding-bottom",
        _ => "padding-left",
    }
}

/// The colour and width of a border on every side of the box, as
/// Netscheme can only draw those.
fn border(style: &Styled) -> Option<(Rgba, f32)> {
    let sides: Vec<(Rgba, f32)> = ["top", "right", "bottom", "left"]
        .iter()
        .map(|side| {
            let get = |what: &str| style.decls.get(&format!("border-{side}-{what}")).map(|v| v.to_ascii_lowercase());
            let shown = get("style").is_some_and(|s| !matches!(&*s, "none" | "hidden"));
            let width = match get("width").as_deref() {
                Some("thin") => 1.0,
                Some("thick") => 5.0,
                None | Some("medium") => 3.0,
                Some(w) => css::length(w, style.font_size)?,
            };
            let color = get("color").and_then(|c| css::color(&c)).or(style.color).unwrap_or([0, 0, 0, 255]);
            (shown && width > 0.0).then_some((color, width))
        })
        .collect::<Option<_>>()?;
    let width = sides.iter().map(|(_, w)| *w).fold(f32::MAX, f32::min);
    Some((sides[0].0, width))
}

impl Css {
    fn is_empty(&self) -> bool {
        self.attrs.is_empty() && self.display == Display::Normal && self.text_align.is_none()
    }

    /// `out`, which an element was translated to, with this CSS applied.
    /// An element that only grouped its children gets a wrapper to hold it.
    fn apply(self, mut out: Vec<Out>) -> Vec<Out> {
        if self.is_empty() {
            return out;
        }
        let wants_block = matches!(self.display, Display::Block | Display::Row);
        let mut el = match out.len() {
            1 if matches!(out[0], Out::Element { .. }) => out.pop().unwrap(),
            _ if wants_block || out.iter().any(Out::is_block) => Out::element("div", Vec::new(), finish_block(out)),
            _ if self.attrs.is_empty() => return out,
            _ => inline_element("span", Vec::new(), out),
        };
        if wants_block && !el.is_block() {
            el = match el {
                // A link laid out as a block holds one, so it stays a link.
                Out::Element { tag: "a", attrs, children } => {
                    let block = self.apply(vec![Out::element("div", Vec::new(), children)]);
                    return vec![Out::Element { tag: "a", attrs, children: block }];
                }
                el => Out::element("div", Vec::new(), vec![el]),
            };
        }
        let Out::Element { tag, attrs, children } = &mut el else { return vec![el] };
        // A block laid out inline holds only text, so it can be a span.
        if self.display == Display::Inline
            && matches!(*tag, "div" | "p" | "h1" | "h2" | "h3" | "row")
            && !children.iter().any(Out::is_block)
        {
            if matches!(*tag, "h1" | "h2" | "h3") {
                attrs.push(("bold", Attr::Bool(true)));
            }
            *tag = "span";
        }
        if self.display == Display::Row && matches!(*tag, "div" | "p") {
            *tag = "row";
            *children = row_children(std::mem::take(children), self.justify.as_deref());
        }
        for (name, value) in self.attrs {
            match attrs.iter_mut().find(|(existing, _)| *existing == name) {
                Some(existing) => existing.1 = value,
                None => attrs.push((name, value)),
            }
        }
        // A block of text is centred by fitting it to its text and centring that.
        let has = |attrs: &Vec<(&str, Attr)>, name: &str| attrs.iter().any(|(existing, _)| *existing == name);
        let text_block = matches!(*tag, "p" | "div" | "h1" | "h2" | "h3") && !children.iter().any(Out::is_block);
        if let Some(align) =
            self.text_align.filter(|_| text_block && !has(attrs, "width") && !has(attrs, "background") && !has(attrs, "border"))
        {
            attrs.push(("width", Attr::Str("fit".into())));
            attrs.push(("align", Attr::Str(align.into())));
        }
        vec![el]
    }
}

/// The children of a flex container, as the columns of a row: text in
/// spans, blocks as wide as their content unless they grow to fill it,
/// and spacers to spread them out as `justify` says.
fn row_children(children: Vec<Out>, justify: Option<&str>) -> Vec<Out> {
    let mut columns: Vec<Out> = Vec::new();
    for child in children {
        match child {
            Out::Text(text) if text.trim().is_empty() => {}
            Out::Text(_) | Out::Break => columns.push(Out::element("span", Vec::new(), vec![child])),
            mut child @ Out::Element { .. } => {
                let block = child.is_block();
                if let Out::Element { attrs, .. } = &mut child
                    && block
                    && !attrs.iter().any(|(name, _)| *name == "width")
                {
                    attrs.push(("width", Attr::Str("fit".into())));
                }
                columns.push(child);
            }
        }
    }
    let spacer = || Out::element("spacer", Vec::new(), Vec::new());
    let grows = columns
        .iter()
        .any(|c| matches!(c, Out::Element { attrs, .. } if attrs.iter().any(|(n, v)| *n == "width" && matches!(v, Attr::Code(":fill")))));
    if grows || columns.len() < 2 && !matches!(justify, Some("center" | "flex-end" | "end" | "right")) {
        return columns;
    }
    match justify {
        Some("space-between" | "space-around" | "space-evenly") => {
            let around = justify != Some("space-between");
            let n = columns.len();
            let mut spread = Vec::new();
            if around {
                spread.push(spacer());
            }
            for (i, column) in columns.into_iter().enumerate() {
                spread.push(column);
                if i + 1 < n || around {
                    spread.push(spacer());
                }
            }
            spread
        }
        Some("center") => std::iter::once(spacer()).chain(columns).chain(std::iter::once(spacer())).collect(),
        Some("flex-end" | "end" | "right") => std::iter::once(spacer()).chain(columns).collect(),
        _ => columns,
    }
}

/// Table cells with no more text than this are as wide as their content.
const SHORT_CELL: usize = 24;

/// How many characters of text `out` holds.
fn text_length(out: &Out) -> usize {
    match out {
        Out::Text(text) => text.trim().chars().count(),
        Out::Element { children, .. } => children.iter().map(text_length).sum(),
        Out::Break => 0,
    }
}

/// An inline element, or a block styled the same if it holds blocks.
fn inline_element(tag: &'static str, attrs: Vec<(&'static str, Attr)>, children: Vec<Out>) -> Out {
    if children.iter().any(Out::is_block) {
        let attrs = match tag {
            "b" => vec![("bold", Attr::Bool(true))],
            "code" => vec![("mono", Attr::Bool(true))],
            _ => attrs,
        };
        return Out::element(if tag == "a" { "a" } else { "div" }, attrs, finish_block(children));
    }
    let children = children.into_iter().map(|child| if let Out::Break = child { Out::Text(" ".into()) } else { child }).collect();
    Out::element(tag, attrs, children)
}

fn image(el: &Html) -> Vec<Out> {
    let Some(src) = el.attr("src").map(str::trim).filter(|src| !src.is_empty()) else { return Vec::new() };
    let alt = collapse(el.attr("alt").unwrap_or("")).trim().to_string();
    // Netscheme only reads PNGs, so other images show as their description.
    let file = src.split(['?', '#']).next().unwrap_or(src).to_ascii_lowercase();
    if !file.ends_with(".png") {
        if alt.is_empty() {
            return Vec::new();
        }
        let muted = Attr::Code(r##"(light-dark "#666" "#999")"##);
        return vec![Out::element("span", vec![("color", muted)], vec![Out::Text(format!("[{alt}]"))])];
    }
    let mut attrs = vec![("src", Attr::Str(src.into()))];
    for side in ["width", "height"] {
        if let Some(n) = el.attr(side).and_then(|n| n.trim().trim_end_matches("px").parse::<f64>().ok()).filter(|n| *n > 0.0) {
            attrs.push((side, Attr::Num(n)));
        }
    }
    if !alt.is_empty() {
        attrs.push(("alt", Attr::Str(alt)));
    }
    vec![Out::element("img", attrs, Vec::new())]
}

/// Tidies the children of a block: trims the whitespace that HTML ignores
/// around blocks, and puts each line ended by a `br` in a block of its own.
fn finish_block(children: Vec<Out>) -> Vec<Out> {
    let has_break = children.iter().any(|child| matches!(child, Out::Break));
    let mut out: Vec<Out> = Vec::new();
    let mut line: Vec<Out> = Vec::new();
    let end_line = |line: &mut Vec<Out>, out: &mut Vec<Out>, force: bool| {
        let run = trim_run(std::mem::take(line));
        if run.is_empty() && !force {
            return;
        }
        if has_break {
            // An empty line still takes up room, as it does in HTML.
            let run = if run.is_empty() { vec![Out::Text("\u{a0}".into())] } else { run };
            out.push(Out::element("div", Vec::new(), run));
        } else {
            out.extend(run);
        }
    };
    let mut after_break = false;
    for child in children {
        match child {
            Out::Break => {
                end_line(&mut line, &mut out, after_break);
                after_break = true;
            }
            child if child.is_block() => {
                end_line(&mut line, &mut out, false);
                out.push(child);
                after_break = false;
            }
            child => {
                if !matches!(&child, Out::Text(t) if t.trim().is_empty()) {
                    after_break = false;
                }
                line.push(child);
            }
        }
    }
    end_line(&mut line, &mut out, false);
    out
}

/// Trims the whitespace at the start and end of a run of inline content.
fn trim_run(mut run: Vec<Out>) -> Vec<Out> {
    while let Some(Out::Text(text)) = run.first_mut() {
        let trimmed = text.trim_start_matches(|c: char| c.is_whitespace() && c != '\u{a0}');
        if trimmed.is_empty() {
            run.remove(0);
        } else {
            *text = trimmed.into();
            break;
        }
    }
    while let Some(Out::Text(text)) = run.last_mut() {
        let trimmed = text.trim_end_matches(|c: char| c.is_whitespace() && c != '\u{a0}');
        if trimmed.is_empty() {
            run.pop();
        } else {
            *text = trimmed.into();
            break;
        }
    }
    run
}

// Printing

/// Prints `out` as Netscheme code, with blocks on lines of their own.
fn write(out: &Out, indent: usize, s: &mut String) {
    match out {
        Out::Text(text) => write_string(text, s),
        Out::Break => s.push_str("\" \""),
        Out::Element { tag, attrs, children } => {
            s.push('(');
            s.push_str(tag);
            for (name, value) in attrs {
                s.push_str(" :");
                s.push_str(name);
                s.push(' ');
                match value {
                    Attr::Str(text) => write_string(text, s),
                    Attr::Num(n) => s.push_str(&n.to_string()),
                    Attr::Bool(b) => s.push_str(if *b { "true" } else { "false" }),
                    Attr::Code(code) => s.push_str(code),
                }
            }
            let on_lines = children.iter().any(Out::is_block);
            let mut inline_run = false;
            for child in children {
                if on_lines && (child.is_block() || !inline_run) {
                    s.push('\n');
                    s.push_str(&" ".repeat(indent + 2));
                } else {
                    s.push(' ');
                }
                inline_run = !child.is_block();
                write(child, indent + 2, s);
            }
            s.push(')');
        }
    }
}

fn write_string(text: &str, s: &mut String) {
    s.push('"');
    for c in text.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\t' => s.push_str("\\t"),
            c => s.push(c),
        }
    }
    s.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page `html` translates to, without the comment and banner.
    fn body(html: &str) -> String {
        let lisp = to_lisp(html);
        let page = lisp.split_once("Lisp.\"))").expect("a banner").1;
        page.split_whitespace().collect::<Vec<_>>().join(" ").strip_suffix(")").expect("a page").trim().to_string()
    }

    #[test]
    fn elements_become_their_nearest_netscheme_elements() {
        let html = "<!DOCTYPE html><html><head><title>Hi &amp; bye</title><style>p{}</style></head>\
                    <body><h1>Title</h1><p>Some <strong>bold</strong> and <em>quiet</em> text.<script>x()</script></body></html>";
        assert!(to_lisp(html).contains(r#"(page :title "Hi & bye""#));
        assert_eq!(body(html), r#"(h1 "Title") (p "Some " (b "bold") " and quiet text.")"#);
    }

    #[test]
    fn unclosed_paragraphs_and_list_items_close_themselves() {
        let html = "<p>One<p>Two<ul><li>a<li>b</ul><ol start=3><li>c</ol>";
        assert_eq!(
            body(html),
            r#"(p "One") (p "Two") (div :padding-left 16 :margin 12 (row :gap 8 :margin 4 (span "•") (div "a")) (row :gap 8 :margin 4 (span "•") (div "b"))) (div :padding-left 16 :margin 12 (row :gap 8 :margin 4 (span "3.") (div "c")))"#
        );
    }

    #[test]
    fn table_rows_close_themselves() {
        assert_eq!(
            body("<table><tr><td>a<td>b<tr><td>c</table>"),
            r#"(div :margin 12 (row :gap 12 :margin 4 (div :width "fit" "a") (div "b")) (row :gap 12 :margin 4 (div "c")))"#
        );
    }

    #[test]
    fn line_breaks_split_a_block_into_lines() {
        assert_eq!(body("<p>one<br>two</p>"), r#"(p (div "one") (div "two"))"#);
    }

    #[test]
    fn links_keep_their_href_and_scripts_are_dropped() {
        assert_eq!(body(r#"<a href="b.html">B</a> <a href="javascript:go()">C</a>"#), r#"(a :href "b.html" "B") " C""#);
    }

    #[test]
    fn preformatted_text_keeps_its_whitespace() {
        assert!(to_lisp("<pre>\na\n  b &lt;c&gt;</pre>").contains(r#"(pre "a\n  b <c>")"#));
    }

    #[test]
    fn the_translation_runs_as_a_page() {
        let html = r#"<html><body><div><p>"Quoted" \ text</p><table><tr><th>A<td>B</table><hr><img src="x.jpg" alt="A cat"></div>"#;
        let page = crate::lisp::run(&to_lisp(html), &crate::loader::test_context());
        assert!(matches!(page, Ok(crate::lisp::Value::Element(el)) if &*el.tag == "page"));
    }

    #[test]
    fn closed_details_show_only_their_summary() {
        assert_eq!(body("<details><summary>More</summary><p>Hidden</p></details>"), r#"(div "More")"#);
        assert!(body("<details open><summary>More</summary><p>Shown</p></details>").contains("Shown"));
    }

    /// The page `html` translates to, without the comment and banner, with
    /// `stylesheets` to link to by name.
    fn styled(html: &str, stylesheets: &[(&str, &str)]) -> String {
        let lisp = to_lisp_with(html, "page.html", &mut |base, href| {
            assert_eq!(base, "page.html");
            stylesheets.iter().find(|(name, _)| *name == href).map(|(_, text)| text.to_string())
        });
        let page = lisp.split_once("Lisp.\"))").expect("a banner").1;
        page.split_whitespace().collect::<Vec<_>>().join(" ").strip_suffix(")").expect("a page").trim().to_string()
    }

    #[test]
    fn stylesheets_style_the_elements_they_select() {
        let html = r#"<link rel="stylesheet" href="site.css"><style>p.note { color: var(--accent); padding: 4px 8px }</style>
                      <p class="note">A</p><p style="color: rgb(0, 0, 255) !important">B</p><p class="gone">C</p>"#;
        let css = ":root { --accent: #c00 } p { margin: 0; color: green !important } .gone { display: none }";
        assert_eq!(
            styled(html, &[("site.css", css)]),
            r##"(p :color "#008000" :padding-top 4 :padding-right 8 :padding-bottom 4 :padding-left 8 :margin-top 0 :margin 0 "A") (p :color "#0000ff" :margin-top 0 :margin 0 "B")"##
        );
    }

    #[test]
    fn flex_containers_become_rows() {
        let html = r#"<style>nav { display: flex; gap: 10px; justify-content: space-between; align-items: center } .grow { flex: 1 }</style>
                      <nav><a href="/">Home</a> <div class="grow">Middle</div> <div>End</div></nav>"#;
        assert_eq!(
            styled(html, &[]),
            r#"(row :gap 10 :align "middle" (a :href "/" "Home") (div :width :fill "Middle") (div :width "fit" "End"))"#
        );
    }

    #[test]
    fn floats_sit_beside_what_follows_them() {
        let html = r#"<style>.side { float: left; width: 40px } li { display: inline }</style>
                      <div><p>Above</p><div class="side">1</div><p>Text</p></div><ul><li>a</li><li>b</li></ul>"#;
        let page = styled(html, &[]);
        assert!(page.starts_with(r#"(div (p "Above") (row :gap 8 (div :width 40 "1") (div :width :fill (p "Text"))))"#), "{page}");
        assert!(page.contains(r#"(span "a")"#), "inline list items lose their markers: {page}");
    }

    #[test]
    fn styled_pages_are_drawn_in_their_own_colours() {
        let page =
            to_lisp_with("<style>body { background: #222; color: #eee; margin: 0 }</style><body><p>x</p></body>", "", &mut |_, _| None);
        assert!(page.contains(r##"(page :background "#222222" :color "#eeeeee" :padding-top 0"##), "{page}");
        assert!(!to_lisp("<p>x</p>").contains(":background \"#ffffff\""), "pages without stylesheets keep the browser's");
    }
}

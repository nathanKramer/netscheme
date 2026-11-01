//! Reads HTML pages by translating them into Netscheme programs. Netscheme
//! has no HTML, CSS or JavaScript, so an HTML page is parsed into a tree,
//! each element is turned into the nearest Netscheme element, and the
//! result is printed as the source of a page. Scripts and stylesheets are
//! left out, and so is anything Netscheme can't draw, like video.
//!
//! The parser is forgiving, as browsers' are: it closes elements that HTML
//! lets authors leave open, like `p` and `li`, and ignores end tags that
//! match nothing.

/// Whether the file at `location`, holding `text`, is an HTML page.
pub fn is_html(location: &str, text: &str) -> bool {
    let file = location.split_once('?').map_or(location, |(file, _)| file).to_ascii_lowercase();
    file.ends_with(".html") || file.ends_with(".htm") || text.trim_start_matches('\u{feff}').trim_start().starts_with('<')
}

/// The Netscheme program for the HTML page `html`.
pub fn to_lisp(html: &str) -> String {
    let root = parse(&html.replace("\r\n", "\n").replace('\r', "\n"));
    let title = find(&root, "title").map(|title| collapse(&text_of(title)).trim().to_string());
    let body = find(&root, "body").unwrap_or(&root);
    let mut children = vec![banner()];
    children.extend(convert_children(body, &Context::default()));

    let mut page = Out::element("page", Vec::new(), finish_block(children));
    if let (Out::Element { attrs, .. }, Some(title)) = (&mut page, title.filter(|t| !t.is_empty())) {
        attrs.push(("title", Attr::Str(title)));
    }
    let mut out = String::from(
        "; Translated from HTML by Netscheme, which doesn't support HTML, CSS or\n\
         ; JavaScript. Stylesheets and scripts were left out.\n\n",
    );
    write(&page, 0, &mut out);
    out.push('\n');
    out
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

#[derive(Clone, Default)]
struct Context {
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
    let mut out = Vec::new();
    let mut number = el.attr("start").and_then(|n| n.trim().parse::<i64>().ok()).unwrap_or(1);
    for child in &el.children {
        match child {
            Node::Text(text) if ctx.pre => out.push(Out::Text(text.clone())),
            Node::Text(text) => out.push(Out::Text(collapse(text))),
            Node::Element(child) if child.name == "li" && el.name == "ol" => {
                if let Some(value) = child.attr("value").and_then(|n| n.trim().parse().ok()) {
                    number = value;
                }
                out.push(list_item(child, &format!("{number}."), ctx));
                number += 1;
            }
            Node::Element(child) => out.extend(convert(child, ctx)),
        }
    }
    out
}

fn list_item(el: &Html, marker: &str, ctx: &Context) -> Out {
    let inner = Context { depth: ctx.depth + 2, ..ctx.clone() };
    let body = Out::element("div", Vec::new(), finish_block(convert_children(el, &inner)));
    let attrs = vec![("gap", Attr::Num(8.0)), ("margin", Attr::Num(4.0))];
    Out::element("row", attrs, vec![Out::element("span", Vec::new(), vec![Out::Text(marker.into())]), body])
}

/// The Netscheme for `el`: usually one element, but nothing for what's
/// left out, and its children alone for elements that only group them.
fn convert(el: &Html, ctx: &Context) -> Vec<Out> {
    let name = el.name.as_str();
    if SKIPPED.contains(&name) || el.attr("hidden").is_some() {
        return Vec::new();
    }
    let inner = Context { depth: ctx.depth + 1, pre: ctx.pre || name == "pre" };
    let children = || convert_children(el, &inner);
    let block = |tag, attrs| vec![Out::element(tag, attrs, finish_block(children()))];
    let inline = |tag, attrs| vec![inline_element(tag, attrs, children())];
    let muted = || Attr::Code(r##"(light-dark "#666" "#999")"##);
    match name {
        "br" => vec![Out::Break],
        "wbr" => Vec::new(),
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
            let cells = children().into_iter().filter(|cell| matches!(cell, Out::Element { .. })).collect();
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
            if out.len() == 1 && out[0].is_block() {
                return out;
            }
            vec![Out::element("div", Vec::new(), std::mem::take(&mut out))]
        }
        // `span`, `em`, `label`, unknown elements and the rest only group
        // their children, as far as Netscheme can show.
        _ => children(),
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
            r#"(div :margin 12 (row :gap 12 :margin 4 (div "a") (div "b")) (row :gap 12 :margin 4 (div "c")))"#
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
}

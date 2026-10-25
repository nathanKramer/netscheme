//! The document tree that a page program evaluates to.

use std::{fmt, rc::Rc};

use crate::lisp::{Value, check_length, write_quoted};

/// Element constructors available to pages as ordinary functions.
/// `frame` is defined separately, as it loads a page when called.
pub const TAGS: &[&str] = &["page", "div", "row", "h1", "h2", "h3", "p", "pre", "span", "b", "code", "a", "button", "input", "img", "spacer", "canvas", "rect", "circle", "line", "polyline", "polygon", "text"];

/// Tags laid out within a line of text rather than as their own block.
pub fn is_inline(tag: &str) -> bool {
    matches!(tag, "span" | "b" | "code" | "a" | "button" | "input" | "img" | "spacer" | "canvas")
}

/// Elements nested deeper than this are refused, so that laying out or
/// dropping a document can't recurse without bound.
pub const MAX_NESTING: u32 = 512;

pub struct Element {
    /// How deeply elements are nested here, counting this one.
    pub depth: u32,
    pub tag: Rc<str>,
    pub attrs: Vec<(Rc<str>, Value)>,
    pub children: Vec<Node>,
}

pub enum Node {
    Element(Rc<Element>),
    Text(Rc<str>),
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&Value> {
        self.attrs.iter().rev().find(|(k, _)| &**k == name).map(|(_, v)| v)
    }

    /// Replaces the children, keeping `depth` up to date.
    pub fn set_children(&mut self, children: Vec<Node>) -> Result<(), String> {
        self.children = children;
        self.depth = depth_of(&self.children);
        if self.depth > MAX_NESTING {
            return Err(format!("({} ...): elements are nested more than {MAX_NESTING} deep", self.tag));
        }
        Ok(())
    }
}

/// The depth of an element with these children. Each child's depth is
/// already known, so this only looks one level down.
fn depth_of(children: &[Node]) -> u32 {
    let deepest_child = children.iter().filter_map(|child| match child {
        Node::Element(el) => Some(el.depth),
        Node::Text(_) => None,
    });
    1 + deepest_child.max().unwrap_or(0)
}

/// Prints the element as the code that builds it.
impl fmt::Display for Element {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}", self.tag)?;
        for (name, value) in &self.attrs {
            write!(f, " :{name} {value:#}")?;
        }
        for child in &self.children {
            match child {
                Node::Text(text) => {
                    write!(f, " ")?;
                    write_quoted(f, text)?;
                }
                Node::Element(el) => write!(f, " {el}")?,
            }
        }
        write!(f, ")")
    }
}

/// `(tag :attr value ... child ...)`: keyword/value pairs become attributes,
/// everything else becomes children. Lists are spliced in, attributes and
/// all, so a style is just a list of attributes. Nil is skipped. When an
/// attribute is given twice, the later one wins.
pub fn build_element(tag: &str, args: &[Value]) -> Result<Element, String> {
    let mut el = Element { depth: 1, tag: tag.into(), attrs: Vec::new(), children: Vec::new() };
    add_args(&mut el, args)?;
    check_length(&format!("({tag} ...)"), el.attrs.len() + el.children.len())?;
    let children = std::mem::take(&mut el.children);
    el.set_children(children)?;
    Ok(el)
}

fn add_args(el: &mut Element, args: &[Value]) -> Result<(), String> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg {
            Value::Keyword(name) => {
                let value = args.next().ok_or_else(|| format!("({} ...): attribute :{name} has no value", el.tag))?;
                el.attrs.push((name.clone(), value.clone()));
            }
            Value::List(items) => {
                // Checked as it goes, as well as at the end, so splicing the same
                // big list many times fails before it uses up memory.
                check_length(&format!("({} ...)", el.tag), el.attrs.len() + el.children.len() + items.len())?;
                add_args(el, items)?
            }
            Value::Nil => {}
            Value::Str(s) => el.children.push(Node::Text(s.clone())),
            Value::Num(_) | Value::Bool(_) => el.children.push(Node::Text(arg.to_string().into())),
            Value::Element(child) => el.children.push(Node::Element(child.clone())),
            other => return Err(format!("can't use {other} as page content")),
        }
    }
    Ok(())
}

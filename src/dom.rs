//! The document tree that a page program evaluates to.

use std::rc::Rc;

use crate::lisp::Value;

/// Element constructors available to pages as ordinary functions.
pub const TAGS: &[&str] = &["page", "div", "h1", "h2", "h3", "p"];

pub struct Element {
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
}

/// `(tag :attr value ... child ...)`: keyword/value pairs become attributes,
/// everything else becomes children. Lists are spliced in, nil is skipped.
pub fn build_element(tag: &str, args: &[Value]) -> Result<Element, String> {
    let mut el = Element { tag: tag.into(), attrs: Vec::new(), children: Vec::new() };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if let Value::Keyword(name) = arg {
            let value = args.next().ok_or_else(|| format!("({tag} ...): attribute :{name} has no value"))?;
            el.attrs.push((name.clone(), value.clone()));
        } else {
            push_child(arg, &mut el.children)?;
        }
    }
    Ok(el)
}

fn push_child(value: &Value, children: &mut Vec<Node>) -> Result<(), String> {
    match value {
        Value::Nil => {}
        Value::Str(s) => children.push(Node::Text(s.clone())),
        Value::Num(_) | Value::Bool(_) => children.push(Node::Text(value.to_string().into())),
        Value::Element(el) => children.push(Node::Element(el.clone())),
        Value::List(items) => {
            for item in items.iter() {
                push_child(item, children)?;
            }
        }
        other => return Err(format!("can't use {other} as page content")),
    }
    Ok(())
}

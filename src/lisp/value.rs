use std::{fmt, rc::Rc};

use super::eval::Lambda;
use crate::dom::Element;

pub type NativeFn = Rc<dyn Fn(&[Value]) -> Result<Value, String>>;

#[derive(Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    Num(f64),
    Str(Rc<str>),
    Sym(Rc<str>),
    /// `:name` — used for element attributes, evaluates to itself.
    Keyword(Rc<str>),
    List(Rc<[Value]>),
    Native(Rc<str>, NativeFn),
    Lambda(Rc<Lambda>),
    Element(Rc<Element>),
}

impl Value {
    pub fn sym(name: &str) -> Value {
        Value::Sym(name.into())
    }

    pub fn list(items: Vec<Value>) -> Value {
        Value::List(items.into())
    }

    pub fn truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }

    pub fn equals(&self, other: &Value) -> bool {
        use Value::*;
        match (self, other) {
            (Nil, Nil) => true,
            (Bool(a), Bool(b)) => a == b,
            (Num(a), Num(b)) => a == b,
            (Str(a), Str(b)) | (Sym(a), Sym(b)) | (Keyword(a), Keyword(b)) => a == b,
            (List(a), List(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.equals(y)),
            _ => false,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "nil"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Num(n) if n.fract() == 0.0 && n.abs() < 1e15 => write!(f, "{}", *n as i64),
            Value::Num(n) => write!(f, "{n}"),
            Value::Str(s) | Value::Sym(s) => write!(f, "{s}"),
            Value::Keyword(k) => write!(f, ":{k}"),
            Value::List(items) => {
                write!(f, "(")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, ")")
            }
            Value::Native(name, _) => write!(f, "#<builtin {name}>"),
            Value::Lambda(_) => write!(f, "#<fn>"),
            Value::Element(el) => write!(f, "#<{}>", el.tag),
        }
    }
}

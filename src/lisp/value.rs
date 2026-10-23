use std::{cell::RefCell, cmp::Ordering, collections::BTreeMap, fmt, rc::Rc};

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
    /// `(dict :key value ...)`, kept in key order.
    Dict(Rc<BTreeMap<Key, Value>>),
    Native(Rc<str>, NativeFn),
    Lambda(Rc<Lambda>),
    Element(Rc<Element>),
    /// What a `define-state` variable is bound to. Evaluating the variable
    /// reads the cell, so this never appears as a value in a running page.
    State(Rc<RefCell<Value>>),
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
            (Dict(a), Dict(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|((k, x), (l, y))| k == l && x.equals(y)),
            _ => false,
        }
    }
}

/// `{}` shows strings raw, for `str`. `{:#}` quotes them, so the output
/// reads back as the same value. Elements always print as the code that
/// builds them.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Str(s) if f.alternate() => write_quoted(f, s),
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
                    if f.alternate() {
                        write!(f, "{item:#}")?;
                    } else {
                        write!(f, "{item}")?;
                    }
                }
                write!(f, ")")
            }
            Value::Dict(entries) => {
                write!(f, "(dict")?;
                for (key, value) in entries.iter() {
                    if f.alternate() {
                        write!(f, " {:#} {value:#}", key.value())?;
                    } else {
                        write!(f, " {} {value}", key.value())?;
                    }
                }
                write!(f, ")")
            }
            Value::Native(name, _) => write!(f, "#<builtin {name}>"),
            Value::Lambda(_) => write!(f, "#<fn>"),
            Value::Element(el) => write!(f, "{el}"),
            Value::State(cell) => cell.borrow().fmt(f),
        }
    }
}

/// A dictionary key: a number, string or keyword. Numbers sort first, then
/// strings, then keywords.
#[derive(Clone)]
pub enum Key {
    Num(f64),
    Str(Rc<str>),
    Keyword(Rc<str>),
}

impl Key {
    pub fn new(value: &Value) -> Result<Key, String> {
        match value {
            Value::Num(n) if !n.is_nan() => Ok(Key::Num(*n)),
            Value::Str(s) => Ok(Key::Str(s.clone())),
            Value::Keyword(k) => Ok(Key::Keyword(k.clone())),
            other => Err(format!("{other:#} can't be a dictionary key: use a number, string or keyword")),
        }
    }

    pub fn value(&self) -> Value {
        match self {
            Key::Num(n) => Value::Num(*n),
            Key::Str(s) => Value::Str(s.clone()),
            Key::Keyword(k) => Value::Keyword(k.clone()),
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Key::Num(_) => 0,
            Key::Str(_) => 1,
            Key::Keyword(_) => 2,
        }
    }
}

impl Ord for Key {
    fn cmp(&self, other: &Key) -> Ordering {
        match (self, other) {
            (Key::Num(a), Key::Num(b)) if a == b => Ordering::Equal, // so 0 and -0 are one key
            (Key::Num(a), Key::Num(b)) => a.total_cmp(b),
            (Key::Str(a), Key::Str(b)) | (Key::Keyword(a), Key::Keyword(b)) => a.cmp(b),
            (a, b) => a.rank().cmp(&b.rank()),
        }
    }
}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Key) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Key) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Key {}

pub fn write_quoted(f: &mut fmt::Formatter<'_>, s: &str) -> fmt::Result {
    write!(f, "\"")?;
    for c in s.chars() {
        match c {
            '"' => write!(f, "\\\"")?,
            '\\' => write!(f, "\\\\")?,
            '\n' => write!(f, "\\n")?,
            '\t' => write!(f, "\\t")?,
            c => write!(f, "{c}")?,
        }
    }
    write!(f, "\"")
}

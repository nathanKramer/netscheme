//! The standard library's native half: numbers, comparisons, lists, strings
//! and dictionaries. These run in Rust, so each call costs a page one
//! evaluation step however much work it does. `prelude.sx` is the other half.

use std::{cmp::Ordering, collections::BTreeMap, fmt, rc::Rc};

use super::{
    budget,
    builtins::{bounded_string, def, list_arg},
    eval::{Env, apply},
    value::{Key, Value},
};

pub fn define_all(env: &Env) {
    numbers(env);
    lists(env);
    strings(env);
    dicts(env);
}

fn numbers(env: &Env) {
    unary(env, "floor", f64::floor);
    unary(env, "ceil", f64::ceil);
    unary(env, "round", f64::round);
    unary(env, "abs", f64::abs);
    unary(env, "sqrt", f64::sqrt);
    def(env, "pow", |args| match args {
        [Value::Num(a), Value::Num(b)] => Ok(Value::Num(a.powf(*b))),
        _ => Err("pow: expected two numbers".into()),
    });
    // Angles are in radians.
    unary(env, "sin", f64::sin);
    unary(env, "cos", f64::cos);
    def(env, "atan2", |args| match args {
        [Value::Num(y), Value::Num(x)] => Ok(Value::Num(y.atan2(*x))),
        _ => Err("atan2: expected two numbers".into()),
    });

    // Comparisons work on numbers or on strings, which compare alphabetically.
    chain(env, "<", Ordering::is_lt);
    chain(env, ">", Ordering::is_gt);
    chain(env, "<=", Ordering::is_le);
    chain(env, ">=", Ordering::is_ge);
    // The smallest or largest argument, or item of a single list argument.
    extreme(env, "min", Ordering::Less);
    extreme(env, "max", Ordering::Greater);
}

fn lists(env: &Env) {
    // (range end), (range start end) or (range start end step): numbers
    // from start up to, but not including, end.
    def(env, "range", |args| {
        let (start, end, step) = match args {
            [Value::Num(end)] => (0.0, *end, 1.0),
            [Value::Num(start), Value::Num(end)] => (*start, *end, 1.0),
            [Value::Num(start), Value::Num(end), Value::Num(step)] => (*start, *end, *step),
            _ => return Err("range: expected an end, or a start and end, and optionally a step".into()),
        };
        if step == 0.0 || step.is_nan() {
            return Err("range: the step can't be 0".into());
        }
        let count = ((end - start) / step).ceil().max(0.0);
        budget::check_length("range", count.min(usize::MAX as f64) as usize)?;
        Ok(Value::list((0..count as usize).map(|i| Value::Num(start + i as f64 * step)).collect()))
    });
    def(env, "repeat", |args| match args {
        [Value::Num(n), x] => {
            let n = n.max(0.0) as usize;
            budget::check_length("repeat", n)?;
            Ok(Value::list(vec![x.clone(); n]))
        }
        _ => Err("repeat: expected a count and a value".into()),
    });
    def(env, "reverse", |args| match args {
        [Value::Str(s)] => Ok(Value::Str(s.chars().rev().collect::<String>().into())),
        _ => Ok(Value::list(list_arg("reverse", args)?.iter().rev().cloned().collect())),
    });
    def(env, "last", |args| Ok(list_arg("last", args)?.last().cloned().unwrap_or(Value::Nil)));
    // The first n items or characters, and everything after them.
    def(env, "take", |args| slice("take", args, |n, len| (0, n.min(len))));
    def(env, "drop", |args| slice("drop", args, |n, len| (n.min(len), len)));
    // Whether a list has an item, a string has a substring, or a dictionary has a key.
    def(env, "contains?", |args| match args {
        [Value::List(xs), x] => Ok(Value::Bool(xs.iter().any(|y| y.equals(x)))),
        [Value::Nil, _] => Ok(Value::Bool(false)),
        [Value::Str(s), Value::Str(sub)] => Ok(Value::Bool(s.contains(&**sub))),
        [Value::Dict(d), key] => Ok(Value::Bool(Key::new(key).is_ok_and(|key| d.contains_key(&key)))),
        _ => Err("contains?: expected a list, string or dictionary, and what to look for".into()),
    });
    // Where an item is in a list, or a substring in a string, or nil.
    def(env, "index-of", |args| match args {
        [Value::List(xs), x] => Ok(xs.iter().position(|y| y.equals(x)).map_or(Value::Nil, |i| Value::Num(i as f64))),
        [Value::Str(s), Value::Str(sub)] => Ok(s.find(&**sub).map_or(Value::Nil, |byte| Value::Num(s[..byte].chars().count() as f64))),
        _ => Err("index-of: expected a list or string, and what to look for".into()),
    });
    def(env, "sort", |args| sort_by("sort", list_arg("sort", args)?, |x| Ok(x.clone())));
    // Sorts by what `key` returns for each item. Equal keys keep their order.
    def(env, "sort-by", |args| match args {
        [key, list] => sort_by("sort-by", list_arg("sort-by", std::slice::from_ref(list))?, |x| apply(key, std::slice::from_ref(x))),
        _ => Err("sort-by: expected a function and a list".into()),
    });
    // (reduce f init list) calls (f acc item) for each item in turn.
    def(env, "reduce", |args| match args {
        [f, init, list] => {
            list_arg("reduce", std::slice::from_ref(list))?.iter().try_fold(init.clone(), |acc, x| apply(f, &[acc, x.clone()]))
        }
        _ => Err("reduce: expected a function, a starting value and a list".into()),
    });
    // (apply f a b list) calls f with a, b and the list's items.
    def(env, "apply", |args| match args {
        [f, before @ .., list] => {
            let list = list_arg("apply", std::slice::from_ref(list))?;
            apply(f, &[before, list].concat())
        }
        _ => Err("apply: expected a function and a list".into()),
    });
}

fn strings(env: &Env) {
    // Joins the items of a list into a string, with `sep` between them.
    def(env, "join", |args| {
        let (items, sep) = match args {
            [list] => (list_arg("join", std::slice::from_ref(list))?, ""),
            [list, Value::Str(sep)] => (list_arg("join", std::slice::from_ref(list))?, &**sep),
            _ => return Err("join: expected a list and optionally a separator".into()),
        };
        bounded_string("join", |out| {
            items.iter().enumerate().try_for_each(|(i, item)| {
                if i > 0 {
                    fmt::Write::write_str(out, sep)?;
                }
                fmt::Write::write_fmt(out, format_args!("{item}"))
            })
        })
    });
    // Splits a string at each `sep`, or into characters when `sep` is "".
    def(env, "split", |args| match args {
        [Value::Str(s), Value::Str(sep)] if sep.is_empty() => {
            Ok(Value::list(s.chars().map(|c| Value::Str(c.to_string().into())).collect()))
        }
        [Value::Str(s), Value::Str(sep)] => Ok(Value::list(s.split(&**sep).map(|part| Value::Str(part.into())).collect())),
        _ => Err("split: expected a string and a separator".into()),
    });
    string_fn(env, "upper", |s| s.to_uppercase());
    string_fn(env, "lower", |s| s.to_lowercase());
    string_fn(env, "trim", |s| s.trim().to_string());
    // Characters from `start` up to, but not including, `end`.
    def(env, "substring", |args| {
        let (s, start, end) = match args {
            [Value::Str(s), Value::Num(start)] => (s, *start, f64::INFINITY),
            [Value::Str(s), Value::Num(start), Value::Num(end)] => (s, *start, *end),
            _ => return Err("substring: expected a string, a start and optionally an end".into()),
        };
        let (start, end) = (start.max(0.0) as usize, end.max(0.0).min(usize::MAX as f64) as usize);
        Ok(Value::Str(s.chars().skip(start).take(end.saturating_sub(start)).collect::<String>().into()))
    });
    def(env, "starts-with?", |args| match args {
        [Value::Str(s), Value::Str(prefix)] => Ok(Value::Bool(s.starts_with(&**prefix))),
        _ => Err("starts-with?: expected two strings".into()),
    });
    def(env, "ends-with?", |args| match args {
        [Value::Str(s), Value::Str(suffix)] => Ok(Value::Bool(s.ends_with(&**suffix))),
        _ => Err("ends-with?: expected two strings".into()),
    });
    // The number a string spells, like "42" or "-1.5", or nil.
    def(env, "parse-number", |args| match args {
        [Value::Str(s)] => Ok(s.trim().parse::<f64>().ok().filter(|n| n.is_finite()).map_or(Value::Nil, Value::Num)),
        _ => Err("parse-number: expected a string".into()),
    });
}

fn dicts(env: &Env) {
    // (dict :key value ...). Keys are numbers, strings or keywords.
    def(env, "dict", |args| Ok(Value::Dict(Rc::new(with_pairs("dict", BTreeMap::new(), args)?))));
    // (get dict key) is nil when the key is missing, or (get dict key default).
    def(env, "get", |args| match args {
        [Value::Dict(d), key] => Ok(d.get(&Key::new(key)?).cloned().unwrap_or(Value::Nil)),
        [Value::Dict(d), key, default] => Ok(d.get(&Key::new(key)?).cloned().unwrap_or_else(|| default.clone())),
        [Value::Nil, _] => Ok(Value::Nil),
        [Value::Nil, _, default] => Ok(default.clone()),
        _ => Err("get: expected a dictionary and a key".into()),
    });
    // A copy of the dictionary with some keys set, or removed.
    def(env, "assoc", |args| match args {
        [Value::Dict(d), pairs @ ..] => Ok(Value::Dict(Rc::new(with_pairs("assoc", (**d).clone(), pairs)?))),
        [Value::Nil, pairs @ ..] => Ok(Value::Dict(Rc::new(with_pairs("assoc", BTreeMap::new(), pairs)?))),
        _ => Err("assoc: expected a dictionary, then keys and values".into()),
    });
    def(env, "dissoc", |args| match args {
        [Value::Dict(d), keys @ ..] => {
            let mut d = (**d).clone();
            for key in keys {
                d.remove(&Key::new(key)?);
            }
            Ok(Value::Dict(Rc::new(d)))
        }
        _ => Err("dissoc: expected a dictionary, then keys".into()),
    });
    def(env, "keys", |args| match args {
        [Value::Dict(d)] => Ok(Value::list(d.keys().map(Key::value).collect())),
        _ => Err("keys: expected a dictionary".into()),
    });
    def(env, "values", |args| match args {
        [Value::Dict(d)] => Ok(Value::list(d.values().cloned().collect())),
        _ => Err("values: expected a dictionary".into()),
    });
    def(env, "dict?", |args| match args {
        [x] => Ok(Value::Bool(matches!(x, Value::Dict(_)))),
        _ => Err("dict?: expected one argument".into()),
    });
}

/// The order of two numbers or two strings.
fn order(a: &Value, b: &Value) -> Result<Ordering, String> {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x.partial_cmp(y).ok_or_else(|| "can't compare NaN".to_string()),
        (Value::Str(x), Value::Str(y)) => Ok(x.cmp(y)),
        _ => Err(format!("can't compare {a:#} with {b:#}: expected two numbers or two strings")),
    }
}

fn chain(env: &Env, name: &'static str, test: fn(Ordering) -> bool) {
    def(env, name, move |args| {
        for pair in args.windows(2) {
            if !test(order(&pair[0], &pair[1]).map_err(|e| format!("{name}: {e}"))?) {
                return Ok(Value::Bool(false));
            }
        }
        Ok(Value::Bool(true))
    });
}

fn extreme(env: &Env, name: &'static str, wanted: Ordering) {
    def(env, name, move |args| {
        let items = match args {
            [Value::List(xs)] => xs,
            _ => args,
        };
        let (first, rest) = items.split_first().ok_or_else(|| format!("{name}: expected at least one value"))?;
        rest.iter().try_fold(first.clone(), |best, x| {
            Ok(if order(x, &best).map_err(|e| format!("{name}: {e}"))? == wanted { x.clone() } else { best })
        })
    });
}

fn unary(env: &Env, name: &'static str, f: fn(f64) -> f64) {
    def(env, name, move |args| match args {
        [Value::Num(n)] => Ok(Value::Num(f(*n))),
        _ => Err(format!("{name}: expected a number")),
    });
}

fn string_fn(env: &Env, name: &'static str, f: fn(&str) -> String) {
    def(env, name, move |args| match args {
        [Value::Str(s)] => Ok(Value::Str(f(s).into())),
        _ => Err(format!("{name}: expected a string")),
    });
}

/// `take` and `drop`: `range` gives the part to keep, from the count and length.
fn slice(name: &str, args: &[Value], range: fn(usize, usize) -> (usize, usize)) -> Result<Value, String> {
    let n = match args.get(1) {
        Some(Value::Num(n)) if args.len() == 2 => n.max(0.0).min(usize::MAX as f64) as usize,
        _ => return Err(format!("{name}: expected a list or string, and a count")),
    };
    match &args[0] {
        Value::Str(s) => {
            let (from, to) = range(n, s.chars().count());
            Ok(Value::Str(s.chars().skip(from).take(to - from).collect::<String>().into()))
        }
        list => {
            let xs = list_arg(name, std::slice::from_ref(list))?;
            let (from, to) = range(n, xs.len());
            Ok(Value::list(xs[from..to].to_vec()))
        }
    }
}

fn sort_by(name: &str, xs: &[Value], key: impl Fn(&Value) -> Result<Value, String>) -> Result<Value, String> {
    let keys = xs.iter().map(key).collect::<Result<Vec<_>, _>>()?;
    let mut order_of = (0..xs.len()).collect::<Vec<_>>();
    let mut error = None;
    order_of.sort_by(|&i, &j| {
        order(&keys[i], &keys[j]).unwrap_or_else(|e| {
            error.get_or_insert(e);
            Ordering::Equal
        })
    });
    match error {
        Some(e) => Err(format!("{name}: {e}")),
        None => Ok(Value::list(order_of.into_iter().map(|i| xs[i].clone()).collect())),
    }
}

fn with_pairs(name: &str, mut d: BTreeMap<Key, Value>, pairs: &[Value]) -> Result<BTreeMap<Key, Value>, String> {
    if pairs.len() % 2 == 1 {
        return Err(format!("{name}: expected keys and values in pairs"));
    }
    for pair in pairs.chunks(2) {
        d.insert(Key::new(&pair[0]).map_err(|e| format!("{name}: {e}"))?, pair[1].clone());
    }
    budget::check_length(name, d.len())?;
    Ok(d)
}

use std::rc::Rc;

use super::{
    eval::{Env, apply},
    reader,
    value::{NativeFn, Value},
};
use crate::{
    dom,
    loader::{self, PageContext},
};

pub fn global_env(ctx: &Rc<PageContext>) -> Rc<Env> {
    let env = Env::new(None);

    def(&env, "+", |args| fold_nums(args, |a, b| a + b));
    def(&env, "-", |args| match args {
        [Value::Num(n)] => Ok(Value::Num(-n)),
        _ => fold_nums(args, |a, b| a - b),
    });
    def(&env, "*", |args| fold_nums(args, |a, b| a * b));
    def(&env, "/", |args| fold_nums(args, |a, b| a / b));
    def(&env, "mod", |args| fold_nums(args, f64::rem_euclid));
    def(&env, "=", |args| Ok(Value::Bool(args.windows(2).all(|w| w[0].equals(&w[1])))));
    def(&env, "<", |args| compare(args, |a, b| a < b));
    def(&env, ">", |args| compare(args, |a, b| a > b));
    def(&env, "not", |args| match args {
        [x] => Ok(Value::Bool(!x.truthy())),
        _ => Err("not: expected one argument".into()),
    });
    def(&env, "str", |args| Ok(Value::Str(args.iter().map(|v| v.to_string()).collect::<String>().into())));
    // The printed form of a value, which reads back as the same value.
    def(&env, "repr", |args| match args {
        [x] => Ok(Value::Str(format!("{x:#}").into())),
        _ => Err("repr: expected one argument".into()),
    });

    // Lists. `nil` counts as the empty list.
    def(&env, "list", |args| Ok(Value::list(args.to_vec())));
    def(&env, "first", |args| Ok(list_arg("first", args)?.first().cloned().unwrap_or(Value::Nil)));
    def(&env, "rest", |args| Ok(Value::list(list_arg("rest", args)?.iter().skip(1).cloned().collect())));
    def(&env, "nth", |args| match args {
        [Value::List(xs), Value::Num(n)] => Ok(xs.get(*n as usize).cloned().unwrap_or(Value::Nil)),
        _ => Err("nth: expected a list and an index".into()),
    });
    def(&env, "cons", |args| match args {
        [x, Value::List(xs)] => Ok(Value::list(std::iter::once(x).chain(xs.iter()).cloned().collect())),
        [x, Value::Nil] => Ok(Value::list(vec![x.clone()])),
        _ => Err("cons: expected a value and a list".into()),
    });
    def(&env, "length", |args| match args {
        [Value::Str(s)] => Ok(Value::Num(s.chars().count() as f64)),
        _ => Ok(Value::Num(list_arg("length", args)?.len() as f64)),
    });
    def(&env, "empty?", |args| Ok(Value::Bool(list_arg("empty?", args)?.is_empty())));
    def(&env, "map", |args| match args {
        [f, Value::List(xs)] => Ok(Value::list(xs.iter().map(|x| apply(f, std::slice::from_ref(x))).collect::<Result<_, _>>()?)),
        [_, Value::Nil] => Ok(Value::list(vec![])),
        _ => Err("map: expected a function and a list".into()),
    });
    def(&env, "filter", |args| match args {
        [f, Value::List(xs)] => {
            let mut kept = Vec::new();
            for x in xs.iter() {
                if apply(f, std::slice::from_ref(x))?.truthy() {
                    kept.push(x.clone());
                }
            }
            Ok(Value::list(kept))
        }
        [_, Value::Nil] => Ok(Value::list(vec![])),
        _ => Err("filter: expected a function and a list".into()),
    });

    predicate(&env, "list?", |v| matches!(v, Value::List(_)));
    predicate(&env, "string?", |v| matches!(v, Value::Str(_)));
    predicate(&env, "number?", |v| matches!(v, Value::Num(_)));
    predicate(&env, "symbol?", |v| matches!(v, Value::Sym(_)));
    predicate(&env, "keyword?", |v| matches!(v, Value::Keyword(_)));
    predicate(&env, "fn?", |v| matches!(v, Value::Native(..) | Value::Lambda(_)));

    // Looking inside elements: the document is data too.
    def(&env, "tag", |args| match args {
        [Value::Element(el)] => Ok(Value::Sym(el.tag.clone())),
        _ => Err("tag: expected an element".into()),
    });
    def(&env, "attr", |args| match args {
        [Value::Element(el), Value::Keyword(name) | Value::Str(name)] => Ok(el.attr(name).cloned().unwrap_or(Value::Nil)),
        [Value::Nil, _] => Ok(Value::Nil),
        _ => Err("attr: expected an element and an attribute name".into()),
    });
    def(&env, "children", |args| match args {
        [Value::Element(el)] => Ok(Value::list(
            el.children
                .iter()
                .map(|child| match child {
                    dom::Node::Element(el) => Value::Element(el.clone()),
                    dom::Node::Text(text) => Value::Str(text.clone()),
                })
                .collect(),
        )),
        _ => Err("children: expected an element".into()),
    });
    def(&env, "url-encode", |args| match args {
        [Value::Str(s)] => Ok(Value::Str(loader::encode(s).into())),
        _ => Err("url-encode: expected a string".into()),
    });

    for &tag in dom::TAGS {
        def(&env, tag, move |args| dom::build_element(tag, args).map(|el| Value::Element(Rc::new(el))));
    }

    // Builtins that depend on where the page was loaded from.
    let c = ctx.clone();
    def(&env, "frame", move |args| {
        let mut el = dom::build_element("frame", args)?;
        loader::fill_frame(&c, &mut el);
        Ok(Value::Element(Rc::new(el)))
    });
    // A page's code as data, without running it.
    let c = ctx.clone();
    def(&env, "source", move |args| {
        let [Value::Str(href)] = args else { return Err("source: expected a location".into()) };
        let text = loader::read_source(&loader::resolve(&c.location, href)?).map_err(|e| format!("source: {e}"))?;
        Ok(Value::list(reader::read_all(&text)?))
    });
    // A query parameter from the page's location: `page.sx?name=value`.
    let c = ctx.clone();
    def(&env, "param", move |args| {
        let [Value::Str(name)] = args else { return Err("param: expected a name".into()) };
        Ok(c.params.iter().find(|(k, _)| k == &**name).map_or(Value::Nil, |(_, v)| Value::Str(v.as_str().into())))
    });

    env
}

fn def(env: &Env, name: &str, f: impl Fn(&[Value]) -> Result<Value, String> + 'static) {
    let f: NativeFn = Rc::new(f);
    env.define(name.into(), Value::Native(name.into(), f));
}

fn predicate(env: &Env, name: &'static str, test: fn(&Value) -> bool) {
    def(env, name, move |args| match args {
        [x] => Ok(Value::Bool(test(x))),
        _ => Err(format!("{name}: expected one argument")),
    });
}

fn list_arg<'a>(name: &str, args: &'a [Value]) -> Result<&'a [Value], String> {
    match args {
        [Value::List(xs)] => Ok(xs),
        [Value::Nil] => Ok(&[]),
        _ => Err(format!("{name}: expected a list")),
    }
}

fn nums(args: &[Value]) -> Result<Vec<f64>, String> {
    args.iter()
        .map(|v| match v {
            Value::Num(n) => Ok(*n),
            other => Err(format!("expected a number, got {other}")),
        })
        .collect()
}

fn fold_nums(args: &[Value], op: fn(f64, f64) -> f64) -> Result<Value, String> {
    let nums = nums(args)?;
    let (first, rest) = nums.split_first().ok_or("expected at least one number")?;
    Ok(Value::Num(rest.iter().fold(*first, |acc, n| op(acc, *n))))
}

fn compare(args: &[Value], op: fn(f64, f64) -> bool) -> Result<Value, String> {
    Ok(Value::Bool(nums(args)?.windows(2).all(|w| op(w[0], w[1]))))
}

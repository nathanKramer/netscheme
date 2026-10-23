use std::{fmt, rc::Rc};

use super::{
    budget::{self, MAX_LENGTH},
    eval::{self, Env, apply},
    library, reader,
    value::{NativeFn, Value},
};
use crate::{
    dom,
    loader::{self, PageContext},
};

pub fn global_env(ctx: &Rc<PageContext>) -> Result<Rc<Env>, String> {
    let env = Env::global(ctx.store.page(&ctx.state_key));

    def(&env, "+", |args| fold_nums(args, |a, b| a + b));
    def(&env, "-", |args| match args {
        [Value::Num(n)] => Ok(Value::Num(-n)),
        _ => fold_nums(args, |a, b| a - b),
    });
    def(&env, "*", |args| fold_nums(args, |a, b| a * b));
    def(&env, "/", |args| fold_nums(args, |a, b| a / b));
    def(&env, "mod", |args| fold_nums(args, f64::rem_euclid));
    def(&env, "=", |args| Ok(Value::Bool(args.windows(2).all(|w| w[0].equals(&w[1])))));
    def(&env, "not", |args| match args {
        [x] => Ok(Value::Bool(!x.truthy())),
        _ => Err("not: expected one argument".into()),
    });
    def(&env, "str", |args| {
        bounded_string("str", |out| args.iter().try_for_each(|v| fmt::Write::write_fmt(out, format_args!("{v}"))))
    });
    // The printed form of a value, which reads back as the same value.
    def(&env, "repr", |args| match args {
        [x] => bounded_string("repr", |out| fmt::Write::write_fmt(out, format_args!("{x:#}"))),
        _ => Err("repr: expected one argument".into()),
    });

    // A style: attributes to give an element, as a list it splices in.
    def(&env, "style", |args| {
        for pair in args.chunks(2) {
            match pair {
                [Value::Keyword(_), _] => {}
                [Value::Keyword(name)] => return Err(format!("style: :{name} has no value")),
                [other, ..] => return Err(format!("style: expected an attribute name like :color, got {other}")),
                [] => unreachable!(),
            }
        }
        Ok(Value::list(args.to_vec()))
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
        [Value::Dict(d)] => Ok(Value::Num(d.len() as f64)),
        _ => Ok(Value::Num(list_arg("length", args)?.len() as f64)),
    });
    def(&env, "empty?", |args| match args {
        [Value::Str(s)] => Ok(Value::Bool(s.is_empty())),
        [Value::Dict(d)] => Ok(Value::Bool(d.is_empty())),
        _ => Ok(Value::Bool(list_arg("empty?", args)?.is_empty())),
    });
    def(&env, "append", |args| {
        let lists = args.iter().map(|arg| list_arg("append", std::slice::from_ref(arg))).collect::<Result<Vec<_>, _>>()?;
        budget::check_length("append", lists.iter().map(|list| list.len()).sum())?;
        Ok(Value::list(lists.concat()))
    });
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
        [Value::Str(s)] => {
            budget::check_length("url-encode", s.len() * 3)?;
            Ok(Value::Str(loader::encode(s).into()))
        }
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
        let location = loader::resolve(&c.location, href);
        let reach = loader::may_reach(&c.location, &location);
        let source = reach.and_then(|()| loader::read_source(&location, &c.host)).map_err(|e| format!("source: {e}"))?;
        Ok(Value::list(reader::read_all(&source.text)?))
    });
    // Asks the browser to go to a location, as if a link to it were clicked,
    // once the current event handler finishes.
    let c = ctx.clone();
    def(&env, "go", move |args| {
        let [Value::Str(href)] = args else { return Err("go: expected a location".into()) };
        c.host.request_navigation(href, &c.location);
        Ok(Value::Nil)
    });
    // Whether the browser is in dark mode, which pages can follow as they like.
    let c = ctx.clone();
    def(&env, "dark-mode", move |_| Ok(Value::Bool(c.host.dark_mode.get())));
    // `light` in light mode and `dark` in dark mode, like CSS's light-dark().
    let c = ctx.clone();
    def(&env, "light-dark", move |args| match args {
        [light, dark] => Ok(if c.host.dark_mode.get() { dark.clone() } else { light.clone() }),
        _ => Err("light-dark: expected a light value and a dark one".into()),
    });
    // Changes the setting. Only the browser's own pages may.
    let c = ctx.clone();
    def(&env, "set-dark-mode", move |args| {
        if !c.location.starts_with("browser:") {
            return Err("set-dark-mode: only browser: pages can change the browser's settings".into());
        }
        let [on] = args else { return Err("set-dark-mode: expected true or false".into()) };
        c.host.dark_mode.set(on.truthy());
        Ok(Value::Nil)
    });
    // The find bar's requests. Only browser pages may search.
    let browser_only = |c: &Rc<PageContext>, name: &str| {
        if c.location.starts_with("browser:") { Ok(()) } else { Err(format!("{name}: only browser: pages can search the page")) }
    };
    let c = ctx.clone();
    def(&env, "set-find", move |args| {
        browser_only(&c, "set-find")?;
        let [Value::Str(query)] = args else { return Err("set-find: expected the text to find".into()) };
        c.host.request_find(loader::FindRequest::Query(query.to_string()));
        Ok(Value::Nil)
    });
    for (name, request) in [
        ("find-next", loader::FindRequest::Next),
        ("find-previous", loader::FindRequest::Previous),
        ("close-find", loader::FindRequest::Close),
    ] {
        let c = ctx.clone();
        def(&env, name, move |_| {
            browser_only(&c, name)?;
            c.host.request_find(request.clone());
            Ok(Value::Nil)
        });
    }
    // A query parameter from the page's location: `page.sx?name=value`.
    let c = ctx.clone();
    def(&env, "param", move |args| {
        let [Value::Str(name)] = args else { return Err("param: expected a name".into()) };
        Ok(c.params.iter().find(|(k, _)| k == &**name).map_or(Value::Nil, |(_, v)| Value::Str(v.as_str().into())))
    });

    library::define_all(&env);
    PRELUDE.with(|prelude| prelude.iter().try_for_each(|form| eval::eval(form, &env).map(drop)))
        .map_err(|e| format!("in the prelude: {e}"))?;
    Ok(env)
}

thread_local! {
    /// The library functions written in the page language, read once.
    static PRELUDE: Vec<Value> = reader::read_all(include_str!("../../ui/prelude.sx")).expect("the prelude reads");
}

/// Builds a string with `write`, stopping once it's longer than `MAX_LENGTH`
/// rather than after building something enormous.
pub(super) fn bounded_string(what: &str, write: impl FnOnce(&mut Bounded) -> fmt::Result) -> Result<Value, String> {
    let mut out = Bounded(String::new());
    match write(&mut out) {
        Ok(()) => Ok(Value::Str(out.0.into())),
        Err(_) => Err(format!("{what}: the result would be longer than {MAX_LENGTH}")),
    }
}

pub(super) struct Bounded(String);

impl fmt::Write for Bounded {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.0.len() + s.len() > MAX_LENGTH {
            return Err(fmt::Error);
        }
        self.0.push_str(s);
        Ok(())
    }
}

pub(super) fn def(env: &Env, name: &str, f: impl Fn(&[Value]) -> Result<Value, String> + 'static) {
    let f: NativeFn = Rc::new(f);
    env.define(name.into(), Value::Native(name.into(), f));
}

fn predicate(env: &Env, name: &'static str, test: fn(&Value) -> bool) {
    def(env, name, move |args| match args {
        [x] => Ok(Value::Bool(test(x))),
        _ => Err(format!("{name}: expected one argument")),
    });
}

pub(super) fn list_arg<'a>(name: &str, args: &'a [Value]) -> Result<&'a [Value], String> {
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


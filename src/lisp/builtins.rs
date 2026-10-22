use std::rc::Rc;

use super::{
    eval::Env,
    value::{NativeFn, Value},
};
use crate::dom;

pub fn global_env() -> Rc<Env> {
    let env = Env::new(None);

    def(&env, "+", |args| fold_nums(args, |a, b| a + b));
    def(&env, "-", |args| match args {
        [Value::Num(n)] => Ok(Value::Num(-n)),
        _ => fold_nums(args, |a, b| a - b),
    });
    def(&env, "*", |args| fold_nums(args, |a, b| a * b));
    def(&env, "/", |args| fold_nums(args, |a, b| a / b));
    def(&env, "=", |args| Ok(Value::Bool(args.windows(2).all(|w| w[0].equals(&w[1])))));
    def(&env, "<", |args| compare(args, |a, b| a < b));
    def(&env, ">", |args| compare(args, |a, b| a > b));
    def(&env, "str", |args| Ok(Value::Str(args.iter().map(|v| v.to_string()).collect::<String>().into())));
    def(&env, "list", |args| Ok(Value::list(args.to_vec())));

    for &tag in dom::TAGS {
        def(&env, tag, move |args| dom::build_element(tag, args).map(|el| Value::Element(Rc::new(el))));
    }

    env
}

fn def(env: &Env, name: &str, f: impl Fn(&[Value]) -> Result<Value, String> + 'static) {
    let f: NativeFn = Rc::new(f);
    env.define(name.into(), Value::Native(name.into(), f));
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

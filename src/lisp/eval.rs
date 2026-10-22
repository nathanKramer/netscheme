use std::{cell::RefCell, collections::HashMap, rc::Rc};

use super::value::Value;

pub struct Env {
    vars: RefCell<HashMap<Rc<str>, Value>>,
    parent: Option<Rc<Env>>,
}

impl Env {
    pub fn new(parent: Option<Rc<Env>>) -> Rc<Env> {
        Rc::new(Env { vars: RefCell::new(HashMap::new()), parent })
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        match self.vars.borrow().get(name) {
            Some(v) => Some(v.clone()),
            None => self.parent.as_ref()?.get(name),
        }
    }

    pub fn define(&self, name: Rc<str>, value: Value) {
        self.vars.borrow_mut().insert(name, value);
    }
}

pub struct Lambda {
    params: Vec<Rc<str>>,
    body: Rc<[Value]>,
    env: Rc<Env>,
}

pub fn eval(expr: &Value, env: &Rc<Env>) -> Result<Value, String> {
    match expr {
        Value::Sym(name) => env.get(name).ok_or_else(|| format!("undefined symbol: {name}")),
        Value::List(items) if !items.is_empty() => eval_list(items, env),
        _ => Ok(expr.clone()),
    }
}

pub fn eval_body(body: &[Value], env: &Rc<Env>) -> Result<Value, String> {
    let mut result = Value::Nil;
    for expr in body {
        result = eval(expr, env)?;
    }
    Ok(result)
}

fn eval_list(items: &[Value], env: &Rc<Env>) -> Result<Value, String> {
    if let Value::Sym(head) = &items[0] {
        match &**head {
            "quote" => return arg(items, 1, "quote").cloned(),
            "if" => {
                let cond = eval(arg(items, 1, "if")?, env)?;
                return if cond.truthy() {
                    eval(arg(items, 2, "if")?, env)
                } else {
                    items.get(3).map_or(Ok(Value::Nil), |e| eval(e, env))
                };
            }
            "do" => return eval_body(&items[1..], env),
            "define" => return eval_define(items, env),
            "fn" => return make_lambda(arg(items, 1, "fn")?, &items[2..], env),
            _ => {}
        }
    }
    let f = eval(&items[0], env)?;
    let args = items[1..].iter().map(|e| eval(e, env)).collect::<Result<Vec<_>, _>>()?;
    apply(&f, &args)
}

pub fn apply(f: &Value, args: &[Value]) -> Result<Value, String> {
    match f {
        Value::Native(_, func) => func(args),
        Value::Lambda(lambda) => {
            if lambda.params.len() != args.len() {
                return Err(format!("expected {} arguments, got {}", lambda.params.len(), args.len()));
            }
            let env = Env::new(Some(lambda.env.clone()));
            for (param, value) in lambda.params.iter().zip(args) {
                env.define(param.clone(), value.clone());
            }
            eval_body(&lambda.body, &env)
        }
        other => Err(format!("not a function: {other}")),
    }
}

/// `(define name value)` or `(define (name params...) body...)`.
fn eval_define(items: &[Value], env: &Rc<Env>) -> Result<Value, String> {
    match items.get(1) {
        Some(Value::Sym(name)) => {
            let value = eval(arg(items, 2, "define")?, env)?;
            env.define(name.clone(), value);
            Ok(Value::Nil)
        }
        Some(Value::List(sig)) if matches!(sig.first(), Some(Value::Sym(_))) => {
            let Value::Sym(name) = &sig[0] else { unreachable!() };
            let params = Value::list(sig[1..].to_vec());
            let lambda = make_lambda(&params, &items[2..], env)?;
            env.define(name.clone(), lambda);
            Ok(Value::Nil)
        }
        _ => Err("define: expected a name or (name params...)".into()),
    }
}

fn make_lambda(params: &Value, body: &[Value], env: &Rc<Env>) -> Result<Value, String> {
    let Value::List(params) = params else {
        return Err("fn: expected a parameter list".into());
    };
    let params = params
        .iter()
        .map(|p| match p {
            Value::Sym(name) => Ok(name.clone()),
            other => Err(format!("fn: parameter must be a symbol, got {other}")),
        })
        .collect::<Result<_, _>>()?;
    Ok(Value::Lambda(Rc::new(Lambda { params, body: body.into(), env: env.clone() })))
}

fn arg<'a>(items: &'a [Value], i: usize, form: &str) -> Result<&'a Value, String> {
    items.get(i).ok_or_else(|| format!("{form}: missing argument"))
}

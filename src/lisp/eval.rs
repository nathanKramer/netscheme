use std::{cell::RefCell, collections::HashMap, rc::Rc};

use super::{budget, value::Value, watch};

/// A page's `define-state` variables, kept between runs of the page.
#[derive(Default)]
pub struct PageState {
    cells: RefCell<HashMap<Rc<str>, Rc<RefCell<Value>>>>,
}

pub struct Env {
    vars: RefCell<HashMap<Rc<str>, Value>>,
    parent: Option<Rc<Env>>,
    /// Set on a page's global environment only.
    state: Option<Rc<PageState>>,
}

impl Env {
    pub fn new(parent: Option<Rc<Env>>) -> Rc<Env> {
        Rc::new(Env { vars: RefCell::new(HashMap::new()), parent, state: None })
    }

    pub fn global(state: Rc<PageState>) -> Rc<Env> {
        Rc::new(Env { vars: RefCell::new(HashMap::new()), parent: None, state: Some(state) })
    }

    fn page_state(&self) -> Option<&Rc<PageState>> {
        match &self.parent {
            Some(parent) => parent.page_state(),
            None => self.state.as_ref(),
        }
    }

    /// Changes an existing variable, writing through to its cell if it's state.
    fn set(&self, name: &str, value: Value) -> Result<(), String> {
        let mut vars = self.vars.borrow_mut();
        match vars.get_mut(name) {
            Some(Value::State(cell)) => {
                watch::wrote(cell);
                *cell.borrow_mut() = value;
            }
            Some(slot) => {
                if self.state.is_some() {
                    watch::wrote_global();
                }
                *slot = value;
            }
            None => {
                drop(vars);
                return self.parent.as_ref().ok_or_else(|| format!("set!: undefined variable: {name}"))?.set(name, value);
            }
        }
        Ok(())
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
    /// Takes any further arguments as a list: `(fn (a & more) ...)`.
    rest: Option<Rc<str>>,
    body: Rc<[Value]>,
    env: Rc<Env>,
}

/// What evaluating one form comes to: a value, or another form to evaluate
/// in its place. The second is a tail call, or the tail of a special form
/// like `if`, and `eval` carries on with it in the same loop, so a function
/// that calls itself last can loop forever without nesting deeper.
enum Step {
    Done(Value),
    Eval(Value, Rc<Env>),
}

pub fn eval(expr: &Value, env: &Rc<Env>) -> Result<Value, String> {
    let _depth = budget::enter()?;
    budget::grow_stack(|| {
        let (mut expr, mut env) = match step(expr, env)? {
            Step::Done(value) => return Ok(value),
            Step::Eval(expr, env) => (expr, env),
        };
        loop {
            budget::step()?;
            (expr, env) = match step(&expr, &env)? {
                Step::Done(value) => return Ok(value),
                Step::Eval(expr, env) => (expr, env),
            };
        }
    })
}

fn step(expr: &Value, env: &Rc<Env>) -> Result<Step, String> {
    match expr {
        Value::Sym(name) => match env.get(name) {
            Some(Value::State(cell)) => {
                watch::read(&cell);
                Ok(Step::Done(cell.borrow().clone()))
            }
            Some(value) => Ok(Step::Done(value)),
            None => Err(format!("undefined symbol: {name}")),
        },
        Value::List(items) if !items.is_empty() => eval_list(items, env),
        _ => Ok(Step::Done(expr.clone())),
    }
}

pub fn eval_body(body: &[Value], env: &Rc<Env>) -> Result<Value, String> {
    let mut result = Value::Nil;
    for expr in body {
        result = eval(expr, env)?;
    }
    Ok(result)
}

/// Evaluates all but the last form of a body, leaving the last as the tail.
fn tail_body(body: &[Value], env: &Rc<Env>) -> Result<Step, String> {
    let Some((last, init)) = body.split_last() else { return Ok(Step::Done(Value::Nil)) };
    for expr in init {
        eval(expr, env)?;
    }
    Ok(Step::Eval(last.clone(), env.clone()))
}

fn eval_list(items: &[Value], env: &Rc<Env>) -> Result<Step, String> {
    let done = |value| Ok(Step::Done(value));
    let tail = |expr: &Value| Ok(Step::Eval(expr.clone(), env.clone()));
    if let Value::Sym(head) = &items[0] {
        match &**head {
            "quote" => return done(arg(items, 1, "quote")?.clone()),
            "if" => {
                let cond = eval(arg(items, 1, "if")?, env)?;
                return if cond.truthy() { tail(arg(items, 2, "if")?) } else { items.get(3).map_or(done(Value::Nil), tail) };
            }
            "do" => return tail_body(&items[1..], env),
            "and" => {
                let Some((last, init)) = items[1..].split_last() else { return done(Value::Bool(true)) };
                for expr in init {
                    let result = eval(expr, env)?;
                    if !result.truthy() {
                        return done(result);
                    }
                }
                return tail(last);
            }
            "or" => {
                let Some((last, init)) = items[1..].split_last() else { return done(Value::Nil) };
                for expr in init {
                    let result = eval(expr, env)?;
                    if result.truthy() {
                        return done(result);
                    }
                }
                return tail(last);
            }
            "cond" => return eval_cond(&items[1..], env),
            "let" => return eval_let(items, env),
            "quasiquote" => return done(quasiquote(arg(items, 1, "quasiquote")?, env)?),
            // Code is data: evaluate the argument, then evaluate the result as code.
            "eval" => return tail(&eval(arg(items, 1, "eval")?, env)?),
            "define" => return done(eval_define(items, env)?),
            "define-state" => return done(eval_define_state(items, env)?),
            "set!" => {
                let Some(Value::Sym(name)) = items.get(1) else {
                    return Err("set!: expected a variable name".into());
                };
                let value = eval(arg(items, 2, "set!")?, env)?;
                env.set(name, value)?;
                return done(Value::Nil);
            }
            "fn" => return done(make_lambda(arg(items, 1, "fn")?, &items[2..], env)?),
            _ => {}
        }
    }
    let f = eval(&items[0], env)?;
    let args = items[1..].iter().map(|e| eval(e, env)).collect::<Result<Vec<_>, _>>()?;
    match &f {
        Value::Lambda(lambda) => tail_body(&lambda.body, &lambda.bind(&args)?),
        _ => done(apply(&f, &args)?),
    }
}

pub fn apply(f: &Value, args: &[Value]) -> Result<Value, String> {
    match f {
        Value::Native(_, func) => func(args),
        Value::Lambda(lambda) => eval_body(&lambda.body, &lambda.bind(args)?),
        other => Err(format!("not a function: {other}")),
    }
}

impl Lambda {
    /// A new environment for a call, with the parameters bound to `args`.
    fn bind(&self, args: &[Value]) -> Result<Rc<Env>, String> {
        let n = self.params.len();
        match self.rest {
            None if args.len() != n => return Err(format!("expected {n} arguments, got {}", args.len())),
            Some(_) if args.len() < n => return Err(format!("expected at least {n} arguments, got {}", args.len())),
            _ => {}
        }
        let env = Env::new(Some(self.env.clone()));
        for (param, value) in self.params.iter().zip(args) {
            env.define(param.clone(), value.clone());
        }
        if let Some(rest) = &self.rest {
            env.define(rest.clone(), Value::list(args[n..].to_vec()));
        }
        Ok(env)
    }
}

/// `(cond (test body...) ... (else body...))`
fn eval_cond(clauses: &[Value], env: &Rc<Env>) -> Result<Step, String> {
    for clause in clauses {
        let Value::List(clause) = clause else {
            return Err("cond: each clause must be a list".into());
        };
        let (test, body) = clause.split_first().ok_or("cond: empty clause")?;
        let is_else = matches!(test, Value::Sym(s) if &**s == "else");
        if is_else || eval(test, env)?.truthy() {
            return tail_body(body, env);
        }
    }
    Ok(Step::Done(Value::Nil))
}

/// `(let ((name value) ...) body...)`. Each value can see the names before it.
fn eval_let(items: &[Value], env: &Rc<Env>) -> Result<Step, String> {
    let Value::List(bindings) = arg(items, 1, "let")? else {
        return Err("let: expected a list of bindings".into());
    };
    let inner = Env::new(Some(env.clone()));
    for binding in bindings.iter() {
        let Value::List(pair) = binding else {
            return Err("let: each binding must be (name value)".into());
        };
        let [Value::Sym(name), expr] = &pair[..] else {
            return Err("let: each binding must be (name value)".into());
        };
        let value = eval(expr, &inner)?;
        inner.define(name.clone(), value);
    }
    tail_body(&items[2..], &inner)
}

/// Builds `form` as data, except that `,x` is evaluated and `,@xs` is
/// evaluated and spliced into the surrounding list.
fn quasiquote(form: &Value, env: &Rc<Env>) -> Result<Value, String> {
    let Value::List(items) = form else { return Ok(form.clone()) };
    if let Some(x) = unquoted(form, "unquote") {
        return eval(x, env);
    }
    let mut out = Vec::new();
    for item in items.iter() {
        match unquoted(item, "unquote-splicing") {
            Some(x) => match eval(x, env)? {
                Value::List(xs) => out.extend(xs.iter().cloned()),
                Value::Nil => {}
                other => return Err(format!("unquote-splicing: expected a list, got {other}")),
            },
            None => out.push(quasiquote(item, env)?),
        }
    }
    Ok(Value::list(out))
}

/// If `form` is `(marker x)`, returns `x`.
fn unquoted<'a>(form: &'a Value, marker: &str) -> Option<&'a Value> {
    match form {
        Value::List(items) => match &items[..] {
            [Value::Sym(head), x] if &**head == marker => Some(x),
            _ => None,
        },
        _ => None,
    }
}

/// `(define-state name initial)`: like `define`, but the variable keeps
/// its value when the page runs again, so `initial` is only used the first
/// time. Change it with `set!`.
fn eval_define_state(items: &[Value], env: &Rc<Env>) -> Result<Value, String> {
    let Some(Value::Sym(name)) = items.get(1) else {
        return Err("define-state: expected a variable name".into());
    };
    let state = env.page_state().ok_or("define-state: only pages can have state")?;
    let existing = state.cells.borrow().get(name).cloned();
    let cell = match existing {
        Some(cell) => cell,
        None => {
            let cell = Rc::new(RefCell::new(eval(arg(items, 2, "define-state")?, env)?));
            state.cells.borrow_mut().insert(name.clone(), cell.clone());
            cell
        }
    };
    env.define(name.clone(), Value::State(cell));
    Ok(Value::Nil)
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
    let names = params
        .iter()
        .map(|p| match p {
            Value::Sym(name) => Ok(name.clone()),
            other => Err(format!("fn: parameter must be a symbol, got {other}")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (params, rest) = match names.iter().position(|name| &**name == "&") {
        None => (names, None),
        Some(i) => match &names[i + 1..] {
            [rest] => (names[..i].to_vec(), Some(rest.clone())),
            _ => return Err("fn: `&` must be followed by exactly one parameter".into()),
        },
    };
    Ok(Value::Lambda(Rc::new(Lambda { params, rest, body: body.into(), env: env.clone() })))
}

fn arg<'a>(items: &'a [Value], i: usize, form: &str) -> Result<&'a Value, String> {
    items.get(i).ok_or_else(|| format!("{form}: missing argument"))
}

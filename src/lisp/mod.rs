//! The page language: a small Lisp whose programs evaluate to a document.

mod builtins;
mod eval;
mod reader;
mod value;

pub use value::Value;

/// Read and evaluate every top-level form, returning the last value.
pub fn run(src: &str) -> Result<Value, String> {
    let env = builtins::global_env();
    let mut result = Value::Nil;
    for form in reader::read_all(src)? {
        result = eval::eval(&form, &env)?;
    }
    Ok(result)
}

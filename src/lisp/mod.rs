//! The page language: a small Lisp whose programs evaluate to a document.

mod budget;
mod builtins;
mod eval;
mod reader;
mod value;

use std::rc::Rc;

pub use budget::{check_length, grow_stack};
pub use eval::PageState;
pub use value::{Value, write_quoted};

use crate::loader::PageContext;

/// Read and evaluate every top-level form, returning the last value.
/// Evaluation stops with an error if it exceeds its budget.
pub fn run(src: &str, ctx: &Rc<PageContext>) -> Result<Value, String> {
    budget::metered(|| {
        let env = builtins::global_env(ctx);
        let mut result = Value::Nil;
        for form in reader::read_all(src)? {
            result = eval::eval(&form, &env)?;
        }
        Ok(result)
    })
}

/// Calls a function, such as an event handler, with a budget of its own.
pub fn apply(f: &Value, args: &[Value]) -> Result<Value, String> {
    budget::metered(|| eval::apply(f, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(src: &str) -> String {
        match run(src, &crate::loader::test_context()) {
            Ok(v) => format!("{v:#}"),
            Err(e) => format!("error: {e}"),
        }
    }

    #[test]
    fn elements_print_as_the_code_that_builds_them() {
        let printed = eval(r##"(p :color "#c33" "say \"hi\"" (b "x") 42)"##);
        assert_eq!(printed, r##"(p :color "#c33" "say \"hi\"" (b "x") "42")"##);
        assert_eq!(eval(&printed), printed, "printing round-trips");
    }

    #[test]
    fn quasiquote_builds_code_that_eval_runs() {
        assert_eq!(eval("(define n 2) `(+ 1 ,n ,@(list 3 4))"), "(+ 1 2 3 4)");
        assert_eq!(eval("(define n 2) (eval `(+ 1 ,n ,@(list 3 4)))"), "10");
        assert_eq!(eval(r#"(define tag 'b) (eval `(,tag "hi"))"#), r#"(b "hi")"#);
    }

    #[test]
    fn special_forms() {
        assert_eq!(eval("(let ((x 2) (y (* x 3))) (+ x y))"), "8");
        assert_eq!(eval("(cond ((= 1 2) 'a) ((= 1 1) 'b) (else 'c))"), "b");
        assert_eq!(eval("(cond (false 'a) (else 'c))"), "c");
        assert_eq!(eval("(list (and 1 nil 2) (and 1 2) (or nil 3) (or nil false))"), "(nil 2 3 false)");
    }

    #[test]
    fn list_builtins() {
        assert_eq!(eval("(map (fn (x) (* x x)) (filter (fn (x) (> x 1)) '(1 2 3)))"), "(4 9)");
        assert_eq!(eval("(list (first '(1 2)) (rest '(1 2)) (cons 0 '(1)) (nth '(a b) 1) (empty? '()))"), "(1 (2) (0 1) b true)");
        assert_eq!(eval(r#"(repr '(a "b" :c 1))"#), r#""(a \"b\" :c 1)""#);
        assert_eq!(eval("(append '(1) '() nil '(2 3))"), "(1 2 3)");
        assert_eq!(eval(r#"(list (empty? "") (empty? "a"))"#), "(true false)");
    }

    #[test]
    fn state_survives_running_the_page_again() {
        let ctx = crate::loader::test_context();
        let src = "(define-state n (+ 1 1)) (define (bump) (set! n (* n 10))) (list n bump)";
        let Ok(Value::List(first)) = run(src, &ctx) else { panic!() };
        assert_eq!(format!("{}", first[0]), "2");
        apply(&first[1], &[]).unwrap();
        let Ok(Value::List(second)) = run(src, &ctx) else { panic!() };
        assert_eq!(format!("{}", second[0]), "20", "the initial value isn't used again");
    }

    #[test]
    fn set_changes_the_nearest_variable() {
        assert_eq!(eval("(define x 1) (define (f) (set! x 5)) (f) x"), "5");
        assert_eq!(eval("(define-state x 1) (define (f x) (set! x 5) x) (list (f 0) x)"), "(5 1)", "a parameter shadows state");
        assert!(eval("(set! nope 1)").starts_with("error: set!: undefined"));
    }

    #[test]
    fn rest_parameters() {
        assert_eq!(eval("(define (f a & more) (list a more)) (list (f 1) (f 1 2 3))"), "((1 ()) (1 (2 3)))");
        assert_eq!(eval("((fn (& all) all) 1 2)"), "(1 2)");
        assert!(eval("(define (f a & more) a) (f)").contains("at least 1"));
        assert!(eval("(fn (a &) a)").contains("exactly one"));
    }

    #[test]
    fn styles_are_lists_of_attributes() {
        let muted = r##"(define muted (style :color "#888" :size 14))"##;
        assert_eq!(eval(&format!("{muted} muted")), r##"(:color "#888" :size 14)"##);
        assert_eq!(eval(&format!(r#"{muted} (p muted "hi")"#)), r##"(p :color "#888" :size 14 "hi")"##);
        // Later attributes win, so what comes last takes precedence.
        let page = eval(&format!(r#"{muted} (define (note & body) (p muted body)) (note :color "red" "hi")"#));
        assert_eq!(page, r##"(p :color "#888" :size 14 :color "red" "hi")"##);
        assert!(eval("(style :color)").contains(":color has no value"));
        assert!(eval(r#"(style "color" 1)"#).contains("expected an attribute name"));
    }

    #[test]
    fn runaway_pages_stop_with_an_error() {
        assert!(eval("(define (f) (f)) (f)").contains("levels of nesting"), "infinite recursion");
        let fib = "(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))) (fib 40)";
        assert!(eval(fib).contains("evaluation steps"), "too much work, without deep recursion");
        let doubling = r#"(define (grow s n) (if (= n 0) s (grow (str s s) (- n 1)))) (grow "ab" 40)"#;
        assert!(eval(doubling).contains("longer than"), "a string that doubles");
        let doubling = "(define (grow l n) (if (= n 0) l (grow (append l l) (- n 1)))) (grow '(1) 40)";
        assert!(eval(doubling).contains("longer than"), "a list that doubles");
        let splicing = r#"(define big (str (repr (list 1 2 3)) "x")) (define (grow l n) (if (= n 0) l (grow (append l l) (- n 1)))) (p (grow (list big) 19) (grow (list big) 19) (grow (list big) 19))"#;
        assert!(eval(splicing).contains("longer than"), "an element that splices big lists");
        let nesting = r#"(define (nest n) (if (= n 0) "x" (div (nest (- n 1))))) (nest 600)"#;
        assert!(eval(nesting).contains("nested more than 512"), "deeply nested elements");
    }

    #[test]
    fn deep_but_finite_recursion_is_fine() {
        // Over a thousand calls deep: more than fits on a test thread's stack
        // without growing it.
        let count = "(define (count n) (if (= n 0) 0 (+ 1 (count (- n 1))))) (count 3000)";
        assert_eq!(eval(count), "3000");
    }

    #[test]
    fn the_reader_refuses_deep_nesting() {
        assert!(eval(&"(".repeat(100_000)).contains("nested more than"));
        assert!(eval(&format!("{}x", "'".repeat(100_000))).contains("nested more than"));
    }

    #[test]
    fn each_run_gets_its_own_budget() {
        let ctx = crate::loader::test_context();
        let src = "(define (spin n) (if (= n 0) 0 (spin (- n 1)))) (define (handler) (spin 2000)) handler";
        let handler = run(src, &ctx).unwrap();
        // Together these take more steps than one budget allows.
        for _ in 0..100 {
            apply(&handler, &[]).unwrap();
        }
    }
}

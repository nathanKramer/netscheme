//! The page language: a small Lisp whose programs evaluate to a document.

mod budget;
mod builtins;
mod eval;
mod library;
mod reader;
mod value;
pub mod watch;

use std::rc::Rc;

pub use budget::{check_length, grow_stack};
pub use eval::{Env, PageState};
pub use value::{Value, write_quoted};

use crate::loader::PageContext;

/// Read and evaluate every top-level form, returning the last value.
/// Evaluation stops with an error if it exceeds its budget.
pub fn run(src: &str, ctx: &Rc<PageContext>) -> Result<Value, String> {
    budget::metered(|| {
        let env = builtins::global_env(ctx)?;
        ctx.host.keep_env(&ctx.state_key, &env);
        eval_in(src, &env)
    })
}

/// Reads and evaluates every form in an environment kept from an earlier
/// run, as the REPL does, returning the last value.
pub fn eval_in(src: &str, env: &Rc<Env>) -> Result<Value, String> {
    budget::metered(|| {
        let mut result = Value::Nil;
        for form in reader::read_all(src)? {
            result = eval::eval(&form, env)?;
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
    fn try_catches_errors_and_read_all_reads_code() {
        assert_eq!(eval("(try (+ 1 2) (fn (e) e))"), "3");
        assert_eq!(eval("(try (nope) (fn (e) (str \"caught: \" e)))"), "\"caught: undefined symbol: nope\"");
        assert_eq!(eval(r#"(read-all "(p :x 1) ; hi\n'y")"#), "((p :x 1) (quote y))");
        assert_eq!(eval(r#"(try (read-all "(p") (fn (e) e))"#), "\"line 1: unclosed '('\"");
        assert_eq!(eval(r#"(eval `(do ,@(read-all "(define x 2) (* x 21)")))"#), "42");
    }

    #[test]
    fn a_runaway_inside_try_leaves_the_page_steps_to_carry_on() {
        let src = "(define (spin) (spin)) (define (count n) (if (= n 0) 'done (count (- n 1))))
                   (list (try (spin) (fn (e) e)) (count 10000))";
        let result = eval(src);
        assert!(result.starts_with("(\"stopped after 4999"), "about half the steps: {result}");
        assert!(result.ends_with(" done)"), "{result}");
        assert!(eval("(define (spin) (spin)) (spin)").contains("stopped after 1000000 evaluation steps"));
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
    fn numbers_and_comparisons() {
        assert_eq!(eval("(list (floor 2.7) (ceil 2.1) (round 2.5) (abs -3) (sqrt 9) (pow 2 10))"), "(2 3 3 3 3 1024)");
        assert_eq!(eval("(list (max 1 5 3) (min '(4 2 8)) (<= 1 1 2) (>= 3 2 2) (< 1 2 2))"), "(5 2 true true false)");
        assert_eq!(eval(r#"(list (< "apple" "banana") (max "pear" "fig"))"#), r#"(true "pear")"#);
        assert!(eval(r#"(< 1 "2")"#).contains("can't compare 1 with \"2\""));
    }

    #[test]
    fn more_list_functions() {
        assert_eq!(eval("(list (range 3) (range 1 4) (range 10 0 -4) (range 3 1))"), "((0 1 2) (1 2 3) (10 6 2) ())");
        assert_eq!(
            eval("(list (reverse '(1 2 3)) (take '(1 2 3) 2) (drop '(1 2 3) 2) (take '(1) 5) (last '(1 2)))"),
            "((3 2 1) (1 2) (3) (1) 2)"
        );
        assert_eq!(eval("(list (contains? '(1 2) 2) (contains? '(1 2) 3) (index-of '(a b) 'b) (index-of '(a) 'z))"), "(true false 1 nil)");
        assert_eq!(eval("(list (sort '(3 1 2)) (sort-by (fn (p) (nth p 1)) '((a 2) (b 1) (c 2))))"), "((1 2 3) ((b 1) (a 2) (c 2)))");
        assert_eq!(eval("(list (reduce + 0 '(1 2 3)) (apply + 1 '(2 3)) (repeat 3 'x))"), "(6 6 (x x x))");
        assert!(eval("(sort '(1 \"a\"))").contains("sort: can't compare"));
        assert!(eval("(range 0 1 0)").contains("can't be 0"));
        assert!(eval("(range 2000000)").contains("longer than"));
    }

    #[test]
    fn string_functions() {
        assert_eq!(
            eval(r#"(list (join '(1 2 3) ", ") (join '("a" "b")) (split "a,b" ",") (split "hé" ""))"#),
            r#"("1, 2, 3" "ab" ("a" "b") ("h" "é"))"#
        );
        assert_eq!(
            eval(r#"(list (upper "hé") (lower "AB") (trim "  x ") (substring "héllo" 1 3) (substring "abc" 1))"#),
            r#"("HÉ" "ab" "x" "él" "bc")"#
        );
        assert_eq!(
            eval(r#"(list (starts-with? "abc" "ab") (ends-with? "abc" "b") (contains? "abc" "bc") (index-of "héllo" "l"))"#),
            "(true false true 2)"
        );
        assert_eq!(
            eval(r#"(list (take "abc" 2) (drop "abc" 2) (reverse "abc") (parse-number " -1.5") (parse-number "x"))"#),
            r#"("ab" "c" "cba" -1.5 nil)"#
        );
    }

    #[test]
    fn dictionaries() {
        let d = r#"(define d (dict :b 2 :a 1 "s" 3 10 "ten"))"#;
        assert_eq!(eval(&format!("{d} d")), r#"(dict 10 "ten" "s" 3 :a 1 :b 2)"#, "keys in order, printed as code");
        assert_eq!(eval(&format!(r#"{d} (= d (dict 10 "ten" "s" 3 :a 1 :b 2))"#)), "true", "the printed form makes an equal dictionary");
        assert_eq!(eval(&format!("{d} (list (get d :a) (get d :z) (get d :z 0) (contains? d \"s\") (length d))")), "(1 nil 0 true 4)");
        assert_eq!(
            eval(&format!("{d} (list (assoc d :a 5 :c 6) (dissoc d :a 10) d)")),
            r#"((dict 10 "ten" "s" 3 :a 5 :b 2 :c 6) (dict "s" 3 :b 2) (dict 10 "ten" "s" 3 :a 1 :b 2))"#,
            "copies, not changes"
        );
        assert_eq!(
            eval("(list (keys (dict :a 1)) (values (dict :a 1)) (empty? (dict)) (dict? (dict)) (assoc nil :a 1))"),
            "((:a) (1) true true (dict :a 1))"
        );
        assert!(eval("(dict '(1) 2)").contains("can't be a dictionary key"));
        assert!(eval("(dict :a)").contains("in pairs"));
    }

    #[test]
    fn the_prelude() {
        assert_eq!(eval("(list (sum '(1 2 3)) (product '(2 3)) (inc 1) (dec 1) (concat '((1) () (2 3))))"), "(6 6 2 0 (1 2 3))");
        assert_eq!(eval("(list (without 2 '(1 2 3 2)) (toggle 2 '(1 2)) (toggle 3 '(1 2)))"), "((1 3) (1) (3 1 2))");
        assert_eq!(eval("(list (any? number? '(a 1)) (all? number? '(a 1)) (map-indexed list '(a b)))"), "(true false ((0 a) (1 b)))");
        assert_eq!(eval(r#"(list (update (dict :n 1) :n inc) (plural 1 "cat") (plural 2 "cat"))"#), r#"((dict :n 2) "1 cat" "2 cats")"#);
        assert_eq!(eval("(list (interpose 0 '(1 2 3)) (interpose 0 '(1)) (chunk 2 '(1 2 3)))"), "((1 0 2 0 3) (1) ((1 2) (3)))");
        assert_eq!(eval("(define (sum xs) 'mine) (sum '(1))"), "mine", "pages can redefine library functions");
    }

    #[test]
    fn reading_other_pages() {
        assert_eq!(
            eval(r#"(list (resolve "pages/a.sx" "b.sx") (resolve "pages/a.sx" "/b.sx") (resolve "browser:x" "b.sx"))"#),
            r#"("pages/b.sx" "b.sx" "b.sx")"#
        );
        assert_eq!(eval(r#"(source "no-such-page.sx" 'none)"#), "none", "the fallback for a page that can't be read");
        assert!(eval(r#"(source "no-such-page.sx")"#).contains("source: couldn't read no-such-page.sx"));
    }

    #[test]
    fn only_browser_pages_search_and_bookmark() {
        assert!(eval(r#"(set-find "x")"#).contains("only browser: pages"));
        assert!(eval("(find-next)").contains("only browser: pages"));
        assert!(eval(r#"(repl-eval "1")"#).contains("only browser: pages"));
        assert!(eval("(repl-log)").contains("only browser: pages"));
        assert!(eval("(bookmarks)").contains("only browser: pages"));
        assert!(eval(r#"(bookmark "a.sx" "A")"#).contains("only browser: pages"));
        assert!(eval(r#"(unbookmark "a.sx")"#).contains("only browser: pages"));
    }

    #[test]
    fn native_functions_cost_one_step() {
        // Sorting this many items in the page language would take far more
        // than a page's budget.
        assert_eq!(eval("(length (sort (reverse (range 100000))))"), "100000");
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
        assert!(eval("(define (f) (+ 1 (f))) (f)").contains("levels of nesting"), "infinite recursion");
        assert!(eval("(define (f) (f)) (f)").contains("evaluation steps"), "an infinite loop of tail calls");
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
    fn tail_calls_loop_without_nesting() {
        // Far deeper than the nesting limit allows for calls that aren't tail calls.
        let count = "(define (count n acc) (if (= n 0) acc (count (- n 1) (+ acc 1)))) (count 50000 0)";
        assert_eq!(eval(count), "50000");
        let mutual = "(define (even? n) (if (= n 0) true (odd? (- n 1))))
                      (define (odd? n) (if (= n 0) false (even? (- n 1))))
                      (list (even? 40000) (odd? 40001))";
        assert_eq!(eval(mutual), "(true true)");
        // Every tail position: the last form of a body, `do`, `let` and `cond`,
        // either branch of `if`, the last of `and` and `or`, and `eval`.
        let forms = "(define (f n) (do 1 (let ((m (- n 1))) (cond ((= n 0) 'done) (else (and true (or false (if false 0 (eval `(f ,m)))))))))) (f 20000)";
        assert_eq!(eval(forms), "done");
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

    #[test]
    fn random_numbers_are_in_range_and_repeat_unless_seeded() {
        let src = "(define xs (map (fn (i) (random)) (range 1000)))
                   (define ns (map (fn (i) (random 6)) (range 1000)))
                   (list (all? (fn (x) (and (>= x 0) (< x 1))) xs)
                         (all? (fn (n) (and (>= n 0) (< n 6) (= n (floor n)))) ns)
                         (length (filter (fn (n) (= n 5)) ns)))";
        let result = eval(src);
        assert!(result.starts_with("(true true "), "{result}");
        let fives: f64 = result.trim_start_matches("(true true ").trim_end_matches(')').parse().unwrap();
        assert!((120.0..220.0).contains(&fives), "about a sixth are 5: {fives}");
        assert_eq!(eval("(random 1)"), "0");
        assert_eq!(eval("(random)"), eval("(random)"), "a fresh browser starts in the same place");
        assert!(eval("(random 2.5)").starts_with("error: random:"));
        assert!(eval("(random 0)").starts_with("error: random:"));

        let host = crate::loader::Host::default();
        host.seed_randomly();
        assert_ne!(host.random(), crate::loader::Host::default().random(), "seeded, it starts somewhere else");
    }
}

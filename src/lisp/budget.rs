//! Limits on how much work a page can make the browser do, so that a page
//! with an infinite loop or runaway recursion shows an error instead of
//! freezing or crashing the browser.
//!
//! A run (evaluating a page, or calling an event handler) gets a number of
//! evaluation steps and a maximum nesting depth. Frames are evaluated inside
//! the page that contains them, so they share its budget.

use std::cell::Cell;

/// Evaluation steps a run may take.
pub const MAX_STEPS: u64 = 1_000_000;
/// How deeply evaluation may nest. A recursive call is about three levels.
/// Evaluation grows its stack as needed (see `grow_stack`), so this guards
/// against runaway recursion rather than stack overflow.
pub const MAX_DEPTH: u32 = 10_000;
/// Longest string or list that builtins will build, in characters or items.
pub const MAX_LENGTH: usize = 1_000_000;

thread_local! {
    static STEPS: Cell<u64> = const { Cell::new(0) };
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` with a fresh budget, or within the current one if a run is
/// already under way, as when a page loads a frame.
pub fn metered<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    if ACTIVE.get() {
        return f();
    }
    struct Run;
    impl Drop for Run {
        fn drop(&mut self) {
            ACTIVE.set(false);
        }
    }
    STEPS.set(0);
    DEPTH.set(0);
    ACTIVE.set(true);
    let _run = Run;
    f()
}

/// Counts one evaluation step and one level of nesting, which lasts until
/// the returned guard is dropped.
pub fn enter() -> Result<Depth, String> {
    if !ACTIVE.get() {
        return Ok(Depth(false));
    }
    step()?;
    let depth = DEPTH.get() + 1;
    if depth > MAX_DEPTH {
        return Err(format!("stopped at {MAX_DEPTH} levels of nesting. Is there runaway recursion?"));
    }
    DEPTH.set(depth);
    Ok(Depth(true))
}

/// Counts one evaluation step without nesting, as when evaluation carries
/// on with a tail call in place.
pub fn step() -> Result<(), String> {
    if !ACTIVE.get() {
        return Ok(());
    }
    let steps = STEPS.get() + 1;
    STEPS.set(steps);
    if steps > MAX_STEPS {
        return Err(format!("stopped after {MAX_STEPS} evaluation steps. Is there an infinite loop?"));
    }
    Ok(())
}

/// One level of nesting; see `enter`.
pub struct Depth(bool);

impl Drop for Depth {
    fn drop(&mut self) {
        if self.0 {
            DEPTH.set(DEPTH.get() - 1);
        }
    }
}

/// Runs `f`, which is about to recurse, on a fresh stack segment if the
/// current one is nearly full.
pub fn grow_stack<T>(f: impl FnOnce() -> T) -> T {
    stacker::maybe_grow(64 * 1024, 1024 * 1024, f)
}

/// Checks that a builtin's result isn't too big.
pub fn check_length(what: &str, length: usize) -> Result<(), String> {
    if length > MAX_LENGTH {
        return Err(format!("{what}: the result would be longer than {MAX_LENGTH}"));
    }
    Ok(())
}

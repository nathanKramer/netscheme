//! Watching what evaluation touches: which `define-state` variables a page
//! reads as it runs, and which variables event handlers change. If handlers
//! changed nothing the page read, running it again would build the same
//! document, so the browser can skip that.

use std::{cell::RefCell, collections::HashSet, rc::Rc};

use super::value::Value;

/// What evaluation touched while it was watched.
#[derive(Default, Debug)]
pub struct Touched {
    /// State cells, by address. The page's state store keeps them alive,
    /// so an address means the same cell for as long as the page is shown.
    pub cells: HashSet<usize>,
    /// Whether a page's global variable that isn't state was changed. Its
    /// new value only lasts until the page runs again, which would reset it,
    /// so anything could depend on it.
    pub globals: bool,
}

impl Touched {
    /// Whether these changes could make a page that read `reads` come out differently.
    pub fn affects(&self, reads: &Touched) -> bool {
        self.globals || self.cells.iter().any(|cell| reads.cells.contains(cell))
    }
}

thread_local! {
    static READS: RefCell<Option<Touched>> = const { RefCell::new(None) };
    static WRITES: RefCell<Option<Touched>> = const { RefCell::new(None) };
}

/// Runs `f`, and returns the state it read along with its result.
pub fn reads<T>(f: impl FnOnce() -> T) -> (T, Touched) {
    watch(&READS, f)
}

/// Runs `f`, and returns what it changed along with its result.
pub fn writes<T>(f: impl FnOnce() -> T) -> (T, Touched) {
    watch(&WRITES, f)
}

fn watch<T>(slot: &'static std::thread::LocalKey<RefCell<Option<Touched>>>, f: impl FnOnce() -> T) -> (T, Touched) {
    let outer = slot.replace(Some(Touched::default()));
    let result = f();
    let touched = slot.replace(outer).unwrap_or_default();
    // Nested watches report to the ones around them too.
    slot.with_borrow_mut(|outer| {
        if let Some(outer) = outer {
            outer.cells.extend(&touched.cells);
            outer.globals |= touched.globals;
        }
    });
    (result, touched)
}

fn address(cell: &Rc<RefCell<Value>>) -> usize {
    Rc::as_ptr(cell) as usize
}

pub(super) fn read(cell: &Rc<RefCell<Value>>) {
    READS.with_borrow_mut(|reads| {
        if let Some(reads) = reads {
            reads.cells.insert(address(cell));
        }
    });
}

pub(super) fn wrote(cell: &Rc<RefCell<Value>>) {
    WRITES.with_borrow_mut(|writes| {
        if let Some(writes) = writes {
            writes.cells.insert(address(cell));
        }
    });
}

pub(super) fn wrote_global() {
    WRITES.with_borrow_mut(|writes| {
        if let Some(writes) = writes {
            writes.globals = true;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lisp::{apply, run};

    #[test]
    fn a_page_reads_the_state_it_shows_and_handlers_change_what_they_set() {
        let src = r#"(define-state shown 0) (define-state hidden 0) (define plain 0)
                     (page (p shown)
                           (button :on-click (fn () (set! hidden (+ hidden 1))))
                           (button :on-click (fn () (set! shown 1)))
                           (button :on-click (fn () (let ((x 0)) (set! x 1))))
                           (button :on-click (fn () (set! plain 1))))"#;
        let ctx = crate::loader::test_context();
        let (page, read) = reads(|| run(src, &ctx));
        let Ok(Value::Element(page)) = page else { panic!("not a page") };
        assert_eq!(read.cells.len(), 1, "only `shown`, as `hidden` is only read in a handler");

        let handler = |i: usize| match &page.children[i] {
            crate::dom::Node::Element(button) => button.attr("on-click").unwrap().clone(),
            _ => panic!("not a button"),
        };
        let changes = |i| writes(|| apply(&handler(i), &[])).1;
        assert!(!changes(1).affects(&read), "a change the page didn't read");
        assert_eq!(changes(1).cells.len(), 1);
        assert!(changes(2).affects(&read), "a change to what the page shows");
        let local = changes(3);
        assert!(!local.affects(&read) && local.cells.is_empty(), "a handler's own variables don't count");
        assert!(changes(4).affects(&read), "a global that isn't state could be anywhere");
    }
}

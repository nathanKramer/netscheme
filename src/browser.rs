//! A browsing session: the site being shown, its history and state, and
//! what the user is doing with it. It knows nothing about windows: the
//! window forwards events to it, and screenshots drive it directly.

use std::{
    rc::Rc,
    time::{Duration, Instant},
};

use crate::{
    dom::Element,
    layout::{Action, Focus, InputBox, Link, Scroller},
    lisp::{self, Value},
    loader::{self, Host, StateStore},
    paint::{Renderer, Scroll, View},
};

/// Logical pixels scrolled per arrow key press or mouse wheel line.
pub const LINE_HEIGHT: f32 = 40.0;
/// Following a link to this location goes back instead.
const BACK: &str = "browser:back";
/// How long the caret stays on, then off.
const BLINK: Duration = Duration::from_millis(530);

/// A key press, as far as the browser cares.
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    /// Typed text.
    Text(String),
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Backspace,
    Delete,
    Enter,
    Escape,
    Tab,
    /// Go back in history.
    Back,
    /// Select everything in the focused input.
    SelectAll,
    /// Select the address bar: the chrome's first input.
    FocusAddressBar,
    /// Load the page afresh: download it again, and start its state over.
    Reload,
}

/// What's under the pointer, for choosing its shape.
pub enum Hover {
    Nothing,
    Link,
    Text,
}

pub struct Browser {
    /// Location of the site being browsed.
    site: String,
    /// Whether the site is shown inside `browser:chrome`.
    chrome: bool,
    /// The page being shown: the chrome with the site framed inside it, or the bare site.
    page: Rc<Element>,
    /// `define-state` variables of the page and its frames.
    page_state: Rc<StateStore>,
    /// Settings, and requests from pages, that outlast navigation.
    host: Rc<Host>,
    /// Sites to return to with "back", with their scroll offsets and state, most recent last.
    history: Vec<(String, Scroll, Rc<StateStore>)>,
    scroll: Scroll,
    focus: Option<Focus>,
    /// The error from the last event handler, which the chrome shows until
    /// something else happens.
    error: Option<String>,
    /// Whether the mouse button went down in the focused input and is still
    /// down, so moving the mouse selects text.
    dragging: bool,
    /// When the caret last moved; it blinks starting from then.
    caret_moved: Instant,
    renderer: Renderer,
    // From the last render:
    links: Vec<Link>,
    scrollers: Vec<Scroller>,
    inputs: Vec<InputBox>,
    /// Height and scale of the window.
    viewport: (f32, f32),
}

impl Browser {
    pub fn new(site: String, chrome: bool) -> Browser {
        let mut browser = Browser {
            site,
            chrome,
            page: loader::error_page("not loaded yet"),
            page_state: Rc::default(),
            host: Rc::default(),
            history: Vec::new(),
            scroll: Scroll::default(),
            focus: None,
            error: None,
            dragging: false,
            caret_moved: Instant::now(),
            renderer: Renderer::new(),
            links: Vec::new(),
            scrollers: Vec::new(),
            inputs: Vec::new(),
            viewport: (0.0, 1.0),
        };
        browser.reload();
        browser
    }

    pub fn set_scroll(&mut self, page: f32) {
        self.scroll.page = page;
    }

    pub fn dark_mode(&self) -> bool {
        self.host.dark_mode.get()
    }

    /// Switches dark mode, and runs the page again so it can follow.
    pub fn set_dark_mode(&mut self, dark: bool) {
        if self.host.dark_mode.replace(dark) != dark {
            self.reload();
        }
    }

    pub fn title(&self) -> String {
        match self.page.attr("title") {
            Some(Value::Str(t)) => format!("{t} — Toy Browser"),
            _ => "Toy Browser".into(),
        }
    }

    /// Draws the page into `buf`, which is `w` × `h` physical pixels.
    pub fn render(&mut self, buf: &mut [u32], w: usize, h: usize, scale: f32) {
        let caret_visible = (self.caret_moved.elapsed().as_millis() / BLINK.as_millis()).is_multiple_of(2);
        let view = View { scale, scroll: &self.scroll, focus: self.focus, caret_visible, dark: self.host.dark_mode.get() };
        let rendered = self.renderer.render(&self.page, buf, w, h, &view);
        (self.links, self.scrollers, self.inputs, self.scroll) = (rendered.links, rendered.scrollers, rendered.inputs, rendered.scroll);
        self.viewport = (h as f32, scale);
        // The page may have changed under the focused input.
        self.focus = self.focus.and_then(|focus| {
            let input = self.inputs.iter().find(|input| input.id == focus.id)?;
            let len = input.caret_xs.len() - 1;
            Some(Focus { cursor: focus.cursor.min(len), anchor: focus.anchor.min(len), ..focus })
        });
    }

    /// When the caret next blinks, if one is showing.
    pub fn next_blink(&self) -> Option<Instant> {
        self.focus?;
        let blinks = self.caret_moved.elapsed().as_millis() / BLINK.as_millis();
        Some(self.caret_moved + BLINK * (blinks as u32 + 1))
    }

    pub fn hover(&self, x: f32, y: f32) -> Hover {
        if self.input_at(x, y).is_some() {
            Hover::Text
        } else if self.link_at(x, y).is_some() {
            Hover::Link
        } else {
            Hover::Nothing
        }
    }

    fn input_at(&self, x: f32, y: f32) -> Option<&InputBox> {
        self.inputs.iter().find(|input| input.area.contains(x, y))
    }

    fn link_at(&self, x: f32, y: f32) -> Option<&Link> {
        self.links.iter().find(|link| link.contains(x, y))
    }

    /// The innermost scrollable frame at a point.
    fn scroller_at(&self, x: f32, y: f32) -> Option<&Scroller> {
        let area = |s: &&Scroller| (s.area.x1 - s.area.x0) * (s.area.y1 - s.area.y0);
        self.scrollers.iter().filter(|s| s.area.contains(x, y)).min_by(|a, b| area(a).total_cmp(&area(b)))
    }

    /// Presses and releases the mouse button at a point. Returns whether
    /// there was anything there to click.
    pub fn click(&mut self, x: f32, y: f32) -> bool {
        let on_input = self.press(x, y, 1);
        self.release(x, y) || on_input
    }

    /// The mouse button went down: the `clicks`th in quick succession.
    /// Inputs respond now, so that dragging can select; links wait for the
    /// release. Returns whether the press was on an input.
    pub fn press(&mut self, x: f32, y: f32, clicks: u32) -> bool {
        let Some(input) = self.input_at(x, y) else {
            self.focus = None;
            return false;
        };
        let at = input.caret_at(x);
        let len = input.caret_xs.len() - 1;
        let newly_focused = self.focus.is_none_or(|focus| focus.id != input.id);
        let select_all = newly_focused && input.element.attr("select-on-focus").is_some_and(Value::truthy);
        let (anchor, cursor) = match clicks {
            _ if select_all => (0, len),
            1 => (at, at),
            2 => word_at(&input.value, at),
            _ => (0, len),
        };
        let id = input.id;
        self.dragging = clicks == 1 && !select_all;
        self.set_focus(Focus { id, cursor, anchor });
        true
    }

    /// The mouse moved to a point. Returns whether that changed the selection.
    pub fn drag_to(&mut self, x: f32) -> bool {
        let Some(focus) = self.focus.filter(|_| self.dragging) else { return false };
        let Some(input) = self.inputs.iter().find(|input| input.id == focus.id) else { return false };
        let cursor = input.caret_at(x);
        if cursor == focus.cursor {
            return false;
        }
        self.set_focus(Focus { cursor, ..focus });
        true
    }

    /// The mouse button came up: follows a link or runs a click handler,
    /// unless the press was in an input. Returns whether anything was clicked.
    pub fn release(&mut self, x: f32, y: f32) -> bool {
        if std::mem::take(&mut self.dragging) || self.input_at(x, y).is_some() {
            return true;
        }
        match self.link_at(x, y).map(|link| link.action.clone()) {
            Some(Action::Go { href, base }) => self.follow(&href, base.as_deref()),
            Some(Action::Run(handler)) => self.run(&handler, &[]),
            None => return false,
        }
        true
    }

    /// Scrolls the frame at a point, or the page if there's none, by `dy`
    /// physical pixels; positive is down. The next render clamps it.
    pub fn scroll_by(&mut self, (x, y): (f32, f32), dy: f32) {
        match self.scroller_at(x, y).map(|s| s.id) {
            Some(id) => *self.scroll.frames.entry(id).or_default() += dy,
            None => self.scroll.page += dy,
        }
    }

    /// Handles a key press. A focused input gets it first; otherwise it
    /// scrolls whatever is at `pointer`.
    pub fn key(&mut self, key: Key, shift: bool, pointer: (f32, f32)) {
        match key {
            Key::Back => return self.back(),
            Key::FocusAddressBar => return self.focus_address_bar(),
            Key::Reload => {
                self.host.forget_downloads();
                self.page_state = Rc::default();
                (self.focus, self.error) = (None, None);
                return self.reload();
            }
            _ => {}
        }
        if let Some(focus) = self.focus
            && self.edit(focus, &key, shift)
        {
            return;
        }
        let (window_height, scale) = self.viewport;
        let height = self.scroller_at(pointer.0, pointer.1).map_or(window_height, |s| s.area.y1 - s.area.y0);
        let page = (height - 40.0 * scale).max(height / 2.0);
        let dy = match key {
            Key::Down => LINE_HEIGHT * scale,
            Key::Up => -LINE_HEIGHT * scale,
            Key::Text(text) if text == " " && shift => -page,
            Key::Text(text) if text == " " => page,
            Key::PageDown => page,
            Key::PageUp => -page,
            // Far enough to reach either end; the next render clamps it.
            Key::Home => -1e9,
            Key::End => 1e9,
            _ => return,
        };
        self.scroll_by(pointer, dy);
    }

    /// Applies a key to the focused input. Returns whether the key was used.
    fn edit(&mut self, focus: Focus, key: &Key, shift: bool) -> bool {
        let Some(input) = self.inputs.iter().find(|input| input.id == focus.id) else {
            self.focus = None;
            return false;
        };
        let (element, value) = (input.element.clone(), input.value.clone());
        match edit(&value, focus, key, shift) {
            None => return false,
            Some(Edit::Select { cursor, anchor }) => self.set_focus(Focus { cursor, anchor, ..focus }),
            Some(Edit::Change(text, cursor)) => {
                // The input shows whatever value the page gives it, so typing
                // only does something if the page listens.
                if let Some(handler) = element.attr("on-input").cloned() {
                    self.set_focus(Focus { cursor, anchor: cursor, ..focus });
                    self.run(&handler, &[Value::Str(text.into())]);
                }
            }
            Some(Edit::Submit) => {
                if let Some(handler) = element.attr("on-submit").cloned() {
                    self.run(&handler, &[Value::Str(value.into())]);
                }
            }
            Some(Edit::Blur) => self.focus = None,
            Some(Edit::Next) => {
                let ids = || self.inputs.iter().map(|input| input.id);
                let next = ids().filter(|&id| id > focus.id).min().or_else(|| ids().min());
                if let Some(next) = next.and_then(|id| self.inputs.iter().find(|input| input.id == id)) {
                    self.set_focus(Focus { id: next.id, cursor: next.caret_xs.len() - 1, anchor: 0 });
                }
            }
        }
        true
    }

    fn focus_address_bar(&mut self) {
        let first = self.inputs.iter().min_by_key(|input| input.id);
        if let Some(input) = first.filter(|_| self.chrome) {
            self.set_focus(Focus { id: input.id, cursor: input.caret_xs.len() - 1, anchor: 0 });
        }
    }

    fn set_focus(&mut self, focus: Focus) {
        self.focus = Some(focus);
        self.caret_moved = Instant::now();
    }

    fn location(&self) -> String {
        if !self.chrome {
            return self.site.clone();
        }
        let back = if self.history.is_empty() { "" } else { "&back=1" };
        let error = self.error.as_ref().map_or(String::new(), |e| format!("&error={}", loader::encode(e)));
        format!("browser:chrome?location={}{back}{error}", loader::encode(&self.site))
    }

    /// Links on the chrome, and in the site framed inside it, all navigate the site.
    fn follow(&mut self, href: &str, base: Option<&str>) {
        if href == BACK {
            return self.back();
        }
        let from = base.map_or_else(|| self.location(), String::from);
        let site = loader::resolve(&from, href);
        if let Err(e) = loader::may_reach(&from, &site) {
            return self.fail(e);
        }
        self.error = None;
        self.host.forget_downloads();
        let previous = (
            std::mem::replace(&mut self.site, site),
            std::mem::take(&mut self.scroll),
            std::mem::take(&mut self.page_state),
        );
        self.history.push(previous);
        self.focus = None;
        self.dragging = false;
        self.reload();
    }

    pub fn back(&mut self) {
        if let Some((site, scroll, page_state)) = self.history.pop() {
            self.error = None;
            self.host.forget_downloads();
            // The site's state comes back, but the toolbar's describes the
            // current site, so the toolbar (the top-level page) starts afresh.
            if self.chrome {
                page_state.forget("");
            }
            (self.site, self.scroll, self.page_state) = (site, scroll, page_state);
            self.focus = None;
            self.reload();
        }
    }

    /// Calls a handler, then goes where it asked to with `go`, or else runs
    /// the page again so it shows any state the handler changed.
    fn run(&mut self, handler: &Value, args: &[Value]) {
        match lisp::apply(handler, args) {
            Ok(_) => match self.host.take_navigation() {
                Some((href, from)) => self.follow(&href, Some(&from)),
                None => {
                    self.error = None;
                    self.reload();
                }
            },
            Err(e) => {
                self.host.take_navigation();
                self.fail(format!("in an event handler: {e}"));
            }
        }
    }

    /// Reports an error from something the user did. The chrome shows it
    /// above the page, which stays usable; without the chrome, an error page
    /// replaces the page.
    fn fail(&mut self, error: String) {
        if self.chrome {
            self.error = Some(error);
            self.reload();
        } else {
            self.page = loader::error_page(&error);
        }
    }

    /// Runs the page, keeping its state, scroll offsets and focus.
    fn reload(&mut self) {
        self.page = loader::open(&self.location(), &self.page_state, &self.host);
        // `go` only means something in an event handler, not while the page runs.
        self.host.take_navigation();
    }
}

/// What a key does to a text input.
#[derive(Debug, PartialEq)]
enum Edit {
    /// Move the caret, and the selection's other end.
    Select { cursor: usize, anchor: usize },
    /// Change the text, with the caret at this position afterwards.
    Change(String, usize),
    Submit,
    Blur,
    /// Focus the next input.
    Next,
}

/// What `key` does to an input holding `value`, focused as `focus`. With
/// `shift` held, moving the caret extends the selection. `None` if the
/// input doesn't use the key.
fn edit(value: &str, focus: Focus, key: &Key, shift: bool) -> Option<Edit> {
    let chars: Vec<char> = value.chars().collect();
    let len = chars.len();
    let (cursor, anchor) = (focus.cursor.min(len), focus.anchor.min(len));
    let (start, end) = (cursor.min(anchor), cursor.max(anchor));
    let selected = start != end;
    let replace = |from: usize, to: usize, text: &str| -> String {
        chars[..from].iter().chain(&text.chars().collect::<Vec<_>>()).chain(&chars[to..]).collect()
    };
    let to = |position: usize| Edit::Select { cursor: position, anchor: if shift { anchor } else { position } };
    Some(match key {
        // Without shift, an arrow key leaves a selection at that end.
        Key::Left if selected && !shift => to(start),
        Key::Right if selected && !shift => to(end),
        Key::Left => to(cursor.saturating_sub(1)),
        Key::Right => to((cursor + 1).min(len)),
        Key::Home => to(0),
        Key::End => to(len),
        Key::SelectAll => Edit::Select { cursor: len, anchor: 0 },
        Key::Backspace | Key::Delete if selected => Edit::Change(replace(start, end, ""), start),
        Key::Backspace if cursor > 0 => Edit::Change(replace(cursor - 1, cursor, ""), cursor - 1),
        Key::Delete if cursor < len => Edit::Change(replace(cursor, cursor + 1, ""), cursor),
        Key::Backspace | Key::Delete => Edit::Select { cursor, anchor },
        Key::Enter => Edit::Submit,
        Key::Escape => Edit::Blur,
        Key::Tab => Edit::Next,
        Key::Text(text) => Edit::Change(replace(start, end, text), start + text.chars().count()),
        _ => return None,
    })
}

/// The word around caret position `at`, as (start, end) caret positions.
/// Letters, digits and `_` make up words; any other character is a word on its own.
fn word_at(value: &str, at: usize) -> (usize, usize) {
    let chars: Vec<char> = value.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    // The character the caret is before, or after at the very end.
    let Some(i) = (at < chars.len()).then_some(at).or(at.checked_sub(1)) else { return (0, 0) };
    if !is_word(chars[i]) {
        return (i, i + 1);
    }
    let start = chars[..i].iter().rposition(|&c| !is_word(c)).map_or(0, |p| p + 1);
    let end = chars[i..].iter().position(|&c| !is_word(c)).map_or(chars.len(), |p| i + p);
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(cursor: usize) -> Focus {
        Focus { id: 0, cursor, anchor: cursor }
    }

    fn selecting(anchor: usize, cursor: usize) -> Focus {
        Focus { id: 0, cursor, anchor }
    }

    #[test]
    fn editing_keys() {
        let text = |s: &str| Key::Text(s.into());
        let select = |cursor, anchor| Some(Edit::Select { cursor, anchor });
        assert_eq!(edit("ac", at(1), &text("b"), false), Some(Edit::Change("abc".into(), 2)));
        assert_eq!(edit("héllo", at(2), &Key::Backspace, false), Some(Edit::Change("hllo".into(), 1)));
        assert_eq!(edit("abc", at(0), &Key::Backspace, false), select(0, 0));
        assert_eq!(edit("abc", at(1), &Key::Delete, false), Some(Edit::Change("ac".into(), 1)));
        assert_eq!(edit("abc", at(3), &Key::Delete, false), select(3, 3));
        assert_eq!(edit("abc", at(3), &Key::Right, false), select(3, 3));
        assert_eq!(edit("abc", at(9), &Key::Left, false), select(2, 2), "a stale cursor is clamped");
        assert_eq!(edit("abc", at(1), &Key::Home, false), select(0, 0));
        assert_eq!(edit("abc", at(1), &Key::End, false), select(3, 3));
        assert_eq!(edit("abc", at(1), &Key::Down, false), None, "left for scrolling");
    }

    #[test]
    fn editing_selections() {
        let text = |s: &str| Key::Text(s.into());
        let select = |cursor, anchor| Some(Edit::Select { cursor, anchor });
        // Shift extends from the anchor, in either direction.
        assert_eq!(edit("abcd", at(1), &Key::Right, true), select(2, 1));
        assert_eq!(edit("abcd", selecting(1, 2), &Key::End, true), select(4, 1));
        assert_eq!(edit("abcd", selecting(3, 2), &Key::Left, true), select(1, 3));
        assert_eq!(edit("abcd", at(2), &Key::SelectAll, false), select(4, 0));
        // Without shift, arrows collapse the selection to one end.
        assert_eq!(edit("abcd", selecting(1, 3), &Key::Left, false), select(1, 1));
        assert_eq!(edit("abcd", selecting(3, 1), &Key::Right, false), select(3, 3));
        // Typing and deleting replace the selection, whichever way it was made.
        assert_eq!(edit("abcd", selecting(3, 1), &text("X"), false), Some(Edit::Change("aXd".into(), 2)));
        assert_eq!(edit("abcd", selecting(1, 3), &Key::Backspace, false), Some(Edit::Change("ad".into(), 1)));
        assert_eq!(edit("abcd", selecting(0, 4), &Key::Delete, false), Some(Edit::Change("".into(), 0)));
    }

    #[test]
    fn words() {
        let path = "pages/hello.sx";
        assert_eq!(word_at(path, 7), (6, 11), "hello");
        assert_eq!(word_at(path, 6), (6, 11), "at the start of a word");
        assert_eq!(word_at(path, 5), (5, 6), "punctuation is its own word");
        assert_eq!(word_at(path, 14), (12, 14), "after the last character");
        assert_eq!(word_at("", 0), (0, 0));
    }

    #[test]
    fn typing_into_a_page() {
        let dir = std::env::temp_dir().join(format!("toy-browser-input-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("form.sx");
        std::fs::write(
            &file,
            r#"(define-state name "") (define-state sent "")
               (page :padding 0
                 (input :value name :on-input (fn (t) (set! name t)) :on-submit (fn (t) (set! sent t)))
                 (p "sent: " sent))"#,
        )
        .unwrap();
        let mut browser = Browser::new(file.to_string_lossy().into(), false);
        let mut buf = vec![0; 400 * 200];
        let mut render = |browser: &mut Browser| browser.render(&mut buf, 400, 200, 1.0);

        render(&mut browser);
        assert!(browser.click(10.0, 10.0), "focus the input");
        for key in [Key::Text("hi".into()), Key::Left, Key::Text("!".into()), Key::End, Key::Backspace, Key::Enter] {
            render(&mut browser);
            browser.key(key, false, (0.0, 0.0));
        }
        render(&mut browser);
        assert_eq!(browser.inputs[0].value, "h!");
        assert!(browser.page.to_string().contains(r#""sent: " "h!""#), "{}", browser.page);

        browser.key(Key::Escape, false, (0.0, 0.0));
        browser.key(Key::Text("x".into()), false, (0.0, 0.0));
        render(&mut browser);
        assert_eq!(browser.inputs[0].value, "h!", "typing does nothing without focus");
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn render(browser: &mut Browser) {
        let mut buf = vec![0; 900 * 600];
        browser.render(&mut buf, 900, 600, 1.0);
    }

    fn press(browser: &mut Browser, keys: impl IntoIterator<Item = Key>) {
        for key in keys {
            render(browser);
            browser.key(key, false, (0.0, 0.0));
        }
        render(browser);
    }

    #[test]
    fn typing_a_location_into_the_address_bar() {
        let mut browser = Browser::new("pages/hello.sx".into(), true);
        press(&mut browser, [Key::FocusAddressBar]);
        assert_eq!(browser.inputs[0].value, "pages/hello.sx");
        assert_eq!(browser.focus, Some(Focus { id: 0, cursor: 14, anchor: 0 }), "Ctrl+L selects the address");

        press(&mut browser, [Key::Text("pages/counter.sx".into()), Key::Enter]);
        assert_eq!(browser.site, "pages/counter.sx");
        assert!(browser.title().starts_with("Counter"), "{}", browser.title());
        assert_eq!(browser.inputs[0].value, "pages/counter.sx", "the address bar shows the new location");
        assert_eq!(browser.focus, None);

        press(&mut browser, [Key::Back]);
        assert_eq!(browser.site, "pages/hello.sx");
        assert_eq!(browser.inputs[0].value, "pages/hello.sx");
    }

    #[test]
    fn handlers_can_navigate_with_go() {
        let dir = std::env::temp_dir().join(format!("toy-browser-go-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("from.sx"), r#"(page :padding 0 (button :on-click (fn () (go "to.sx")) "Go"))"#).unwrap();
        std::fs::write(dir.join("to.sx"), r#"(go "elsewhere.sx") (page :title "Arrived")"#).unwrap();
        let from = dir.join("from.sx").to_string_lossy().into_owned();

        let mut browser = Browser::new(from, false);
        render(&mut browser);
        assert!(browser.click(5.0, 5.0));
        assert_eq!(browser.title(), "Arrived — Toy Browser", "resolved relative to the page, and not followed again");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn clicking_the_address_bar_selects_it() {
        let mut browser = Browser::new("pages/hello.sx".into(), true);
        render(&mut browser);
        let address = browser.inputs[0].area;
        let (x, y) = (address.x0 + 30.0, (address.y0 + address.y1) / 2.0);

        assert!(browser.click(x, y));
        assert_eq!(browser.focus, Some(Focus { id: 0, cursor: 14, anchor: 0 }), "all of it");
        render(&mut browser);
        assert!(browser.click(x, y));
        let caret = browser.focus.unwrap();
        assert!(caret.cursor == caret.anchor && caret.cursor > 0, "a second click places the caret: {caret:?}");

        // Typing over a selection replaces it.
        browser.press(x, y, 3);
        press(&mut browser, [Key::Text("pages/inputs.sx".into()), Key::Enter]);
        assert_eq!(browser.site, "pages/inputs.sx");
    }

    #[test]
    fn selecting_with_the_mouse() {
        let mut browser = Browser::new("pages/inputs.sx".into(), false);
        render(&mut browser);
        // Focus the name input and type into it, then select with the mouse.
        let name = browser.inputs[0].area;
        let y = (name.y0 + name.y1) / 2.0;
        browser.click(name.x0 + 5.0, y);
        press(&mut browser, [Key::Text("Ada Lovelace".into())]);
        let xs = browser.inputs[0].caret_xs.clone();

        browser.press(xs[1], y, 1);
        assert!(browser.drag_to(xs[3]));
        assert!(browser.release(xs[3], y));
        assert_eq!(browser.focus.map(|f| (f.anchor, f.cursor)), Some((1, 3)));

        browser.press(xs[6], y, 1);
        browser.press(xs[6], y, 2);
        assert!(!browser.drag_to(xs[0]), "only a single click starts a drag");
        assert_eq!(browser.focus.map(|f| (f.anchor, f.cursor)), Some((4, 12)), "double-click selects a word");

        browser.press(xs[6], y, 3);
        assert_eq!(browser.focus.map(|f| (f.anchor, f.cursor)), Some((0, 12)), "triple-click selects everything");
    }

    #[test]
    fn dark_mode_is_a_browser_setting() {
        let mut browser = Browser::new("pages/hello.sx".into(), true);
        render(&mut browser);
        let toggle = browser.links.iter().find(|link| matches!(link.action, Action::Run(_))).unwrap();
        let (x, y) = (toggle.x + 1.0, toggle.y + 1.0);
        assert!(browser.click(x, y));
        assert!(browser.dark_mode(), "the toolbar's button turns it on");

        press(&mut browser, [Key::FocusAddressBar, Key::Text("pages/about.sx".into()), Key::Enter]);
        press(&mut browser, [Key::Back]);
        assert!(browser.dark_mode(), "it outlasts navigation");
    }

    #[test]
    fn only_browser_pages_change_settings() {
        let dir = std::env::temp_dir().join(format!("toy-browser-dark-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sneaky.sx");
        std::fs::write(&file, r#"(page :padding 0 (button :on-click (fn () (set-dark-mode true)) "Go dark"))"#).unwrap();
        let mut browser = Browser::new(file.to_string_lossy().into(), false);
        render(&mut browser);
        browser.click(5.0, 5.0);
        assert!(!browser.dark_mode());
        assert!(browser.page.to_string().contains("only browser: pages"), "{}", browser.page);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn handler_errors_leave_the_page_usable() {
        // The toolbar's dark mode button comes first, then the page's three.
        let button = |browser: &Browser, i: usize| {
            let link = browser.links.iter().filter(|l| matches!(l.action, Action::Run(_))).nth(i).unwrap();
            (link.x + 1.0, link.y + 1.0)
        };
        let mut browser = Browser::new("pages/runaway.sx".into(), true);
        render(&mut browser);
        let forever = button(&browser, 1);

        browser.click(forever.0, forever.1);
        render(&mut browser);
        assert!(browser.error.as_deref().is_some_and(|e| e.contains("levels of nesting")));
        assert!(browser.page.to_string().contains("levels of nesting"), "the chrome shows it");
        assert_eq!(browser.site, "pages/runaway.sx", "and the page is still there");

        // The banner moved the page down.
        let deep = button(&browser, 3);
        browser.click(deep.0, deep.1);
        render(&mut browser);
        assert_eq!(browser.error, None, "the next thing that works clears it");
        assert!(browser.page.to_string().contains(r#"(b "done")"#));
    }

    /// Clicks the last link whose action satisfies `which`. The site comes
    /// after the toolbar, so this prefers its links.
    fn click_link(browser: &mut Browser, which: impl Fn(&Action) -> bool) {
        render(browser);
        let link = browser.links.iter().rfind(|link| which(&link.action)).expect("a link to click");
        let (x, y) = (link.x + 1.0, link.y + 1.0);
        assert!(browser.click(x, y));
        render(browser);
    }

    fn to(href: &'static str) -> impl Fn(&Action) -> bool {
        move |action| matches!(action, Action::Go { href: h, .. } if &**h == href)
    }

    #[test]
    fn browsing_the_web() {
        let server = crate::test_server::TestServer::start(&[
            ("index.sx", r#"(page :title "Remote" (p (a :href "other.sx" "other")) (frame :src "/sub/framed.sx") (p "forms: " (length (source "other.sx"))))"#),
            ("other.sx", r#"(page :title "Other")"#),
            ("sub/framed.sx", r#"(page (a :href "next.sx" "next"))"#),
            ("sub/next.sx", r#"(page :title "Next")"#),
            ("sub/index.sx", r#"(page :title "Moved here" (a :href "next.sx" "next"))"#),
        ]);
        let mut browser = Browser::new(server.url("index.sx"), true);
        render(&mut browser);
        assert_eq!(browser.title(), "Remote — Toy Browser");
        assert!(browser.page.to_string().contains(r#""forms: " "1""#), "source reads other pages: {}", browser.page);

        click_link(&mut browser, to("other.sx"));
        assert_eq!(browser.site, server.url("other.sx"));
        assert_eq!(browser.title(), "Other — Toy Browser");

        press(&mut browser, [Key::Back]);
        click_link(&mut browser, to("next.sx"));
        assert_eq!(browser.site, server.url("sub/next.sx"), "relative to the framed page");

        press(&mut browser, [Key::FocusAddressBar, Key::Text(server.url("moved")), Key::Enter]);
        assert_eq!(browser.title(), "Moved here — Toy Browser");
        click_link(&mut browser, to("next.sx"));
        assert_eq!(browser.site, server.url("sub/next.sx"), "relative to where the redirect led");
    }

    #[test]
    fn web_pages_cannot_reach_local_ones() {
        let server = crate::test_server::TestServer::start(&[(
            "sneaky.sx",
            r#"(page (frame :src "browser:chrome")
                     (p (button :on-click (fn () (source "browser:chrome")) "Read"))
                     (p (a :href "browser:view-source?page=pages/hello.sx" "Peek")))"#,
        )]);
        let mut browser = Browser::new(server.url("sneaky.sx"), true);
        render(&mut browser);
        assert!(browser.page.to_string().contains("is a web page, so it can't open browser:chrome"), "no framing");

        click_link(&mut browser, |action| matches!(action, Action::Run(_)));
        assert!(browser.error.as_deref().is_some_and(|e| e.contains("can't open browser:chrome")), "no reading");

        click_link(&mut browser, to("browser:view-source?page=pages/hello.sx"));
        assert!(browser.error.as_deref().is_some_and(|e| e.contains("can't open browser:view-source")), "no linking");
        assert_eq!(browser.site, server.url("sneaky.sx"));
    }

    #[test]
    fn downloads_are_reused_until_navigating() {
        let server = crate::test_server::TestServer::start(&[(
            "form.sx",
            r#"(define-state text "") (page :padding 0 (input :value text :on-input (fn (t) (set! text t))))"#,
        )]);
        let mut browser = Browser::new(server.url("form.sx"), false);
        render(&mut browser);
        assert!(browser.click(5.0, 5.0));
        press(&mut browser, [Key::Text("a".into()), Key::Text("b".into()), Key::Text("c".into())]);
        assert_eq!(browser.inputs[0].value, "abc");
        assert_eq!(server.requests(), 1, "typing runs the page again, but doesn't download it again");

        press(&mut browser, [Key::Reload]);
        assert_eq!(server.requests(), 2);
        assert_eq!(browser.inputs[0].value, "", "reloading starts the page's state over");
    }

    #[test]
    fn failed_downloads_show_why() {
        let server = crate::test_server::TestServer::start(&[("page.html", "<html><body>hi</body></html>")]);
        let mut browser = Browser::new(server.url("missing.sx"), true);
        render(&mut browser);
        assert!(browser.page.to_string().contains("404"), "{}", browser.page);

        let mut browser = Browser::new(server.url("page.html"), true);
        render(&mut browser);
        assert!(browser.page.to_string().contains("looks like HTML"));
    }
}

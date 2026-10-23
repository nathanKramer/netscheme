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
    loader::{self, StateStore},
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
    /// Put the caret in the address bar: the chrome's first input.
    FocusAddressBar,
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
    /// Sites to return to with "back", with their scroll offsets and state, most recent last.
    history: Vec<(String, Scroll, Rc<StateStore>)>,
    scroll: Scroll,
    focus: Option<Focus>,
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
            history: Vec::new(),
            scroll: Scroll::default(),
            focus: None,
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

    pub fn title(&self) -> String {
        match self.page.attr("title") {
            Some(Value::Str(t)) => format!("{t} — Toy Browser"),
            _ => "Toy Browser".into(),
        }
    }

    /// Draws the page into `buf`, which is `w` × `h` physical pixels.
    pub fn render(&mut self, buf: &mut [u32], w: usize, h: usize, scale: f32) {
        let caret_visible = (self.caret_moved.elapsed().as_millis() / BLINK.as_millis()).is_multiple_of(2);
        let view = View { scale, scroll: &self.scroll, focus: self.focus, caret_visible };
        let rendered = self.renderer.render(&self.page, buf, w, h, &view);
        (self.links, self.scrollers, self.inputs, self.scroll) = (rendered.links, rendered.scrollers, rendered.inputs, rendered.scroll);
        self.viewport = (h as f32, scale);
        // The page may have changed under the focused input.
        self.focus = self.focus.and_then(|focus| {
            let input = self.inputs.iter().find(|input| input.id == focus.id)?;
            Some(Focus { cursor: focus.cursor.min(input.caret_xs.len() - 1), ..focus })
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

    /// Clicks at a point. Returns whether there was anything there to click.
    pub fn click(&mut self, x: f32, y: f32) -> bool {
        if let Some(input) = self.input_at(x, y) {
            let distance = |i: usize| (input.caret_xs[i] - x).abs();
            let cursor = (0..input.caret_xs.len()).min_by(|&a, &b| distance(a).total_cmp(&distance(b))).unwrap_or(0);
            self.set_focus(Focus { id: input.id, cursor });
            return true;
        }
        self.focus = None;
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
            _ => {}
        }
        if let Some(focus) = self.focus
            && self.edit(focus, &key)
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
    fn edit(&mut self, focus: Focus, key: &Key) -> bool {
        let Some(input) = self.inputs.iter().find(|input| input.id == focus.id) else {
            self.focus = None;
            return false;
        };
        let (element, value) = (input.element.clone(), input.value.clone());
        match edit(&value, focus.cursor, key) {
            None => return false,
            Some(Edit::Move(cursor)) => self.set_focus(Focus { cursor, ..focus }),
            Some(Edit::Change(text, cursor)) => {
                // The input shows whatever value the page gives it, so typing
                // only does something if the page listens.
                if let Some(handler) = element.attr("on-input").cloned() {
                    self.set_focus(Focus { cursor, ..focus });
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
                    self.set_focus(Focus { id: next.id, cursor: next.caret_xs.len() - 1 });
                }
            }
        }
        true
    }

    fn focus_address_bar(&mut self) {
        let first = self.inputs.iter().min_by_key(|input| input.id);
        if let Some(input) = first.filter(|_| self.chrome) {
            self.set_focus(Focus { id: input.id, cursor: input.caret_xs.len() - 1 });
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
        format!("browser:chrome?location={}{back}", loader::encode(&self.site))
    }

    /// Links on the chrome, and in the site framed inside it, all navigate the site.
    fn follow(&mut self, href: &str, base: Option<&str>) {
        if href == BACK {
            return self.back();
        }
        let site = loader::resolve(base.unwrap_or(&self.location()), href);
        let previous = (
            std::mem::replace(&mut self.site, site),
            std::mem::take(&mut self.scroll),
            std::mem::take(&mut self.page_state),
        );
        self.history.push(previous);
        self.focus = None;
        self.reload();
    }

    pub fn back(&mut self) {
        if let Some((site, scroll, page_state)) = self.history.pop() {
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
            Ok(_) => match self.page_state.take_navigation() {
                Some((href, from)) => self.follow(&href, Some(&from)),
                None => self.reload(),
            },
            Err(e) => self.page = loader::error_page(&format!("in an event handler: {e}")),
        }
    }

    /// Runs the page, keeping its state, scroll offsets and focus.
    fn reload(&mut self) {
        self.page = loader::open(&self.location(), &self.page_state);
        // `go` only means something in an event handler, not while the page runs.
        self.page_state.take_navigation();
    }
}

/// What a key does to a text input.
#[derive(Debug, PartialEq)]
enum Edit {
    /// Move the caret to this position.
    Move(usize),
    /// Change the text, with the caret at this position afterwards.
    Change(String, usize),
    Submit,
    Blur,
    /// Focus the next input.
    Next,
}

/// What `key` does to an input holding `value`, with the caret at `cursor`
/// characters in. `None` if the input doesn't use the key.
fn edit(value: &str, cursor: usize, key: &Key) -> Option<Edit> {
    let chars: Vec<char> = value.chars().collect();
    let c = cursor.min(chars.len());
    let without = |i: usize| chars.iter().enumerate().filter(|&(j, _)| j != i).map(|(_, ch)| ch).collect();
    Some(match key {
        Key::Left => Edit::Move(c.saturating_sub(1)),
        Key::Right => Edit::Move((c + 1).min(chars.len())),
        Key::Home => Edit::Move(0),
        Key::End => Edit::Move(chars.len()),
        Key::Backspace if c > 0 => Edit::Change(without(c - 1), c - 1),
        Key::Delete if c < chars.len() => Edit::Change(without(c), c),
        Key::Backspace | Key::Delete => Edit::Move(c),
        Key::Enter => Edit::Submit,
        Key::Escape => Edit::Blur,
        Key::Tab => Edit::Next,
        Key::Text(text) => {
            let mut changed: String = chars[..c].iter().collect();
            changed.push_str(text);
            changed.extend(&chars[c..]);
            Edit::Change(changed, c + text.chars().count())
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_keys() {
        let text = |s: &str| Key::Text(s.into());
        assert_eq!(edit("ac", 1, &text("b")), Some(Edit::Change("abc".into(), 2)));
        assert_eq!(edit("héllo", 2, &Key::Backspace), Some(Edit::Change("hllo".into(), 1)));
        assert_eq!(edit("abc", 0, &Key::Backspace), Some(Edit::Move(0)));
        assert_eq!(edit("abc", 1, &Key::Delete), Some(Edit::Change("ac".into(), 1)));
        assert_eq!(edit("abc", 3, &Key::Delete), Some(Edit::Move(3)));
        assert_eq!(edit("abc", 3, &Key::Right), Some(Edit::Move(3)));
        assert_eq!(edit("abc", 9, &Key::Left), Some(Edit::Move(2)), "a stale cursor is clamped");
        assert_eq!(edit("abc", 1, &Key::Home), Some(Edit::Move(0)));
        assert_eq!(edit("abc", 1, &Key::End), Some(Edit::Move(3)));
        assert_eq!(edit("abc", 1, &Key::Down), None, "left for scrolling");
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

        let erase = std::iter::repeat_n(Key::Backspace, "hello.sx".len());
        press(&mut browser, erase.chain([Key::Text("counter.sx".into()), Key::Enter]));
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
}

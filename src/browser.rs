//! A browsing session: the site being shown, its history and state, and
//! what the user is doing with it. It knows nothing about windows: the
//! window forwards events to it, and screenshots drive it directly.

use std::{
    rc::Rc,
    time::{Duration, Instant},
};

use crate::{
    dom::Element,
    layout::{Action, Focus, InputBox, Link, Match, Scrollbar, Scroller, Search},
    lisp::{self, Value},
    loader::{self, FindRequest, Host, ReplEntry, ReplRequest, StateStore},
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
    /// Open the find bar, or select what's in it if it's open.
    Find,
    /// Go to the next match, or with shift the previous one.
    FindNext,
    /// Open the REPL, or close it if it's open.
    Repl,
}

/// The search in the find bar.
#[derive(Debug, Default)]
struct Find {
    query: String,
    /// Which match is current, counting from 0 in layout order.
    current: usize,
    /// How many matches the chrome was last told about.
    count: usize,
}

/// What's under the pointer, for choosing its shape.
pub enum Hover {
    Nothing,
    Link,
    Text,
}

/// Something that scrolls: the page, or a frame, by its `Scroller::id`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Scrolling {
    Page,
    Frame(usize),
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
    /// A scrollbar being dragged, and how far below the top of its thumb it was grabbed.
    scroll_drag: Option<(Scrolling, f32)>,
    /// The search, while the find bar is open.
    find: Option<Find>,
    /// Whether to scroll the current match into view after the next render.
    reveal: bool,
    /// Whether the REPL is open.
    repl: bool,
    /// Inputs with `:autofocus` in the last render, which are only focused
    /// when they first appear; and whether to focus one anyway.
    autofocused: Vec<usize>,
    refocus: bool,
    /// When the caret last moved; it blinks starting from then.
    caret_moved: Instant,
    renderer: Renderer,
    // From the last render:
    links: Vec<Link>,
    scrollers: Vec<Scroller>,
    inputs: Vec<InputBox>,
    bar: Option<Scrollbar>,
    matches: Vec<Match>,
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
            host: Rc::new(Host::new(if chrome { "/0" } else { "" })),
            history: Vec::new(),
            scroll: Scroll::default(),
            focus: None,
            error: None,
            dragging: false,
            scroll_drag: None,
            find: None,
            reveal: false,
            repl: false,
            autofocused: Vec::new(),
            refocus: false,
            caret_moved: Instant::now(),
            renderer: Renderer::new(),
            links: Vec::new(),
            scrollers: Vec::new(),
            inputs: Vec::new(),
            bar: None,
            matches: Vec::new(),
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
            Some(Value::Str(t)) => format!("{t} — Netscheme"),
            _ => "Netscheme".into(),
        }
    }

    /// Draws the page into `buf`, which is `w` × `h` physical pixels.
    pub fn render(&mut self, buf: &mut [u32], w: usize, h: usize, scale: f32) {
        self.render_once(buf, w, h, scale);
        // Finding text can mean drawing again: once the chrome has been told
        // how many matches there are, or to scroll the current one into view.
        if self.after_search() {
            self.render_once(buf, w, h, scale);
        }
        self.autofocus();
    }

    fn render_once(&mut self, buf: &mut [u32], w: usize, h: usize, scale: f32) {
        let caret_visible = (self.caret_moved.elapsed().as_millis() / BLINK.as_millis()).is_multiple_of(2);
        let search = self.find.as_ref().map(|find| Search { query: &find.query, current: find.current, frames_only: self.chrome });
        let images = self.host.images.borrow();
        let dark = self.host.dark_mode.get();
        let view = View { scale, scroll: &self.scroll, focus: self.focus, caret_visible, dark, search, images: Some(&images) };
        let rendered = self.renderer.render(&self.page, buf, w, h, &view);
        drop(images);
        (self.links, self.scrollers, self.inputs, self.scroll) = (rendered.links, rendered.scrollers, rendered.inputs, rendered.scroll);
        (self.bar, self.matches) = (rendered.bar, rendered.matches);
        self.viewport = (h as f32, scale);
        // The page may have changed under the focused input.
        self.focus = self.focus.and_then(|focus| {
            let input = self.inputs.iter().find(|input| input.id == focus.id)?;
            let len = input.caret_xs.len() - 1;
            Some(Focus { cursor: focus.cursor.min(len), anchor: focus.anchor.min(len), ..focus })
        });
    }

    /// Catches up with what the last render found. Returns whether to draw again.
    fn after_search(&mut self) -> bool {
        let Some(find) = &mut self.find else { return false };
        let count = self.matches.len();
        if find.current >= count {
            find.current = 0;
        }
        let told = std::mem::replace(&mut find.count, count) == count;
        let current = find.current;
        if !told {
            self.reload();
        }
        let revealed = std::mem::take(&mut self.reveal) && self.reveal_match(current);
        !told || revealed
    }

    /// Scrolls whatever the match is in so that it's in view. Returns whether that scrolled.
    fn reveal_match(&mut self, i: usize) -> bool {
        let Some(m) = self.matches.get(i).copied() else { return false };
        let (top, bottom, scroll) = match m.frame.and_then(|id| self.scrollers.iter().find(|s| s.id == id)) {
            Some(s) => (s.area.y0, s.area.y1, s.scroll),
            None => (0.0, self.viewport.0, self.scroll.page),
        };
        if m.area.y0 >= top && m.area.y1 <= bottom {
            return false;
        }
        // A third of the way down, so there's some context above it.
        let target = scroll + m.area.y0 - top - (bottom - top) / 3.0;
        match m.frame {
            Some(id) => _ = self.scroll.frames.insert(id, target),
            None => self.scroll.page = target,
        }
        true
    }

    /// Focuses an input with `:autofocus` when it first appears, or when asked to.
    fn autofocus(&mut self) {
        let autofocus = |input: &&InputBox| input.element.attr("autofocus").is_some_and(Value::truthy);
        let ids: Vec<usize> = self.inputs.iter().filter(autofocus).map(|input| input.id).collect();
        let new = ids.iter().find(|id| !self.autofocused.contains(id)).copied();
        let target = new.or(ids.first().copied().filter(|_| self.refocus));
        if let Some(input) = target.and_then(|id| self.inputs.iter().find(|input| input.id == id)) {
            let len = input.caret_xs.len() - 1;
            self.set_focus(Focus { id: input.id, cursor: len, anchor: 0 });
        }
        self.refocus = false;
        self.autofocused = ids;
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

    /// The scrollbar at a point: the innermost frame's, or else the page's.
    fn scrollbar_at(&self, x: f32, y: f32) -> Option<(Scrolling, Scrollbar)> {
        let area = |s: &&Scroller| (s.area.x1 - s.area.x0) * (s.area.y1 - s.area.y0);
        let frame = self
            .scrollers
            .iter()
            .filter(|s| s.area.contains(x, y) && s.bar.is_some_and(|bar| bar.track.contains(x, y)))
            .min_by(|a, b| area(a).total_cmp(&area(b)));
        match frame {
            Some(s) => Some((Scrolling::Frame(s.id), s.bar?)),
            None => self.bar.filter(|bar| bar.track.contains(x, y)).map(|bar| (Scrolling::Page, bar)),
        }
    }

    /// Moves a dragged scrollbar's thumb so that the point it was grabbed at is at `y`.
    fn drag_scrollbar(&mut self, (target, grab): (Scrolling, f32), y: f32) {
        let bar = match target {
            Scrolling::Page => self.bar,
            Scrolling::Frame(id) => self.scrollers.iter().find(|s| s.id == id).and_then(|s| s.bar),
        };
        let Some(bar) = bar else { return };
        let scroll = bar.scroll_for(y - grab);
        match target {
            Scrolling::Page => self.scroll.page = scroll,
            Scrolling::Frame(id) => _ = self.scroll.frames.insert(id, scroll),
        }
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
        // On a scrollbar, the thumb can be dragged. Pressing elsewhere on its
        // track jumps there, with the thumb centred on the pointer.
        if let Some((target, bar)) = self.scrollbar_at(x, y) {
            let grab = if bar.thumb.contains(x, y) { y - bar.thumb.y0 } else { (bar.thumb.y1 - bar.thumb.y0) / 2.0 };
            self.scroll_drag = Some((target, grab));
            self.drag_scrollbar((target, grab), y);
            return false;
        }
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

    /// The mouse moved to a point. Returns whether that scrolled or changed the selection.
    pub fn drag_to(&mut self, x: f32, y: f32) -> bool {
        if let Some(drag) = self.scroll_drag {
            self.drag_scrollbar(drag, y);
            return true;
        }
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
        if self.scroll_drag.take().is_some() {
            return true;
        }
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
                self.host.images.borrow_mut().clear();
                self.page_state = Rc::default();
                (self.focus, self.error) = (None, None);
                return self.reload();
            }
            // The find bar is part of the chrome, so there's none without it.
            Key::Find if self.chrome => {
                self.find.get_or_insert_default();
                self.refocus = true;
                return self.reload();
            }
            Key::FindNext if self.find.is_some() => {
                self.find_request(if shift { FindRequest::Previous } else { FindRequest::Next });
                return self.reload();
            }
            // The REPL is part of the chrome too.
            Key::Repl if self.chrome => {
                self.repl = !self.repl;
                return self.reload();
            }
            Key::Escape if self.focus.is_none() && self.find.is_some() => {
                self.find_request(FindRequest::Close);
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
                // Shift+Enter has a handler of its own, if the input wants one.
                let shifted = element.attr("on-shift-submit").filter(|_| shift);
                if let Some(handler) = shifted.or(element.attr("on-submit")).cloned() {
                    self.run(&handler, &[Value::Str(value.into())]);
                }
            }
            Some(Edit::Blur) => {
                self.focus = None;
                if let Some(handler) = element.attr("on-escape").cloned() {
                    self.run(&handler, &[]);
                }
            }
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
        let find = self.find.as_ref().map_or(String::new(), |find| {
            format!("&find={}&found={}&at={}", loader::encode(&find.query), find.count, find.current + 1)
        });
        let repl = if self.repl { "&repl=1" } else { "" };
        format!("browser:chrome?location={}{back}{error}{find}{repl}", loader::encode(&self.site))
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
        let result = lisp::apply(handler, args);
        if let Some(request) = self.host.take_find().filter(|_| result.is_ok()) {
            self.find_request(request);
        }
        // Before following `go`, so code run in the REPL can navigate.
        if let Some(request) = self.host.take_repl().filter(|_| result.is_ok()) {
            self.repl_request(request);
        }
        match result {
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

    fn find_request(&mut self, request: FindRequest) {
        let Some(find) = &mut self.find else { return };
        let count = find.count.max(1);
        match request {
            FindRequest::Query(query) => *find = Find { query, current: 0, count: find.count },
            FindRequest::Next => find.current = (find.current + 1) % count,
            FindRequest::Previous => find.current = (find.current + count - 1) % count,
            FindRequest::Close => self.find = None,
        }
        self.reveal = self.find.is_some();
    }

    /// Runs code from the REPL in the site's environment from its latest
    /// run, so it sees the site's variables and can `set!` its state.
    fn repl_request(&mut self, request: ReplRequest) {
        match request {
            ReplRequest::Eval(input) => {
                let output = match self.host.site_env() {
                    Some(env) => lisp::eval_in(&input, &env).map(|value| format!("{value:#}")),
                    None => Err("the page hasn't run yet".into()),
                };
                self.host.repl_log.borrow_mut().push(ReplEntry { input, output });
            }
            ReplRequest::Close => self.repl = false,
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
        assert_eq!(browser.title(), "Arrived — Netscheme", "resolved relative to the page, and not followed again");
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
        assert!(browser.drag_to(xs[3], y));
        assert!(browser.release(xs[3], y));
        assert_eq!(browser.focus.map(|f| (f.anchor, f.cursor)), Some((1, 3)));

        browser.press(xs[6], y, 1);
        browser.press(xs[6], y, 2);
        assert!(!browser.drag_to(xs[0], y), "only a single click starts a drag");
        assert_eq!(browser.focus.map(|f| (f.anchor, f.cursor)), Some((4, 12)), "double-click selects a word");

        browser.press(xs[6], y, 3);
        assert_eq!(browser.focus.map(|f| (f.anchor, f.cursor)), Some((0, 12)), "triple-click selects everything");
    }

    /// A page of 200 lines, far taller than the 600px test window, in a
    /// folder of its own, next to a page that frames it in a 300px frame.
    /// Returns the folder.
    fn tall_pages(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("toy-browser-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tall.sx"), r#"(page (map (fn (i) (p (a :href "elsewhere.sx" "line " i))) (range 200)))"#).unwrap();
        std::fs::write(dir.join("framed.sx"), r#"(page (p "Above") (frame :src "tall.sx" :height 300))"#).unwrap();
        dir
    }

    #[test]
    fn dragging_the_page_scrollbar() {
        let dir = tall_pages("scrollbar");
        let mut browser = Browser::new(dir.join("tall.sx").to_string_lossy().into(), false);
        render(&mut browser);
        let bar = browser.bar.expect("a scrollbar");
        assert!(bar.thumb.x1 <= 900.0 && bar.thumb.x0 > 880.0, "at the right edge: {bar:?}");
        assert_eq!(bar.thumb.y0, bar.track.y0, "at the top");

        // Drag the thumb past the bottom.
        let x = (bar.thumb.x0 + bar.thumb.x1) / 2.0;
        assert!(!browser.press(x, bar.thumb.y0 + 5.0, 1));
        assert!(browser.drag_to(x, 2000.0));
        assert!(browser.release(x, 2000.0), "the release ends the drag");
        render(&mut browser);
        assert!(browser.history.is_empty(), "rather than following a link under the scrollbar");
        assert_eq!(browser.scroll.page, bar.max_scroll.round(), "scrolled to the end");
        assert_eq!(browser.bar.unwrap().thumb.y1, bar.track.y1, "with the thumb at the bottom");

        // Pressing the track jumps to that point.
        browser.click(x, (bar.track.y0 + bar.track.y1) / 2.0);
        render(&mut browser);
        let middle = bar.max_scroll / 2.0;
        assert!((browser.scroll.page - middle).abs() < 2.0, "{} is about {middle}", browser.scroll.page);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn frames_have_scrollbars_too() {
        let dir = tall_pages("frame-scrollbar");
        let mut browser = Browser::new(dir.join("framed.sx").to_string_lossy().into(), false);
        render(&mut browser);
        assert_eq!(browser.bar, None, "the outer page fits, so it has none");
        let frame = &browser.scrollers[0];
        let (id, bar) = (frame.id, frame.bar.expect("the frame's scrollbar"));
        assert!(bar.track.y0 > frame.area.y0 && bar.track.y1 < frame.area.y1, "inside the frame");
        let x = (bar.thumb.x0 + bar.thumb.x1) / 2.0;
        browser.press(x, bar.thumb.y0 + 1.0, 1);
        browser.drag_to(x, bar.thumb.y0 + 101.0);
        browser.release(x, bar.thumb.y0 + 101.0);
        render(&mut browser);
        assert_eq!(browser.scroll.page, 0.0);
        assert_eq!(browser.scroll.frames[&id], bar.scroll_for(bar.thumb.y0 + 100.0).round());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn finding_text_in_the_page() {
        // The chrome frames the site by a relative location, so the page goes in `target`.
        let dir = format!("target/test-pages/find-{}", std::process::id());
        std::fs::create_dir_all(&dir).unwrap();
        let file = format!("{dir}/page.sx");
        let line = |i: usize| if [5, 150, 190].contains(&i) { format!("a Needle at {i}") } else { format!("line {i}") };
        let lines: Vec<String> = (0..200).map(|i| format!("(p {:?})", line(i))).collect();
        std::fs::write(&file, format!("(page {})", lines.join(" "))).unwrap();

        let mut browser = Browser::new(file, true);
        press(&mut browser, [Key::Find]);
        assert_eq!(browser.inputs.len(), 2, "the find bar opened");
        assert_eq!(browser.focus.map(|f| f.id), Some(browser.inputs[1].id), "with its input focused");

        press(&mut browser, [Key::Text("needle".into())]);
        assert_eq!(browser.matches.len(), 3, "ignoring case");
        assert!(browser.page.to_string().contains("1 of 3"), "the find bar says so");
        assert_eq!(browser.scroll.frames.get(&0).copied().unwrap_or(0.0), 0.0, "the first is already in view");

        press(&mut browser, [Key::Enter]);
        assert!(browser.page.to_string().contains("2 of 3"));
        let (site, current) = (&browser.scrollers[0], browser.matches[1].area);
        assert!(site.scroll > 0.0, "scrolled to the second");
        assert!(current.y0 >= site.area.y0 && current.y1 <= site.area.y1, "which is in view: {current:?} in {:?}", site.area);

        browser.key(Key::Enter, true, (0.0, 0.0));
        render(&mut browser);
        assert!(browser.page.to_string().contains("1 of 3"), "Shift+Enter goes back");
        press(&mut browser, [Key::FindNext, Key::FindNext, Key::FindNext]);
        assert!(browser.page.to_string().contains("1 of 3"), "and the matches wrap around");

        press(&mut browser, [Key::SelectAll, Key::Text("haystack".into())]);
        assert!(browser.page.to_string().contains("No matches"));

        press(&mut browser, [Key::Escape]);
        assert_eq!(browser.inputs.len(), 1, "Escape closes it");
        assert!(browser.matches.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_repl_runs_code_in_the_site() {
        let mut browser = Browser::new("pages/counter.sx".into(), true);
        press(&mut browser, [Key::Repl]);
        assert_eq!(browser.inputs.len(), 2, "the REPL opened");
        assert_eq!(browser.focus.map(|f| f.id), Some(browser.inputs[1].id), "with its input focused");

        press(&mut browser, [Key::Text("(set! count 41)".into()), Key::Enter, Key::Text("(* count 2)".into()), Key::Enter]);
        press(&mut browser, [Key::Text("(nope)".into()), Key::Enter]);
        let log: Vec<_> = browser.host.repl_log.borrow().iter().map(|e| e.output.clone()).collect();
        assert_eq!(log, [Ok("nil".into()), Ok("82".into()), Err("undefined symbol: nope".into())]);
        assert!(browser.title().starts_with("Counter: 41"), "set! changed the site's state: {}", browser.title());
        assert_eq!(browser.inputs[1].value, "", "and the input clears");

        press(&mut browser, [Key::Escape]);
        assert_eq!(browser.inputs.len(), 1, "Escape closes it");
        press(&mut browser, [Key::Repl]);
        assert!(browser.page.to_string().contains("undefined symbol: nope"), "the log outlasts closing");
    }

    #[test]
    fn dark_mode_is_a_browser_setting() {
        let mut browser = Browser::new("pages/hello.sx".into(), true);
        render(&mut browser);
        let toggle = browser.links.iter().find(|link| matches!(link.action, Action::Run(_))).unwrap();
        let (x, y) = (toggle.x + 1.0, toggle.y + 1.0);
        assert!(browser.click(x, y));
        assert!(browser.dark_mode(), "the toolbar's button turns it on");

        press(&mut browser, [Key::FocusAddressBar, Key::Text("pages/tour.sx".into()), Key::Enter]);
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
        assert_eq!(browser.title(), "Remote — Netscheme");
        assert!(browser.page.to_string().contains(r#""forms: " "1""#), "source reads other pages: {}", browser.page);

        click_link(&mut browser, to("other.sx"));
        assert_eq!(browser.site, server.url("other.sx"));
        assert_eq!(browser.title(), "Other — Netscheme");

        press(&mut browser, [Key::Back]);
        click_link(&mut browser, to("next.sx"));
        assert_eq!(browser.site, server.url("sub/next.sx"), "relative to the framed page");

        press(&mut browser, [Key::FocusAddressBar, Key::Text(server.url("moved")), Key::Enter]);
        assert_eq!(browser.title(), "Moved here — Netscheme");
        click_link(&mut browser, to("next.sx"));
        assert_eq!(browser.site, server.url("sub/next.sx"), "relative to where the redirect led");
    }

    #[test]
    fn images_load_from_files_and_the_web() {
        let mut browser = Browser::new("pages/hello.sx".into(), false);
        render(&mut browser);
        let logo = browser.host.images.borrow().get("pages/../assets/images/logo.png").cloned();
        assert_eq!(logo.expect("the home page shows the logo").map(|image| image.width), Ok(1254));

        let mut browser = Browser::new("pages/hello.sx".into(), true);
        render(&mut browser);
        let moon = browser.host.images.borrow().get("browser:icons/moon.png").cloned();
        assert!(moon.expect("the toolbar shows the mode switch").is_ok(), "from the browser's own folder");

        let server = crate::test_server::TestServer::start(&[("index.sx", r#"(page (img :src "logo.png" :width 40) (img :src "missing.png"))"#)]);
        server.add("logo.png", &std::fs::read("assets/images/logo.png").unwrap());
        let mut browser = Browser::new(server.url("index.sx"), false);
        render(&mut browser);
        let images = browser.host.images.borrow();
        assert!(images[&server.url("logo.png")].is_ok(), "downloaded relative to the page");
        assert!(images[&server.url("missing.png")].as_ref().is_err_and(|e| e.contains("404")), "{:?}", images[&server.url("missing.png")].as_ref().err());
        drop(images);

        let requests = server.requests();
        press(&mut browser, [Key::Tab, Key::Tab]);
        assert_eq!(server.requests(), requests, "images aren't downloaded again when the page runs again");
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

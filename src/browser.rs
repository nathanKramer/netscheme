//! A browsing session: the site being shown, its history and state, and
//! what the user is doing with it. It knows nothing about windows: the
//! window forwards events to it, and screenshots drive it directly.

use std::{
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

use crate::{
    dom::{self, Element, Node},
    layout::{Action, Anchor, Clip, Drawings, Focus, InputBox, KeyTarget, Link, Match, Scrollbar, Scroller, Search, TextLine},
    lisp::{
        self, Value,
        watch::{self, Touched},
    },
    loader::{self, FindRequest, Host, ReplEntry, ReplRequest, StateStore, Wake},
    paint::{Renderer, Scroll, View},
    settings::Settings,
};

/// Logical pixels scrolled per arrow key press or mouse wheel line.
pub const LINE_HEIGHT: f32 = 40.0;
/// Following a link to this location goes back instead.
const BACK: &str = "browser:back";
/// How long the caret stays on, then off.
const BLINK: Duration = Duration::from_millis(530);
/// Time between frames of animation: 60 a second.
pub const FRAME: Duration = Duration::from_micros(16_667);
/// The longest time a frame of animation is told has passed, so that after
/// a slow frame or a pause, things move on by a little rather than jump.
const MAX_FRAME_TIME: Duration = Duration::from_millis(100);

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

/// A way of going to another site.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Navigation {
    /// Following a link, with fresh state.
    Follow,
    /// Back to the last site in history, with its state as it was left.
    Back,
    /// The same site, downloaded again and with its state started over.
    Reload,
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
    /// The state the page read as it ran: if event handlers change none of
    /// it, running it again would make the same page.
    page_reads: Touched,
    /// What the page's canvases' `:draw` functions returned.
    drawings: Drawings,
    /// `define-state` variables of the page and its frames.
    page_state: Rc<StateStore>,
    /// Settings, and requests from pages, that outlast navigation.
    host: Rc<Host>,
    /// Where settings are kept between runs of the browser, if they are.
    settings: Option<Settings>,
    /// Sites to return to with "back", with their scroll offsets and state, most recent last.
    history: Vec<(String, Scroll, Rc<StateStore>)>,
    /// Where the browser is going, while the site there downloads in the
    /// background; the current site stays until it arrives.
    navigating: Option<(Navigation, String)>,
    scroll: Scroll,
    focus: Option<Focus>,
    /// The canvas that has the keyboard, by `KeyTarget::id`, and the keys
    /// it's been told went down and not yet that they went up.
    key_focus: Option<usize>,
    held: Vec<String>,
    /// The error from the last event handler, which the chrome shows until
    /// something else happens.
    error: Option<String>,
    /// Whether the mouse button went down in the focused input and is still
    /// down, so moving the mouse selects text.
    dragging: bool,
    /// The selected text on the page, as the position the selection started
    /// from and the one it reaches; see `TextLine`.
    selection: Option<(usize, usize)>,
    /// The frame the selection is in, which dragging stays inside, and
    /// whether the mouse button is down and selecting.
    selection_frame: Option<usize>,
    selecting: bool,
    /// A scrollbar being dragged, and how far below the top of its thumb it was grabbed.
    scroll_drag: Option<(Scrolling, f32)>,
    /// The search, while the find bar is open.
    find: Option<Find>,
    /// Whether to scroll the current match into view after the next render.
    reveal: bool,
    /// Whether to scroll to the element the site's location names after
    /// `#` after the next render, as it does on first opening the site and
    /// after following a link there.
    reveal_anchor: bool,
    /// Whether the REPL is open.
    repl: bool,
    /// Inputs with `:autofocus` in the last render, which are only focused
    /// when they first appear; and whether to focus one anyway.
    autofocused: Vec<usize>,
    refocus: bool,
    /// When the caret last moved; it blinks starting from then.
    caret_moved: Instant,
    /// When the last frame of animation ran, while the page is animating.
    last_frame: Option<Instant>,
    /// Whether an `:on-frame` handler failed, which stops animation until
    /// the page is next loaded.
    frames_failed: bool,
    renderer: Renderer,
    // From the last render:
    links: Vec<Link>,
    scrollers: Vec<Scroller>,
    inputs: Vec<InputBox>,
    key_targets: Vec<KeyTarget>,
    bar: Option<Scrollbar>,
    matches: Vec<Match>,
    lines: Vec<TextLine>,
    hover_areas: Vec<Clip>,
    anchors: Vec<Anchor>,
    /// Where the mouse pointer is, while it's in the window.
    pointer: Option<(f32, f32)>,
    /// Whether Ctrl is held, which links with `:ctrl-click` need.
    ctrl: bool,
    /// Height and scale of the window.
    viewport: (f32, f32),
}

impl Browser {
    pub fn new(site: String, chrome: bool) -> Browser {
        let mut browser = Browser::unloaded(site, chrome);
        browser.reload();
        browser
    }

    /// A browser that downloads web pages and images on other threads, so
    /// that a slow server doesn't keep it waiting. `wake` is called when a
    /// download finishes, after which `loaded` should be called.
    pub fn loading_in_background(site: String, chrome: bool, wake: Wake) -> Browser {
        let mut browser = Browser::unloaded(site, chrome);
        browser.host.load_in_background(wake);
        browser.reload();
        browser
    }

    /// A browser that hasn't run its page yet.
    fn unloaded(site: String, chrome: bool) -> Browser {
        Browser {
            site,
            chrome,
            page: loader::error_page("not loaded yet"),
            page_reads: Touched::default(),
            drawings: HashMap::new(),
            page_state: Rc::default(),
            host: Rc::new(Host::new(if chrome { "/0" } else { "" })),
            settings: None,
            history: Vec::new(),
            navigating: None,
            scroll: Scroll::default(),
            focus: None,
            key_focus: None,
            held: Vec::new(),
            error: None,
            dragging: false,
            selection: None,
            selection_frame: None,
            selecting: false,
            scroll_drag: None,
            find: None,
            reveal: false,
            reveal_anchor: true,
            repl: false,
            autofocused: Vec::new(),
            refocus: false,
            caret_moved: Instant::now(),
            last_frame: None,
            frames_failed: false,
            renderer: Renderer::new(),
            links: Vec::new(),
            scrollers: Vec::new(),
            inputs: Vec::new(),
            key_targets: Vec::new(),
            bar: None,
            matches: Vec::new(),
            lines: Vec::new(),
            hover_areas: Vec::new(),
            anchors: Vec::new(),
            pointer: None,
            ctrl: false,
            viewport: (0.0, 1.0),
        }
    }

    /// Catches up with downloads that finished in the background: goes to
    /// the site the browser was going to once it's here, or else runs the
    /// page again with what arrived. Returns whether anything did.
    pub fn loaded(&mut self) -> bool {
        if !self.host.take_loaded() {
            return false;
        }
        match self.navigating.take() {
            Some((navigation, site)) if !self.host.is_loading(&site) => {
                self.host.forget_downloads_but(&site);
                self.arrive(navigation, site);
            }
            navigating => {
                self.navigating = navigating;
                self.reload();
            }
        }
        true
    }

    /// Whether anything is downloading in the background, including the
    /// site the browser is going to.
    fn loading(&self) -> bool {
        self.navigating.is_some() || self.host.loading()
    }

    pub fn set_scroll(&mut self, page: f32) {
        self.scroll.page = page;
    }

    pub fn dark_mode(&self) -> bool {
        self.host.dark_mode.get()
    }

    /// Keeps settings in `settings` between runs: starts in the mode saved
    /// there, and saves the mode whenever a page's switch changes it.
    pub fn keep_settings(&mut self, settings: Settings) {
        if let Some(dark) = settings.dark_mode() {
            self.set_dark_mode(dark);
        }
        self.settings = Some(settings);
    }

    /// Whether the user has chosen a mode, which then wins over the system's.
    pub fn dark_mode_chosen(&self) -> bool {
        self.settings.as_ref().is_some_and(|settings| settings.dark_mode().is_some())
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

    /// Draws the page into `buf`, which is `w` × `h` physical pixels and
    /// was last drawn into `age` frames ago, or never if 0. Returns the parts
    /// of it that were drawn, or `None` for all of it.
    pub fn render(&mut self, buf: &mut [u32], w: usize, h: usize, scale: f32, age: u8) -> Option<Vec<Clip>> {
        let mut damage = self.render_once(buf, w, h, scale, age);
        // Finding text can mean drawing again: once the chrome has been told
        // how many matches there are, or to scroll the current one into view.
        // Both, so neither waits for the other.
        if self.after_search() | self.after_arrival() {
            let again = self.render_once(buf, w, h, scale, 1);
            damage = damage.zip(again).map(|(first, second)| first.into_iter().chain(second).collect());
        }
        self.autofocus();
        damage
    }

    fn render_once(&mut self, buf: &mut [u32], w: usize, h: usize, scale: f32, age: u8) -> Option<Vec<Clip>> {
        let caret_visible = (self.caret_moved.elapsed().as_millis() / BLINK.as_millis()).is_multiple_of(2);
        let search = self.find.as_ref().map(|find| Search { query: &find.query, current: find.current, frames_only: self.chrome });
        let images = self.host.images.borrow();
        let dark = self.host.dark_mode.get();
        let selection = self.selection.map(|(a, b)| (a.min(b), a.max(b))).filter(|(a, b)| a < b);
        let key_focus = self.key_focus;
        let view = View {
            scale,
            scroll: &self.scroll,
            focus: self.focus,
            key_focus,
            caret_visible,
            dark,
            search,
            selection,
            images: Some(&images),
            drawings: Some(&self.drawings),
            pointer: self.pointer,
            ctrl: self.ctrl,
        };
        let rendered = self.renderer.render(&self.page, buf, w, h, &view, age);
        drop(images);
        (self.links, self.scrollers, self.inputs, self.scroll) = (rendered.links, rendered.scrollers, rendered.inputs, rendered.scroll);
        self.key_targets = rendered.key_targets;
        // A canvas that's gone, or stopped listening, loses the keyboard.
        if !self.key_focus.is_some_and(|id| self.key_targets.iter().any(|target| target.id == id)) {
            self.key_focus = None;
            self.held.clear();
        }
        (self.bar, self.matches, self.lines) = (rendered.bar, rendered.matches, rendered.lines);
        (self.hover_areas, self.anchors) = (rendered.hover_areas, rendered.anchors);
        self.viewport = (h as f32, scale);
        // The page may have changed under the focused input.
        self.focus = self.focus.and_then(|focus| {
            let input = self.inputs.iter().find(|input| input.id == focus.id)?;
            let len = input.caret_xs.len() - 1;
            Some(Focus { cursor: focus.cursor.min(len), anchor: focus.anchor.min(len), ..focus })
        });
        rendered.damage
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
        let (top, bottom, _) = self.view_of(m.frame);
        if m.area.y0 >= top && m.area.y1 <= bottom {
            return false;
        }
        // A third of the way down, so there's some context above it.
        self.scroll_to(m.area, m.frame, (bottom - top) / 3.0);
        true
    }

    /// Catches up with arriving at a location ending in `#id`: scrolls
    /// the element with that `:id` to the top of what it's in. Returns
    /// whether to draw again.
    fn after_arrival(&mut self) -> bool {
        if !std::mem::take(&mut self.reveal_anchor) {
            return false;
        }
        let Some((_, id)) = self.site.split_once('#') else { return false };
        let Some(anchor) = self.anchors.iter().find(|anchor| &*anchor.id == id) else { return false };
        let (area, frame) = (anchor.area, anchor.frame);
        self.scroll_to(area, frame, 0.0);
        true
    }

    /// The top and bottom of the visible part of frame `frame`, or of the
    /// window for `None`, and how far it's scrolled.
    fn view_of(&self, frame: Option<usize>) -> (f32, f32, f32) {
        match frame.and_then(|id| self.scrollers.iter().find(|s| s.id == id)) {
            Some(s) => (s.area.y0, s.area.y1, s.scroll),
            None => (0.0, self.viewport.0, self.scroll.page),
        }
    }

    /// Scrolls `frame`, or the page, so that `area` is `gap` below the top
    /// of what's visible. The next render clamps it.
    fn scroll_to(&mut self, area: Clip, frame: Option<usize>, gap: f32) {
        let (top, _, scroll) = self.view_of(frame);
        let target = scroll + area.y0 - top - gap;
        match frame {
            Some(id) => _ = self.scroll.frames.insert(id, target),
            None => self.scroll.page = target,
        }
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

    /// When the next frame of animation is due, if the page has elements
    /// with `:on-frame` handlers.
    pub fn next_frame(&mut self) -> Option<Instant> {
        if self.frames_failed || !animates(&self.page) {
            self.last_frame = None;
            return None;
        }
        Some(*self.last_frame.get_or_insert_with(Instant::now) + FRAME)
    }

    /// Runs a frame of animation if one is due at `now`. Returns whether it did.
    pub fn animate(&mut self, now: Instant) -> bool {
        let Some(last) = self.last_frame.filter(|&last| now >= last + FRAME) else { return false };
        self.last_frame = Some(now);
        self.frame((now - last).min(MAX_FRAME_TIME));
        true
    }

    /// Calls every `:on-frame` handler with the milliseconds `dt` since the
    /// last frame, then runs the page again.
    pub fn frame(&mut self, dt: Duration) {
        let mut handlers = Vec::new();
        frame_handlers(&self.page, &mut handlers);
        if !handlers.is_empty() && !self.run_all(&handlers, &[Value::Num(dt.as_secs_f64() * 1000.0)], false) {
            self.frames_failed = true;
        }
    }

    pub fn hover(&self, x: f32, y: f32) -> Hover {
        if self.input_at(x, y).is_some() {
            Hover::Text
        } else if self.link_at(x, y).is_some() {
            Hover::Link
        } else if self.lines.iter().any(|line| line.visible.contains(x, y)) {
            Hover::Text
        } else {
            Hover::Nothing
        }
    }

    fn key_target_at(&self, x: f32, y: f32) -> Option<&KeyTarget> {
        self.key_targets.iter().find(|target| target.area.contains(x, y))
    }

    /// Whether a canvas has the keyboard, so keys go to it rather than the browser.
    pub fn listening(&self) -> bool {
        self.key_focus.is_some()
    }

    /// Gives the keyboard to key target `id`, or to nothing. The canvas that
    /// had it is told its held keys went up.
    fn focus_keys(&mut self, id: Option<usize>) {
        if self.key_focus != id {
            self.release_keys();
            self.key_focus = id;
        }
    }

    /// A key named `name` went down, as the web's `KeyboardEvent.key` names
    /// it, while a canvas has the keyboard. Repeats, from holding it, are ignored.
    pub fn key_down(&mut self, name: &str, repeat: bool) {
        if self.key_focus.is_none() || repeat || self.held.iter().any(|held| held == name) {
            return;
        }
        self.held.push(name.to_string());
        self.key_handler("on-key-down", name);
    }

    /// A key named `name` went up. Only keys the canvas saw go down count.
    pub fn key_up(&mut self, name: &str) {
        let Some(i) = self.held.iter().position(|held| held == name) else { return };
        self.held.remove(i);
        self.key_handler("on-key-up", name);
    }

    /// Lets go of every held key, as when the window loses focus, so a
    /// game doesn't think one is still down.
    pub fn release_keys(&mut self) {
        for name in std::mem::take(&mut self.held) {
            self.key_handler("on-key-up", &name);
        }
    }

    fn key_handler(&mut self, attr: &str, name: &str) {
        let target = self.key_targets.iter().find(|target| Some(target.id) == self.key_focus);
        if let Some(handler) = target.and_then(|target| target.element.attr(attr)).filter(|handler| handler.truthy()).cloned() {
            self.run(&handler, &[Value::Str(name.into())]);
        }
    }

    fn input_at(&self, x: f32, y: f32) -> Option<&InputBox> {
        self.inputs.iter().find(|input| input.area.contains(x, y))
    }

    fn link_at(&self, x: f32, y: f32) -> Option<&Link> {
        self.links.iter().find(|link| link.contains(x, y) && (self.ctrl || !link.needs_ctrl()))
    }

    /// Ctrl went down or up. Links that need it are only links, and
    /// underlined, while it's held. Returns whether that changed anything.
    pub fn set_ctrl(&mut self, held: bool) -> bool {
        std::mem::replace(&mut self.ctrl, held) != held
    }

    /// The visible line of text nearest a point, among those in `frame`.
    fn line_near(&self, x: f32, y: f32, frame: Option<usize>) -> Option<&TextLine> {
        let distance = |line: &TextLine| {
            let v = line.visible;
            ((v.y0 - y).max(y - v.y1).max(0.0), (v.x0 - x).max(x - v.x1).max(0.0))
        };
        let lines = self.lines.iter().filter(|line| line.frame == frame && !line.visible.is_empty());
        lines.min_by(|a, b| distance(a).partial_cmp(&distance(b)).expect("distances are numbers"))
    }

    /// The text position nearest a point in `frame`. Above or below the
    /// nearest line, that's the line's start or end.
    fn position_near(&self, x: f32, y: f32, frame: Option<usize>) -> Option<usize> {
        let line = self.line_near(x, y, frame)?;
        Some(match y {
            _ if y < line.visible.y0 => line.start,
            _ if y >= line.visible.y1 => line.end(),
            _ => line.position_at(x),
        })
    }

    /// Starts selecting the page's text at a point: from there for one
    /// click, the word there for two, and its paragraph for three.
    fn select_from(&mut self, x: f32, y: f32, clicks: u32) {
        let frame = self.scroller_at(x, y).map(|s| s.id);
        let selection = self.line_near(x, y, frame).map(|line| {
            let at = line.position_at(x);
            match clicks {
                1 => (at, at),
                2 => {
                    let (start, end) = word_at(&line.text.iter().collect::<String>(), at - line.start);
                    (line.start + start, line.start + end)
                }
                _ => {
                    let paragraph = self.lines.iter().filter(|other| other.frame == frame && other.paragraph == line.paragraph);
                    paragraph.fold((line.start, line.end()), |(start, end), other| (start.min(other.start), end.max(other.end())))
                }
            }
        });
        (self.selection, self.selection_frame, self.selecting) = (selection, frame, clicks == 1);
    }

    /// The selected text: in the focused input, or else on the page. Lines
    /// that wrapped from one paragraph are joined with a space, and
    /// paragraphs with a line break.
    pub fn selected_text(&self) -> Option<String> {
        if let Some(focus) = self.focus.filter(|focus| focus.cursor != focus.anchor) {
            let input = self.inputs.iter().find(|input| input.id == focus.id)?;
            let (start, end) = (focus.cursor.min(focus.anchor), focus.cursor.max(focus.anchor));
            return Some(input.value.chars().skip(start).take(end - start).collect());
        }
        let (start, end) = self.selection.map(|(a, b)| (a.min(b), a.max(b))).filter(|(a, b)| a < b)?;
        let mut text = String::new();
        let mut paragraph = None;
        for line in &self.lines {
            // A blank line, as in code, has no characters, but is inside a selection around it.
            let inside = (line.start < end && line.end() > start) || (line.text.is_empty() && start < line.start && line.start < end);
            if !inside {
                continue;
            }
            if let Some(previous) = paragraph {
                text.push(if previous == line.paragraph { ' ' } else { '\n' });
            }
            paragraph = Some(line.paragraph);
            let (from, to) = (start.max(line.start) - line.start, end.min(line.end()) - line.start);
            text.extend(&line.text[from..to]);
        }
        Some(text)
    }

    /// Types `text`, from the clipboard, into the focused input, replacing
    /// its selection. Inputs hold one line, so line breaks and tabs become spaces.
    pub fn paste(&mut self, text: &str) {
        let Some(focus) = self.focus else { return };
        let text: String = text
            .trim_end_matches(['\r', '\n'])
            .replace("\r\n", " ")
            .chars()
            .map(|c| if c.is_whitespace() { ' ' } else { c })
            .filter(|c| !c.is_control())
            .collect();
        if !text.is_empty() {
            self.edit(focus, &Key::Text(text), false);
        }
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
    /// release. Returns whether the press was on an input,
    /// or on a canvas that listens for keys.
    pub fn press(&mut self, x: f32, y: f32, clicks: u32) -> bool {
        // On a scrollbar, the thumb can be dragged. Pressing elsewhere on its
        // track jumps there, with the thumb centred on the pointer.
        if let Some((target, bar)) = self.scrollbar_at(x, y) {
            let grab = if bar.thumb.contains(x, y) { y - bar.thumb.y0 } else { (bar.thumb.y1 - bar.thumb.y0) / 2.0 };
            self.scroll_drag = Some((target, grab));
            self.drag_scrollbar((target, grab), y);
            return false;
        }
        // A canvas that listens for keys takes the keyboard, rather than starting a selection.
        if let Some(id) = self.key_target_at(x, y).map(|target| target.id) {
            self.focus = None;
            self.focus_keys(Some(id));
            return true;
        }
        self.focus_keys(None);
        let Some(input) = self.input_at(x, y) else {
            self.focus = None;
            self.select_from(x, y, clicks);
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
        self.selection = None;
        self.set_focus(Focus { id, cursor, anchor });
        true
    }

    /// The mouse pointer moved to a point, or out of the window. Returns
    /// whether it moved onto or off an element with a hover style, so that
    /// the page needs drawing again.
    pub fn point_at(&mut self, pointer: Option<(f32, f32)>) -> bool {
        let over = |area: &Clip, pointer: Option<(f32, f32)>| pointer.is_some_and(|(x, y)| area.contains(x, y));
        let changed = self.hover_areas.iter().any(|area| over(area, self.pointer) != over(area, pointer));
        self.pointer = pointer;
        changed
    }

    /// The mouse moved to a point. Returns whether that scrolled or changed the selection.
    pub fn drag_to(&mut self, x: f32, y: f32) -> bool {
        if let Some(drag) = self.scroll_drag {
            self.drag_scrollbar(drag, y);
            return true;
        }
        if self.selecting {
            let Some((anchor, reach)) = self.selection else { return false };
            let Some(to) = self.position_near(x, y, self.selection_frame).filter(|&to| to != reach) else { return false };
            self.selection = Some((anchor, to));
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
        // Selecting text, rather than clicking what it's on.
        self.selecting = false;
        if self.selection.is_some_and(|(a, b)| a != b) {
            return true;
        }
        self.selection = None;
        match self.link_at(x, y).map(|link| link.action.clone()) {
            Some(Action::Go { href, base, .. }) => self.follow(&href, base.as_deref()),
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
        // Out of an input, some letters are shortcuts, as in vim.
        let key = match key {
            Key::Text(text) if self.focus.is_none() => match text.as_str() {
                "/" => Key::Find,
                "j" => Key::Down,
                "k" => Key::Up,
                _ => Key::Text(text),
            },
            key => key,
        };
        match key {
            Key::Back => return self.back(),
            Key::FocusAddressBar => return self.focus_address_bar(),
            Key::Reload => return self.navigate(Navigation::Reload, self.site.clone()),
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
            Key::Escape if self.key_focus.is_some() => return self.focus_keys(None),
            // Stops going to another site, and stays on this one.
            Key::Escape if self.focus.is_none() && self.navigating.is_some() => {
                self.stop();
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
        self.focus_keys(None);
        self.focus = Some(focus);
        self.caret_moved = Instant::now();
    }

    fn location(&self) -> String {
        if !self.chrome {
            return self.site.clone();
        }
        let back = if self.history.is_empty() { "" } else { "&back=1" };
        let error = self.error.as_ref().map_or(String::new(), |e| format!("&error={}", loader::encode(e)));
        let find = self
            .find
            .as_ref()
            .map_or(String::new(), |find| format!("&find={}&found={}&at={}", loader::encode(&find.query), find.count, find.current + 1));
        let repl = if self.repl { "&repl=1" } else { "" };
        let loading = if self.loading() { "&loading=1" } else { "" };
        format!("browser:chrome?location={}{back}{error}{find}{repl}{loading}", loader::encode(&self.site))
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
        self.navigate(Navigation::Follow, site);
    }

    pub fn back(&mut self) {
        if let Some((site, ..)) = self.history.last() {
            self.navigate(Navigation::Back, site.clone());
        }
    }

    /// Goes to `site` by way of `navigation`. Loading in the background, a
    /// site on the web is downloaded first, while the current one stays,
    /// and `loaded` arrives there.
    fn navigate(&mut self, navigation: Navigation, site: String) {
        self.stop();
        if self.host.in_background() && loader::is_remote(&site) {
            self.host.download_again(&site);
            self.navigating = Some((navigation, site));
            // So the toolbar shows that it's loading.
            return self.reload();
        }
        self.host.forget_downloads();
        self.arrive(navigation, site);
    }

    /// Stops going to the site the browser was going to, if any.
    fn stop(&mut self) {
        if let Some((_, site)) = self.navigating.take() {
            self.host.abandon(&site);
        }
    }

    /// Shows `site`, by way of `navigation`, now that it's here.
    fn arrive(&mut self, navigation: Navigation, site: String) {
        self.navigating = None;
        // The page's state is kept for coming back, so it's told its keys went up.
        self.release_keys();
        (self.error, self.frames_failed) = (None, false);
        match navigation {
            Navigation::Follow => self.history.push((
                std::mem::replace(&mut self.site, site),
                std::mem::take(&mut self.scroll),
                std::mem::take(&mut self.page_state),
            )),
            Navigation::Back => {
                let Some((site, scroll, page_state)) = self.history.pop() else { return };
                // The site's state comes back, but the toolbar's describes the
                // current site, so the toolbar (the top-level page) starts afresh.
                if self.chrome {
                    page_state.forget("");
                }
                (self.site, self.scroll, self.page_state) = (site, scroll, page_state);
            }
            Navigation::Reload => {
                self.host.images.borrow_mut().clear();
                self.page_state = Rc::default();
            }
        }
        (self.focus, self.selection) = (None, None);
        (self.key_focus, self.held) = (None, Vec::new());
        self.dragging = false;
        self.reveal_anchor = navigation == Navigation::Follow;
        self.reload();
    }

    /// Calls a handler, then goes where it asked to with `go`, or else runs
    /// the page again so it shows any state the handler changed.
    fn run(&mut self, handler: &Value, args: &[Value]) {
        self.run_all(std::slice::from_ref(handler), args, true);
    }

    /// Like `run`, for several handlers called with the same arguments, in
    /// order, before the page runs again. Success clears the last error if
    /// `clear_error`, as animation shouldn't hide an error as soon as it's
    /// shown. Returns whether they all succeeded.
    fn run_all(&mut self, handlers: &[Value], args: &[Value], clear_error: bool) -> bool {
        let dark = self.dark_mode();
        let (result, changed) = watch::writes(|| handlers.iter().try_for_each(|handler| lisp::apply(handler, args).map(drop)));
        // The page and the chrome show these, so changing them means running the page again.
        let mut shown_changed = self.dark_mode() != dark;
        if let Some(settings) = self.settings.as_mut().filter(|_| shown_changed) {
            settings.set_dark_mode(!dark);
        }
        if let Some(request) = self.host.take_find().filter(|_| result.is_ok()) {
            self.find_request(request);
            shown_changed = true;
        }
        // Before following `go`, so code run in the REPL can navigate.
        if let Some(request) = self.host.take_repl().filter(|_| result.is_ok()) {
            self.repl_request(request);
            shown_changed = true;
        }
        match result {
            Ok(_) => match self.host.take_navigation() {
                Some((href, from)) => self.follow(&href, Some(&from)),
                None => {
                    if clear_error && self.error.take().is_some() {
                        shown_changed = true;
                    }
                    if shown_changed || changed.affects(&self.page_reads) {
                        self.reload();
                    } else if !changed.cells.is_empty() {
                        // Only state the page didn't read changed, which only `:draw` functions can show.
                        self.draw();
                    }
                }
            },
            Err(e) => {
                self.host.take_navigation();
                self.fail(format!("in an event handler: {e}"));
                return false;
            }
        }
        true
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
            self.show(loader::error_page(&error), Touched::default());
        }
    }

    /// Runs the page, keeping its state, scroll offsets and focus, then
    /// its canvases' `:draw` functions.
    fn reload(&mut self) {
        self.run_page();
        self.draw();
    }

    /// Runs the page, noting the state it reads.
    fn run_page(&mut self) {
        let loading = self.loading();
        self.run_page_once();
        // The page may have started downloads, which the toolbar shows.
        if self.chrome && self.loading() != loading {
            self.run_page_once();
        }
    }

    fn run_page_once(&mut self) {
        let (page, reads) = watch::reads(|| loader::open(&self.location(), &self.page_state, &self.host));
        self.show(page, reads);
        // `go` only means something in an event handler, not while the page runs.
        self.host.take_navigation();
    }

    fn show(&mut self, page: Rc<Element>, reads: Touched) {
        (self.page, self.page_reads) = (page, reads);
        // Drawings are by the address of their canvas, which a new page could reuse.
        self.drawings.clear();
    }

    /// Calls the page's canvases' `:draw` functions, keeping what they return
    /// for the canvases to draw. If one fails, it stops animation, as a
    /// failing `:on-frame` handler does, and shows why.
    fn draw(&mut self) {
        let mut draws = Vec::new();
        draw_functions(&self.page, &mut draws);
        let drawn: Result<Drawings, String> = draws
            .into_iter()
            .map(|(canvas, draw)| {
                let shapes = lisp::apply(&draw, &[])?;
                Ok((canvas, dom::build_element("canvas", &[shapes])?.children))
            })
            .collect();
        match drawn {
            Ok(drawn) => self.drawings = drawn,
            Err(e) => {
                let e = format!("in a canvas's :draw: {e}");
                self.frames_failed = true;
                self.drawings.clear();
                // The chrome shows the error, so it runs again, but not the drawing that failed.
                match self.chrome {
                    true => {
                        self.error = Some(e);
                        self.run_page();
                    }
                    false => self.show(loader::error_page(&e), Touched::default()),
                }
            }
        }
    }
}

/// An element's `:on-frame` handler. `nil` is none, so a page can pause.
fn frame_handler(el: &Element) -> Option<&Value> {
    el.attr("on-frame").filter(|handler| handler.truthy())
}

/// Every canvas in `el` and in pages framed inside it that has a `:draw`
/// function, by address, with the function.
fn draw_functions(el: &Rc<Element>, out: &mut Vec<(usize, Value)>) {
    if &*el.tag == "canvas"
        && let Some(draw) = el.attr("draw").filter(|draw| draw.truthy())
    {
        out.push((Rc::as_ptr(el) as usize, draw.clone()));
    }
    for child in &el.children {
        if let Node::Element(child) = child {
            draw_functions(child, out);
        }
    }
}

/// Whether any element in `el`, or in pages framed inside it, has an `:on-frame` handler.
fn animates(el: &Element) -> bool {
    frame_handler(el).is_some() || el.children.iter().any(|child| matches!(child, Node::Element(child) if animates(child)))
}

/// Every `:on-frame` handler in `el` and in pages framed inside it, in document order.
fn frame_handlers(el: &Element, out: &mut Vec<Value>) {
    out.extend(frame_handler(el).cloned());
    for child in &el.children {
        if let Node::Element(child) = child {
            frame_handlers(child, out);
        }
    }
}

/// What a key does to a text input.
#[derive(Debug, PartialEq)]
enum Edit {
    /// Move the caret, and the selection's other end.
    Select {
        cursor: usize,
        anchor: usize,
    },
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
        let mut render = |browser: &mut Browser| browser.render(&mut buf, 400, 200, 1.0, 0);

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

    /// A browser showing `src`, with or without the chrome. The page is in
    /// `target`, as the chrome takes locations starting with `/` to be in
    /// the working directory.
    fn browse(name: &str, src: &str, chrome: bool) -> (Browser, std::path::PathBuf) {
        let dir = std::path::Path::new("target").join(format!("test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("page.sx");
        std::fs::write(&file, src).unwrap();
        (Browser::new(file.to_string_lossy().into(), chrome), dir)
    }

    #[test]
    fn frames_of_animation_are_told_the_time_and_can_pause() {
        let (mut browser, dir) = browse(
            "frames",
            r#"(define-state t 0) (define-state ticks 0) (define-state running true)
               (page :on-frame (if running (fn (dt) (set! ticks (inc ticks))))
                 (p :on-frame (fn (dt) (set! t (+ t dt)) (if (>= ticks 2) (set! running false)))
                    "t: " (round t) ", ticks: " ticks))"#,
            false,
        );
        let start = browser.next_frame().expect("animating");
        assert!(!browser.animate(start - FRAME / 2), "not due yet");
        assert!(browser.animate(start));
        assert!(browser.page.to_string().contains(r#""t: " "17" ", ticks: " "1""#), "{}", browser.page);
        assert!(browser.animate(start + Duration::from_secs(5)));
        assert!(browser.page.to_string().contains(r#""t: " "117" ", ticks: " "2""#), "a long pause counts as 100ms: {}", browser.page);
        assert!(browser.next_frame().is_some(), "the paragraph still animates after the page's handler is nil");
        browser.frame(FRAME);
        assert!(browser.page.to_string().contains(r#"", ticks: " "2""#), "{}", browser.page);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failing_frame_stops_animation_until_the_page_loads_again() {
        let (mut browser, dir) = browse(
            "frame-error",
            r#"(define-state n 0)
               (page (p :on-frame (fn (dt) (set! n (inc n)) (if (> n 1) (car n))) "n: " n)
                     (button :on-click (fn () (car 1)) "fail"))"#,
            true,
        );
        render(&mut browser);
        browser.frame(FRAME);
        assert!(browser.next_frame().is_some());
        render(&mut browser);
        assert!(browser.error.is_none());
        browser.frame(FRAME);
        assert!(browser.error.is_some(), "the second frame fails");
        assert!(browser.next_frame().is_none(), "and stops the animation");
        browser.key(Key::Reload, false, (0.0, 0.0));
        assert!(browser.next_frame().is_some(), "until the page is reloaded");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_clicked_canvas_gets_keys_until_it_lets_go() {
        let (mut browser, dir) = browse(
            "keys",
            r#"(define-state log (list))
               (define (note s) (set! log (append log (list s))))
               (page :padding 0
                 (canvas :width 100 :height 50 :on-key-down (fn (k) (note (str "+" k))) :on-key-up (fn (k) (note (str "-" k))))
                 (p "log: " (join log " ")))"#,
            false,
        );
        let log = |browser: &Browser| {
            let page = browser.page.to_string();
            page[page.find("\"log: \" ").unwrap() + 8..].trim_end_matches(['"', ')']).trim_start_matches('"').to_string()
        };
        render(&mut browser);
        assert!(!browser.listening());
        browser.key_down("a", false);
        assert_eq!(log(&browser), "", "nothing has the keyboard");

        assert!(browser.click(50.0, 25.0), "clicking the canvas gives it the keyboard");
        assert!(browser.listening());
        browser.key_down("a", false);
        browser.key_down("a", true);
        browser.key_down("a", false);
        browser.key_down(" ", false);
        browser.key_up("a");
        browser.key_up("b");
        assert_eq!(log(&browser), "+a +  -a", "repeats, and keys it didn't see go down, are ignored");

        browser.key(Key::Escape, false, (0.0, 0.0));
        assert!(!browser.listening());
        assert_eq!(log(&browser), "+a +  -a - ", "letting go of the keyboard lets go of held keys");

        render(&mut browser);
        browser.click(50.0, 25.0);
        browser.key_down("x", false);
        render(&mut browser);
        browser.press(50.0, 500.0, 1);
        assert!(!browser.listening(), "clicking elsewhere takes the keyboard back");
        assert!(log(&browser).ends_with("+x -x"), "{}", log(&browser));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_canvases_redraw_when_handlers_change_state_the_page_didnt_read() {
        let (mut browser, dir) = browse(
            "draw",
            r#"(define-state x 0) (define-state score 0)
               (page :padding 0
                 (canvas :width 100 :height 50 :draw (fn () (rect :x x :y 0 :w 10 :h 10))
                         :on-frame (fn (dt) (set! x (+ x 1)) (if (= x 3) (set! score 1))))
                 (p "score: " score))"#,
            true,
        );
        let x = |browser: &Browser| {
            let shapes: Vec<String> = browser
                .drawings
                .values()
                .flatten()
                .map(|node| match node {
                    Node::Element(el) => el.to_string(),
                    Node::Text(text) => text.to_string(),
                })
                .collect();
            shapes.join(" ")
        };
        render(&mut browser);
        assert_eq!(x(&browser), r#"(rect :x "0" :y "0" :w "10" :h "10")"#.replace('"', ""));
        let page = browser.page.clone();
        browser.frame(FRAME);
        assert!(Rc::ptr_eq(&page, &browser.page), "the page didn't run again");
        assert!(x(&browser).contains(":x 1 "), "{}", x(&browser));
        render(&mut browser);
        browser.frame(FRAME);
        assert!(Rc::ptr_eq(&page, &browser.page));
        browser.frame(FRAME);
        assert!(!Rc::ptr_eq(&page, &browser.page), "the score changed, which the page shows");
        assert!(browser.page.to_string().contains(r#""score: " "1""#));
        assert!(x(&browser).contains(":x 3 "), "and the canvas drew again too");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failing_draw_stops_animation_and_shows_why() {
        let (mut browser, dir) = browse(
            "draw-error",
            r#"(define-state n 0)
               (page (canvas :draw (fn () (if (> n 0) (car n))) :on-frame (fn (dt) (set! n (inc n)))))"#,
            true,
        );
        render(&mut browser);
        assert!(browser.next_frame().is_some());
        browser.frame(FRAME);
        assert!(browser.error.as_ref().is_some_and(|e| e.contains(":draw")), "{:?}", browser.error);
        assert!(browser.next_frame().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn render(browser: &mut Browser) {
        let mut buf = vec![0; 900 * 600];
        browser.render(&mut buf, 900, 600, 1.0, 0);
    }

    fn press(browser: &mut Browser, keys: impl IntoIterator<Item = Key>) {
        for key in keys {
            render(browser);
            browser.key(key, false, (0.0, 0.0));
        }
        render(browser);
    }

    #[test]
    fn vim_keys_scroll_and_find_outside_inputs() {
        let lines: Vec<String> = (0..200).map(|i| format!("(p \"line {i}\")")).collect();
        let (mut browser, dir) = browse("vim-keys", &format!("(page {})", lines.join(" ")), true);
        let text = |s: &str| Key::Text(s.into());
        let page_scroll = |browser: &Browser| browser.scrollers[0].scroll;
        // Over the site, which is what keys scroll.
        let press = |browser: &mut Browser, keys: Vec<Key>| {
            for key in keys {
                render(browser);
                browser.key(key, false, (450.0, 300.0));
            }
            render(browser);
        };

        press(&mut browser, vec![text("j"), text("j")]);
        assert_eq!(page_scroll(&browser), 2.0 * LINE_HEIGHT, "j scrolls down");
        press(&mut browser, vec![text("k")]);
        assert_eq!(page_scroll(&browser), LINE_HEIGHT, "and k up");

        press(&mut browser, vec![text("/")]);
        assert_eq!(browser.inputs.len(), 2, "/ opens the find bar");
        press(&mut browser, vec![text("j"), text("/"), text("k")]);
        assert_eq!(browser.inputs[1].value, "j/k", "which types them like any input");
        assert_eq!(page_scroll(&browser), LINE_HEIGHT);
        std::fs::remove_dir_all(dir).unwrap();
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
    fn selecting_text_on_the_page() {
        let dir = std::env::temp_dir().join(format!("toy-browser-select-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("page.sx");
        let long = "one two three four five six seven eight nine ten eleven twelve";
        std::fs::write(&file, format!(r#"(page :padding 0 (p :margin 0 "Hello world") (p :margin 0 (a :href "x.sx" "a link")) (div :max-width 120 (p :margin 0 "{long}")))"#)).unwrap();
        let mut browser = Browser::new(file.to_string_lossy().into(), false);
        render(&mut browser);
        let (hello, link) = (&browser.lines[0], &browser.lines[1]);
        let y = (hello.area.y0 + hello.area.y1) / 2.0;
        let (h, o, w) = (hello.caret_xs[0] + 1.0, hello.caret_xs[5], (hello.caret_xs[6] + hello.caret_xs[7]) / 2.0);
        let link_y = (link.area.y0 + link.area.y1) / 2.0;
        let link_x = link.caret_xs[1];

        browser.press(h, y, 1);
        assert!(browser.drag_to(o, y));
        assert!(browser.release(o, y));
        render(&mut browser);
        assert_eq!(browser.selected_text().as_deref(), Some("Hello"));

        browser.press(w, y, 1);
        browser.press(w, y, 2);
        assert_eq!(browser.selected_text().as_deref(), Some("world"), "double-click selects a word");

        // Dragging from the link's line down past the end of the page takes
        // everything from there, wrapped lines joined by spaces.
        browser.press(link_x, link_y, 1);
        assert!(browser.drag_to(link_x, 10_000.0));
        assert!(browser.release(link_x, 10_000.0), "the link isn't followed");
        assert!(browser.history.is_empty());
        assert_eq!(browser.selected_text(), Some(format!(" link\n{long}")));
        render(&mut browser);
        let wrapped = browser.lines.iter().filter(|line| line.paragraph == browser.lines[2].paragraph).count();
        assert!(wrapped > 1, "the long paragraph wrapped");

        browser.press(link_x, link_y, 3);
        assert_eq!(browser.selected_text().as_deref(), Some("a link"), "triple-click selects the paragraph");

        assert!(browser.click(link_x, link_y), "a click without dragging still follows the link");
        assert_eq!(browser.history.len(), 1);
        assert_eq!(browser.selected_text(), None, "and navigating clears the selection");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn copying_from_an_input() {
        let mut browser = Browser::new("pages/hello.sx".into(), true);
        press(&mut browser, [Key::FocusAddressBar]);
        assert_eq!(browser.selected_text().as_deref(), Some("pages/hello.sx"));
        press(&mut browser, [Key::End]);
        assert_eq!(browser.selected_text(), None, "nothing's selected");
    }

    #[test]
    fn pasting_into_an_input() {
        let mut browser = Browser::new("pages/hello.sx".into(), true);
        browser.paste("ignored");
        assert_eq!(browser.inputs.len(), 0, "nothing's focused, so nothing happens");
        press(&mut browser, [Key::FocusAddressBar]);
        browser.paste("pages/\tcounter\r\n.sx\n");
        render(&mut browser);
        assert_eq!(browser.inputs[0].value, "pages/ counter .sx", "replacing the selection, on one line");
        assert_eq!(browser.focus.map(|f| (f.anchor, f.cursor)), Some((18, 18)), "with the caret after it");
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
    fn the_chosen_mode_outlasts_the_browser() {
        let dir = std::env::temp_dir().join(format!("toy-browser-settings-test-{}", std::process::id()));
        let file = dir.join("netscheme").join("settings");
        let mut browser = Browser::new("pages/hello.sx".into(), true);
        browser.keep_settings(Settings::at(file.clone()));
        assert!(!browser.dark_mode_chosen(), "nothing saved yet, so it follows the system");
        click_link(&mut browser, |action| matches!(action, Action::Run(_)));
        assert!(browser.dark_mode());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "dark-mode true\n");

        let mut browser = Browser::new("pages/hello.sx".into(), true);
        browser.keep_settings(Settings::at(file));
        assert!(browser.dark_mode() && browser.dark_mode_chosen(), "the next run starts dark");
        std::fs::remove_dir_all(dir).unwrap();
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
            (
                "index.sx",
                r#"(page :title "Remote" (p (a :href "other.sx" "other")) (frame :src "/sub/framed.sx") (p "forms: " (length (source "other.sx"))))"#,
            ),
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

        let server =
            crate::test_server::TestServer::start(&[("index.sx", r#"(page (img :src "logo.png" :width 40) (img :src "missing.png"))"#)]);
        server.add("logo.png", &std::fs::read("assets/images/logo.png").unwrap());
        let mut browser = Browser::new(server.url("index.sx"), false);
        render(&mut browser);
        let images = browser.host.images.borrow();
        assert!(images[&server.url("logo.png")].is_ok(), "downloaded relative to the page");
        assert!(
            images[&server.url("missing.png")].as_ref().is_err_and(|e| e.contains("404")),
            "{:?}",
            images[&server.url("missing.png")].as_ref().err()
        );
        drop(images);

        let requests = server.requests();
        press(&mut browser, [Key::Tab, Key::Tab]);
        assert_eq!(server.requests(), requests, "images aren't downloaded again when the page runs again");
    }

    #[test]
    fn ctrl_clicking_a_name_in_view_source_goes_to_its_definition() {
        let dir = format!("target/test-pages/definitions-{}", std::process::id());
        std::fs::create_dir_all(&dir).unwrap();
        // Enough definitions around `shout` that it's far down the page, and
        // can scroll to the top.
        let filler: String = (0..80).map(|i| format!("(define filler-{i} {i})\n")).collect();
        std::fs::write(format!("{dir}/lib.sx"), format!("{filler}(define (shout s) (upper s))\n{filler}")).unwrap();
        std::fs::write(
            format!("{dir}/page.sx"),
            "(eval `(do ,@(source \"lib.sx\"))) (define (local) (inc 1)) (page (p (shout \"hi\") (local)))",
        )
        .unwrap();
        let mut browser = Browser::new(format!("browser:view-source?page={dir}/page.sx"), true);
        let lib = format!("browser:view-source?page={dir}/lib.sx#shout");
        let definitions =
            [lib.clone(), "browser:view-source?page=browser:prelude#inc".into(), format!("browser:view-source?page={dir}/page.sx#local")];
        render(&mut browser);
        let link_to = |browser: &Browser, href: &str| {
            let link = browser.links.iter().find(|link| matches!(&link.action, Action::Go { href: h, .. } if &**h == href));
            link.map(|link| (link.x + 1.0, link.y + 1.0)).unwrap_or_else(|| panic!("a link to {href}"))
        };
        for href in &definitions {
            link_to(&browser, href);
        }

        let (x, y) = link_to(&browser, &lib);
        assert!(matches!(browser.hover(x, y), Hover::Text), "without Ctrl, it's text");
        browser.click(x, y);
        render(&mut browser);
        assert_eq!(browser.site, format!("browser:view-source?page={dir}/page.sx"), "a plain click doesn't follow it");

        browser.set_ctrl(true);
        assert!(matches!(browser.hover(x, y), Hover::Link));
        assert!(browser.click(x, y));
        render(&mut browser);
        assert_eq!(browser.site, lib);
        let anchor = browser.anchors.iter().find(|anchor| &*anchor.id == "shout").expect("the definition");
        let frame = browser.scrollers.iter().find(|s| Some(s.id) == anchor.frame).expect("in the site's frame");
        assert!(frame.scroll > 0.0, "scrolled down to it");
        // Scroll offsets are whole pixels.
        assert!((anchor.area.y0 - frame.area.y0).abs() < 1.0, "at the top of the frame: {} vs {}", anchor.area.y0, frame.area.y0);

        browser.back();
        render(&mut browser);
        assert_eq!(browser.site, format!("browser:view-source?page={dir}/page.sx"));
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

    /// A browser loading `url` in the background, with the chrome, and what
    /// its downloads send to as they finish.
    fn in_background(url: String) -> (Browser, std::sync::mpsc::Receiver<()>) {
        let (sender, woken) = std::sync::mpsc::channel();
        (Browser::loading_in_background(url, true, std::sync::Arc::new(move || _ = sender.send(()))), woken)
    }

    /// Waits for downloads, catching up with each, until none are left.
    fn settle(browser: &mut Browser, woken: &std::sync::mpsc::Receiver<()>) {
        while browser.loading() {
            woken.recv_timeout(Duration::from_secs(5)).expect("a download to finish");
            browser.loaded();
        }
        render(browser);
    }

    #[test]
    fn loading_in_the_background_keeps_the_page_until_the_next_arrives() {
        let server = crate::test_server::TestServer::start(&[
            ("index.sx", r#"(page :title "Remote" (p (a :href "other.sx" "other")) (frame :src "sub/framed.sx") (img :src "logo.png"))"#),
            ("other.sx", r#"(page :title "Other" (p "forms: " (length (source "index.sx"))))"#),
            ("sub/framed.sx", r#"(page (p "framed"))"#),
        ]);
        server.add("logo.png", &std::fs::read("assets/images/logo.png").unwrap());
        let loading = |browser: &Browser| browser.page.to_string().contains("Loading…");
        let (mut browser, woken) = in_background(server.url("index.sx"));
        assert!(loading(&browser), "the site isn't here yet: {}", browser.page);
        settle(&mut browser, &woken);
        assert_eq!(browser.title(), "Remote — Netscheme");
        assert!(browser.page.to_string().contains(r#"(p "framed")"#), "then its frame came: {}", browser.page);
        assert!(browser.host.images.borrow()[&server.url("logo.png")].is_ok(), "and its image");
        assert!(!loading(&browser));

        click_link(&mut browser, to("other.sx"));
        assert_eq!(browser.site, server.url("index.sx"), "the page stays while the next downloads");
        assert!(loading(&browser), "and the toolbar says it's loading");
        settle(&mut browser, &woken);
        assert_eq!(browser.site, server.url("other.sx"));
        assert!(browser.page.to_string().contains(r#""forms: " "1""#), "source waits for its download too: {}", browser.page);

        press(&mut browser, [Key::Back]);
        assert_eq!(browser.site, server.url("other.sx"));
        settle(&mut browser, &woken);
        assert_eq!(browser.site, server.url("index.sx"), "going back downloads the page first too");

        click_link(&mut browser, to("other.sx"));
        press(&mut browser, [Key::Escape]);
        assert!(!loading(&browser), "Escape stops going there");
        settle(&mut browser, &woken);
        assert_eq!(browser.site, server.url("index.sx"), "even once it arrives");

        let requests = server.requests();
        press(&mut browser, [Key::Reload]);
        assert_eq!(browser.title(), "Remote — Netscheme", "the page stays while it downloads again");
        settle(&mut browser, &woken);
        assert!(server.requests() > requests, "reloading downloads it again");
        assert!(!loading(&browser));
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

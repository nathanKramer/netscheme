//! Turns locations into documents. A location is a file path relative to
//! the working directory, like `pages/hello.sx`, or a browser page like
//! `browser:view-source`, which lives in this project's `ui/` folder. Either
//! can end in a query: `browser:view-source?page=pages/hello.sx`.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fs,
    path::Path,
    rc::Rc,
};

use crate::{
    dom::{Element, Node, build_element},
    lisp::{self, PageState, Value},
};

/// Frames nested deeper than this are left empty, so a page that frames
/// itself terminates.
const MAX_FRAME_DEPTH: u32 = 8;
/// Frames drawn smaller than this are left empty too; nobody can see them.
const MIN_FRAME_SCALE: f32 = 0.04;

/// What a running page knows about where it was loaded from.
pub struct PageContext {
    pub location: String,
    pub params: Vec<(String, String)>,
    /// How many frames deep this page is; 0 for the top-level page.
    pub depth: u32,
    /// The product of the `:scale` of every frame around this page.
    pub scale: f32,
    /// Where this page's state lives in `store`: its position in the tree
    /// of frames, so two frames of the same page have separate state.
    pub state_key: String,
    pub store: Rc<StateStore>,
    /// How many frames this page has created so far, to key their state.
    pub frames_created: Cell<usize>,
}

/// The `define-state` variables of a page and every frame inside it.
#[derive(Default)]
pub struct StateStore {
    pages: RefCell<HashMap<String, Rc<PageState>>>,
    /// A navigation requested with `go`, as an href and the location of the
    /// page that asked, for the browser to carry out.
    navigation: RefCell<Option<(String, String)>>,
}

impl StateStore {
    pub fn page(&self, key: &str) -> Rc<PageState> {
        self.pages.borrow_mut().entry(key.into()).or_default().clone()
    }

    /// Drops the state of the page at `key`, so it starts afresh next time.
    pub fn forget(&self, key: &str) {
        self.pages.borrow_mut().remove(key);
    }

    pub fn request_navigation(&self, href: &str, from: &str) {
        *self.navigation.borrow_mut() = Some((href.into(), from.into()));
    }

    pub fn take_navigation(&self) -> Option<(String, String)> {
        self.navigation.borrow_mut().take()
    }
}

#[cfg(test)]
pub fn test_context() -> Rc<PageContext> {
    Rc::new(PageContext {
        location: "test.sx".into(),
        params: Vec::new(),
        depth: 0,
        scale: 1.0,
        state_key: String::new(),
        store: Rc::default(),
        frames_created: Cell::new(0),
    })
}

/// Loads and evaluates the top-level page at `location`. State from earlier
/// runs of the page is taken from, and kept in, `store`.
pub fn open(location: &str, store: &Rc<StateStore>) -> Rc<Element> {
    open_nested(location, 0, 1.0, String::new(), store)
}

fn open_nested(location: &str, depth: u32, scale: f32, state_key: String, store: &Rc<StateStore>) -> Rc<Element> {
    let (_, query) = location.split_once('?').unwrap_or((location, ""));
    let params = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(k), decode(v))
        })
        .collect();
    let ctx = Rc::new(PageContext {
        location: location.into(),
        params,
        depth,
        scale,
        state_key,
        store: store.clone(),
        frames_created: Cell::new(0),
    });
    match read_source(location) {
        Ok(source) => evaluate(&source, &ctx),
        Err(e) => error_page(&e),
    }
}

/// The source code of the page at `location`.
pub fn read_source(location: &str) -> Result<String, String> {
    let file = location.split_once('?').map_or(location, |(file, _)| file);
    let path = match file.strip_prefix(BROWSER) {
        Some(name) => Path::new(env!("CARGO_MANIFEST_DIR")).join("ui").join(format!("{name}.sx")),
        None => file.into(),
    };
    if file.contains("://") {
        return Err(format!("only local pages work so far, so this browser can't open {file}"));
    }
    fs::read_to_string(&path).map_err(|e| format!("couldn't read {file}: {e}"))
}

const BROWSER: &str = "browser:";

/// Resolves `href` against the location of the page it appears on. Links
/// starting with `/`, and every link on a browser page, are relative to the
/// working directory instead.
pub fn resolve(current: &str, href: &str) -> String {
    if href.starts_with(BROWSER) || href.contains("://") {
        return href.into();
    }
    if let Some(rooted) = href.strip_prefix('/') {
        return rooted.into();
    }
    if current.starts_with(BROWSER) {
        return href.into();
    }
    let file = current.split_once('?').map_or(current, |(file, _)| file);
    Path::new(file).parent().unwrap_or(Path::new("")).join(href).to_string_lossy().into_owned()
}

/// Percent-encodes `s` so it can be a query parameter's value.
pub fn encode(s: &str) -> String {
    let mut out = String::new();
    for byte in s.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~/:".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (b'+', _) => {
                out.push(b' ');
                i += 1;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Loads the page named by a frame's `:src` as the frame's only child,
/// unless the frame is too deep or too small to be worth drawing.
pub fn fill_frame(parent: &PageContext, frame: &mut Element) {
    let Some(Value::Str(src)) = frame.attr("src").cloned() else { return };
    let scale = parent.scale
        * match frame.attr("scale") {
            Some(Value::Num(n)) => *n as f32,
            _ => 1.0,
        };
    if parent.depth >= MAX_FRAME_DEPTH || scale < MIN_FRAME_SCALE {
        return;
    }
    let index = parent.frames_created.replace(parent.frames_created.get() + 1);
    let state_key = format!("{}/{index}", parent.state_key);
    let location = resolve(&parent.location, &src);
    let page = open_nested(&location, parent.depth + 1, scale, state_key, &parent.store);
    frame.attrs.push(("location".into(), Value::Str(location.into())));
    frame.children = vec![Node::Element(page)];
}

/// Evaluates a page program. Errors become an error page, like a browser would show.
fn evaluate(source: &str, ctx: &Rc<PageContext>) -> Rc<Element> {
    match lisp::run(source, ctx) {
        Ok(Value::Element(el)) if &*el.tag == "page" => el,
        Ok(other) => error_page(&format!("a page must evaluate to (page ...), got {other}")),
        Err(e) => error_page(&e),
    }
}

pub fn error_page(message: &str) -> Rc<Element> {
    let heading = build_element("h1", &[Value::Keyword("color".into()), Value::Str("#c00".into()), Value::Str("Page error".into())]);
    let body = build_element("p", &[Value::Str(message.into())]);
    let page = build_element("page", &[Value::Element(Rc::new(heading.unwrap())), Value::Element(Rc::new(body.unwrap()))]);
    Rc::new(page.unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_resolve_relative_to_the_current_page() {
        assert_eq!(resolve("pages/hello.sx", "about.sx"), "pages/about.sx");
        assert_eq!(resolve("hello.sx", "sub/a.sx"), "sub/a.sx");
        assert_eq!(resolve("pages/v.sx?page=sub/x.sx", "a.sx"), "pages/a.sx");
        assert_eq!(resolve("pages/hello.sx", "https://example.com"), "https://example.com");
        assert!(read_source("https://example.com").unwrap_err().contains("only local pages"));
        assert_eq!(resolve("pages/hello.sx", "/x.sx"), "x.sx");
        assert_eq!(resolve("pages/hello.sx", "browser:chrome"), "browser:chrome");
        assert_eq!(resolve("browser:view-source?page=a", "pages/hello.sx"), "pages/hello.sx");
    }

    #[test]
    fn query_values_round_trip_through_encoding() {
        let location = "browser:view-source?page=pages/a b.sx&x=1";
        assert_eq!(decode(&encode(location)), location);
        assert!(!encode(location).contains(['?', '&', '=', ' ']));
    }

    #[test]
    fn each_frame_has_its_own_state() {
        let dir = std::env::temp_dir().join(format!("toy-browser-state-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("counter.sx"), "(define-state n 0) (page :on-click (fn () (set! n (+ n 1))) n)").unwrap();
        fs::write(dir.join("two.sx"), r#"(page (frame :src "counter.sx") (frame :src "counter.sx"))"#).unwrap();
        let two = dir.join("two.sx").to_string_lossy().into_owned();
        let store = Rc::new(StateStore::default());

        let counters = |page: &Element| -> Vec<Rc<Element>> {
            page.children
                .iter()
                .filter_map(|frame| match frame {
                    Node::Element(frame) => match frame.children.first() {
                        Some(Node::Element(counter)) => Some(counter.clone()),
                        _ => None,
                    },
                    Node::Text(_) => None,
                })
                .collect()
        };
        let page = open(&two, &store);
        let second = counters(&page)[1].clone();
        lisp::apply(second.attr("on-click").unwrap(), &[]).unwrap();

        let page = open(&two, &store);
        let shown: Vec<String> = counters(&page).iter().map(|c| c.to_string()).collect();
        assert!(shown[0].ends_with(r#""0")"#) && shown[1].ends_with(r#""1")"#), "{shown:?}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_page_that_frames_itself_stops() {
        let dir = std::env::temp_dir().join(format!("toy-browser-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("self.sx");
        fs::write(&file, r#"(page (row (frame :src "self.sx" :scale 0.5) (frame :src "self.sx" :scale 0.5)))"#).unwrap();

        fn depth(el: &Element) -> u32 {
            let deepest = el.children.iter().filter_map(|c| match c {
                Node::Element(child) => Some(depth(child)),
                Node::Text(_) => None,
            });
            deepest.max().unwrap_or(0) + (&*el.tag == "frame") as u32
        }
        let page = open(file.to_str().unwrap(), &Rc::default());
        // 0.5^4 is above MIN_FRAME_SCALE; 0.5^5 is below it.
        assert_eq!(depth(&page), 5, "4 frames with pages, then an empty one");
        fs::remove_dir_all(dir).unwrap();
    }
}

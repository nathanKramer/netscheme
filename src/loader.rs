//! Turns locations into documents. A location is one of:
//!
//! - a file path relative to the working directory, like `pages/hello.sx`;
//! - a browser page like `browser:view-source`, which lives in this
//!   project's `ui/` folder;
//! - a web address, like `http://localhost:8000/hello.sx`.
//!
//! Any of them can end in a query: `browser:view-source?page=pages/hello.sx`.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fs,
    path::Path,
    rc::Rc,
    time::Duration,
};

use ureq::ResponseExt;

use crate::{
    dom::{Element, Node, build_element},
    image::Image,
    lisp::{self, PageState, Value},
};

/// Frames nested deeper than this are left empty, so a page that frames
/// itself terminates.
const MAX_FRAME_DEPTH: u32 = 8;
/// Frames drawn smaller than this are left empty too; nobody can see them.
const MIN_FRAME_SCALE: f32 = 0.04;
/// Pages bigger than this aren't downloaded.
const MAX_PAGE_BYTES: u64 = 4 * 1024 * 1024;
/// The biggest image file the browser will read or download.
const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// Decoded images, by location, or why one couldn't be loaded.
pub type Images = HashMap<String, Result<Rc<Image>, String>>;

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
    pub host: Rc<Host>,
    /// How many frames this page has created so far, to key their state.
    pub frames_created: Cell<usize>,
}

/// The `define-state` variables of a page and every frame inside it.
#[derive(Default)]
pub struct StateStore {
    pages: RefCell<HashMap<String, Rc<PageState>>>,
}

impl StateStore {
    pub fn page(&self, key: &str) -> Rc<PageState> {
        self.pages.borrow_mut().entry(key.into()).or_default().clone()
    }

    /// Drops the state of the page at `key`, so it starts afresh next time.
    pub fn forget(&self, key: &str) {
        self.pages.borrow_mut().remove(key);
    }
}

/// What pages can know about, and ask of, the browser showing them.
#[derive(Default)]
pub struct Host {
    pub dark_mode: Cell<bool>,
    /// A navigation requested with `go`, as an href and the location of the
    /// page that asked, for the browser to carry out.
    navigation: RefCell<Option<(String, String)>>,
    /// Web pages downloaded since the browser last navigated. A page runs
    /// again on every click and keystroke, and shouldn't download itself
    /// again each time.
    downloads: RefCell<HashMap<String, Result<Source, String>>>,
    /// A change to the search asked for by the chrome's find bar.
    find: RefCell<Option<FindRequest>>,
    /// Images that pages have shown. Unlike downloaded pages, they're kept
    /// when the browser navigates, until a reload.
    pub images: RefCell<Images>,
}

/// What the find bar can ask of the browser.
#[derive(Clone, Debug, PartialEq)]
pub enum FindRequest {
    Query(String),
    Next,
    Previous,
    Close,
}

impl Host {
    /// Downloads a web page, or returns the copy from earlier.
    fn download(&self, url: &str) -> Result<Source, String> {
        if let Some(download) = self.downloads.borrow().get(url) {
            return download.clone();
        }
        let download = download(url);
        self.downloads.borrow_mut().insert(url.into(), download.clone());
        download
    }

    /// Forgets downloaded pages, so they're downloaded afresh next time.
    pub fn forget_downloads(&self) {
        self.downloads.borrow_mut().clear();
    }

    /// Reads and decodes the image at `location`, unless it already has.
    /// Errors are kept too, so a missing image isn't looked for again.
    pub fn load_image(&self, location: &str) -> Result<Rc<Image>, String> {
        if let Some(image) = self.images.borrow().get(location) {
            return image.clone();
        }
        let image = read_bytes(location, MAX_IMAGE_BYTES).and_then(|bytes| Image::decode_png(&bytes)).map(Rc::new);
        let image = image.map_err(|e| format!("{location}: {e}"));
        if let Err(e) = &image {
            eprintln!("image {e}");
        }
        self.images.borrow_mut().insert(location.into(), image.clone());
        image
    }

    pub fn request_navigation(&self, href: &str, from: &str) {
        *self.navigation.borrow_mut() = Some((href.into(), from.into()));
    }

    pub fn take_navigation(&self) -> Option<(String, String)> {
        self.navigation.borrow_mut().take()
    }

    pub fn request_find(&self, request: FindRequest) {
        *self.find.borrow_mut() = Some(request);
    }

    pub fn take_find(&self) -> Option<FindRequest> {
        self.find.borrow_mut().take()
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
        host: Rc::default(),
        frames_created: Cell::new(0),
    })
}

/// Loads and evaluates the top-level page at `location`. State from earlier
/// runs of the page is taken from, and kept in, `store`.
pub fn open(location: &str, store: &Rc<StateStore>, host: &Rc<Host>) -> Rc<Element> {
    let top = Frame { depth: 0, scale: 1.0, state_key: String::new(), store: store.clone(), host: host.clone() };
    open_nested(location, top).0
}

/// Where a page is being loaded: at the top, or in a frame.
struct Frame {
    depth: u32,
    scale: f32,
    state_key: String,
    store: Rc<StateStore>,
    host: Rc<Host>,
}

/// Loads and evaluates the page at `location`. Also returns where the page
/// actually came from, which differs if a web server redirected.
fn open_nested(location: &str, frame: Frame) -> (Rc<Element>, String) {
    let Frame { depth, scale, state_key, store, host } = frame;
    let source = match read_source(location, &host) {
        Ok(source) => source,
        Err(e) => return (error_page(&e), location.into()),
    };
    let location = source.location.as_str();
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
        store,
        host,
        frames_created: Cell::new(0),
    });
    (evaluate(&source.text, &ctx), source.location)
}

/// The source code of the page at `location`.
/// A page's source code, and where it came from.
#[derive(Clone, Debug)]
pub struct Source {
    pub text: String,
    /// The page's location, after any redirects.
    pub location: String,
}

/// The source code of the page at `location`.
pub fn read_source(location: &str, host: &Host) -> Result<Source, String> {
    // The part after `#` names a place in the page, not a different page.
    let location = location.split_once('#').map_or(location, |(page, _)| page);
    if is_remote(location) {
        return host.download(location);
    }
    let file = location.split_once('?').map_or(location, |(file, _)| file);
    if file.contains("://") {
        return Err(format!("this browser can open web pages over http and https, but not {file}"));
    }
    let path = match file.strip_prefix(BROWSER) {
        Some(name) => Path::new(env!("CARGO_MANIFEST_DIR")).join("ui").join(format!("{name}.sx")),
        None => file.into(),
    };
    let text = fs::read_to_string(&path).map_err(|e| format!("couldn't read {file}: {e}"))?;
    Ok(Source { text, location: location.into() })
}

fn download(url: &str) -> Result<Source, String> {
    let (bytes, location) = fetch(url, MAX_PAGE_BYTES)?;
    let text = String::from_utf8(bytes).map_err(|_| format!("couldn't load {url}: it isn't text"))?;
    if text.trim_start().starts_with('<') {
        return Err(format!("{url} looks like HTML. This browser only reads pages written in its own Lisp."));
    }
    Ok(Source { text, location })
}

/// Downloads up to `limit` bytes from `url`, giving up after a while.
/// Returns them with the location they came from, after any redirects.
fn fetch(url: &str, limit: u64) -> Result<(Vec<u8>, String), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(5)))
        .timeout_global(Some(Duration::from_secs(10)))
        .user_agent("toy-browser")
        .build()
        .into();
    let failed = |e: ureq::Error| format!("couldn't load {url}: {e}");
    let mut response = agent.get(url).call().map_err(failed)?;
    let location = response.get_uri().to_string();
    let bytes = response.body_mut().with_config().limit(limit).read_to_vec().map_err(failed)?;
    Ok((bytes, location))
}

/// The contents of a file or web resource, up to `limit` bytes.
fn read_bytes(location: &str, limit: u64) -> Result<Vec<u8>, String> {
    if is_remote(location) {
        return fetch(location, limit).map(|(bytes, _)| bytes);
    }
    if location.contains("://") {
        return Err("only files and web resources can be loaded".into());
    }
    // The browser's own files, like the toolbar's icons, live in its `ui` folder.
    let path = match location.strip_prefix(BROWSER) {
        Some(name) => Path::new(env!("CARGO_MANIFEST_DIR")).join("ui").join(name),
        None => location.into(),
    };
    let location = path.as_path();
    let size = fs::metadata(location).map_err(|e| format!("couldn't read it: {e}"))?.len();
    if size > limit {
        return Err(format!("it's {size} bytes, which is more than the limit of {limit}"));
    }
    fs::read(location).map_err(|e| format!("couldn't read it: {e}"))
}

pub fn is_remote(location: &str) -> bool {
    location.starts_with("http://") || location.starts_with("https://")
}

/// Whether the page at `from` may load, frame, read or link to `to`. Web
/// pages can only reach other web pages, not files or the browser's own pages.
pub fn may_reach(from: &str, to: &str) -> Result<(), String> {
    if is_remote(from) && !is_remote(to) {
        return Err(format!("{from} is a web page, so it can't open {to}"));
    }
    Ok(())
}

const BROWSER: &str = "browser:";

/// Resolves `href` against the location of the page it appears on. On a web
/// page, links resolve as URLs do. Otherwise, links starting with `/`, and
/// every link on a browser page, are relative to the working directory.
pub fn resolve(current: &str, href: &str) -> String {
    if href.starts_with(BROWSER) || href.contains("://") {
        return href.into();
    }
    if is_remote(current) {
        return url::Url::parse(current).and_then(|base| base.join(href)).map_or_else(|_| href.into(), String::from);
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
    let inner = Frame { depth: parent.depth + 1, scale, state_key, store: parent.store.clone(), host: parent.host.clone() };
    let (page, location) = match may_reach(&parent.location, &location) {
        Ok(()) => open_nested(&location, inner),
        Err(e) => (error_page(&e), location),
    };
    // Links on the framed page resolve against where it came from.
    frame.attrs.push(("location".into(), Value::Str(location.into())));
    if let Err(e) = frame.set_children(vec![Node::Element(page)]) {
        // Too deep to show the page; show why instead, which is shallow.
        frame.set_children(vec![Node::Element(error_page(&e))]).expect("an error page fits");
    }
}

/// Resolves an image's `:src`, loads it, and records where it came from as
/// its `:location`, for layout to find it by.
pub fn fill_image(parent: &PageContext, img: &mut Element) {
    let Some(Value::Str(src)) = img.attr("src").cloned() else { return };
    let location = resolve(&parent.location, &src);
    // An image that can't be loaded shows as an empty box.
    let _ = may_reach(&parent.location, &location).and_then(|()| parent.host.load_image(&location));
    img.attrs.push(("location".into(), Value::Str(location.into())));
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
        assert_eq!(resolve("http://h/a/b.sx?x=1", "c.sx"), "http://h/a/c.sx");
        assert_eq!(resolve("http://h/a/b.sx", "/c.sx"), "http://h/c.sx");
        assert_eq!(resolve("http://h/a/b.sx", "../c.sx?y=2"), "http://h/c.sx?y=2");
        assert!(read_source("ftp://example.com", &Host::default()).unwrap_err().contains("http and https"));
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
        let page = open(&two, &store, &Rc::default());
        let second = counters(&page)[1].clone();
        lisp::apply(second.attr("on-click").unwrap(), &[]).unwrap();

        let page = open(&two, &store, &Rc::default());
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
        let page = open(file.to_str().unwrap(), &Rc::default(), &Rc::default());
        // 0.5^4 is above MIN_FRAME_SCALE; 0.5^5 is below it.
        assert_eq!(depth(&page), 5, "4 frames with pages, then an empty one");
        fs::remove_dir_all(dir).unwrap();
    }
}

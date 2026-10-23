//! Turns locations like `pages/view-source.sx?page=hello.sx` into documents.

use std::{fs, path::Path, rc::Rc};

use crate::{
    dom::{Element, Node, build_element},
    lisp::{self, Value},
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
}

#[cfg(test)]
pub fn test_context() -> Rc<PageContext> {
    Rc::new(PageContext { location: "test.sx".into(), params: Vec::new(), depth: 0, scale: 1.0 })
}

/// Loads and evaluates the top-level page at `location`.
pub fn open(location: &str) -> Rc<Element> {
    open_nested(location, 0, 1.0)
}

fn open_nested(location: &str, depth: u32, scale: f32) -> Rc<Element> {
    let (file, query) = location.split_once('?').unwrap_or((location, ""));
    let params = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (k.to_string(), v.to_string())
        })
        .collect();
    let ctx = Rc::new(PageContext { location: location.into(), params, depth, scale });
    match fs::read_to_string(file) {
        Ok(source) => evaluate(&source, &ctx),
        Err(e) => error_page(&format!("couldn't read {file}: {e}")),
    }
}

/// Resolves `href` against the location of the page it appears on.
pub fn resolve(current: &str, href: &str) -> Result<String, String> {
    if href.contains("://") {
        return Err(format!("only local links work so far: {href}"));
    }
    let file = current.split_once('?').map_or(current, |(file, _)| file);
    Ok(Path::new(file).parent().unwrap_or(Path::new("")).join(href).to_string_lossy().into_owned())
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
    let page = match resolve(&parent.location, &src) {
        Ok(location) => {
            let page = open_nested(&location, parent.depth + 1, scale);
            frame.attrs.push(("location".into(), Value::Str(location.into())));
            page
        }
        Err(e) => error_page(&e),
    };
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
        assert_eq!(resolve("pages/hello.sx", "about.sx"), Ok("pages/about.sx".into()));
        assert_eq!(resolve("hello.sx", "sub/a.sx"), Ok("sub/a.sx".into()));
        assert_eq!(resolve("pages/v.sx?page=sub/x.sx", "a.sx"), Ok("pages/a.sx".into()));
        assert!(resolve("pages/hello.sx", "https://example.com").is_err());
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
        let page = open(file.to_str().unwrap());
        // 0.5^4 is above MIN_FRAME_SCALE; 0.5^5 is below it.
        assert_eq!(depth(&page), 5, "4 frames with pages, then an empty one");
        fs::remove_dir_all(dir).unwrap();
    }
}

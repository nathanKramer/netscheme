mod dom;
mod layout;
mod lisp;
mod paint;
mod style;
mod window;

use std::{fs, process::ExitCode, rc::Rc};

use dom::{Element, build_element};
use lisp::Value;
use paint::Renderer;

const USAGE: &str = "usage: toy-browser [PAGE.sx] [--screenshot OUT.png] [--width N] [--height N]";

fn main() -> ExitCode {
    let mut path = "pages/hello.sx".to_string();
    let mut screenshot = None;
    let (mut width, mut height) = (900, 700);

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(USAGE);
        let parsed = match arg.as_str() {
            "--screenshot" => value().map(|v| screenshot = Some(v)),
            "--width" => value().and_then(|v| v.parse().map(|n| width = n).map_err(|_| USAGE)),
            "--height" => value().and_then(|v| v.parse().map(|n| height = n).map_err(|_| USAGE)),
            _ if !arg.starts_with('-') => {
                path = arg;
                Ok(())
            }
            _ => Err(USAGE),
        };
        if let Err(usage) = parsed {
            eprintln!("{usage}");
            return ExitCode::FAILURE;
        }
    }

    let page = match fs::read_to_string(&path) {
        Ok(source) => load_page(&source),
        Err(e) => error_page(&format!("couldn't read {path}: {e}")),
    };
    let mut renderer = Renderer::new();

    match screenshot {
        Some(out) => match write_screenshot(&mut renderer, &page, width, height, &out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("screenshot failed: {e}");
                ExitCode::FAILURE
            }
        },
        None => {
            window::run(page, renderer);
            ExitCode::SUCCESS
        }
    }
}

/// Evaluates a page program. Errors become an error page, like a browser would show.
fn load_page(source: &str) -> Rc<Element> {
    match lisp::run(source) {
        Ok(Value::Element(el)) if &*el.tag == "page" => el,
        Ok(other) => error_page(&format!("a page must evaluate to (page ...), got {other}")),
        Err(e) => error_page(&e),
    }
}

fn error_page(message: &str) -> Rc<Element> {
    let heading = build_element("h1", &[Value::Keyword("color".into()), Value::Str("#c00".into()), Value::Str("Page error".into())]);
    let body = build_element("p", &[Value::Str(message.into())]);
    let page = build_element("page", &[Value::Element(Rc::new(heading.unwrap())), Value::Element(Rc::new(body.unwrap()))]);
    Rc::new(page.unwrap())
}

fn write_screenshot(renderer: &mut Renderer, page: &Element, w: usize, h: usize, out: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut buf = vec![0u32; w * h];
    renderer.render(page, &mut buf, w, h, 1.0);
    let rgb: Vec<u8> = buf.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(fs::File::create(out)?), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgb)?;
    Ok(())
}

mod dom;
mod layout;
mod loader;
mod lisp;
mod paint;
mod style;
mod window;

use std::{fs, process::ExitCode};

use layout::Action;
use paint::Renderer;

const USAGE: &str = "usage: toy-browser [PAGE.sx] [--screenshot OUT.png] [--width N] [--height N] [--scroll N] [--click X,Y]...";

fn main() -> ExitCode {
    let mut location = "pages/hello.sx".to_string();
    let mut screenshot = None;
    let (mut width, mut height) = (900, 700);
    let mut scroll = 0.0;
    let mut clicks = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(USAGE);
        let parsed = match arg.as_str() {
            "--screenshot" => value().map(|v| screenshot = Some(v)),
            "--width" => value().and_then(|v| v.parse().map(|n| width = n).map_err(|_| USAGE)),
            "--height" => value().and_then(|v| v.parse().map(|n| height = n).map_err(|_| USAGE)),
            "--scroll" => value().and_then(|v| v.parse().map(|n| scroll = n).map_err(|_| USAGE)),
            "--click" => value().and_then(|v| {
                let (x, y) = v.split_once(',').ok_or(USAGE)?;
                clicks.push((x.parse::<f32>().map_err(|_| USAGE)?, y.parse::<f32>().map_err(|_| USAGE)?));
                Ok(())
            }),
            _ if !arg.starts_with('-') => {
                location = arg;
                Ok(())
            }
            _ => Err(USAGE),
        };
        if let Err(usage) = parsed {
            eprintln!("{usage}");
            return ExitCode::FAILURE;
        }
    }

    let mut renderer = Renderer::new();

    // Screenshots show just the page at `location`; the window wraps it in
    // the browser chrome.
    match screenshot {
        Some(out) => match write_screenshot(&mut renderer, &location, width, height, scroll, &clicks, &out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("screenshot failed: {e}");
                ExitCode::FAILURE
            }
        },
        None => {
            window::run(location, renderer);
            ExitCode::SUCCESS
        }
    }
}

/// Renders `location` to a PNG. Each click runs the handler at that point,
/// as clicking in the window would, before the final render.
#[allow(clippy::too_many_arguments)]
fn write_screenshot(
    renderer: &mut Renderer,
    location: &str,
    w: usize,
    h: usize,
    scroll: f32,
    clicks: &[(f32, f32)],
    out: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let state = Default::default();
    let scroll = paint::Scroll { page: scroll, ..Default::default() };
    let mut buf = vec![0u32; w * h];
    let mut page = loader::open(location, &state);
    for &(x, y) in clicks {
        let links = renderer.render(&page, &mut buf, w, h, 1.0, &scroll).links;
        match links.iter().find(|link| link.contains(x, y)).map(|link| &link.action) {
            Some(Action::Run(handler)) => lisp::apply(handler, &[])?,
            _ => return Err(format!("nothing to click at {x},{y}").into()),
        };
        page = loader::open(location, &state);
    }
    renderer.render(&page, &mut buf, w, h, 1.0, &scroll);
    let rgb: Vec<u8> = buf.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(fs::File::create(out)?), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgb)?;
    Ok(())
}

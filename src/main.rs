mod dom;
mod layout;
mod loader;
mod lisp;
mod paint;
mod style;
mod window;

use std::{fs, process::ExitCode};

use dom::Element;
use paint::Renderer;

const USAGE: &str = "usage: toy-browser [PAGE.sx] [--screenshot OUT.png] [--width N] [--height N] [--scroll N]";

fn main() -> ExitCode {
    let mut location = "pages/hello.sx".to_string();
    let mut screenshot = None;
    let (mut width, mut height) = (900, 700);
    let mut scroll = 0.0;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(USAGE);
        let parsed = match arg.as_str() {
            "--screenshot" => value().map(|v| screenshot = Some(v)),
            "--width" => value().and_then(|v| v.parse().map(|n| width = n).map_err(|_| USAGE)),
            "--height" => value().and_then(|v| v.parse().map(|n| height = n).map_err(|_| USAGE)),
            "--scroll" => value().and_then(|v| v.parse().map(|n| scroll = n).map_err(|_| USAGE)),
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

    let page = loader::open(&location);
    let mut renderer = Renderer::new();

    match screenshot {
        Some(out) => match write_screenshot(&mut renderer, &page, width, height, scroll, &out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("screenshot failed: {e}");
                ExitCode::FAILURE
            }
        },
        None => {
            window::run(location, page, renderer);
            ExitCode::SUCCESS
        }
    }
}

fn write_screenshot(renderer: &mut Renderer, page: &Element, w: usize, h: usize, scroll: f32, out: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut buf = vec![0u32; w * h];
    renderer.render(page, &mut buf, w, h, 1.0, scroll);
    let rgb: Vec<u8> = buf.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(fs::File::create(out)?), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgb)?;
    Ok(())
}

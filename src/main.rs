mod browser;
mod dom;
mod layout;
mod loader;
mod lisp;
mod paint;
mod style;
mod window;

use std::{fs, process::ExitCode};

use browser::{Browser, Key};

const USAGE: &str = "usage: toy-browser [PAGE.sx] [--screenshot OUT.png] [--width N] [--height N] [--scroll N] [--chrome] [--dark] [--click X,Y | --type TEXT]...";

fn main() -> ExitCode {
    let mut location = "pages/hello.sx".to_string();
    let mut screenshot = None;
    let (mut width, mut height) = (900, 700);
    let mut scroll = 0.0;
    let mut steps = Vec::new();
    let mut chrome = false;
    let mut dark = false;

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
                steps.push(Step::Click(x.parse().map_err(|_| USAGE)?, y.parse().map_err(|_| USAGE)?));
                Ok(())
            }),
            "--type" => value().map(|v| steps.push(Step::Type(v))),
            "--chrome" => {
                chrome = true;
                Ok(())
            }
            "--dark" => {
                dark = true;
                Ok(())
            }
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

    // Screenshots show just the page at `location` unless asked for the
    // chrome; the window always shows it.
    match screenshot {
        Some(out) => {
            let mut browser = Browser::new(location, chrome);
            browser.set_dark_mode(dark);
            match write_screenshot(browser, width, height, scroll, &steps, &out) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("screenshot failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        None => {
            window::run(Browser::new(location, true));
            ExitCode::SUCCESS
        }
    }
}

/// Something to do to the page before taking a screenshot.
enum Step {
    Click(f32, f32),
    /// Each character is a key press; `\n` is Enter, `\t` is Tab and `\x08`
    /// (`\b` in a shell's `$'...'`) is Backspace.
    Type(String),
}

/// Renders the browser's page to a PNG, after carrying out `steps` as the
/// window would.
fn write_screenshot(mut browser: Browser, w: usize, h: usize, scroll: f32, steps: &[Step], out: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut buf = vec![0u32; w * h];
    browser.set_scroll(scroll);
    let keys = |text: &str| -> Vec<Key> {
        let key = |c| match c {
            '\n' => Key::Enter,
            '\t' => Key::Tab,
            '\x08' => Key::Backspace,
            c => Key::Text(String::from(c)),
        };
        text.chars().map(key).collect()
    };
    for step in steps {
        match step {
            Step::Click(x, y) => {
                browser.render(&mut buf, w, h, 1.0);
                if !browser.click(*x, *y) {
                    return Err(format!("nothing to click at {x},{y}").into());
                }
            }
            Step::Type(text) => {
                for key in keys(text) {
                    browser.render(&mut buf, w, h, 1.0);
                    browser.key(key, false, (0.0, 0.0));
                }
            }
        }
    }
    browser.render(&mut buf, w, h, 1.0);
    let rgb: Vec<u8> = buf.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(fs::File::create(out)?), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgb)?;
    Ok(())
}

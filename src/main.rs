mod browser;
mod canvas;
mod damage;
mod dom;
mod highlight;
mod image;
mod layout;
mod lisp;
mod loader;
mod paint;
mod settings;
mod style;
#[cfg(test)]
mod test_server;
mod window;

use std::{fs, process::ExitCode};

use browser::{Browser, Key};

const USAGE: &str = "usage: toy-browser [PAGE.sx] [--screenshot OUT.png] [--width N] [--height N] [--scroll N] [--chrome] [--dark] [--click X,Y | --hover X,Y | --drag X,Y,X,Y | --type TEXT | --find TEXT | --repl TEXT | --frames N | --key-down KEY | --key-up KEY]...";

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
            "--hover" => value().and_then(|v| {
                let (x, y) = v.split_once(',').ok_or(USAGE)?;
                steps.push(Step::Hover(x.parse().map_err(|_| USAGE)?, y.parse().map_err(|_| USAGE)?));
                Ok(())
            }),
            "--drag" => value().and_then(|v| {
                let n: Vec<f32> = v.split(',').map(str::parse).collect::<Result<_, _>>().map_err(|_| USAGE)?;
                let [x0, y0, x1, y1] = n[..] else { return Err(USAGE) };
                steps.push(Step::Drag((x0, y0), (x1, y1)));
                Ok(())
            }),
            "--type" => value().map(|v| steps.push(Step::Type(v))),
            "--find" => value().map(|v| steps.push(Step::Find(v))),
            "--repl" => value().map(|v| steps.push(Step::Repl(v))),
            "--key-down" => value().map(|v| steps.push(Step::KeyDown(v))),
            "--key-up" => value().map(|v| steps.push(Step::KeyUp(v))),
            "--frames" => value().and_then(|v| v.parse().map(|n| steps.push(Step::Frames(n))).map_err(|_| USAGE)),
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
        // Screenshots wait for downloads; the window doesn't.
        None => {
            window::run(|wake| {
                let mut browser = Browser::loading_in_background(location, true, wake);
                // Only the window keeps settings, so screenshots don't depend on them.
                if let Some(settings) = settings::Settings::standard() {
                    browser.keep_settings(settings);
                }
                browser
            });
            ExitCode::SUCCESS
        }
    }
}

/// Something to do to the page before taking a screenshot.
enum Step {
    Click(f32, f32),
    /// The pointer moves to a point, and stays there for the screenshot.
    Hover(f32, f32),
    /// Press at one point, move to the other and release, as in selecting text.
    Drag((f32, f32), (f32, f32)),
    /// Each character is a key press; `\n` is Enter, `\t` is Tab and `\x08`
    /// (`\b` in a shell's `$'...'`) is Backspace.
    Type(String),
    /// Ctrl+F, then typing, as `Type` does.
    Find(String),
    /// Ctrl+Shift+J, then typing, as `Type` does.
    Repl(String),
    /// This many frames of animation, each a 60th of a second after the last.
    Frames(u32),
    /// A key going down or up, by its name, for the canvas that has the
    /// keyboard: click it first.
    KeyDown(String),
    KeyUp(String),
}

/// Renders the browser's page to a PNG, after carrying out `steps` as the
/// window would.
fn write_screenshot(
    mut browser: Browser,
    w: usize,
    h: usize,
    scroll: f32,
    steps: &[Step],
    out: &str,
) -> Result<(), Box<dyn std::error::Error>> {
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
                browser.render(&mut buf, w, h, 1.0, 0);
                if !browser.click(*x, *y) {
                    return Err(format!("nothing to click at {x},{y}").into());
                }
            }
            Step::Hover(x, y) => {
                // Laid out first, so it knows what's at that point.
                browser.render(&mut buf, w, h, 1.0, 0);
                browser.point_at(Some((*x, *y)));
            }
            Step::Drag((x0, y0), (x1, y1)) => {
                browser.render(&mut buf, w, h, 1.0, 0);
                browser.press(*x0, *y0, 1);
                browser.drag_to(*x1, *y1);
                browser.release(*x1, *y1);
            }
            Step::KeyDown(name) | Step::KeyUp(name) => {
                browser.render(&mut buf, w, h, 1.0, 0);
                if !browser.listening() {
                    return Err(format!("no canvas has the keyboard for {name:?}: click one that listens for keys first").into());
                }
                match step {
                    Step::KeyDown(_) => browser.key_down(name, false),
                    _ => browser.key_up(name),
                }
            }
            Step::Frames(n) => {
                for _ in 0..*n {
                    browser.render(&mut buf, w, h, 1.0, 0);
                    browser.frame(browser::FRAME);
                }
            }
            Step::Type(text) | Step::Find(text) | Step::Repl(text) => {
                let opening = match step {
                    Step::Find(_) => Some(Key::Find),
                    Step::Repl(_) => Some(Key::Repl),
                    _ => None,
                };
                if let Some(key) = opening {
                    browser.render(&mut buf, w, h, 1.0, 0);
                    browser.key(key, false, (0.0, 0.0));
                }
                for key in keys(text) {
                    browser.render(&mut buf, w, h, 1.0, 0);
                    browser.key(key, false, (0.0, 0.0));
                }
            }
        }
    }
    browser.render(&mut buf, w, h, 1.0, 0);
    let rgb: Vec<u8> = buf.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(fs::File::create(out)?), w as u32, h as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgb)?;
    Ok(())
}

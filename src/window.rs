//! The browser window: turns window events into calls on a `Browser`.

use std::{
    num::NonZeroU32,
    rc::Rc,
    time::{Duration, Instant},
};

use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key as WinitKey, ModifiersState, NamedKey},
    window::{CursorIcon, Icon, Theme, Window, WindowAttributes, WindowId},
};

use crate::{
    browser::{Browser, Hover, Key, LINE_HEIGHT},
    image::Image,
};

/// What desktops know the browser as. On Linux, taskbars and launchers find
/// a window's icon through the desktop entry with this name.
const APP_ID: &str = "netscheme";

/// Presses closer together than this, in time and space, count as a double
/// or triple click.
const MULTI_CLICK_TIME: Duration = Duration::from_millis(400);
const MULTI_CLICK_DISTANCE: f32 = 4.0;

pub fn run(browser: Browser) {
    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App {
        browser,
        pointer: (0.0, 0.0),
        modifiers: ModifiersState::empty(),
        last_press: None,
        state: None,
    };
    event_loop.run_app(&mut app).expect("run event loop");
}

struct App {
    browser: Browser,
    pointer: (f32, f32),
    modifiers: ModifiersState,
    /// When and where the left button last went down, and how many quick presses that made.
    last_press: Option<(Instant, (f32, f32), u32)>,
    state: Option<WindowState>,
}

struct WindowState {
    window: Rc<Window>,
    _context: softbuffer::Context<Rc<Window>>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
}

impl App {
    fn redraw(&self) {
        if let Some(state) = &self.state {
            state.window.set_title(&self.browser.title());
            // Keep the title bar in step with the browser's setting.
            let theme = if self.browser.dark_mode() { Theme::Dark } else { Theme::Light };
            if state.window.theme() != Some(theme) {
                state.window.set_theme(Some(theme));
            }
            state.window.request_redraw();
        }
    }

    fn update_cursor(&self) {
        if let Some(state) = &self.state {
            state.window.set_cursor(match self.browser.hover(self.pointer.0, self.pointer.1) {
                Hover::Nothing => CursorIcon::Default,
                Hover::Link => CursorIcon::Pointer,
                Hover::Text => CursorIcon::Text,
            });
        }
    }

    /// Counts this press of the left button: 1, or more for a quick
    /// succession in the same place.
    fn count_press(&mut self) -> u32 {
        let now = Instant::now();
        let (x, y) = self.pointer;
        let clicks = match self.last_press {
            Some((time, (px, py), clicks))
                if now - time < MULTI_CLICK_TIME
                    && (x - px).abs() <= MULTI_CLICK_DISTANCE
                    && (y - py).abs() <= MULTI_CLICK_DISTANCE =>
            {
                clicks + 1
            }
            _ => 1,
        };
        self.last_press = Some((now, self.pointer, clicks));
        clicks
    }

    fn key(&self, event: &KeyEvent) -> Option<Key> {
        let m = self.modifiers;
        if m.alt_key() && event.logical_key == WinitKey::Named(NamedKey::ArrowLeft) {
            return Some(Key::Back);
        }
        Some(match &event.logical_key {
            WinitKey::Named(NamedKey::ArrowLeft) => Key::Left,
            WinitKey::Named(NamedKey::ArrowRight) => Key::Right,
            WinitKey::Named(NamedKey::ArrowUp) => Key::Up,
            WinitKey::Named(NamedKey::ArrowDown) => Key::Down,
            WinitKey::Named(NamedKey::Home) => Key::Home,
            WinitKey::Named(NamedKey::End) => Key::End,
            WinitKey::Named(NamedKey::PageUp) => Key::PageUp,
            WinitKey::Named(NamedKey::PageDown) => Key::PageDown,
            WinitKey::Named(NamedKey::Backspace) => Key::Backspace,
            WinitKey::Named(NamedKey::Delete) => Key::Delete,
            WinitKey::Named(NamedKey::Enter) => Key::Enter,
            WinitKey::Named(NamedKey::Escape) => Key::Escape,
            WinitKey::Named(NamedKey::Tab) => Key::Tab,
            WinitKey::Character(c) if m.control_key() && c.eq_ignore_ascii_case("l") => Key::FocusAddressBar,
            WinitKey::Character(c) if m.control_key() && c.eq_ignore_ascii_case("a") => Key::SelectAll,
            WinitKey::Character(c) if m.control_key() && c.eq_ignore_ascii_case("r") => Key::Reload,
            WinitKey::Named(NamedKey::F5) => Key::Reload,
            WinitKey::Character(c) if m.control_key() && c.eq_ignore_ascii_case("f") => Key::Find,
            WinitKey::Character(c) if m.control_key() && c.eq_ignore_ascii_case("g") => Key::FindNext,
            WinitKey::Named(NamedKey::F3) => Key::FindNext,
            WinitKey::Character(c) if m.control_key() && m.shift_key() && c.eq_ignore_ascii_case("j") => Key::Repl,
            // Other shortcuts aren't typing.
            _ if m.control_key() || m.alt_key() || m.super_key() => return None,
            _ => {
                let text = event.text.as_ref()?;
                if text.is_empty() || text.chars().any(char::is_control) {
                    return None;
                }
                Key::Text(text.to_string())
            }
        })
    }
}

/// Names the window, and gives it the logo as its icon where the platform
/// takes one from the window: Windows and X11, but not Wayland or macOS.
fn with_identity(attrs: WindowAttributes, event_loop: &ActiveEventLoop) -> WindowAttributes {
    #[cfg(all(unix, not(target_os = "macos")))]
    let attrs = {
        use winit::platform::wayland::{ActiveEventLoopExtWayland, WindowAttributesExtWayland};
        // This sets the app ID on Wayland, and the window's class on X11.
        let attrs = attrs.with_name(APP_ID, APP_ID);
        if event_loop.is_wayland() {
            return attrs;
        }
        attrs
    };
    let _ = event_loop;
    attrs.with_window_icon(logo_icon())
}

/// The logo at 256 × 256, the largest size window icons are usually shown at.
fn logo_icon() -> Option<Icon> {
    const SIZE: u32 = 256;
    let logo = Image::decode_png(include_bytes!("../assets/images/logo.png")).ok()?;
    // Icons take straight RGBA, not premultiplied.
    let scaled = logo.scaled(SIZE, SIZE);
    let rgba = scaled
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| {
            let unmultiply = |c: u8| if a == 0 { 0 } else { (c as u32 * 255 / a as u32).min(255) as u8 };
            [unmultiply(r), unmultiply(g), unmultiply(b), a]
        })
        .collect();
    Icon::from_rgba(rgba, SIZE, SIZE).ok()
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title(self.browser.title()).with_inner_size(LogicalSize::new(900.0, 700.0));
        let attrs = with_identity(attrs, event_loop);
        let window = Rc::new(event_loop.create_window(attrs).expect("create window"));
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let surface = softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        // Start in the system's theme, if it says.
        if let Some(theme) = window.theme() {
            self.browser.set_dark_mode(theme == Theme::Dark);
        }
        self.state = Some(WindowState { window, _context: context, surface });
        self.redraw();
    }

    /// Wakes up when the caret should blink.
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            self.redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(match self.browser.next_blink() {
            Some(time) => ControlFlow::WaitUntil(time),
            None => ControlFlow::Wait,
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = &mut self.state else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => state.window.request_redraw(),
            WindowEvent::RedrawRequested => {
                let size = state.window.inner_size();
                let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
                    return;
                };
                state.surface.resize(w, h).expect("resize surface");
                let mut buffer = state.surface.buffer_mut().expect("surface buffer");
                let scale = state.window.scale_factor() as f32;
                self.browser.render(&mut buffer, size.width as usize, size.height as usize, scale);
                buffer.present().expect("present buffer");
                // Scrolling and re-running the page move things under a still pointer.
                self.update_cursor();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = (position.x as f32, position.y as f32);
                self.update_cursor();
                if self.browser.drag_to(self.pointer.0, self.pointer.1) {
                    self.redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    // Mouse wheels move in notches.
                    MouseScrollDelta::LineDelta(_, lines) => -lines * 3.0 * LINE_HEIGHT * state.window.scale_factor() as f32,
                    // Trackpads report exact physical pixels.
                    MouseScrollDelta::PixelDelta(position) => -position.y as f32,
                };
                self.browser.scroll_by(self.pointer, dy);
                self.redraw();
            }
            WindowEvent::MouseInput { state: button_state, button, .. } => {
                let (x, y) = self.pointer;
                match (button, button_state) {
                    (MouseButton::Left, ElementState::Pressed) => {
                        let clicks = self.count_press();
                        self.browser.press(x, y, clicks);
                    }
                    (MouseButton::Left, ElementState::Released) => {
                        self.browser.release(x, y);
                    }
                    (MouseButton::Back, ElementState::Released) => self.browser.back(),
                    _ => return,
                }
                self.redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::ThemeChanged(theme) => {
                self.browser.set_dark_mode(theme == Theme::Dark);
                self.redraw();
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Some(key) = self.key(&event) {
                    self.browser.key(key, self.modifiers.shift_key(), self.pointer);
                    self.redraw();
                }
            }
            _ => {}
        }
    }
}

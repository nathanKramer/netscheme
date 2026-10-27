//! The browser window: turns window events into calls on a `Browser`.

use std::{
    num::NonZeroU32,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use softbuffer::Rect;
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{Key as WinitKey, ModifiersState, NamedKey},
    platform::modifier_supplement::KeyEventExtModifierSupplement,
    window::{CursorIcon, Theme, Window, WindowAttributes, WindowId},
};

use crate::{
    browser::{Browser, Hover, Key, LINE_HEIGHT},
    layout::Clip,
    loader::Wake,
};

// Only the window icon, which macOS hasn't got, needs these.
#[cfg(not(target_os = "macos"))]
use {crate::image::Image, winit::window::Icon};

/// What desktops know the browser as. On Linux, taskbars and launchers find
/// a window's icon through the desktop entry with this name.
#[cfg(all(unix, not(target_os = "macos")))]
const APP_ID: &str = "netscheme";

/// Presses closer together than this, in time and space, count as a double
/// or triple click.
const MULTI_CLICK_TIME: Duration = Duration::from_millis(400);
const MULTI_CLICK_DISTANCE: f32 = 4.0;

/// Opens a window on the browser that `browser` makes, which should load in
/// the background, waking the window with what it's given.
pub fn run(browser: impl FnOnce(Wake) -> Browser) {
    let event_loop = EventLoop::<Loaded>::with_user_event().build().expect("create event loop");
    let proxy: EventLoopProxy<Loaded> = event_loop.create_proxy();
    let mut app = App {
        browser: browser(Arc::new(move || _ = proxy.send_event(Loaded))),
        pointer: (0.0, 0.0),
        modifiers: ModifiersState::empty(),
        last_press: None,
        // Kept for the whole session: on X11, copied text is only available while it lives.
        clipboard: arboard::Clipboard::new().map_err(|e| eprintln!("no clipboard: {e}")).ok(),
        occluded: false,
        state: None,
    };
    event_loop.run_app(&mut app).expect("run event loop");
}

/// Sent from another thread when a download has finished.
struct Loaded;

struct App {
    browser: Browser,
    pointer: (f32, f32),
    modifiers: ModifiersState,
    /// When and where the left button last went down, and how many quick presses that made.
    last_press: Option<(Instant, (f32, f32), u32)>,
    clipboard: Option<arboard::Clipboard>,
    /// Whether the window is hidden from view, as when minimised.
    occluded: bool,
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
                if now - time < MULTI_CLICK_TIME && (x - px).abs() <= MULTI_CLICK_DISTANCE && (y - py).abs() <= MULTI_CLICK_DISTANCE =>
            {
                clicks + 1
            }
            _ => 1,
        };
        self.last_press = Some((now, self.pointer, clicks));
        clicks
    }

    /// Puts the selected text on the clipboard.
    fn copy(&mut self) {
        if let (Some(text), Some(clipboard)) = (self.browser.selected_text(), &mut self.clipboard)
            && let Err(e) = clipboard.set_text(text)
        {
            eprintln!("couldn't copy: {e}");
        }
    }

    /// Types the clipboard's text into the focused input.
    fn paste(&mut self) {
        let Some(clipboard) = &mut self.clipboard else { return };
        match clipboard.get_text() {
            Ok(text) => {
                self.browser.paste(&text);
                self.redraw();
            }
            Err(e) => eprintln!("couldn't paste: {e}"),
        }
    }

    /// Whether `event` is Ctrl and `letter` going down.
    fn is_ctrl(&self, event: &KeyEvent, letter: &str) -> bool {
        event.state == ElementState::Pressed
            && self.modifiers.control_key()
            && matches!(&event.logical_key, WinitKey::Character(c) if c.eq_ignore_ascii_case(letter))
    }

    /// Whether a key press is for the browser even while a canvas has the
    /// keyboard: with Ctrl, Alt or Super held, Escape, and function keys.
    fn is_browser_key(&self, event: &KeyEvent) -> bool {
        let m = self.modifiers;
        m.control_key()
            || m.alt_key()
            || m.super_key()
            || matches!(
                &event.logical_key,
                WinitKey::Named(
                    NamedKey::Escape
                        | NamedKey::F1
                        | NamedKey::F2
                        | NamedKey::F3
                        | NamedKey::F4
                        | NamedKey::F5
                        | NamedKey::F6
                        | NamedKey::F7
                        | NamedKey::F8
                        | NamedKey::F9
                        | NamedKey::F10
                        | NamedKey::F11
                        | NamedKey::F12
                )
            )
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

/// The name pages know a key by: the web's `KeyboardEvent.key`, but as if
/// Shift weren't held, so that a key goes up with the name it went down with.
fn key_name(event: &KeyEvent) -> Option<String> {
    match event.key_without_modifiers() {
        WinitKey::Named(NamedKey::Space) => Some(" ".into()),
        WinitKey::Named(named) => Some(format!("{named:?}")),
        WinitKey::Character(c) => Some(c.to_string()),
        _ => None,
    }
}

/// Names the window, and gives it the logo as its icon where the platform
/// takes one from the window: Windows and X11, but not Wayland or macOS.
///
/// Those two want the icon from somewhere else: Wayland from a desktop entry
/// named `APP_ID`, macOS from the running application, in `set_dock_icon`.
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
    #[cfg(target_os = "macos")]
    return attrs;
    #[cfg(not(target_os = "macos"))]
    attrs.with_window_icon(logo_icon())
}

/// The logo at 256 × 256, the largest size window icons are usually shown at.
#[cfg(not(target_os = "macos"))]
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

/// Gives the Dock and the app switcher the logo, which on macOS belongs to the
/// application rather than to any window. Without a bundle to read an icon
/// from, as when the browser is started with `cargo run`, this is the only
/// thing that sets it.
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    // The event loop runs on the main thread, so `resumed` is called on it.
    let Some(mtm) = MainThreadMarker::new() else { return };
    // NSImage decodes the PNG itself, so the logo goes in as it is on disk.
    let data = NSData::with_bytes(include_bytes!("../assets/images/logo.png"));
    let Some(logo) = NSImage::initWithData(NSImage::alloc(), &data) else {
        eprintln!("no dock icon: the logo wouldn't decode");
        return;
    };
    // SAFETY: an NSImage of the application's own icon, set on the main thread.
    unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&logo)) };
}

impl ApplicationHandler<Loaded> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title(self.browser.title()).with_inner_size(LogicalSize::new(900.0, 700.0));
        let attrs = with_identity(attrs, event_loop);
        #[cfg(target_os = "macos")]
        set_dock_icon();
        let window = Rc::new(event_loop.create_window(attrs).expect("create window"));
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let surface = softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        // Start in the system's theme, if it says, unless the user chose one.
        if let Some(theme) = window.theme()
            && !self.browser.dark_mode_chosen()
        {
            self.browser.set_dark_mode(theme == Theme::Dark);
        }
        self.state = Some(WindowState { window, _context: context, surface });
        self.redraw();
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, Loaded: Loaded) {
        if self.browser.loaded() {
            self.redraw();
        }
    }

    /// Wakes up when the caret should blink, or the page's animation is due a frame.
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause {
            self.browser.animate(Instant::now());
            self.redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // A window that can't be seen doesn't animate, and starts again from
        // where it was when it's back.
        let frame = if self.occluded { None } else { self.browser.next_frame() };
        let wake = [self.browser.next_blink(), frame].into_iter().flatten().min();
        event_loop.set_control_flow(match wake {
            Some(time) => ControlFlow::WaitUntil(time),
            None => ControlFlow::Wait,
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = &mut self.state else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Occluded(occluded) => self.occluded = occluded,
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => state.window.request_redraw(),
            WindowEvent::RedrawRequested => {
                let size = state.window.inner_size();
                let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
                    return;
                };
                state.surface.resize(w, h).expect("resize surface");
                let mut buffer = state.surface.buffer_mut().expect("surface buffer");
                let scale = state.window.scale_factor() as f32;
                let age = buffer.age();
                match self.browser.render(&mut buffer, size.width as usize, size.height as usize, scale, age) {
                    // Only the parts that changed need sending to the compositor.
                    Some(areas) => {
                        let rects: Vec<Rect> = areas.iter().filter_map(|area| to_rect(*area)).collect();
                        buffer.present_with_damage(&rects).expect("present buffer");
                    }
                    None => buffer.present().expect("present buffer"),
                }
                // Scrolling and re-running the page move things under a still pointer.
                self.update_cursor();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = (position.x as f32, position.y as f32);
                self.update_cursor();
                let hover_changed = self.browser.point_at(Some(self.pointer));
                if self.browser.drag_to(self.pointer.0, self.pointer.1) || hover_changed {
                    self.redraw();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if self.browser.point_at(None) {
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
            WindowEvent::ThemeChanged(theme) if !self.browser.dark_mode_chosen() => {
                self.browser.set_dark_mode(theme == Theme::Dark);
                self.redraw();
            }
            WindowEvent::Focused(false) => {
                self.browser.release_keys();
                self.redraw();
            }
            // Releases always go to the canvas, so a key can't stay down for it
            // because a modifier went down while it was held.
            WindowEvent::KeyboardInput { event, .. }
                if self.browser.listening() && (event.state == ElementState::Released || !self.is_browser_key(&event)) =>
            {
                if let Some(name) = key_name(&event) {
                    match event.state {
                        ElementState::Pressed => self.browser.key_down(&name, event.repeat),
                        ElementState::Released => self.browser.key_up(&name),
                    }
                    self.redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } if self.is_ctrl(&event, "c") => self.copy(),
            WindowEvent::KeyboardInput { event, .. } if self.is_ctrl(&event, "v") => self.paste(),
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

/// A painted area as softbuffer's rectangle, or `None` if it's empty.
fn to_rect(area: Clip) -> Option<Rect> {
    let (width, height) = (NonZeroU32::new((area.x1 - area.x0) as u32)?, NonZeroU32::new((area.y1 - area.y0) as u32)?);
    Some(Rect { x: area.x0 as u32, y: area.y0 as u32, width, height })
}
